//! Purpose: Execute follow and duplex streaming commands.
//! Exports: `FollowArgs`, `DuplexArgs`, `follow`, `duplex`.
//! Role: Keep shared streaming configuration, filtering, and cancellation together.

use super::context::CliContext;
use super::feed_support::{
    FeedIngestContext, RemoteFeedIngestContext, ingest_from_stdin, ingest_from_stdin_remote,
};
use super::output::OutputMode;
use super::result::CommandResult;
use super::stream_support::{FollowSignal, follow_local, follow_remote, follow_udp};
use super::support::{
    add_missing_pool_create_hint, parse_duration, remote_client, resolve_pool_target,
};
use crate::{InputMode, PoolTarget};
use plasmite::api::{Durability, Error, ErrorKind, LocalClient, Pool, PoolRef, Read, Since};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub(super) struct FollowArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) last: Option<u64>,
    pub(super) one: bool,
    pub(super) since: Option<String>,
    pub(super) tags: Vec<String>,
}

pub(super) struct DuplexArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) last: Option<u64>,
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
    let read = args.read()?;
    match resolve_pool_target(&args.pool, context.optional_pool_dir())? {
        PoolTarget::LocalPath(path) => {
            let pool = LocalClient::new()
                .open_pool(&PoolRef::path(&path), args.create)
                .map_err(add_missing_pool_create_hint)?;
            follow_local(&pool, &args, read, context, &mut |_| {})
        }
        PoolTarget::Remote { base_url, pool } => {
            let handle = remote_client(base_url)?.open_pool(&PoolRef::name(pool), args.create)?;
            follow_remote(&handle, &args, read, context, &mut |_| {})
        }
        PoolTarget::Udp { endpoint, pool } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("UDP follow does not support --create"));
            }
            let handle =
                plasmite::api::UdpClient::new(endpoint)?.open_pool(&PoolRef::name(pool))?;
            follow_udp(&handle, &args, read, context, &mut |_| {})
        }
    }
}

impl FollowArgs {
    fn read(&self) -> Result<Read, Error> {
        Ok(match (self.since.as_deref(), self.last) {
            (Some(since), _) => Read::since(since.parse::<Since>()?),
            (_, Some(last)) => Read::last(last),
            _ => Read::new(),
        })
    }
}

pub(super) fn duplex(args: DuplexArgs, context: &CliContext) -> Result<CommandResult, Error> {
    let output_mode = context.output_mode();
    if output_mode == OutputMode::Lite3Raw {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("duplex does not support --format lite3-raw")
            .with_hint("Use --format lite3 for a framed stream."));
    }
    let follow = FollowArgs {
        pool: args.pool,
        create: args.create,
        last: args.last,
        since: args.since,
        tags: Vec::new(),
        one: false,
    };
    let read = follow.read()?;
    let timeout = args.timeout.as_deref().map(parse_duration).transpose()?;
    let color_mode = context.color_mode();
    let context = context.clone();

    match resolve_pool_target(&follow.pool, context.optional_pool_dir())? {
        PoolTarget::LocalPath(path) => {
            let receiver = LocalClient::new()
                .open_pool(&PoolRef::path(&path), args.create)
                .map_err(add_missing_pool_create_hint)?;
            let mut sender = Pool::open(&path)?;
            duplex_session(
                timeout,
                move |signal| follow_local(&receiver, &follow, read, &context, signal),
                move |stop| {
                    ingest_from_stdin(
                        io::stdin().lock(),
                        FeedIngestContext {
                            tags: &[],
                            durability: Durability::Fast,
                            pool_handle: &mut sender,
                            color_mode,
                            output_mode,
                            input: InputMode::Auto,
                            stop: Some(stop),
                        },
                        false,
                    )
                },
            )
        }
        PoolTarget::Remote { base_url, pool } => {
            let client = remote_client(base_url)?;
            let sender = client.open_pool(&PoolRef::name(pool), args.create)?;
            let receiver = sender.clone();
            duplex_session(
                timeout,
                move |signal| follow_remote(&receiver, &follow, read, &context, signal),
                move |stop| {
                    ingest_from_stdin_remote(
                        io::stdin().lock(),
                        RemoteFeedIngestContext {
                            tags: &[],
                            durability: Durability::Fast,
                            remote_pool: &sender,
                            color_mode,
                            output_mode,
                            input: InputMode::Auto,
                            stop: Some(stop),
                        },
                        false,
                    )
                },
            )
        }
        PoolTarget::Udp { endpoint, pool } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("UDP duplex does not support --create"));
            }
            let sender =
                plasmite::api::UdpClient::new(endpoint)?.open_pool(&PoolRef::name(pool))?;
            let receiver = sender.clone();
            duplex_session(
                timeout,
                move |signal| follow_udp(&receiver, &follow, read, &context, signal),
                move |stop| {
                    super::feed_support::ingest_from_stdin_udp(
                        io::stdin().lock(),
                        &sender,
                        &[],
                        InputMode::Auto,
                        Some(stop),
                    )
                },
            )
        }
    }
}

fn duplex_session(
    timeout: Option<Duration>,
    receive: impl FnOnce(&mut dyn FnMut(FollowSignal)) -> Result<CommandResult, Error> + Send + 'static,
    send: impl FnOnce(&AtomicBool) -> Result<u64, Error> + Send + 'static,
) -> Result<CommandResult, Error> {
    enum Event {
        Output,
        Sending(Result<u64, Error>),
        Receiving(Result<CommandResult, Error>),
    }
    let (events, replies) = mpsc::sync_channel(2);
    let (ready, opened) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let sender_stop = stop.clone();
    let sender_events = events.clone();
    std::thread::spawn(move || {
        if opened.recv().is_ok() {
            let _ = sender_events.send(Event::Sending(send(&sender_stop)));
        }
    });
    std::thread::spawn(move || {
        let mut ready = Some(ready);
        let result = receive(&mut |event| match event {
            FollowSignal::Opened => {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
            }
            FollowSignal::Message => {
                if timeout.is_some() {
                    let _ = events.try_send(Event::Output);
                }
            }
        });
        let _ = events.send(Event::Receiving(result));
    });
    let outcome = loop {
        let next = match timeout {
            Some(timeout) => replies.recv_timeout(timeout),
            None => replies
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Event::Output | Event::Sending(Ok(_))) => {}
            Ok(Event::Sending(Err(error))) => break Err(error),
            Ok(Event::Receiving(outcome)) => break outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => break Ok(CommandResult::with_code(124)),
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(CommandResult::ok()),
        }
    };
    stop.store(true, Ordering::Release);
    outcome
}
