//! Purpose: Execute follow and duplex streaming commands.
//! Exports: `FollowArgs`, `DuplexArgs`, `follow`, `duplex`.
//! Role: Keep shared streaming configuration, filtering, and cancellation together.

use super::context::CliContext;
use super::feed_support::{
    FeedIngestContext, RemoteFeedIngestContext, ingest_from_stdin, ingest_from_stdin_remote,
};
use super::output::OutputMode;
use super::output_support::emit_follow_timeout_human;
use super::result::CommandResult;
use super::stream_support::{FollowConfig, follow_pool, follow_remote, follow_udp};
use super::support::{
    DEFAULT_POOL_SIZE, add_missing_pool_create_hint, ensure_pool_dir, now_ns, parse_duration,
    parse_since, remote_client, resolve_pool_target,
};
use crate::jq_filter::compile_filters;
use crate::{InputMode, PoolTarget};
use plasmite::api::{Durability, Error, ErrorKind, Pool, PoolOptions, PoolRef};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

pub(super) struct FollowArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) tail: u64,
    pub(super) one: bool,
    pub(super) no_follow: bool,
    pub(super) timeout: Option<String>,
    pub(super) since: Option<String>,
    pub(super) where_expr: Vec<String>,
    pub(super) tags: Vec<String>,
    pub(super) quiet_drops: bool,
    pub(super) no_notify: bool,
    pub(super) replay: Option<f64>,
}

pub(super) struct DuplexArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) tail: u64,
    pub(super) timeout: Option<String>,
    pub(super) since: Option<String>,
}

pub(super) fn follow(args: FollowArgs, context: &CliContext) -> Result<CommandResult, Error> {
    let output_mode = context.output_mode();
    if output_mode == OutputMode::Lite3Raw {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("follow does not support --format lite3-raw")
            .with_hint("Use --format lite3 for a framed stream."));
    }
    let pretty = output_mode == OutputMode::Human;
    let since_ns = args
        .since
        .as_deref()
        .map(|value| parse_since(value, now_ns()?))
        .transpose()?;
    let timeout_input = args.timeout.as_deref();
    let timeout = timeout_input.map(parse_duration).transpose()?;
    let cfg = FollowConfig {
        tail: args.tail,
        output_mode,
        one: args.one,
        timeout,
        since_ns,
        no_follow: args.no_follow || args.replay.is_some(),
        required_tags: args.tags,
        where_predicates: compile_filters(&args.where_expr)?,
        quiet_drops: args.quiet_drops,
        notify: !args.no_notify,
        color_mode: if pretty {
            context.color_mode()
        } else {
            crate::ColorMode::Never
        },
        replay_speed: args.replay,
        ready: None,
        stop: None,
    };
    let target = resolve_pool_target(&args.pool, context.pool_dir())?;
    match target {
        PoolTarget::LocalPath(path) => {
            if let Some(speed) = args.replay {
                if speed < 0.0 {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--replay speed must be non-negative")
                        .with_hint("Use --replay 1 for realtime, --replay 2 for 2x, --replay 0 for no delay."));
                }
                if !speed.is_finite() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--replay speed must be a finite number")
                        .with_hint("Use --replay 1 for realtime, --replay 2 for 2x, --replay 0 for no delay."));
                }
                if args.tail == 0 && args.since.is_none() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--replay requires --tail or --since")
                        .with_hint(
                            "Replay needs historical messages. Use --tail N or --since DURATION.",
                        ));
                }
            }
            let pool_handle = match Pool::open(&path) {
                Ok(pool_handle) => pool_handle,
                Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(context.pool_dir())?;
                    Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE))?
                }
                Err(err) => {
                    return Err(add_missing_pool_create_hint(err));
                }
            };
            let outcome = follow_pool(&pool_handle, &args.pool, &path, cfg)?;
            if outcome.exit_code == 124
                && pretty
                && let Some(timeout_input) = timeout_input
            {
                emit_follow_timeout_human(timeout_input);
            }
            Ok(outcome)
        }
        PoolTarget::Remote { base_url, pool } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("remote follow does not support --create")
                    .with_hint(
                        "Create remote pools with server-side tooling, then rerun follow.",
                    ));
            }
            let client = remote_client(base_url)?;
            let outcome = follow_remote(&client, &pool, &cfg)?;
            if outcome.exit_code == 124
                && pretty
                && let Some(timeout_input) = timeout_input
            {
                emit_follow_timeout_human(timeout_input);
            }
            Ok(outcome)
        }
        PoolTarget::Udp { endpoint, pool } => {
            if args.create || args.replay.is_some() || args.no_notify {
                return Err(Error::new(ErrorKind::Usage).with_message(
                    "UDP follow does not support --create, --replay, or --no-notify",
                ));
            }
            let outcome = follow_udp(&endpoint, &pool, &cfg)?;
            if outcome.exit_code == 124
                && pretty
                && let Some(timeout_input) = timeout_input
            {
                emit_follow_timeout_human(timeout_input);
            }
            Ok(outcome)
        }
    }
}

pub(super) fn duplex(args: DuplexArgs, context: &CliContext) -> Result<CommandResult, Error> {
    let output_mode = context.output_mode();
    if output_mode == OutputMode::Lite3Raw {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("duplex does not support --format lite3-raw")
            .with_hint("Use --format lite3 for a framed stream."));
    }
    let since_ns = args
        .since
        .as_deref()
        .map(|value| parse_since(value, now_ns()?))
        .transpose()?;
    let timeout = args.timeout.as_deref().map(parse_duration).transpose()?;
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let cfg = FollowConfig {
        tail: args.tail,
        output_mode,
        one: false,
        timeout,
        since_ns,
        no_follow: false,
        required_tags: Vec::new(),
        where_predicates: Vec::new(),
        quiet_drops: false,
        notify: true,
        color_mode: context.color_mode(),
        replay_speed: None,
        ready: Some(ready_tx),
        stop: Some(stop.clone()),
    };
    enum DuplexSide {
        Follow,
        Send,
    }
    let (event_tx, event_rx) = mpsc::channel();
    let color_mode = context.color_mode();
    match resolve_pool_target(&args.pool, context.pool_dir())? {
        PoolTarget::LocalPath(path) => {
            let follow_pool_handle = match Pool::open(&path) {
                Ok(pool) => pool,
                Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(context.pool_dir())?;
                    Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE))?
                }
                Err(err) => return Err(add_missing_pool_create_hint(err)),
            };
            let mut send_pool = Pool::open(&path)?;
            let follow_tx = event_tx.clone();
            std::thread::spawn(move || {
                let outcome = follow_pool(&follow_pool_handle, &args.pool, &path, cfg);
                let _ = follow_tx.send((DuplexSide::Follow, outcome));
            });
            let stop = stop.clone();
            std::thread::spawn(move || {
                if ready_rx.recv().is_err() {
                    return;
                }
                let outcome = ingest_from_stdin(
                    io::stdin().lock(),
                    FeedIngestContext {
                        tags: &[],
                        durability: Durability::Fast,
                        retry_config: None,
                        pool_handle: &mut send_pool,
                        color_mode,
                        output_mode,
                        input: InputMode::Auto,
                        stop: Some(&stop),
                    },
                    false,
                )
                .map(|_| CommandResult::ok());
                let _ = event_tx.send((DuplexSide::Send, outcome));
            });
        }
        PoolTarget::Remote { base_url, pool } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("remote duplex does not support --create")
                    .with_hint("Create the pool on the server, then rerun duplex."));
            }
            let client = remote_client(base_url)?;
            let remote_pool = client.open_pool(&PoolRef::name(&pool))?;
            let follow_tx = event_tx.clone();
            std::thread::spawn(move || {
                let outcome = follow_remote(&client, &pool, &cfg);
                let _ = follow_tx.send((DuplexSide::Follow, outcome));
            });
            let stop = stop.clone();
            std::thread::spawn(move || {
                if ready_rx.recv().is_err() {
                    return;
                }
                let outcome = ingest_from_stdin_remote(
                    io::stdin().lock(),
                    RemoteFeedIngestContext {
                        tags: &[],
                        durability: Durability::Fast,
                        retry_config: None,
                        remote_pool: &remote_pool,
                        color_mode,
                        output_mode,
                        input: InputMode::Auto,
                        stop: Some(&stop),
                    },
                    false,
                )
                .map(|_| CommandResult::ok());
                let _ = event_tx.send((DuplexSide::Send, outcome));
            });
        }
        PoolTarget::Udp { endpoint, pool } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("UDP duplex does not support --create"));
            }
            let client = plasmite::api::UdpClient::new(&endpoint)?;
            let send_pool = client.open_pool(&PoolRef::name(&pool))?;
            let follow_tx = event_tx.clone();
            std::thread::spawn(move || {
                let outcome = follow_udp(&endpoint, &pool, &cfg);
                let _ = follow_tx.send((DuplexSide::Follow, outcome));
            });
            let stop = stop.clone();
            std::thread::spawn(move || {
                if ready_rx.recv().is_err() {
                    return;
                }
                let outcome = super::feed_support::ingest_from_stdin_udp(
                    io::stdin().lock(),
                    &send_pool,
                    &[],
                    InputMode::Auto,
                    Some(&stop),
                )
                .map(|_| CommandResult::ok());
                let _ = event_tx.send((DuplexSide::Send, outcome));
            });
        }
    }
    for (side, outcome) in event_rx {
        if matches!(side, DuplexSide::Send) && outcome.is_ok() {
            continue; // EOF closes sending; replies may still arrive.
        }
        stop.store(true, Ordering::Release);
        return outcome;
    }
    Ok(CommandResult::ok())
}
