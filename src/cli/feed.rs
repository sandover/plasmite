//! Purpose: Execute feed ingestion and exact fetch commands.
//! Exports: `FeedArgs`, `run`, `fetch`.
//! Role: Adapt local and remote targets to the existing shared ingestion path.

use super::context::CliContext;
use super::output::emit_json;
use super::result::CommandResult;
use crate::{
    DEFAULT_POOL_SIZE, ErrorPolicyCli, FeedExactCreateHint, FeedIngestContext, InputMode,
    PoolTarget, RemoteFeedIngestContext, add_missing_pool_create_hint, add_missing_pool_hint,
    add_missing_seq_hint, emit_feed_receipt, ensure_pool_dir, feed_exact_create_command_hint,
    feed_receipt_from_message, feed_receipt_json, ingest_from_stdin, ingest_from_stdin_remote,
    message_from_frame, missing_feed_data_error, now_ns, open_feed_reader, parse_durability,
    parse_inline_json, parse_retry_config, parse_size, reject_remote_only_flags_for_local_target,
    resolve_pool_target, resolve_poolref, resolve_token_value, retry_with_config,
};
use plasmite::api::{
    AppendOptions, Error, ErrorKind, Pool, PoolOptions, PoolRef, RemoteClient, lite3,
};
use std::io::{self, IsTerminal};
use std::path::PathBuf;

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
    pub(super) token: Option<String>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) tls_ca: Option<PathBuf>,
    pub(super) tls_skip_verify: bool,
}

pub(super) fn run(args: FeedArgs, context: &CliContext) -> Result<CommandResult, Error> {
    let pool_dir = context.pool_dir();
    let target = resolve_pool_target(&args.pool, pool_dir)?;
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
            reject_remote_only_flags_for_local_target(
                "feed",
                args.token.as_deref(),
                args.token_file.as_deref(),
                args.tls_ca.as_deref(),
                args.tls_skip_verify,
            )?;
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
            if let Some(data) = args.data.as_deref() {
                let data = parse_inline_json(data)?;
                let payload = lite3::encode_message(&args.tags, &data)?;
                let (seq, timestamp_ns) = retry_with_config(retry_config, || {
                    let timestamp_ns = now_ns()?;
                    let options = AppendOptions::new(timestamp_ns, durability);
                    let seq = pool_handle.append_with_options(payload.as_slice(), options)?;
                    Ok((seq, timestamp_ns))
                })?;
                emit_feed_receipt(
                    feed_receipt_json(seq, timestamp_ns, &args.tags)?,
                    context.color_mode(),
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
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("remote feed does not support --create")
                    .with_hint("Create remote pools with server-side tooling, not feed."));
            }
            let token_value = resolve_token_value(args.token, args.token_file)?;
            let mut client = RemoteClient::new(base_url)?;
            if let Some(token_value) = token_value {
                client = client.with_token(token_value);
            }
            if let Some(path) = args.tls_ca {
                client = client.with_tls_ca_file(path)?;
            }
            if args.tls_skip_verify {
                eprintln!(
                    "warning: --tls-skip-verify disables TLS certificate verification (unsafe)"
                );
                client = client.with_tls_skip_verify();
            }
            let remote_pool = client
                .open_pool(&PoolRef::name(name.clone()))
                .map_err(|err| add_missing_pool_hint(err, &args.pool, &args.pool))?;
            if let Some(data) = args.data.as_deref() {
                let data = parse_inline_json(data)?;
                let message = retry_with_config(retry_config, || {
                    remote_pool.append_json_now(&data, &args.tags, durability)
                })?;
                emit_feed_receipt(feed_receipt_from_message(&message), context.color_mode());
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

pub(super) fn fetch(pool: &str, seq: u64, context: &CliContext) -> Result<CommandResult, Error> {
    let path = resolve_poolref(pool, context.pool_dir())?;
    let pool_handle = Pool::open(&path).map_err(|err| add_missing_pool_hint(err, pool, pool))?;
    let frame = pool_handle
        .get(seq)
        .map_err(|err| add_missing_seq_hint(err, pool))?;
    emit_json(message_from_frame(&frame)?, context.color_mode());
    Ok(CommandResult::ok())
}
