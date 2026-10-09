//! Purpose: Execute feed ingestion and exact fetch commands.
//! Exports: `FeedArgs`, `run`, `fetch`.
//! Role: Adapt local and remote targets to the existing shared ingestion path.

use super::args::FetchFormat;
use super::context::CliContext;
use super::feed_support::{
    FeedIngestContext, RemoteFeedIngestContext, ingest_from_stdin, ingest_from_stdin_remote,
    missing_feed_data_error, open_feed_reader, parse_inline_json,
};
use super::output_support::{emit_feed_receipt, emit_message};
use super::result::CommandResult;
use super::support::{
    DEFAULT_POOL_SIZE, FeedExactCreateHint, add_missing_pool_create_hint, add_missing_pool_hint,
    add_missing_seq_hint, ensure_pool_dir, feed_exact_create_command_hint,
    feed_receipt_from_message, feed_receipt_json, message_from_frame, message_to_json, now_ns,
    parse_durability, parse_retry_config, parse_size, remote_client, resolve_pool_target,
    retry_append,
};
use crate::{ErrorPolicyCli, InputMode, PoolTarget};
use plasmite::api::{
    AppendOptions, Error, ErrorKind, Pool, PoolApiExt, PoolOptions, PoolRef, lite3,
};
use std::io::{self, IsTerminal, Read, Write};

pub(super) struct FeedArgs {
    pub(super) pool: String,
    pub(super) tags: Vec<String>,
    pub(super) data: Option<String>,
    pub(super) file: Option<String>,
    pub(super) durability: String,
    pub(super) create: bool,
    pub(super) create_size: Option<String>,
    pub(super) retry: u32,
    pub(super) retry_delay: Option<String>,
    pub(super) input: InputMode,
    pub(super) errors: ErrorPolicyCli,
}

pub(super) fn run(args: FeedArgs, context: &CliContext) -> Result<CommandResult, Error> {
    let pool_dir = context.pool_dir();
    let target = resolve_pool_target(&args.pool, pool_dir)?;
    if args.create && matches!(target, PoolTarget::Remote { .. }) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote feed does not support --create")
            .with_hint("Create remote pools with server-side tooling, not feed."));
    }
    if args.create_size.is_some() && !args.create {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--create-size requires --create")
            .with_hint("Add --create or remove --create-size."));
    }
    if args.retry_delay.is_some() && args.retry == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--retry-delay requires --retry")
            .with_hint("Add --retry or remove --retry-delay."));
    }
    let durability = parse_durability(&args.durability)?;
    let retry_config = parse_retry_config(args.retry, args.retry_delay.as_deref())?;
    if args.data.is_some() && args.file.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("multiple data inputs provided")
            .with_hint("Use only one of DATA, --file, or stdin."));
    }
    let file = args.file.as_deref();
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdin_stream = args.data.is_none() && file.is_none() && !stdin_is_terminal;
    let single_input = args.data.is_some() || file.is_some() || stdin_is_terminal;
    let binary_input = if args.input == InputMode::Lite3 {
        if args.data.is_some() || !args.tags.is_empty() || args.errors == ErrorPolicyCli::Skip {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("--in lite3 rejects inline DATA, --tag, and --errors skip")
                .with_hint("Read one complete Lite3 message from --file or piped stdin."));
        }
        let reader = if let Some(file) = file {
            open_feed_reader(file)?
        } else if stdin_stream {
            Box::new(io::stdin())
        } else {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("missing Lite3 input")
                .with_hint("Use --file PATH, --file -, or pipe Lite3 bytes to stdin."));
        };
        Some(read_lite3_input(reader, lite3::MAX_LITE3_BUF)?)
    } else {
        None
    };
    let exact_create_hint = feed_exact_create_command_hint(
        &args.pool,
        FeedExactCreateHint {
            tags: &args.tags,
            data: &args.data,
            file: &args.file,
            durability,
            retry: args.retry,
            retry_delay: args.retry_delay.as_deref(),
            input: args.input,
            errors: args.errors,
            single_input,
        },
    );

    match target {
        PoolTarget::LocalPath(path) => {
            let mut pool_handle = match Pool::open(&path) {
                Ok(pool) => pool,
                Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(pool_dir)?;
                    let size = args
                        .create_size
                        .as_deref()
                        .map(parse_size)
                        .transpose()?
                        .unwrap_or(DEFAULT_POOL_SIZE);
                    Pool::create(&path, PoolOptions::new(size))?
                }
                Err(err) => {
                    return Err(add_missing_pool_create_hint(
                        err,
                        "feed",
                        &args.pool,
                        &args.pool,
                        exact_create_hint,
                    ));
                }
            };
            if let Some(payload) = &binary_input {
                let message = retry_append(retry_config, || {
                    pool_handle.append_lite3_with_receipt_now(payload, durability)
                })?;
                emit_feed_receipt(
                    feed_receipt_from_message(&message),
                    context.color_mode(),
                    context.json_output(),
                );
            } else if let Some(data) = args.data.as_deref() {
                let data = parse_inline_json(data)?;
                let payload = lite3::encode_message(&args.tags, &data)?;
                let (seq, timestamp_ns) = retry_append(retry_config, || {
                    let timestamp_ns = now_ns()?;
                    let options = AppendOptions::new(timestamp_ns, durability);
                    let seq = pool_handle.append_with_options(payload.as_slice(), options)?;
                    Ok((seq, timestamp_ns))
                })?;
                emit_feed_receipt(
                    feed_receipt_json(seq, timestamp_ns, &args.tags)?,
                    context.color_mode(),
                    context.json_output(),
                );
            } else {
                let pool_path_label = path.display().to_string();
                let outcome = if let Some(file) = file {
                    let reader = open_feed_reader(file)?;
                    ingest_from_stdin(
                        reader,
                        FeedIngestContext {
                            pool_ref: &args.pool,
                            pool_path_label: &pool_path_label,
                            tags: &args.tags,
                            durability,
                            retry_config,
                            pool_handle: &mut pool_handle,
                            color_mode: context.color_mode(),
                            json_output: context.json_output(),
                            input: args.input,
                            errors: args.errors,
                        },
                        true,
                    )?
                } else if stdin_stream {
                    ingest_from_stdin(
                        io::stdin().lock(),
                        FeedIngestContext {
                            pool_ref: &args.pool,
                            pool_path_label: &pool_path_label,
                            tags: &args.tags,
                            durability,
                            retry_config,
                            pool_handle: &mut pool_handle,
                            color_mode: context.color_mode(),
                            json_output: context.json_output(),
                            input: args.input,
                            errors: args.errors,
                        },
                        true,
                    )?
                } else {
                    return Err(missing_feed_data_error());
                };
                if outcome.records_total == 0 {
                    return Err(missing_feed_data_error());
                }
                if outcome.failed > 0 {
                    return Ok(CommandResult::with_code(1));
                }
            }
        }
        PoolTarget::Remote {
            base_url,
            pool: name,
        } => {
            let client = remote_client(base_url)?;
            let remote_pool = client
                .open_pool(&PoolRef::name(name.clone()))
                .map_err(|err| add_missing_pool_hint(err, &args.pool, &args.pool))?;
            if let Some(payload) = &binary_input {
                let message = retry_append(retry_config, || {
                    remote_pool
                        .append_lite3_with_receipt(payload, AppendOptions::new(0, durability))
                })?;
                emit_feed_receipt(
                    feed_receipt_from_message(&message),
                    context.color_mode(),
                    context.json_output(),
                );
            } else if let Some(data) = args.data.as_deref() {
                let data = parse_inline_json(data)?;
                let message = retry_append(retry_config, || {
                    remote_pool.append_json_now(&data, &args.tags, durability)
                })?;
                emit_feed_receipt(
                    feed_receipt_from_message(&message),
                    context.color_mode(),
                    context.json_output(),
                );
            } else {
                let pool_path_label = format!("{}/{}", client.base_url(), name);
                let outcome = if let Some(file) = file {
                    let reader = open_feed_reader(file)?;
                    ingest_from_stdin_remote(
                        reader,
                        RemoteFeedIngestContext {
                            pool_ref: &args.pool,
                            pool_path_label: &pool_path_label,
                            tags: &args.tags,
                            durability,
                            retry_config,
                            remote_pool: &remote_pool,
                            color_mode: context.color_mode(),
                            json_output: context.json_output(),
                            input: args.input,
                            errors: args.errors,
                        },
                        true,
                    )?
                } else if stdin_stream {
                    ingest_from_stdin_remote(
                        io::stdin().lock(),
                        RemoteFeedIngestContext {
                            pool_ref: &args.pool,
                            pool_path_label: &pool_path_label,
                            tags: &args.tags,
                            durability,
                            retry_config,
                            remote_pool: &remote_pool,
                            color_mode: context.color_mode(),
                            json_output: context.json_output(),
                            input: args.input,
                            errors: args.errors,
                        },
                        true,
                    )?
                } else {
                    return Err(missing_feed_data_error());
                };
                if outcome.records_total == 0 {
                    return Err(missing_feed_data_error());
                }
                if outcome.failed > 0 {
                    return Ok(CommandResult::with_code(1));
                }
            }
        }
    }
    Ok(CommandResult::ok())
}

pub(super) fn fetch(
    pool: &str,
    seq: u64,
    format: Option<FetchFormat>,
    context: &CliContext,
) -> Result<CommandResult, Error> {
    if context.json_output() && matches!(format, Some(FetchFormat::Pretty | FetchFormat::Lite3)) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--json conflicts with --format pretty or lite3")
            .with_hint("Use --json or --format json for a JSON envelope."));
    }
    if format == Some(FetchFormat::Lite3) {
        let payload = match resolve_pool_target(pool, context.pool_dir())? {
            PoolTarget::LocalPath(path) => {
                let pool_handle =
                    Pool::open(&path).map_err(|err| add_missing_pool_hint(err, pool, pool))?;
                pool_handle
                    .get_lite3(seq)
                    .map_err(|err| add_missing_seq_hint(err, pool))?
                    .payload
            }
            PoolTarget::Remote {
                base_url,
                pool: name,
            } => {
                let client = remote_client(base_url)?;
                let remote_pool = client
                    .open_pool(&PoolRef::name(name))
                    .map_err(|err| add_missing_pool_hint(err, pool, pool))?;
                remote_pool
                    .get_lite3(seq)
                    .map_err(|err| add_missing_seq_hint(err, pool))?
            }
        };
        lite3::validate_bytes(&payload)?;
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&payload)
            .and_then(|()| stdout.flush())
            .map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to write Lite3 output")
                    .with_source(err)
            })?;
        return Ok(CommandResult::ok());
    }
    let message = match resolve_pool_target(pool, context.pool_dir())? {
        PoolTarget::LocalPath(path) => {
            let pool_handle =
                Pool::open(&path).map_err(|err| add_missing_pool_hint(err, pool, pool))?;
            let frame = pool_handle
                .get(seq)
                .map_err(|err| add_missing_seq_hint(err, pool))?;
            message_from_frame(&frame)?
        }
        PoolTarget::Remote {
            base_url,
            pool: name,
        } => {
            let client = remote_client(base_url)?;
            let remote_pool = client
                .open_pool(&PoolRef::name(name))
                .map_err(|err| add_missing_pool_hint(err, pool, pool))?;
            let message = remote_pool
                .get_message(seq)
                .map_err(|err| add_missing_seq_hint(err, pool))?;
            message_to_json(&message)
        }
    };
    emit_message(message, !context.json_output(), context.color_mode());
    Ok(CommandResult::ok())
}

fn read_lite3_input(reader: impl Read, max_bytes: usize) -> Result<Vec<u8>, Error> {
    let mut payload = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut payload)
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read Lite3 input")
                .with_source(err)
        })?;
    if payload.len() > max_bytes {
        return Err(Error::new(ErrorKind::Usage).with_message("Lite3 input exceeds 256 MiB"));
    }
    if payload.is_empty() {
        return Err(Error::new(ErrorKind::Usage).with_message("empty Lite3 input"));
    }
    lite3::validate_bytes(&payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::read_lite3_input;
    use plasmite::api::{ErrorKind, lite3};
    use serde_json::json;
    use std::io::{self, Cursor, Read};

    #[test]
    fn binary_input_limit_accepts_exact_size_and_rejects_one_extra_byte() {
        let payload = lite3::encode_message(&[], &json!({"x": "y"})).expect("encode");
        let bytes = payload.as_slice();
        assert_eq!(
            read_lite3_input(Cursor::new(bytes), bytes.len()).expect("at limit"),
            bytes
        );
        let over = [bytes, &[0]].concat();
        assert_eq!(
            read_lite3_input(Cursor::new(over), bytes.len())
                .expect_err("over limit")
                .kind(),
            ErrorKind::Usage
        );
    }

    #[test]
    fn binary_input_stops_reading_after_limit_plus_one() {
        struct Endless(usize);
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.0 += buf.len();
                assert!(self.0 <= 17, "reader exceeded bounded input");
                buf.fill(0);
                Ok(buf.len())
            }
        }
        assert_eq!(
            read_lite3_input(Endless(0), 16)
                .expect_err("oversize")
                .kind(),
            ErrorKind::Usage
        );
    }

    #[test]
    fn binary_input_preserves_unused_bytes_in_the_buffer() {
        let payload = lite3::encode_message(&[], &json!({"x": "y"})).expect("encode");
        let mut bytes = payload.as_slice().to_vec();
        bytes.push(0xaa);
        assert_eq!(
            read_lite3_input(Cursor::new(&bytes), bytes.len()).expect("buffer"),
            bytes
        );
    }
}
