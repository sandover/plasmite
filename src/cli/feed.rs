//! Feed and fetch complete messages using the shared local and remote APIs.

use super::context::CliContext;
use super::feed_support::{
    FeedIngestContext, RemoteFeedIngestContext, ingest_from_stdin, ingest_from_stdin_remote,
    missing_feed_data_error,
};
use super::message_stream::InputFormat;
use super::output_support::{emit_lite3, emit_message};
use super::result::CommandResult;
use super::support::{
    add_missing_pool_create_hint, add_missing_pool_hint, add_missing_seq_hint, message_from_frame,
    parse_durability, remote_client, resolve_pool_target,
};
use crate::PoolTarget;
use plasmite::api::internal::{LocalPoolTransport, RemotePoolTransport};
use plasmite::api::{Error, ErrorKind, LocalClient, Pool, PoolRef};
use std::io::{self, IsTerminal, Read};

pub(super) struct FeedArgs {
    pub(super) pool: String,
    pub(super) tags: Vec<String>,
    pub(super) data: Option<String>,
    pub(super) durability: String,
    pub(super) create: bool,
    pub(super) input: InputFormat,
}

pub(super) fn run(args: FeedArgs, context: &CliContext) -> Result<CommandResult, Error> {
    let target = resolve_pool_target(&args.pool, context.optional_pool_dir())?;
    if context.output_mode() == super::output::OutputMode::Lite3Raw {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("raw Lite3 output requires fetch")
            .with_hint("Use --format lite3 for feed output."));
    }
    if matches!(args.input, InputFormat::Lite3 | InputFormat::Lite3Raw)
        && (!args.tags.is_empty() || args.data.is_some())
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("binary input requires complete messages on stdin")
            .with_hint("Remove --tag and the inline argument for Lite3 input."));
    }
    let durability = parse_durability(&args.durability)?;
    if matches!(target, PoolTarget::Udp { .. })
        && (args.create || durability != plasmite::api::Durability::Fast)
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("UDP feed does not support --create or flush durability"));
    }
    let reader: Box<dyn Read> = if let Some(data) = &args.data {
        Box::new(io::Cursor::new(data.as_bytes()))
    } else if !io::stdin().is_terminal() {
        Box::new(io::stdin())
    } else {
        return Err(missing_feed_data_error());
    };
    match target {
        PoolTarget::LocalPath(path) => {
            let mut pool = LocalClient::new()
                .open_pool(&PoolRef::path(path), args.create)
                .map_err(add_missing_pool_create_hint)?;
            ingest_from_stdin(
                reader,
                FeedIngestContext {
                    tags: &args.tags,
                    durability,
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
                .open_pool(&PoolRef::name(&name), args.create)
                .map_err(|err| add_missing_pool_hint(err, &args.pool))?;
            ingest_from_stdin_remote(
                reader,
                RemoteFeedIngestContext {
                    tags: &args.tags,
                    durability,
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

pub(super) fn fetch(
    pool: &str,
    seq: Option<u64>,
    since: Option<&str>,
    last: Option<u64>,
    tags: Vec<String>,
    context: &CliContext,
) -> Result<CommandResult, Error> {
    use plasmite::api::{Read as ReadOptions, Since};

    let mode = context.output_mode();
    if mode == super::output::OutputMode::Lite3Raw && seq.is_none() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("lite3-raw requires an exact sequence")
        );
    }
    let options = match (seq, since, last) {
        (Some(seq), _, _) => ReadOptions::since(Since::Seq(seq)),
        (_, Some(since), _) => ReadOptions::since(since.parse()?),
        (_, _, Some(last)) => ReadOptions::last(last),
        _ => ReadOptions::new(),
    }
    .tags(tags);
    let read_error = |error: Error| match seq {
        Some(seq) if error.kind() == ErrorKind::Usage => error
            .with_message("SEQ must be a positive sequence at or before the next unwritten message")
            .with_seq(seq),
        _ => error,
    };
    let mut found = false;
    match resolve_pool_target(pool, context.optional_pool_dir())? {
        PoolTarget::LocalPath(path) => {
            let handle = Pool::open(&path).map_err(|err| add_missing_pool_hint(err, pool))?;
            for frame in handle.fetch_lite3(options).map_err(read_error)? {
                let frame = frame?;
                if mode.is_binary() {
                    emit_lite3(frame.seq, frame.time, &frame.payload, mode)?;
                } else {
                    emit_message(message_from_frame(&frame)?, mode, context.color_mode())?;
                }
                found = true;
                if seq.is_some() {
                    break;
                }
            }
        }
        PoolTarget::Remote {
            base_url,
            pool: name,
        } => {
            let client = remote_client(base_url)?;
            let handle = client
                .open_pool(&PoolRef::name(name), false)
                .map_err(|err| add_missing_pool_hint(err, pool))?;
            if mode.is_binary() {
                for frame in handle.fetch_lite3(options).map_err(read_error)? {
                    let frame = frame?;
                    emit_lite3(frame.seq, frame.time, &frame.payload, mode)?;
                    found = true;
                    if seq.is_some() {
                        break;
                    }
                }
            } else {
                for message in handle.fetch(options).map_err(read_error)? {
                    let message = message?;
                    emit_message(
                        super::support::message_from_lite3(
                            message.seq,
                            message.time,
                            message.raw(),
                        )?,
                        mode,
                        context.color_mode(),
                    )?;
                    found = true;
                    if seq.is_some() {
                        break;
                    }
                }
            }
        }
        PoolTarget::Udp {
            endpoint,
            pool: name,
        } => {
            let handle =
                plasmite::api::UdpClient::new(endpoint)?.open_pool(&PoolRef::name(name))?;
            let mut reader = handle.fetch(options).map_err(read_error)?;
            let mut skipped = false;
            while let Some(event) = reader.next_event()? {
                match event {
                    plasmite::api::UdpEvent::Frame(frame) => {
                        if seq.is_some_and(|seq| frame.seq != seq) {
                            continue;
                        }
                        if mode.is_binary() {
                            emit_lite3(frame.seq, frame.time, frame.raw(), mode)?;
                        } else {
                            emit_message(
                                super::support::message_from_lite3(
                                    frame.seq,
                                    frame.time,
                                    frame.raw(),
                                )?,
                                mode,
                                context.color_mode(),
                            )?;
                        }
                        found = true;
                        if seq.is_some() {
                            break;
                        }
                    }
                    plasmite::api::UdpEvent::Skip {
                        first,
                        last,
                        reason,
                    } => {
                        if seq.is_some_and(|seq| !(first..=last).contains(&seq)) {
                            continue;
                        }
                        skipped = true;
                        super::stream_support::report_udp_skip(
                            "fetch", pool, first, last, reason, context,
                        );
                    }
                    plasmite::api::UdpEvent::Progress { .. } => {}
                }
            }
            if skipped && seq.is_none() {
                return Ok(CommandResult::with_code(1));
            }
        }
    }
    if let Some(seq) = seq.filter(|_| !found) {
        return Err(add_missing_seq_hint(
            Error::new(ErrorKind::NotFound)
                .with_message("message not found")
                .with_seq(seq),
            pool,
        ));
    }
    if mode == super::output::OutputMode::Lite3 {
        super::output::emit_lite3_header()?;
    }
    Ok(CommandResult::ok())
}
