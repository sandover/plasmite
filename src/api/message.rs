//! Purpose: Define public message types and append/get/tail/replay helpers for the API.
//! Exports: `Message`, `Meta`, `TailOptions`, `Tail`, `Lite3Tail`, `ReplayOptions`, `Replay`.
//! Role: Stable message envelope aligned with the CLI contract.
//! Invariants: Message fields mirror CLI JSON; time is RFC3339 UTC.
//! Invariants: Tail streams preserve ordering and avoid unbounded buffering.
//! Invariants: Replay is bounded; all messages are collected up front.
#![allow(clippy::result_large_err)]

use crate::core::cursor::{Cursor, CursorResult, FrameRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{Lite3DocRef, sys, validate_bytes};
use crate::core::notify::{NotifyError, PoolSemaphore, WaitOutcome, open_for_path};
use crate::core::pool::{AppendOptions, Durability, Pool};
use serde_json::Value;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Meta {
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub seq: u64,
    pub time: String,
    pub meta: Meta,
    pub data: Value,
}

#[derive(Clone, Debug)]
pub struct TailOptions {
    pub since_seq: Option<u64>,
    pub max_messages: Option<usize>,
    pub tags: Vec<String>,
    pub poll_interval: Duration,
    pub timeout: Option<Duration>,
    pub notify: bool,
    pub gap_policy: GapPolicy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GapPolicy {
    #[default]
    Continue,
    Error,
}

impl TailOptions {
    pub fn new() -> Self {
        Self {
            since_seq: None,
            max_messages: None,
            tags: Vec::new(),
            poll_interval: Duration::from_millis(50),
            timeout: None,
            notify: true,
            gap_policy: GapPolicy::Continue,
        }
    }
}

impl Default for TailOptions {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Tail<'a> {
    pool: &'a Pool,
    cursor: Cursor,
    options: TailOptions,
    seen: usize,
    deadline: Option<Instant>,
    notify: Option<PoolSemaphore>,
    expected_seq: Option<u64>,
    terminated: bool,
}

pub struct Lite3Tail<'a> {
    pool: &'a Pool,
    cursor: Cursor,
    options: TailOptions,
    seen: usize,
    deadline: Option<Instant>,
    notify: Option<PoolSemaphore>,
    expected_seq: Option<u64>,
    terminated: bool,
}

#[derive(Clone, Debug)]
pub struct ReplayOptions {
    pub speed: f64,
    pub tail: Option<u64>,
    pub since_ns: Option<u64>,
}

impl ReplayOptions {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            tail: None,
            since_ns: None,
        }
    }
}

pub struct Replay {
    messages: Vec<Message>,
    timestamps_ns: Vec<u64>,
    index: usize,
    speed: f64,
}

impl Replay {
    fn new(pool: &Pool, options: ReplayOptions) -> Result<Self, Error> {
        let mut cursor = Cursor::new();
        let mut entries: Vec<(u64, Message)> = Vec::new();

        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if let Some(since) = options.since_ns {
                        if frame.timestamp_ns < since {
                            continue;
                        }
                    }
                    let ts = frame.timestamp_ns;
                    let msg = message_from_frame(&frame)?;
                    entries.push((ts, msg));
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => continue,
            }
        }

        if let Some(n) = options.tail {
            let n = n as usize;
            if entries.len() > n {
                entries = entries.split_off(entries.len() - n);
            }
        }

        let (timestamps_ns, messages): (Vec<u64>, Vec<Message>) = entries.into_iter().unzip();

        Ok(Self {
            messages,
            timestamps_ns,
            index: 0,
            speed: options.speed,
        })
    }

    pub fn next_message(&mut self) -> Option<&Message> {
        if self.index >= self.messages.len() {
            return None;
        }

        if self.index > 0 {
            let prev_ts = self.timestamps_ns[self.index - 1];
            let curr_ts = self.timestamps_ns[self.index];
            if curr_ts > prev_ts {
                let delta_ns = curr_ts - prev_ts;
                let sleep_ns = (delta_ns as f64 / self.speed) as u64;
                if sleep_ns > 0 {
                    std::thread::sleep(Duration::from_nanos(sleep_ns));
                }
            }
        }

        let msg = &self.messages[self.index];
        self.index += 1;
        Some(msg)
    }
}

impl<'a> Tail<'a> {
    fn new(pool: &'a Pool, options: TailOptions) -> Self {
        let deadline = options.timeout.map(|duration| Instant::now() + duration);
        let expected_seq = options.since_seq;
        let notify = if options.notify {
            open_for_path(pool.path()).ok()
        } else {
            None
        };
        Self {
            pool,
            cursor: Cursor::new(),
            options,
            seen: 0,
            deadline,
            notify,
            expected_seq,
            terminated: false,
        }
    }

    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
        if self.terminated {
            return Ok(None);
        }
        if let Some(max) = self.options.max_messages {
            if self.seen >= max {
                return Ok(None);
            }
        }

        loop {
            if let Some(deadline) = self.deadline {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
            }

            match self.cursor.next(self.pool)? {
                CursorResult::Message(frame) => {
                    let should_process = match observe_sequence(
                        &mut self.expected_seq,
                        frame.seq,
                        self.options.gap_policy,
                    ) {
                        Ok(should_process) => should_process,
                        Err(err) => {
                            self.terminated = true;
                            return Err(err);
                        }
                    };
                    if !should_process {
                        continue;
                    }
                    let message = message_from_frame(&frame)?;
                    if !has_required_tags(&message.meta.tags, self.options.tags.as_slice()) {
                        continue;
                    }
                    self.seen += 1;
                    return Ok(Some(message));
                }
                CursorResult::WouldBlock => {
                    let wait_for = wait_interval(self.deadline, self.options.poll_interval);
                    if let Some(notify) = &mut self.notify {
                        match notify.wait(wait_for) {
                            Ok(WaitOutcome::Signaled) | Ok(WaitOutcome::TimedOut) => {}
                            Err(NotifyError::Unavailable) => {
                                self.notify = None;
                                std::thread::sleep(wait_for);
                            }
                            Err(NotifyError::Io(err)) => {
                                let _ = err.kind();
                                std::thread::sleep(wait_for);
                            }
                        }
                    } else {
                        std::thread::sleep(wait_for);
                    }
                }
                CursorResult::FellBehind => {
                    continue;
                }
            }
        }
    }
}

impl<'a> Lite3Tail<'a> {
    fn new(pool: &'a Pool, options: TailOptions) -> Self {
        let deadline = options.timeout.map(|duration| Instant::now() + duration);
        let expected_seq = options.since_seq;
        let notify = if options.notify {
            open_for_path(pool.path()).ok()
        } else {
            None
        };
        Self {
            pool,
            cursor: Cursor::new(),
            options,
            seen: 0,
            deadline,
            notify,
            expected_seq,
            terminated: false,
        }
    }

    pub fn next_frame(&mut self) -> Result<Option<FrameRef<'a>>, Error> {
        if self.terminated {
            return Ok(None);
        }
        if let Some(max) = self.options.max_messages {
            if self.seen >= max {
                return Ok(None);
            }
        }

        loop {
            if let Some(deadline) = self.deadline {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
            }

            match self.cursor.next(self.pool)? {
                CursorResult::Message(frame) => {
                    let should_process = match observe_sequence(
                        &mut self.expected_seq,
                        frame.seq,
                        self.options.gap_policy,
                    ) {
                        Ok(should_process) => should_process,
                        Err(err) => {
                            self.terminated = true;
                            return Err(err);
                        }
                    };
                    if !should_process {
                        continue;
                    }
                    let (meta, _) = decode_payload(frame.payload)?;
                    if !has_required_tags(&meta.tags, self.options.tags.as_slice()) {
                        continue;
                    }
                    self.seen += 1;
                    return Ok(Some(frame));
                }
                CursorResult::WouldBlock => {
                    let wait_for = wait_interval(self.deadline, self.options.poll_interval);
                    if let Some(notify) = &mut self.notify {
                        match notify.wait(wait_for) {
                            Ok(WaitOutcome::Signaled) | Ok(WaitOutcome::TimedOut) => {}
                            Err(NotifyError::Unavailable) => {
                                self.notify = None;
                                std::thread::sleep(wait_for);
                            }
                            Err(NotifyError::Io(err)) => {
                                let _ = err.kind();
                                std::thread::sleep(wait_for);
                            }
                        }
                    } else {
                        std::thread::sleep(wait_for);
                    }
                }
                CursorResult::FellBehind => {
                    continue;
                }
            }
        }
    }
}

pub(crate) fn observe_sequence(
    expected_seq: &mut Option<u64>,
    available_seq: u64,
    policy: GapPolicy,
) -> Result<bool, Error> {
    let Some(expected) = *expected_seq else {
        *expected_seq = available_seq.checked_add(1);
        return Ok(true);
    };
    if available_seq < expected {
        return Ok(false);
    }
    if available_seq > expected && policy == GapPolicy::Error {
        let last_missing = available_seq - 1;
        return Err(Error::new(ErrorKind::RetentionGap)
            .with_message(format!(
                "messages {expected}-{last_missing} are no longer retained; the next available message is {available_seq}"
            ))
            .with_hint("Inspect current pool bounds, then reopen the tail or rebuild from an authoritative source.")
            .with_seq(expected));
    }
    *expected_seq = available_seq.checked_add(1);
    Ok(true)
}

fn has_required_tags(message_tags: &[String], required_tags: &[String]) -> bool {
    required_tags
        .iter()
        .all(|required| message_tags.iter().any(|tag| tag == required))
}

fn wait_interval(deadline: Option<Instant>, poll_interval: Duration) -> Duration {
    if let Some(deadline) = deadline {
        let now = Instant::now();
        if now >= deadline {
            return Duration::from_millis(0);
        }
        let remaining = deadline - now;
        if remaining < poll_interval {
            remaining
        } else {
            poll_interval
        }
    } else {
        poll_interval
    }
}

pub trait PoolApiExt {
    fn append_json(
        &mut self,
        data: &Value,
        tags: &[String],
        options: AppendOptions,
    ) -> Result<Message, Error>;

    fn append_json_now(
        &mut self,
        data: &Value,
        tags: &[String],
        durability: Durability,
    ) -> Result<Message, Error>;

    /// Append a pre-encoded Lite3 payload without JSON encoding/decoding.
    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error>;

    /// Append a pre-encoded Lite3 payload with a generated timestamp.
    fn append_lite3_now(&mut self, payload: &[u8], durability: Durability) -> Result<u64, Error>;

    fn get_message(&self, seq: u64) -> Result<Message, Error>;

    /// Fetch the raw Lite3 payload for a sequence number.
    fn get_lite3(&self, seq: u64) -> Result<FrameRef<'_>, Error>;

    fn tail(&self, options: TailOptions) -> Tail<'_>;

    /// Tail frames without JSON decoding.
    fn tail_lite3(&self, options: TailOptions) -> Lite3Tail<'_>;

    fn replay(&self, options: ReplayOptions) -> Result<Replay, Error>;
}

impl PoolApiExt for Pool {
    fn append_json(
        &mut self,
        data: &Value,
        tags: &[String],
        options: AppendOptions,
    ) -> Result<Message, Error> {
        let payload = crate::core::lite3::encode_message(tags, data)?;
        let seq = self.append_with_options(payload.as_slice(), options)?;
        Ok(Message {
            seq,
            time: format_ts(options.timestamp_ns)?,
            meta: Meta {
                tags: tags.to_vec(),
            },
            data: data.clone(),
        })
    }

    fn append_json_now(
        &mut self,
        data: &Value,
        tags: &[String],
        durability: Durability,
    ) -> Result<Message, Error> {
        let timestamp_ns = now_ns()?;
        let options = AppendOptions::new(timestamp_ns, durability);
        self.append_json(data, tags, options)
    }

    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error> {
        validate_bytes(payload)?;
        self.append_with_options(payload, options)
    }

    fn append_lite3_now(&mut self, payload: &[u8], durability: Durability) -> Result<u64, Error> {
        let timestamp_ns = now_ns()?;
        let options = AppendOptions::new(timestamp_ns, durability);
        self.append_lite3(payload, options)
    }

    fn get_message(&self, seq: u64) -> Result<Message, Error> {
        let frame = self.get(seq)?;
        message_from_frame(&frame)
    }

    fn get_lite3(&self, seq: u64) -> Result<FrameRef<'_>, Error> {
        self.get(seq)
    }

    fn tail(&self, options: TailOptions) -> Tail<'_> {
        Tail::new(self, options)
    }

    fn tail_lite3(&self, options: TailOptions) -> Lite3Tail<'_> {
        Lite3Tail::new(self, options)
    }

    fn replay(&self, options: ReplayOptions) -> Result<Replay, Error> {
        Replay::new(self, options)
    }
}

fn message_from_frame(frame: &FrameRef<'_>) -> Result<Message, Error> {
    let (meta, data) = decode_payload(frame.payload)?;
    Ok(Message {
        seq: frame.seq,
        time: format_ts(frame.timestamp_ns)?,
        meta,
        data,
    })
}

fn decode_payload(payload: &[u8]) -> Result<(Meta, Value), Error> {
    let doc = Lite3DocRef::new(payload);
    let meta_type = doc
        .type_at_key(0, "meta")
        .map_err(|err| err.with_message("missing meta"))?;
    if meta_type != sys::LITE3_TYPE_OBJECT {
        return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object"));
    }

    let meta_ofs = doc
        .key_offset("meta")
        .map_err(|err| err.with_message("missing meta"))?;
    let tags_ofs = doc
        .key_offset_at(meta_ofs, "tags")
        .map_err(|err| err.with_message("missing meta.tags"))?;
    let tags_count = doc
        .count_at(tags_ofs)
        .map_err(|_| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"))?;
    let mut tags = Vec::with_capacity(tags_count as usize);
    for index in 0..tags_count {
        let item_type = doc.array_item_type(tags_ofs, index).map_err(|_| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;
        if item_type != sys::LITE3_TYPE_STRING {
            return Err(
                Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
            );
        }
        let tag = doc.array_string_at(tags_ofs, index).map_err(|_| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;
        tags.push(tag);
    }

    let data_ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    let data_json = doc.to_json_at(data_ofs, false)?;
    let data: Value = serde_json::from_str(&data_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;

    Ok((Meta { tags }, data))
}

fn now_ns() -> Result<u64, Error> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("time went backwards")
                .with_source(err)
        })?;
    Ok(duration.as_nanos() as u64)
}

fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Rfc3339;
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    ts.format(&Rfc3339).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("timestamp format failed")
            .with_source(err)
    })
}

#[cfg(test)]
mod tests {
    use super::{GapPolicy, Meta, PoolApiExt, ReplayOptions, TailOptions, decode_payload};
    use crate::core::error::ErrorKind;
    use crate::core::lite3::{encode_message, json_counter_snapshot, reset_json_counters};
    use crate::core::pool::{Durability, Pool, PoolOptions};
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn decode_payload_round_trip() {
        let data = json!({"x": 1});
        let payload = encode_message(&["tag".to_string()], &data).expect("encode");
        let (meta, out) = decode_payload(payload.as_slice()).expect("decode");
        assert_eq!(
            meta,
            Meta {
                tags: vec!["tag".to_string()]
            }
        );
        assert_eq!(out, data);
    }

    #[test]
    fn decode_payload_avoids_full_doc_json() {
        let data = json!({"x": 1});
        let payload = encode_message(&["tag".to_string()], &data).expect("encode");
        reset_json_counters();
        let _ = decode_payload(payload.as_slice()).expect("decode");
        let (full, partial) = json_counter_snapshot();
        assert_eq!(full, 0);
        assert_eq!(partial, 1);
    }

    #[test]
    fn append_get_tail_lite3() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");

        let payload = encode_message(&["tag".to_string()], &json!({"x": 1})).expect("payload");
        let seq = pool
            .append_lite3(
                payload.as_slice(),
                crate::core::pool::AppendOptions::default(),
            )
            .expect("append");

        let frame = pool.get_lite3(seq).expect("get");
        assert_eq!(frame.seq, seq);
        assert_eq!(frame.payload, payload.as_slice());

        let mut options = TailOptions::new();
        options.since_seq = Some(seq);
        options.max_messages = Some(1);
        let mut tail = pool.tail_lite3(options);
        let frame = tail.next_frame().expect("tail").expect("frame");
        assert_eq!(frame.seq, seq);
        assert_eq!(frame.payload, payload.as_slice());
    }

    #[test]
    fn tail_notify_opt_out_disables_notify() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");

        let mut options = TailOptions::new();
        options.notify = false;
        let tail = pool.tail(options);
        assert!(tail.notify.is_none());
    }

    #[test]
    fn tail_filters_by_required_tags() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");

        let first = pool
            .append_json_now(
                &json!({"n": 1}),
                &["drop".to_string()],
                crate::core::pool::Durability::Fast,
            )
            .expect("append");
        let second = pool
            .append_json_now(
                &json!({"n": 2}),
                &["keep".to_string()],
                crate::core::pool::Durability::Fast,
            )
            .expect("append");

        let mut options = TailOptions::new();
        options.since_seq = Some(first.seq);
        options.max_messages = Some(1);
        options.tags = vec!["keep".to_string()];
        let mut tail = pool.tail(options);
        let message = tail.next_message().expect("tail").expect("message");
        assert_eq!(message.seq, second.seq);
        assert_eq!(message.data, json!({"n": 2}));
    }

    fn append_until_oldest_after(pool: &mut Pool, seq: u64) {
        for n in 0..10_000_u64 {
            pool.append_json_now(
                &json!({"n": n, "padding": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}),
                &["drop".to_string()],
                Durability::Fast,
            )
            .expect("append while filling pool");
            if pool
                .info()
                .expect("pool info")
                .bounds
                .oldest_seq
                .is_some_and(|oldest| oldest > seq)
            {
                return;
            }
        }
        panic!("pool bounds did not advance beyond sequence {seq}");
    }

    #[test]
    fn tail_error_policy_reports_overwrite_after_position_is_established() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0))
            .expect("create");
        let first = writer
            .append_json_now(&json!({"n": 1}), &["keep".to_string()], Durability::Fast)
            .expect("append first");
        let strict_reader = Pool::open(&path).expect("open strict reader");
        let continuing_reader = Pool::open(&path).expect("open continuing reader");

        let mut strict_options = TailOptions::new();
        strict_options.since_seq = Some(first.seq);
        strict_options.gap_policy = GapPolicy::Error;
        let mut strict = strict_reader.tail(strict_options);
        assert_eq!(
            strict
                .next_message()
                .expect("first strict read")
                .expect("first message")
                .seq,
            first.seq
        );

        let mut continuing_options = TailOptions::new();
        continuing_options.since_seq = Some(first.seq);
        let mut continuing = continuing_reader.tail(continuing_options);
        assert_eq!(
            continuing
                .next_message()
                .expect("first continuing read")
                .expect("first message")
                .seq,
            first.seq
        );

        append_until_oldest_after(&mut writer, first.seq + 1);
        let oldest = writer
            .info()
            .expect("pool info")
            .bounds
            .oldest_seq
            .expect("oldest");

        let err = strict.next_message().expect_err("strict tail must fail");
        assert_eq!(err.kind(), ErrorKind::RetentionGap);
        assert_eq!(err.seq(), Some(first.seq + 1));
        assert!(
            err.message()
                .is_some_and(|message| message.contains(&oldest.to_string()))
        );
        assert!(
            strict
                .next_message()
                .expect("terminated strict tail")
                .is_none()
        );

        let resumed = continuing
            .next_message()
            .expect("default tail continues")
            .expect("retained message");
        assert_eq!(resumed.seq, oldest);
    }

    #[test]
    fn stale_start_fails_before_tag_filtering_but_implicit_start_does_not() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0))
            .expect("create");
        let first = writer
            .append_json_now(&json!({"n": 1}), &["keep".to_string()], Durability::Fast)
            .expect("append first");
        append_until_oldest_after(&mut writer, first.seq);
        let oldest = writer
            .info()
            .expect("pool info")
            .bounds
            .oldest_seq
            .expect("oldest");

        let strict_reader = Pool::open(&path).expect("open strict reader");
        let mut strict_options = TailOptions::new();
        strict_options.since_seq = Some(first.seq);
        strict_options.tags = vec!["never".to_string()];
        strict_options.gap_policy = GapPolicy::Error;
        let mut strict = strict_reader.tail(strict_options);
        let err = strict.next_message().expect_err("stale start must fail");
        assert_eq!(err.kind(), ErrorKind::RetentionGap);
        assert_eq!(err.seq(), Some(first.seq));

        let implicit_reader = Pool::open(&path).expect("open implicit reader");
        let mut implicit_options = TailOptions::new();
        implicit_options.max_messages = Some(1);
        implicit_options.gap_policy = GapPolicy::Error;
        let mut implicit = implicit_reader.tail(implicit_options);
        let message = implicit
            .next_message()
            .expect("implicit read")
            .expect("retained message");
        assert_eq!(message.seq, oldest);
    }

    #[test]
    fn lite3_tail_reports_stale_start() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0))
            .expect("create");
        let first = writer
            .append_json_now(&json!({"n": 1}), &[], Durability::Fast)
            .expect("append first");
        append_until_oldest_after(&mut writer, first.seq);

        let reader = Pool::open(&path).expect("open reader");
        let mut options = TailOptions::new();
        options.since_seq = Some(first.seq);
        options.gap_policy = GapPolicy::Error;
        let mut tail = reader.tail_lite3(options);
        let err = tail.next_frame().expect_err("stale Lite3 start must fail");
        assert_eq!(err.kind(), ErrorKind::RetentionGap);
        assert_eq!(err.seq(), Some(first.seq));
    }

    #[test]
    fn replay_returns_messages_in_order() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");

        let values = [json!({"n": 1}), json!({"n": 2}), json!({"n": 3})];
        for (i, data) in values.iter().enumerate() {
            let ts = 1_000_000_000 + (i as u64) * 10_000_000;
            let opts =
                crate::core::pool::AppendOptions::new(ts, crate::core::pool::Durability::Flush);
            pool.append_json(data, &["tag".to_string()], opts)
                .expect("append");
        }

        let options = ReplayOptions::new(100.0);
        let mut replay = pool.replay(options).expect("replay");

        let mut collected = Vec::new();
        while let Some(msg) = replay.next_message() {
            collected.push(msg.data.clone());
        }

        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0], values[0]);
        assert_eq!(collected[1], values[1]);
        assert_eq!(collected[2], values[2]);
    }
}
