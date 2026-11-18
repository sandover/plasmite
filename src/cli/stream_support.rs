//! CLI output and cancellation for local, HTTPS and UDP readers.

use crate::RunOutcome;
use plasmite::api::{Error, Pool, UdpEvent};
use plasmite::notice::Notice;
use serde_json::{Map, json};
use std::time::Duration;

use super::output_support::{emit_error, emit_lite3, emit_message, emit_notice, notice_time_now};
use super::support::message_from_lite3;

// Redirected output can have a downstream reader, including a socketpair.
fn watches_stdout() -> bool {
    #[cfg(unix)]
    {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat writes one stat to the supplied buffer on success.
        unsafe {
            libc::fstat(libc::STDOUT_FILENO, stat.as_mut_ptr()) == 0
                && stat.assume_init().st_mode & libc::S_IFMT != libc::S_IFREG
                && libc::isatty(libc::STDOUT_FILENO) == 0
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn stdout_closed() -> bool {
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
    let (Some(_), Some(next_seq)) = (error.missing(), error.next_seq()) else {
        return Err(error);
    };
    emit_error(&error, context.color_mode(), context.output_mode());
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
    signal: &mut dyn FnMut(),
) -> Result<RunOutcome, Error> {
    use plasmite::api::ReadPoll;
    let mut options = read.tags(args.tags.clone());
    let watch = watches_stdout();
    loop {
        let result = (|| {
            let mut reader = pool.follow_lite3(options)?;
            signal();
            loop {
                if watch && stdout_closed() {
                    return Ok(());
                }
                let status = if watch {
                    reader.poll(Duration::from_millis(50))?
                } else {
                    match reader.next_frame()? {
                        Some(frame) => ReadPoll::Message(frame),
                        None => ReadPoll::Complete,
                    }
                };
                match status {
                    ReadPoll::Message(frame) => {
                        emit_frame(frame.seq, frame.time, &frame.payload, context)?;
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
            kind: "udp-skip".into(),
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
    signal: &mut dyn FnMut(),
) -> Result<RunOutcome, Error> {
    let mut options = read.tags(args.tags.clone());
    let watch = watches_stdout();
    loop {
        let result = (|| {
            let mut reader = pool.follow(options)?;
            signal();
            loop {
                if watch && stdout_closed() {
                    return Ok(());
                }
                let event = if watch {
                    reader.next_event_timeout(Duration::from_millis(50))?
                } else {
                    reader.next_event()?
                };
                if let Some(event) = event {
                    match event {
                        UdpEvent::Frame(frame) => {
                            emit_frame(frame.seq, frame.time, &frame.payload, context)?;
                            if args.one {
                                return Ok(());
                            }
                        }
                        UdpEvent::Skip {
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
    signal: &mut dyn FnMut(),
) -> Result<RunOutcome, Error> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let cancel = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let watcher = watches_stdout().then(|| {
            scope.spawn(|| {
                while !cancel.load(Ordering::Acquire) {
                    if stdout_closed() {
                        cancel.store(true, Ordering::Release);
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(50));
                }
            })
        });
        let result = (|| {
            let mut options = read.tags(args.tags.clone());
            loop {
                if watcher.is_some() {
                    options = options.cancel(cancel.clone());
                }
                let result = (|| {
                    let reader = pool.follow_lite3(options)?;
                    signal();
                    for frame in reader {
                        let frame = frame?;
                        emit_frame(frame.seq, frame.time, &frame.payload, context)?;
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
        })();
        cancel.store(true, Ordering::Release);
        if let Some(watcher) = watcher {
            watcher.thread().unpark();
        }
        result
    })
}
