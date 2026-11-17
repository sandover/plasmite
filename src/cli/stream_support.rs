//! CLI output and cancellation for local, HTTPS and UDP readers.

use crate::RunOutcome;
use plasmite::api::{Error, Pool, UdpEvent};
use plasmite::notice::Notice;
use serde_json::{Map, json};
use std::time::Duration;

use super::output_support::{emit_lite3, emit_message, emit_notice, notice_time_now};
use super::support::message_from_lite3;

pub(super) enum FollowSignal {
    Opened,
    Message,
}

/// Only poll a pipe. Regular files and terminals never have a downstream reader.
pub(super) fn stdout_is_pipe() -> bool {
    #[cfg(unix)]
    {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat writes one stat to the supplied buffer on success.
        unsafe {
            libc::fstat(libc::STDOUT_FILENO, stat.as_mut_ptr()) == 0
                && stat.assume_init().st_mode & libc::S_IFMT == libc::S_IFIFO
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn stdout_pipe_closed() -> bool {
    #[cfg(unix)]
    {
        let mut output = libc::pollfd {
            fd: libc::STDOUT_FILENO,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: poll receives one initialized descriptor and does not retain it.
        (unsafe { libc::poll(&mut output, 1, 0) } > 0)
            && output.revents & (libc::POLLERR | libc::POLLHUP) != 0
    }
    #[cfg(not(unix))]
    false
}

fn report_read_gap(
    error: Error,
    args: &super::stream::FollowArgs,
    context: &super::context::CliContext,
) -> Result<plasmite::api::Read, Error> {
    let (Some(missing), Some(next_seq)) = (error.missing(), error.next_seq()) else {
        return Err(error);
    };
    if let Some(time) = notice_time_now() {
        emit_notice(
            &Notice {
                kind: "drop".into(),
                time,
                cmd: "follow".into(),
                pool: args.pool.clone(),
                message: format!("dropped {} messages", next_seq - missing),
                details: Map::from_iter([
                    ("last_seen_seq".into(), json!(missing.saturating_sub(1))),
                    ("next_seen_seq".into(), json!(next_seq)),
                    ("dropped_count".into(), json!(next_seq - missing)),
                ]),
            },
            context.color_mode(),
            context.output_mode(),
        );
    }
    Ok(plasmite::api::Read::since(plasmite::api::Since::Seq(next_seq)).tags(args.tags.clone()))
}

fn emit_frame(
    seq: u64,
    time: u64,
    payload: &[u8],
    context: &super::context::CliContext,
) -> Result<(), Error> {
    let mode = context.output_mode();
    if mode.is_binary() {
        return emit_lite3(seq, time, payload, mode);
    }
    let message = message_from_lite3(seq, time, payload)?;
    emit_message(message, mode, context.color_mode())
}

pub(super) fn follow_local(
    pool: &Pool,
    args: &super::stream::FollowArgs,
    read: plasmite::api::Read,
    context: &super::context::CliContext,
    signal: &mut dyn FnMut(FollowSignal),
) -> Result<RunOutcome, Error> {
    use plasmite::api::ReadPoll;
    let mut options = read.tags(args.tags.clone());
    let watch = stdout_is_pipe();
    loop {
        let result = (|| {
            let mut reader = pool.follow_lite3(options)?;
            signal(FollowSignal::Opened);
            loop {
                if watch && stdout_pipe_closed() {
                    return Ok(());
                }
                match reader.poll(Duration::from_millis(50))? {
                    ReadPoll::Message(frame) => {
                        emit_frame(frame.seq, frame.time, &frame.payload, context)?;
                        signal(FollowSignal::Message);
                        if args.one {
                            return Ok(());
                        }
                    }
                    ReadPoll::Waiting => {}
                    ReadPoll::Complete | ReadPoll::Cancelled => return Ok(()),
                }
            }
        })();
        match result {
            Ok(()) => return Ok(RunOutcome::with_code(0)),
            Err(error) => options = report_read_gap(error, args, context)?,
        }
    }
}

pub(super) fn report_udp_skip(
    command: &str,
    pool: &str,
    first: u64,
    last: u64,
    reason: plasmite::api::UdpSkipReason,
    context: &super::context::CliContext,
) {
    emit_notice(
        &Notice {
            kind: "udp-gap".into(),
            time: notice_time_now().unwrap_or_default(),
            cmd: command.into(),
            pool: pool.into(),
            message: format!("UDP server skipped sequences {first} through {last}"),
            details: Map::from_iter([
                ("first_seq".into(), json!(first)),
                ("last_seq".into(), json!(last)),
                ("reason".into(), json!(format!("{reason:?}").to_lowercase())),
            ]),
        },
        context.color_mode(),
        context.output_mode(),
    );
}

pub(super) fn follow_udp(
    pool: &plasmite::api::UdpPool,
    args: &super::stream::FollowArgs,
    read: plasmite::api::Read,
    context: &super::context::CliContext,
    signal: &mut dyn FnMut(FollowSignal),
) -> Result<RunOutcome, Error> {
    let mut options = read.tags(args.tags.clone());
    let watch = stdout_is_pipe();
    loop {
        let result = (|| {
            let mut reader = pool.follow(options)?;
            signal(FollowSignal::Opened);
            loop {
                if watch && stdout_pipe_closed() {
                    return Ok(());
                }
                if let Some(event) = reader.next_event_timeout(Duration::from_millis(50))? {
                    match event {
                        UdpEvent::Frame(frame) => {
                            emit_frame(frame.seq, frame.time, &frame.payload, context)?;
                            signal(FollowSignal::Message);
                            if args.one {
                                return Ok(());
                            }
                        }
                        UdpEvent::Gap {
                            first,
                            last,
                            reason,
                        } => {
                            report_udp_skip("follow", &args.pool, first, last, reason, context);
                        }
                        UdpEvent::Progress { .. } => {}
                    }
                } else if reader.is_finished() {
                    return Ok(());
                }
            }
        })();
        match result {
            Ok(()) => return Ok(RunOutcome::with_code(0)),
            Err(error) => options = report_read_gap(error, args, context)?,
        }
    }
}

pub(super) fn follow_remote(
    pool: &plasmite::api::RemotePool,
    args: &super::stream::FollowArgs,
    read: plasmite::api::Read,
    context: &super::context::CliContext,
    signal: &mut dyn FnMut(FollowSignal),
) -> Result<RunOutcome, Error> {
    let mut options = read.tags(args.tags.clone());
    loop {
        let result = (|| {
            let reader = pool.follow_lite3(options)?;
            signal(FollowSignal::Opened);
            for frame in reader {
                let frame = frame?;
                emit_frame(frame.seq, frame.time, &frame.payload, context)?;
                signal(FollowSignal::Message);
                if args.one {
                    break;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(RunOutcome::with_code(0)),
            Err(error) => options = report_read_gap(error, args, context)?,
        }
    }
}
