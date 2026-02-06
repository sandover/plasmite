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
    pub poll_interval: Duration,
    pub timeout: Option<Duration>,
    pub notify: bool,
}

impl TailOptions {
    pub fn new() -> Self {
        Self {
            since_seq: None,
            max_messages: None,
            poll_interval: Duration::from_millis(50),
            timeout: None,
            notify: true,
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
}

pub struct Lite3Tail<'a> {
    pool: &'a Pool,
    cursor: Cursor,
    options: TailOptions,
    seen: usize,
    deadline: Option<Instant>,
    notify: Option<PoolSemaphore>,
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
        }
    }

    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
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
                    if let Some(min_seq) = self.options.since_seq {
                        if frame.seq < min_seq {
                            continue;
                        }
                    }
                    let message = message_from_frame(&frame)?;
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
        }
    }

    pub fn next_frame(&mut self) -> Result<Option<FrameRef<'a>>, Error> {
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
                    if let Some(min_seq) = self.options.since_seq {
                        if frame.seq < min_seq {
                            continue;
                        }
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
    let descrips_ofs = doc
        .key_offset_at(meta_ofs, "tags")
        .map_err(|err| err.with_message("missing meta.tags"))?;
    let descrips_json = doc.to_json_at(descrips_ofs, false)?;
    let descrips_value: Value = serde_json::from_str(&descrips_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let tags = descrips_value
        .as_array()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"))?
        .iter()
        .map(|item| item.as_str().map(|s| s.to_string()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;

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
    use super::{Meta, PoolApiExt, ReplayOptions, TailOptions, decode_payload};
    use crate::core::lite3::{encode_message, json_counter_snapshot, reset_json_counters};
    use crate::core::pool::{Pool, PoolOptions};
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
        assert!(partial >= 2);
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
