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
use std::collections::VecDeque;
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
}

impl DropNotice {
    fn dropped_count(&self) -> u64 {
        self.next_seen_seq.saturating_sub(self.last_seen_seq + 1)
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
    if cfg.since_ns.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --since")
            .with_hint("Use --tail N for remote refs, or run --since against a local pool path."));
    }
    if !cfg.notify {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --no-notify")
            .with_hint("--no-notify only applies to local pool semaphores."));
    }
    if cfg.quiet_drops {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --quiet-drops")
            .with_hint("--quiet-drops only applies to local drop notices."));
    }

    let remote_pool = client.open_pool(&PoolRef::name(pool))?;

    let mut next_since_seq = if cfg.tail > 0 {
        let info = remote_pool.info()?;
        match (info.bounds.oldest_seq, info.bounds.newest_seq) {
            (Some(oldest), Some(newest)) => Some(
                newest
                    .saturating_sub(cfg.tail.saturating_sub(1))
                    .max(oldest),
            ),
            _ => None,
        }
    } else {
        None
    };

    let mut tail_wait_matches = VecDeque::new();
    loop {
        if follow_should_stop(cfg.stop.as_ref()) {
            return Ok(RunOutcome::ok());
        }

        let mut options = TailOptions::new();
        options.since_seq = next_since_seq;
        options.timeout = cfg.timeout;
        let mut tail = remote_pool.tail(options)?;

        let mut emitted_in_cycle = false;
        while let Some(message) = tail.next_message()? {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            next_since_seq = Some(message.seq.saturating_add(1));
            let value = message_to_json(&message);
            if should_suppress_message(cfg, &value)
                || !matches_required_tags(cfg.required_tags.as_slice(), &value)
                || !matches_all(cfg.where_predicates.as_slice(), &value)?
            {
                continue;
            }

            if cfg.one && cfg.tail > 0 {
                tail_wait_matches.push_back(value);
                while tail_wait_matches.len() > cfg.tail as usize {
                    tail_wait_matches.pop_front();
                }
                if tail_wait_matches.len() == cfg.tail as usize {
                    if let Some(latest) = tail_wait_matches.back() {
                        emit_message(
                            output_value(latest.clone(), cfg.data_only),
                            cfg.pretty,
                            cfg.color_mode,
                        );
                    }
                    return Ok(RunOutcome::ok());
                }
                emitted_in_cycle = true;
                continue;
            }

            emit_message(
                output_value(value, cfg.data_only),
                cfg.pretty,
                cfg.color_mode,
            );
            emitted_in_cycle = true;
            if cfg.one {
                return Ok(RunOutcome::ok());
            }
        }

        if cfg.timeout.is_some() && !emitted_in_cycle {
            return Ok(RunOutcome::with_code(124));
        }
    }
}

pub(crate) fn follow_pool(
    pool: &Pool,
    pool_ref: &str,
    pool_path: &Path,
    cfg: FollowConfig,
) -> Result<RunOutcome, Error> {
    if cfg.replay_speed.is_some() {
        return follow_replay(pool, &cfg);
    }

    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut emit = VecDeque::new();
    let mut last_seen_seq = None::<u64>;
    let mut pending_drop: Option<DropNotice> = None;
    let mut last_notice_at: Option<Instant> = None;
    let notice_interval = Duration::from_secs(1);
    let tail_wait = cfg.one && cfg.tail > 0;
    let mut timeout_deadline = cfg.timeout.map(|duration| Instant::now() + duration);
    let mut notify_enabled = cfg.notify;
    let mut notify_handle = if notify_enabled {
        notify::open_for_path(pool_path)
    } else {
        None
    };
    if notify_enabled && notify_handle.is_none() {
        notify_enabled = false;
    }

    let bump_timeout = |deadline: &mut Option<Instant>| {
        if let Some(duration) = cfg.timeout {
            *deadline = Some(Instant::now() + duration);
        }
    };

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if follow_should_stop(cfg.stop.as_ref()) {
                        return Ok(RunOutcome::ok());
                    }
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if !should_suppress_message(&cfg, &message)
                            && matches_required_tags(cfg.required_tags.as_slice(), &message)
                            && matches_all(cfg.where_predicates.as_slice(), &message)?
                        {
                            emit_message(
                                output_value(message, cfg.data_only),
                                cfg.pretty,
                                cfg.color_mode,
                            );
                            bump_timeout(&mut timeout_deadline);
                            if cfg.one {
                                return Ok(RunOutcome::ok());
                            }
                        }
                        last_seen_seq = Some(frame.seq);
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    } else if cfg.tail > 0 {
        cursor.seek_to(header.tail_off as usize);
        loop {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if follow_should_stop(cfg.stop.as_ref()) {
                        return Ok(RunOutcome::ok());
                    }
                    let message = message_from_frame(&frame)?;
                    if !should_suppress_message(&cfg, &message)
                        && matches_required_tags(cfg.required_tags.as_slice(), &message)
                        && matches_all(cfg.where_predicates.as_slice(), &message)?
                    {
                        emit.push_back(message);
                    }
                    last_seen_seq = Some(frame.seq);
                    while emit.len() > cfg.tail as usize {
                        emit.pop_front();
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
        if tail_wait {
            if emit.len() >= cfg.tail as usize {
                if let Some(value) = emit.back() {
                    emit_message(
                        output_value(value.clone(), cfg.data_only),
                        cfg.pretty,
                        cfg.color_mode,
                    );
                }
                return Ok(RunOutcome::ok());
            }
        } else {
            for value in emit.drain(..) {
                emit_message(
                    output_value(value, cfg.data_only),
                    cfg.pretty,
                    cfg.color_mode,
                );
                bump_timeout(&mut timeout_deadline);
            }
        }
    }

    if cfg.since_ns.is_none() && cfg.tail == 0 {
        cursor.seek_to(header.head_off as usize);
    }

    let mut backoff = Duration::from_millis(1);
    let max_backoff = Duration::from_millis(50);

    let pool_ref = pool_ref.to_string();
    let pool_path_label = pool_path.display().to_string();

    let maybe_emit_pending = |pending: &mut Option<DropNotice>,
                              last_notice_at: &mut Option<Instant>| {
        if cfg.quiet_drops {
            pending.take();
            return;
        }
        let Some(pending_notice) = pending.as_ref() else {
            return;
        };
        let ready = last_notice_at
            .map(|instant| instant.elapsed() >= notice_interval)
            .unwrap_or(true);
        if !ready {
            return;
        }
        let time = match notice_time_now() {
            Some(time) => time,
            None => {
                pending.take();
                return;
            }
        };
        let dropped_count = pending_notice.dropped_count();
        let mut details = Map::new();
        details.insert(
            "last_seen_seq".to_string(),
            json!(pending_notice.last_seen_seq),
        );
        details.insert(
            "next_seen_seq".to_string(),
            json!(pending_notice.next_seen_seq),
        );
        details.insert("dropped_count".to_string(), json!(dropped_count));
        details.insert("pool_path".to_string(), json!(pool_path_label.as_str()));
        let notice = Notice {
            kind: "drop".to_string(),
            time,
            cmd: "follow".to_string(),
            pool: pool_ref.clone(),
            message: format!("dropped {dropped_count} messages"),
            details,
        };
        emit_notice(&notice, cfg.color_mode);
        *last_notice_at = Some(Instant::now());
        pending.take();
    };

    let queue_drop = |last_seen_seq: u64, next_seen_seq: u64, pending: &mut Option<DropNotice>| {
        if cfg.quiet_drops {
            return;
        }
        match pending {
            Some(existing) => {
                existing.next_seen_seq = next_seen_seq;
            }
            None => {
                *pending = Some(DropNotice {
                    last_seen_seq,
                    next_seen_seq,
                });
            }
        }
    };

    loop {
        if follow_should_stop(cfg.stop.as_ref()) {
            return Ok(RunOutcome::ok());
        }
        match cursor.next(pool)? {
            CursorResult::Message(frame) => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                if let Some(last_seen_seq) = last_seen_seq {
                    if frame.seq > last_seen_seq + 1 {
                        queue_drop(last_seen_seq, frame.seq, &mut pending_drop);
                        maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                    }
                }
                let message = message_from_frame(&frame)?;
                if !should_suppress_message(&cfg, &message)
                    && matches_required_tags(cfg.required_tags.as_slice(), &message)
                    && matches_all(cfg.where_predicates.as_slice(), &message)?
                {
                    if tail_wait {
                        emit.push_back(message);
                        while emit.len() > cfg.tail as usize {
                            emit.pop_front();
                        }
                        if emit.len() == cfg.tail as usize {
                            if let Some(value) = emit.back() {
                                emit_message(
                                    output_value(value.clone(), cfg.data_only),
                                    cfg.pretty,
                                    cfg.color_mode,
                                );
                            }
                            return Ok(RunOutcome::ok());
                        }
                    } else {
                        emit_message(
                            output_value(message, cfg.data_only),
                            cfg.pretty,
                            cfg.color_mode,
                        );
                        bump_timeout(&mut timeout_deadline);
                        if cfg.one {
                            return Ok(RunOutcome::ok());
                        }
                    }
                }
                last_seen_seq = Some(frame.seq);
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                backoff = Duration::from_millis(1);
            }
            CursorResult::WouldBlock => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                if let Some(deadline) = timeout_deadline {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(RunOutcome::with_code(124));
                    }
                    let remaining = deadline.duration_since(now);
                    let wait_for = std::cmp::min(backoff, remaining);
                    if notify_enabled {
                        match notify_handle
                            .as_mut()
                            .map(|handle| handle.wait(wait_for))
                            .unwrap_or(NotifyWait::Unavailable)
                        {
                            NotifyWait::Signaled | NotifyWait::TimedOut => {}
                            NotifyWait::Unavailable => {
                                notify_enabled = false;
                                notify_handle = None;
                                std::thread::sleep(wait_for);
                            }
                        }
                    } else {
                        std::thread::sleep(wait_for);
                    }
                } else if notify_enabled {
                    match notify_handle
                        .as_mut()
                        .map(|handle| handle.wait(backoff))
                        .unwrap_or(NotifyWait::Unavailable)
                    {
                        NotifyWait::Signaled | NotifyWait::TimedOut => {}
                        NotifyWait::Unavailable => {
                            notify_enabled = false;
                            notify_handle = None;
                            std::thread::sleep(backoff);
                        }
                    }
                } else {
                    std::thread::sleep(backoff);
                }
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            CursorResult::FellBehind => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                header = pool.header_from_mmap()?;
                if cfg.tail > 0 {
                    cursor.seek_to(header.tail_off as usize);
                } else {
                    // Jumping to the live end skips every message after the last one
                    // seen. Report it now: if the pool goes quiet, no later message
                    // would reveal the gap. Reading resumes after the newest message.
                    if let Some(seen) = last_seen_seq
                        && header.newest_seq > seen
                    {
                        queue_drop(seen, header.newest_seq + 1, &mut pending_drop);
                        maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                        last_seen_seq = Some(header.newest_seq);
                    }
                    cursor.seek_to(header.head_off as usize);
                }
            }
        }
    }
}

pub(crate) fn follow_replay(pool: &Pool, cfg: &FollowConfig) -> Result<RunOutcome, Error> {
    let speed = cfg.replay_speed.unwrap_or(0.0);
    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut collected: Vec<(u64, Value)> = Vec::new();

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if matches_required_tags(cfg.required_tags.as_slice(), &message)
                            && matches_all(cfg.where_predicates.as_slice(), &message)?
                        {
                            collected.push((frame.timestamp_ns, message));
                        }
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    } else {
        cursor.seek_to(header.tail_off as usize);
        let mut buffer: VecDeque<(u64, Value)> = VecDeque::new();
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    let message = message_from_frame(&frame)?;
                    if matches_required_tags(cfg.required_tags.as_slice(), &message)
                        && matches_all(cfg.where_predicates.as_slice(), &message)?
                    {
                        if cfg.tail > 0 {
                            buffer.push_back((frame.timestamp_ns, message));
                            while buffer.len() > cfg.tail as usize {
                                buffer.pop_front();
                            }
                        } else {
                            collected.push((frame.timestamp_ns, message));
                        }
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
        if cfg.tail > 0 {
            collected = buffer.into_iter().collect();
        }
    }

    if collected.is_empty() {
        return Ok(RunOutcome::ok());
    }

    let mut prev_ts = collected[0].0;
    for (i, (ts, message)) in collected.into_iter().enumerate() {
        if i > 0 && speed > 0.0 {
            let delta_ns = ts.saturating_sub(prev_ts);
            let delay_ns = (delta_ns as f64 / speed) as u64;
            if delay_ns > 0 {
                std::thread::sleep(Duration::from_nanos(delay_ns));
            }
        }
        emit_message(
            output_value(message, cfg.data_only),
            cfg.pretty,
            cfg.color_mode,
        );
        prev_ts = ts;
        if cfg.one {
            return Ok(RunOutcome::ok());
        }
    }

    Ok(RunOutcome::ok())
}
