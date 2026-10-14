//! Feed and fetch complete messages using the shared local and remote APIs.

use super::context::CliContext;
use super::feed_support::{
    FeedIngestContext, RemoteFeedIngestContext, ingest_from_stdin, ingest_from_stdin_remote,
    message_timestamp, missing_feed_data_error, open_feed_reader,
};
use super::message_stream::InputFormat;
use super::output_support::{emit_lite3, emit_message};
use super::result::CommandResult;
use super::support::{
    DEFAULT_POOL_SIZE, add_missing_pool_create_hint, add_missing_pool_hint, add_missing_seq_hint,
    ensure_pool_dir, message_from_frame, message_to_json, parse_durability, parse_retry_config,
    parse_size, remote_client, resolve_pool_target,
};
use crate::PoolTarget;
use plasmite::api::{Error, ErrorKind, Pool, PoolOptions, PoolRef};
use std::io::{self, IsTerminal, Read};

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
    pub(super) input: InputFormat,
}

pub(super) fn run(mut args: FeedArgs, context: &CliContext) -> Result<CommandResult, Error> {
    args.create |= args.create_size.is_some();
    let target = resolve_pool_target(&args.pool, context.pool_dir())?;
    if args.create && matches!(target, PoolTarget::Remote { .. }) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote feed does not support --create or --create-size")
            .with_hint("Create the pool on the server with pool create."));
    }

    if args.retry_delay.is_some() && args.retry == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--retry-delay requires --retry")
            .with_hint("Add --retry or remove --retry-delay."));
    }
    if context.output_mode() == super::output::OutputMode::Lite3Raw {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("raw Lite3 output requires fetch")
            .with_hint("Use --format lite3 for feed output."));
    }
    if matches!(args.input, InputFormat::Lite3 | InputFormat::Lite3Raw)
        && (!args.tags.is_empty() || args.data.is_some())
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("binary input requires a file or pipe of complete messages")
            .with_hint("Remove --tag and the inline argument for Lite3 input."));
    }
    let durability = parse_durability(&args.durability)?;
    if matches!(target, PoolTarget::Udp { .. })
        && (args.create
            || args.create_size.is_some()
            || args.retry > 0
            || args.retry_delay.is_some()
            || durability != plasmite::api::Durability::Fast)
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("UDP feed does not support create, retry, or flush durability")
            .with_hint(
                "Remove --create, --create-size, --retry, --retry-delay, and --durability flush.",
            ));
    }
    let retry_config = parse_retry_config(args.retry, args.retry_delay.as_deref())?;
    let reader: Box<dyn Read> = if let Some(data) = &args.data {
        Box::new(io::Cursor::new(data.as_bytes().to_vec()))
    } else if let Some(file) = &args.file {
        open_feed_reader(file)?
    } else if !io::stdin().is_terminal() {
        Box::new(io::stdin())
    } else {
        return Err(missing_feed_data_error());
    };
    match target {
        PoolTarget::LocalPath(path) => {
            let mut pool = match Pool::open(&path) {
                Ok(pool) => pool,
                Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(context.pool_dir())?;
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
                        Some(format!(
                            "plasmite pool create {}",
                            super::support::render_shell_agnostic_token(&args.pool)
                        )),
                    ));
                }
            };
            ingest_from_stdin(
                reader,
                FeedIngestContext {
                    tags: &args.tags,
                    durability,
                    retry_config,
                    pool_handle: &mut pool,
                    color_mode: context.color_mode(),
                    output_mode: context.output_mode(),
                    input: args.input,
                    stop: None,
                },
                true,
            )?;
        }
        PoolTarget::Remote {
            base_url,
            pool: name,
        } => {
            let client = remote_client(base_url)?;
            let pool = client
                .open_pool(&PoolRef::name(&name))
                .map_err(|err| add_missing_pool_hint(err, &args.pool, &args.pool))?;
            ingest_from_stdin_remote(
                reader,
                RemoteFeedIngestContext {
                    tags: &args.tags,
                    durability,
                    retry_config,
                    remote_pool: &pool,
                    color_mode: context.color_mode(),
                    output_mode: context.output_mode(),
                    input: args.input,
                    stop: None,
                },
                true,
            )?;
        }
        PoolTarget::Udp {
            endpoint,
            pool: name,
        } => {
            let client = plasmite::api::UdpClient::new(endpoint)?;
            let pool = client.open_pool(&PoolRef::name(name))?;
            super::output_support::emit_notice(
                &plasmite::notice::Notice {
                    kind: "udp-send".into(),
                    time: super::output_support::notice_time_now().unwrap_or_default(),
                    cmd: "feed".into(),
                    pool: args.pool.clone(),
                    message: "UDP sends do not confirm storage; messages may be lost, duplicated, or reordered.\nMaximum UDP datagram size: 65507 bytes, including routing and application framing.".into(),
                    details: serde_json::Map::new(),
                }, context.color_mode(), context.output_mode());
            super::feed_support::ingest_from_stdin_udp(
                reader, &pool, &args.tags, args.input, None,
            )?;
        }
    }
    Ok(CommandResult::ok())
}

pub(super) fn fetch(pool: &str, seq: u64, context: &CliContext) -> Result<CommandResult, Error> {
    match resolve_pool_target(pool, context.pool_dir())? {
        PoolTarget::LocalPath(path) => {
            let handle = Pool::open(&path).map_err(|err| add_missing_pool_hint(err, pool, pool))?;
            let frame = handle
                .get(seq)
                .map_err(|err| add_missing_seq_hint(err, pool))?;
            if context.output_mode().is_binary() {
                emit_lite3(
                    frame.seq,
                    frame.timestamp_ns,
                    &frame.payload,
                    context.output_mode(),
                )?;
            } else {
                emit_message(
                    message_from_frame(&frame)?,
                    context.output_mode(),
                    context.color_mode(),
                )?;
            }
        }
        PoolTarget::Remote {
            base_url,
            pool: name,
        } => {
            let client = remote_client(base_url)?;
            let handle = client
                .open_pool(&PoolRef::name(name))
                .map_err(|err| add_missing_pool_hint(err, pool, pool))?;
            if context.output_mode().is_binary() {
                let ns = if context.output_mode() == super::output::OutputMode::Lite3Raw {
                    0
                } else {
                    message_timestamp(
                        &handle
                            .get_message(seq)
                            .map_err(|err| add_missing_seq_hint(err, pool))?,
                    )?
                };
                let payload = handle
                    .get_lite3(seq)
                    .map_err(|err| add_missing_seq_hint(err, pool))?;
                emit_lite3(seq, ns, &payload, context.output_mode())?;
            } else {
                let message = handle
                    .get_message(seq)
                    .map_err(|err| add_missing_seq_hint(err, pool))?;
                emit_message(
                    message_to_json(&message),
                    context.output_mode(),
                    context.color_mode(),
                )?;
            }
        }
        PoolTarget::Udp { .. } => return Err(super::support::udp_unsupported()),
    }
    Ok(CommandResult::ok())
}
