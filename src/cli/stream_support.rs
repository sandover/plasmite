//! CLI helpers for stream commands.

use crate::ColorMode;
use crate::RunOutcome;
use crate::jq_filter::JqFilter;
use crate::jq_filter::matches_all;
use plasmite::api::Cursor;
use plasmite::api::CursorResult;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::Pool;
use plasmite::api::PoolRef;
use plasmite::api::RemoteClient;
use plasmite::api::TailOptions;
use plasmite::api::notify;
use plasmite::api::notify::NotifyWait;
use plasmite::notice::Notice;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::output_support::emit_message;
use super::output_support::emit_notice;
use super::output_support::notice_time_now;
use super::support::message_from_frame;
use super::support::message_to_json;
use super::support::output_value;

#[derive(Debug, Clone)]
pub(crate) struct DropNotice {
    last_seen_seq: u64,
    next_seen_seq: u64,
    count: u64,
}

impl DropNotice {
    fn dropped_count(&self) -> u64 {
        self.count
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FollowConfig {
    pub(crate) tail: u64,
    pub(crate) pretty: bool,
    pub(crate) one: bool,
    pub(crate) timeout: Option<Duration>,
    pub(crate) data_only: bool,
    pub(crate) since_ns: Option<u64>,
    pub(crate) no_follow: bool,
    pub(crate) required_tags: Vec<String>,
    pub(crate) where_predicates: Vec<JqFilter>,
    pub(crate) quiet_drops: bool,
    pub(crate) notify: bool,
    pub(crate) color_mode: ColorMode,
    pub(crate) replay_speed: Option<f64>,
    pub(crate) suppress_sender: Option<String>,
    pub(crate) stop: Option<Arc<AtomicBool>>,
}

pub(crate) fn matches_required_tags(required_tags: &[String], message: &Value) -> bool {
    if required_tags.is_empty() {
        return true;
    }
    let Some(tags) = message
        .get("meta")
        .and_then(|meta| meta.get("tags"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    required_tags.iter().all(|required| {
        tags.iter()
            .any(|tag| tag.as_str().is_some_and(|value| value == required))
    })
}

pub(crate) fn should_suppress_sender(message: &Value, sender: &str) -> bool {
    message
        .get("data")
        .and_then(|data| data.get("from"))
        .and_then(Value::as_str)
        .is_some_and(|value| value == sender)
}

pub(crate) fn duplex_requires_me_when_tty(stdin_is_terminal: bool, me: Option<&str>) -> bool {
    stdin_is_terminal && me.is_none()
}

pub(crate) fn parse_duplex_tty_line(me: &str, line: &str) -> Option<Value> {
    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
    if trimmed.trim().is_empty() {
        return None;
    }
    Some(json!({
        "from": me,
        "msg": trimmed,
    }))
}

pub(crate) fn should_suppress_message(cfg: &FollowConfig, message: &Value) -> bool {
    cfg.suppress_sender
        .as_deref()
        .is_some_and(|sender| should_suppress_sender(message, sender))
}

pub(crate) fn follow_should_stop(stop: Option<&Arc<AtomicBool>>) -> bool {
    stop.is_some_and(|flag| flag.load(Ordering::Acquire))
}

// Sequence bounds and relative time use one start snapshot. Tail selects retained
// frames, not matching frames; all selection predicates run after that selection.
fn history_start(oldest: u64, newest: u64, cfg: &FollowConfig) -> u64 {
    if oldest == 0 {
        newest.saturating_add(1).max(1)
    } else if cfg.since_ns.is_some() {
        oldest.max(1)
    } else if cfg.tail > 0 {
        newest
            .saturating_sub(cfg.tail.saturating_sub(1))
            .max(oldest)
            .max(1)
    } else {
        newest.saturating_add(1).max(1)
    }
}

fn matches_message(
    cfg: &FollowConfig,
    since_ns: Option<u64>,
    timestamp_ns: u64,
    message: &Value,
) -> Result<bool, Error> {
    Ok(since_ns.is_none_or(|since| timestamp_ns >= since)
        && !should_suppress_message(cfg, message)
        && matches_required_tags(&cfg.required_tags, message)
        && matches_all(&cfg.where_predicates, message)?)
}

struct DropReporter<'a> {
    cfg: &'a FollowConfig,
    pool: &'a str,
    path: String,
    pending: Option<DropNotice>,
    last_notice_at: Option<Instant>,
}

impl<'a> DropReporter<'a> {
    fn new(cfg: &'a FollowConfig, pool: &'a str, path: String) -> Self {
        Self {
            cfg,
            pool,
            path,
            pending: None,
            last_notice_at: None,
        }
    }

    fn gap(&mut self, expected_seq: u64, next_seq: u64) {
        if self.cfg.quiet_drops || next_seq <= expected_seq {
            return;
        }
        match &mut self.pending {
            Some(pending) => {
                pending.next_seen_seq = next_seq;
                pending.count = pending.count.saturating_add(next_seq - expected_seq);
            }
            None => {
                self.pending = Some(DropNotice {
                    last_seen_seq: expected_seq.saturating_sub(1),
                    next_seen_seq: next_seq,
                    count: next_seq - expected_seq,
                })
            }
        }
        self.flush(false);
    }

    fn flush(&mut self, force: bool) {
        if !force
            && self
                .last_notice_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        let Some(time) = notice_time_now() else {
            return;
        };
        let dropped_count = pending.dropped_count();
        let mut details = Map::new();
        details.insert("last_seen_seq".into(), json!(pending.last_seen_seq));
        details.insert("next_seen_seq".into(), json!(pending.next_seen_seq));
        details.insert("dropped_count".into(), json!(dropped_count));
        details.insert("pool_path".into(), json!(self.path));
        emit_notice(
            &Notice {
                kind: "drop".into(),
                time,
                cmd: "follow".into(),
                pool: self.pool.into(),
                message: format!("dropped {dropped_count} messages"),
                details,
            },
            self.cfg.color_mode,
            !self.cfg.pretty,
        );
        self.last_notice_at = Some(Instant::now());
    }
}

pub(crate) fn follow_remote(
    client: &RemoteClient,
    pool: &str,
    cfg: &FollowConfig,
) -> Result<RunOutcome, Error> {
    if cfg.replay_speed.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --replay")
            .with_hint("Use local follow with --replay, or omit --replay for remote streams."));
    }
    if !cfg.notify {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --no-notify")
            .with_hint("--no-notify only applies to local pool semaphores."));
    }
    let remote_pool = client.open_pool(&PoolRef::name(pool))?;
    let bounds = remote_pool.info()?.bounds;
    let upper = bounds.newest_seq.unwrap_or(0);
    let mut expected = history_start(bounds.oldest_seq.unwrap_or(0), upper, cfg);
    let since_ns = cfg.since_ns;
    let mut drops = DropReporter::new(cfg, pool, format!("{}/{}", client.base_url(), pool));
    if cfg.no_follow && expected > upper {
        return Ok(RunOutcome::ok());
    }
    let mut deadline = cfg.timeout.map(|duration| Instant::now() + duration);
    let mut remote_wait_ms = 100;
    loop {
        if follow_should_stop(cfg.stop.as_ref()) {
            drops.flush(true);
            return Ok(RunOutcome::ok());
        }
        if cfg.no_follow && expected > upper {
            drops.flush(true);
            return Ok(RunOutcome::ok());
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            drops.flush(true);
            return Ok(RunOutcome::with_code(124));
        }
        let mut options = TailOptions::new();
        options.since_seq = Some(expected);
        // Finite reads and cancellable duplex sessions use short server waits
        // so they can observe retention/cancellation without another message.
        // An ordinary unbounded follow uses the server's streaming lifetime.
        options.timeout = if cfg.no_follow || cfg.stop.is_some() || deadline.is_some() {
            Some(
                deadline
                    .map(|at| {
                        at.saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(remote_wait_ms))
                    })
                    .unwrap_or(Duration::from_millis(remote_wait_ms)),
            )
        } else {
            None
        };
        if cfg.no_follow {
            options.max_messages = Some(
                usize::try_from(upper.saturating_sub(expected).saturating_add(1))
                    .unwrap_or(usize::MAX),
            );
        }
        let mut tail = match remote_pool.tail(options) {
            Ok(tail) => tail,
            Err(err)
                if err.kind() == ErrorKind::Usage
                    && err.message() == Some("tail timeout exceeds server limit")
                    && remote_wait_ms > 1 =>
            {
                // The v0 protocol reports this limit as an error rather than
                // metadata. At most seven smaller waits reach every supported
                // positive limit, and the accepted wait persists for this run.
                remote_wait_ms = (remote_wait_ms / 2).max(1);
                continue;
            }
            Err(err) => return Err(err),
        };
        while let Some(message) = tail.next_message()? {
            if follow_should_stop(cfg.stop.as_ref()) {
                drops.flush(true);
                return Ok(RunOutcome::ok());
            }
            if cfg.no_follow && message.seq > upper {
                drops.gap(expected, upper.saturating_add(1));
                drops.flush(true);
                return Ok(RunOutcome::ok());
            }
            if message.seq < expected {
                continue;
            }
            drops.gap(expected, message.seq);
            expected = message.seq.saturating_add(1);
            let timestamp_ns = if since_ns.is_some() {
                let time = time::OffsetDateTime::parse(
                    &message.time,
                    &time::format_description::well_known::Rfc3339,
                )
                .map_err(|err| {
                    Error::new(ErrorKind::Corrupt)
                        .with_message("invalid remote message timestamp")
                        .with_source(err)
                })?;
                u64::try_from(time.unix_timestamp_nanos()).unwrap_or(0)
            } else {
                0
            };
            let value = message_to_json(&message);
            if matches_message(cfg, since_ns, timestamp_ns, &value)? {
                // Receiving a matching line after the deadline must not reset
                // the timer, even if the transport buffered or delayed it.
                if deadline.is_some_and(|at| Instant::now() >= at) {
                    drops.flush(true);
                    return Ok(RunOutcome::with_code(124));
                }
                emit_message(
                    output_value(value, cfg.data_only),
                    cfg.pretty,
                    cfg.color_mode,
                );
                deadline = cfg.timeout.map(|duration| Instant::now() + duration);
                if cfg.one {
                    drops.flush(true);
                    return Ok(RunOutcome::ok());
                }
            }
            if cfg.no_follow && message.seq >= upper {
                drops.flush(true);
                return Ok(RunOutcome::ok());
            }
            if deadline.is_some_and(|at| Instant::now() >= at) {
                drops.flush(true);
                return Ok(RunOutcome::with_code(124));
            }
        }
        if cfg.no_follow {
            // The stream can end after retention removes the rest of the
            // snapshot. A fresh bound identifies that gap without waiting for
            // future messages to fill the original count.
            let retained = remote_pool.info()?.bounds;
            let next = retained
                .oldest_seq
                .unwrap_or_else(|| upper.saturating_add(1));
            if next > expected {
                let next = next.min(upper.saturating_add(1));
                drops.gap(expected, next);
                expected = next;
            }
        }
        drops.flush(false);
    }
}

pub(crate) fn follow_pool(
    pool: &Pool,
    pool_ref: &str,
    pool_path: &Path,
    cfg: FollowConfig,
) -> Result<RunOutcome, Error> {
    let header = pool.header_from_mmap()?;
    let upper = header.newest_seq;
    let mut expected = history_start(header.oldest_seq, upper, &cfg);
    let since_ns = cfg.since_ns;
    let mut cursor = Cursor::new();
    // Scan retained frames once. The sequence lower bound skips earlier frames
    // without buffering them and stays fixed if the writer advances the ring.
    cursor.seek_to(if cfg.since_ns.is_some() || cfg.tail > 0 {
        header.tail_off
    } else {
        header.head_off
    } as usize);
    let finite = cfg.no_follow || cfg.replay_speed.is_some();
    let mut previous_timestamp = None::<u64>;
    let mut drops = DropReporter::new(&cfg, pool_ref, pool_path.display().to_string());
    let mut deadline = cfg.timeout.map(|duration| Instant::now() + duration);
    let mut notify_handle = if cfg.notify {
        notify::open_for_path(pool_path)
    } else {
        None
    };
    let mut backoff = Duration::from_millis(1);
    loop {
        if follow_should_stop(cfg.stop.as_ref()) || (finite && expected > upper) {
            drops.flush(true);
            return Ok(RunOutcome::ok());
        }
        if deadline.is_some_and(|at| Instant::now() >= at) {
            drops.flush(true);
            return Ok(RunOutcome::with_code(124));
        }
        match cursor.next(pool)? {
            CursorResult::Message(frame) => {
                if frame.seq < expected {
                    continue;
                }
                if finite && frame.seq > upper {
                    drops.gap(expected, upper.saturating_add(1));
                    drops.flush(true);
                    return Ok(RunOutcome::ok());
                }
                drops.gap(expected, frame.seq);
                expected = frame.seq.saturating_add(1);
                let message = message_from_frame(&frame)?;
                if matches_message(&cfg, since_ns, frame.timestamp_ns, &message)? {
                    if let (Some(speed), Some(previous)) = (cfg.replay_speed, previous_timestamp) {
                        if speed > 0.0 {
                            let delay_ns =
                                (frame.timestamp_ns.saturating_sub(previous) as f64 / speed) as u64;
                            // Bounded waits keep duplex/caller cancellation responsive.
                            let until = Instant::now()
                                .checked_add(Duration::from_nanos(delay_ns))
                                .ok_or_else(|| {
                                    Error::new(ErrorKind::Usage)
                                        .with_message("replay delay is too large")
                                })?;
                            while Instant::now() < until {
                                if follow_should_stop(cfg.stop.as_ref()) {
                                    drops.flush(true);
                                    return Ok(RunOutcome::ok());
                                }
                                if deadline.is_some_and(|at| Instant::now() >= at) {
                                    drops.flush(true);
                                    return Ok(RunOutcome::with_code(124));
                                }
                                let wait = until
                                    .saturating_duration_since(Instant::now())
                                    .min(Duration::from_millis(50));
                                let wait = deadline
                                    .map(|at| {
                                        at.saturating_duration_since(Instant::now()).min(wait)
                                    })
                                    .unwrap_or(wait);
                                std::thread::sleep(wait);
                            }
                        }
                    }
                    // A playback delay may finish on the same wake-up that
                    // crosses the output deadline; check before emitting.
                    if deadline.is_some_and(|at| Instant::now() >= at) {
                        drops.flush(true);
                        return Ok(RunOutcome::with_code(124));
                    }
                    if follow_should_stop(cfg.stop.as_ref()) {
                        drops.flush(true);
                        return Ok(RunOutcome::ok());
                    }
                    emit_message(
                        output_value(message, cfg.data_only),
                        cfg.pretty,
                        cfg.color_mode,
                    );
                    previous_timestamp = Some(frame.timestamp_ns);
                    deadline = cfg.timeout.map(|duration| Instant::now() + duration);
                    if cfg.one {
                        drops.flush(true);
                        return Ok(RunOutcome::ok());
                    }
                }
                drops.flush(false);
                backoff = Duration::from_millis(1);
            }
            CursorResult::FellBehind => {
                let header = pool.header_from_mmap()?;
                let next = if header.oldest_seq == 0 {
                    header.newest_seq.saturating_add(1)
                } else {
                    header.oldest_seq
                };
                let next = if finite {
                    next.min(upper.saturating_add(1))
                } else {
                    next
                };
                drops.gap(expected, next);
                expected = expected.max(next);
                cursor.seek_to(header.tail_off as usize);
            }
            CursorResult::WouldBlock => {
                if finite {
                    // All committed frames through the initial upper bound
                    // have been visited or removed. No later append belongs
                    // to this read.
                    drops.gap(expected, upper.saturating_add(1));
                    drops.flush(true);
                    return Ok(RunOutcome::ok());
                }
                drops.flush(false);
                let wait = deadline
                    .map(|at| at.saturating_duration_since(Instant::now()).min(backoff))
                    .unwrap_or(backoff);
                let waited = notify_handle.as_mut().map(|handle| handle.wait(wait));
                if matches!(waited, None | Some(NotifyWait::Unavailable)) {
                    notify_handle = None;
                    std::thread::sleep(wait);
                }
                backoff = (backoff * 2).min(Duration::from_millis(50));
            }
        }
    }
}
