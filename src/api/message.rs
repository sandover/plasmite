//! Purpose: Define public message types and append/get/tail helpers for the API.
//! Exports: `Message`, `Meta`, `TailOptions`, `Tail`, `Lite3Tail`.
//! Role: Stable message envelope aligned with the CLI contract.
//! Invariants: Message fields mirror CLI JSON; time is RFC3339 UTC.
//! Invariants: Tail streams preserve ordering and avoid unbounded buffering.
#![allow(clippy::result_large_err)]

use crate::core::cursor::{Cursor, CursorResult, FrameRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{Lite3DocRef, sys, validate_bytes};
use crate::core::notify::{NotifyError, PoolSemaphore, WaitOutcome, open_for_path};
use crate::core::pool::{AppendOptions, Durability, Pool};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

impl Message {
    /// Decode a message payload using the sequence and timestamp of its frame.
    pub fn from_lite3(seq: u64, timestamp_ns: u64, payload: &[u8]) -> Result<Self, Error> {
        let (meta, data) = decode_payload(payload)?;
        Ok(Self {
            seq,
            time: format_ts(timestamp_ns)?,
            meta,
            data,
        })
    }

    /// Return the message timestamp in nanoseconds since the Unix epoch.
    pub fn timestamp_ns(&self) -> Result<u64, Error> {
        parse_timestamp_ns(&self.time)
    }
}

pub(super) fn parse_timestamp_ns(time: &str) -> Result<u64, Error> {
    let time = time::OffsetDateTime::parse(time, &time::format_description::well_known::Rfc3339)
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid message time")
                .with_source(err)
        })?;
    u64::try_from(time.unix_timestamp_nanos()).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("message time is out of range")
            .with_source(err)
    })
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
    /// Stop a blocking tail when its caller withdraws access.
    pub cancel: Option<Arc<AtomicBool>>,
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
            cancel: None,
        }
    }
}

impl Default for TailOptions {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Tail<'a>(TailReader<'a>);

pub struct Lite3Tail<'a>(TailReader<'a>);

struct TailReader<'a> {
    pool: &'a Pool,
    state: TailState,
}

// Shared policy, independent of whether the adapter borrows or owns its Pool.
pub(crate) struct TailState {
    cursor: Cursor,
    options: TailOptions,
    seen: usize,
    deadline: Option<Instant>,
    notify: Option<PoolSemaphore>,
    expected_seq: Option<u64>,
    terminated: bool,
}

pub(crate) enum TailOutcome<T> {
    Item(T),
    WaitExpired,
    Finished,
    Cancelled,
}

impl<'a> TailReader<'a> {
    fn new(pool: &'a Pool, options: TailOptions) -> Self {
        Self {
            pool,
            state: TailState::new(pool, options),
        }
    }

    fn next<T>(
        &mut self,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        self.state.next(self.pool, select)
    }
}

impl TailState {
    pub(crate) fn new(pool: &Pool, options: TailOptions) -> Self {
        let deadline = options.timeout.map(|duration| Instant::now() + duration);
        let expected_seq = options.since_seq;
        let notify = if options.notify {
            open_for_path(pool.path()).ok()
        } else {
            None
        };
        Self {
            cursor: Cursor::new(),
            options,
            seen: 0,
            deadline,
            notify,
            expected_seq,
            terminated: false,
        }
    }

    pub(crate) fn next<T>(
        &mut self,
        pool: &Pool,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        match self.next_wait(pool, None, select)? {
            TailOutcome::Item(item) => Ok(Some(item)),
            _ => Ok(None),
        }
    }

    pub(crate) fn next_wait<T>(
        &mut self,
        pool: &Pool,
        wait: Option<Duration>,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        let wait_deadline = wait
            .map(|duration| {
                Instant::now().checked_add(duration).ok_or_else(|| {
                    Error::new(ErrorKind::Usage).with_message("stream wait is too large")
                })
            })
            .transpose()?;
        if self.terminated {
            return Ok(TailOutcome::Finished);
        }
        if let Some(max) = self.options.max_messages
            && self.seen >= max
        {
            return Ok(TailOutcome::Finished);
        }

        let mut polled = false;
        loop {
            if self
                .options
                .cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Acquire))
            {
                return Ok(TailOutcome::Cancelled);
            }
            if let Some(deadline) = self.deadline
                && Instant::now() >= deadline
            {
                return Ok(TailOutcome::Finished);
            }

            if polled && wait_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(TailOutcome::WaitExpired);
            }
            polled = true;

            match self.cursor.next(pool)? {
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
                    // Sequence space cannot advance after the last u64 value.
                    self.terminated = frame.seq == u64::MAX;
                    if let Some(item) = select(frame, &self.options.tags)? {
                        self.seen += 1;
                        return Ok(TailOutcome::Item(item));
                    }
                    if self.terminated {
                        return Ok(TailOutcome::Finished);
                    }
                }
                CursorResult::WouldBlock => {
                    if wait_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Ok(TailOutcome::WaitExpired);
                    }
                    let poll_interval = if self.options.cancel.is_some() {
                        self.options.poll_interval.min(Duration::from_millis(50))
                    } else {
                        self.options.poll_interval
                    };
                    let wait_for =
                        wait_interval(wait_deadline, wait_interval(self.deadline, poll_interval));
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

impl Tail<'_> {
    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
        self.0.next(|frame, tags| {
            let message = message_from_frame(&frame)?;
            Ok(has_required_tags(&message.meta.tags, tags).then_some(message))
        })
    }
}

impl Lite3Tail<'_> {
    pub fn next_frame(&mut self) -> Result<Option<FrameRef>, Error> {
        self.0.next(|frame, tags| {
            let doc = Lite3DocRef::new(&frame.payload);
            let meta = decode_meta(doc)?;
            // Check readability without constructing a discarded data tree.
            let json = data_json(doc)?;
            let _: serde::de::IgnoredAny =
                serde_json::from_str(&json).map_err(payload_json_error)?;
            Ok(has_required_tags(&meta.tags, tags).then_some(frame))
        })
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

    /// Validate and append a pre-encoded Lite3 payload without re-encoding it.
    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error>;

    /// Append a pre-encoded Lite3 payload with a generated timestamp.
    fn append_lite3_now(&mut self, payload: &[u8], durability: Durability) -> Result<u64, Error>;

    /// Append the original Lite3 bytes and return a receipt prepared from input.
    /// A concurrent overwrite cannot remove the receipt after a successful append.
    fn append_lite3_with_receipt(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<Message, Error> {
        let (meta, data) = decode_payload(payload)?;
        let time = format_ts(options.timestamp_ns)?;
        let seq = self.append_lite3(payload, options)?;
        Ok(Message {
            seq,
            time,
            meta,
            data,
        })
    }

    /// Append with a generated timestamp and a receipt prepared from input.
    fn append_lite3_with_receipt_now(
        &mut self,
        payload: &[u8],
        durability: Durability,
    ) -> Result<Message, Error> {
        self.append_lite3_with_receipt(payload, AppendOptions::new(now_ns()?, durability))
    }

    fn get_message(&self, seq: u64) -> Result<Message, Error>;

    /// Fetch the raw Lite3 payload for a sequence number.
    fn get_lite3(&self, seq: u64) -> Result<FrameRef, Error>;

    fn tail(&self, options: TailOptions) -> Tail<'_>;

    /// Tail frames without JSON decoding.
    fn tail_lite3(&self, options: TailOptions) -> Lite3Tail<'_>;
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

    fn get_lite3(&self, seq: u64) -> Result<FrameRef, Error> {
        self.get(seq)
    }

    fn tail(&self, options: TailOptions) -> Tail<'_> {
        Tail(TailReader::new(self, options))
    }

    fn tail_lite3(&self, options: TailOptions) -> Lite3Tail<'_> {
        Lite3Tail(TailReader::new(self, options))
    }
}

pub(crate) fn message_from_frame(frame: &FrameRef) -> Result<Message, Error> {
    Message::from_lite3(frame.seq, frame.timestamp_ns, &frame.payload)
}

pub(crate) fn message_from_udp_payload(
    seq: u64,
    timestamp_ns: u64,
    payload: &[u8],
) -> Result<Message, Error> {
    // The Lite3 reader expects its document to start at an aligned address.
    let aligned = payload.to_vec();
    validate_bytes(&aligned)?;
    Message::from_lite3(seq, timestamp_ns, &aligned)
}

#[cfg(test)]
std::thread_local! {
    static DATA_TREE_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn decode_payload(payload: &[u8]) -> Result<(Meta, Value), Error> {
    let doc = Lite3DocRef::new(payload);
    let meta = decode_meta(doc)?;
    let json = data_json(doc)?;
    #[cfg(test)]
    DATA_TREE_DECODES.with(|count| count.set(count.get() + 1));
    let data = serde_json::from_str(&json).map_err(payload_json_error)?;
    Ok((meta, data))
}

fn payload_json_error(err: serde_json::Error) -> Error {
    Error::new(ErrorKind::Corrupt)
        .with_message("invalid payload json")
        .with_source(err)
}

fn decode_meta(doc: Lite3DocRef<'_>) -> Result<Meta, Error> {
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

    Ok(Meta { tags })
}

fn data_json(doc: Lite3DocRef<'_>) -> Result<String, Error> {
    let data_ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    doc.to_json_at(data_ofs, false)
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
    use super::{GapPolicy, PoolApiExt, TailOptions, decode_payload};
    use crate::core::error::ErrorKind;
    use crate::core::lite3::{encode_message, json_counter_snapshot, reset_json_counters};
    use crate::core::pool::{Durability, Pool, PoolOptions};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn stored_lite3_messages_preserve_nested_data_and_report_malformed_tags() {
        let dir = tempdir().unwrap();
        let mut pool =
            Pool::create(dir.path().join("legacy.plasmite"), PoolOptions::new(65536)).unwrap();
        let options = crate::core::pool::AppendOptions::new(123, Durability::Fast);
        let data = json!({"nested": [null, {"x": "α\u{0000}β"}], "large": i64::MAX});
        let tags = vec!["sensor".to_owned(), "north".to_owned()];
        let payload = encode_message(&tags, &data).unwrap();
        let seq = pool
            .append_with_options(payload.as_slice(), options)
            .unwrap();
        let message = pool.get_message(seq).unwrap();
        assert_eq!(message.time, "1970-01-01T00:00:00.000000123Z");
        assert_eq!(message.meta.tags, tags);
        assert_eq!(message.data, data);

        let invalid = crate::core::lite3::Lite3Buf::from_json_str(
            r#"{"meta":{"tags":["sensor",7]},"data":{"x":1}}"#,
        )
        .unwrap();
        let seq = pool
            .append_with_options(invalid.as_slice(), options)
            .unwrap();
        let error = pool.get_message(seq).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Corrupt);
        assert_eq!(error.message(), Some("meta.tags must be string array"));
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
    fn raw_tail_checks_readability_and_tags_without_a_data_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool =
            Pool::create(dir.path().join("raw.plasmite"), PoolOptions::new(65536)).unwrap();
        let payload =
            encode_message(&["keep".into()], &json!({"nested": [{"text": "original"}]})).unwrap();
        pool.append_with_timestamp(payload.as_slice(), 123).unwrap();
        super::DATA_TREE_DECODES.with(|count| count.set(0));
        let options = TailOptions {
            tags: vec!["keep".into()],
            max_messages: Some(1),
            ..TailOptions::default()
        };
        let frame = pool.tail_lite3(options).next_frame().unwrap().unwrap();
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.timestamp_ns, 123);
        assert_eq!(frame.payload, payload.as_slice());
        super::DATA_TREE_DECODES.with(|count| assert_eq!(count.get(), 0));
        pool.get_message(1).unwrap();
        super::DATA_TREE_DECODES.with(|count| assert_eq!(count.get(), 1));

        let payload = encode_message(&[], &json!({"text": "broken"})).unwrap();
        let doc = payload.as_doc();
        let data = doc.key_offset("data").unwrap();
        let text = doc.key_offset_at(data, "text").unwrap();
        let mut bytes = payload.as_slice().to_vec();
        bytes[text + 1..text + 5].copy_from_slice(&0u32.to_le_bytes());
        let seq = pool.append(&bytes).unwrap(); // Simulate damaged bytes after publication.
        let options = TailOptions {
            since_seq: Some(seq),
            tags: vec!["never".into()],
            ..TailOptions::default()
        };
        assert_eq!(
            pool.tail_lite3(options.clone())
                .next_frame()
                .unwrap_err()
                .kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(
            pool.tail(options).next_message().unwrap_err().kind(),
            ErrorKind::Corrupt
        );
    }

    #[test]
    fn tails_finish_after_the_last_sequence_without_wrapping_a_checkpoint() {
        use std::io::{Seek, SeekFrom, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("last.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(65536)).unwrap();
        pool.append_json_now(&json!({"last": true}), &[], Durability::Fast)
            .unwrap();
        let header = pool.header_from_mmap().unwrap();
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        for offset in [88, 96, header.ring_offset + 16] {
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(&u64::MAX.to_le_bytes()).unwrap();
        }
        let options = TailOptions {
            since_seq: Some(u64::MAX),
            ..TailOptions::default()
        };
        let mut raw = pool.tail_lite3(options.clone());
        assert_eq!(raw.next_frame().unwrap().unwrap().seq, u64::MAX);
        assert!(raw.next_frame().unwrap().is_none());
        let mut decoded = pool.tail(options);
        assert_eq!(decoded.next_message().unwrap().unwrap().seq, u64::MAX);
        assert!(decoded.next_message().unwrap().is_none());
    }

    #[test]
    fn lite3_receipt_keeps_input_and_exact_identity_after_overwrite() {
        let dir = tempdir().expect("tempdir");
        let mut pool = Pool::create(
            dir.path().join("receipt.plasmite"),
            PoolOptions::new(64 * 1024),
        )
        .expect("create");
        let tags = vec!["original".to_string()];
        let data = json!({"text": "x".repeat(4096)});
        let payload = encode_message(&tags, &data).expect("encode");
        let receipt = pool
            .append_lite3_with_receipt(
                payload.as_slice(),
                crate::core::pool::AppendOptions::new(123, Durability::Fast),
            )
            .expect("append with receipt");
        assert_eq!(receipt.seq, 1);
        assert_eq!(receipt.time, "1970-01-01T00:00:00.000000123Z");
        assert_eq!(pool.get_lite3(1).expect("raw").payload, payload.as_slice());
        for _ in 0..32 {
            pool.append_json_now(&data, &[], Durability::Fast)
                .expect("overwrite");
        }
        assert_eq!(
            pool.get_message(1).expect_err("overwritten").kind(),
            ErrorKind::NotFound
        );
        assert_eq!(receipt.meta.tags, tags);
        assert_eq!(receipt.data, data);
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
    fn tails_read_new_messages_with_notifications_disabled() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        let reader = Pool::open(&path).expect("open reader");

        let mut options = TailOptions::new();
        options.notify = false;
        options.timeout = Some(Duration::from_secs(2));
        let mut json = reader.tail(options.clone());
        let mut lite3 = reader.tail_lite3(options);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                writer
                    .append_json_now(&json!({"n": 1}), &[], Durability::Fast)
                    .expect("append after opening tails");
            });
            assert_eq!(json.next_message().unwrap().unwrap().seq, 1);
            assert_eq!(lite3.next_frame().unwrap().unwrap().seq, 1);
        });
    }

    #[test]
    fn tails_count_only_matching_messages_toward_the_limit() {
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
        pool.append_json_now(&json!({"n": 3}), &["keep".into()], Durability::Fast)
            .expect("append another matching message");

        let mut options = TailOptions::new();
        options.since_seq = Some(first.seq);
        options.max_messages = Some(1);
        options.tags = vec!["keep".to_string()];
        let mut raw = pool.tail_lite3(options.clone());
        let mut tail = pool.tail(options);
        let message = tail.next_message().expect("tail").expect("message");
        assert_eq!(message.seq, second.seq);
        assert_eq!(message.data, json!({"n": 2}));
        assert_eq!(
            raw.next_frame().unwrap().unwrap(),
            pool.get_lite3(second.seq).unwrap()
        );
        assert!(tail.next_message().unwrap().is_none());
        assert!(raw.next_frame().unwrap().is_none());
    }

    #[test]
    fn tails_stop_on_cancellation_and_expired_deadline_with_messages_available() {
        let dir = tempdir().unwrap();
        let mut pool = Pool::create(
            dir.path().join("pool.plasmite"),
            PoolOptions::new(1024 * 1024),
        )
        .unwrap();
        pool.append_json_now(&json!({"n": 1}), &[], Durability::Fast)
            .unwrap();
        pool.append_json_now(&json!({"n": 2}), &[], Durability::Fast)
            .unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let mut options = TailOptions::new();
        options.cancel = Some(cancel.clone());
        let mut json = pool.tail(options.clone());
        let mut lite3 = pool.tail_lite3(options);
        assert_eq!(json.next_message().unwrap().unwrap().seq, 1);
        assert_eq!(lite3.next_frame().unwrap().unwrap().seq, 1);
        cancel.store(true, Ordering::Release);
        assert!(json.next_message().unwrap().is_none());
        assert!(lite3.next_frame().unwrap().is_none());

        let mut expired = TailOptions::new();
        expired.timeout = Some(Duration::ZERO);
        assert!(pool.tail(expired.clone()).next_message().unwrap().is_none());
        assert!(pool.tail_lite3(expired).next_frame().unwrap().is_none());
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
        let mut strict_lite3 = strict_reader.tail_lite3(strict_options.clone());
        let mut strict = strict_reader.tail(strict_options);
        assert_eq!(
            strict
                .next_message()
                .expect("first strict read")
                .expect("first message")
                .seq,
            first.seq
        );
        assert_eq!(strict_lite3.next_frame().unwrap().unwrap().seq, first.seq);

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
        let raw_error = strict_lite3
            .next_frame()
            .expect_err("strict Lite3 tail must fail");
        assert_eq!(raw_error.kind(), ErrorKind::RetentionGap);
        assert_eq!(raw_error.seq(), Some(first.seq + 1));
        assert!(strict_lite3.next_frame().unwrap().is_none());

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
    fn tail_reads_wrapped_messages_in_sequence_order() {
        const RING_SIZE: u64 = 4096;

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + RING_SIZE).with_index_capacity(0),
        )
        .expect("create");

        let mut wrapped_layout = None;
        for n in 0..1_000_u64 {
            pool.append_json_now(
                &json!({"n": n, "padding": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}),
                &[],
                Durability::Fast,
            )
            .expect("append");

            let layout = pool.ring_layout(4_096).expect("ring layout");
            let has_padded_wrap = layout.frames.as_ref().is_some_and(|frames| {
                frames
                    .windows(2)
                    .any(|pair| pair[0].offset + pair[0].len < RING_SIZE && pair[1].offset == 0)
            });
            if has_padded_wrap {
                wrapped_layout = Some(layout);
                break;
            }
        }

        let layout = wrapped_layout.expect("pool should retain frames across an uneven wrap");
        let frames = layout.frames.expect("stored frames");
        assert_eq!(frames[0].offset, layout.tail);
        let expected = frames.iter().map(|frame| frame.seq).collect::<Vec<_>>();
        let oldest = expected[0];

        let mut options = TailOptions::new();
        options.since_seq = Some(oldest);
        options.max_messages = Some(expected.len());
        options.gap_policy = GapPolicy::Error;
        options.notify = false;
        let mut tail = pool.tail(options);
        let mut tailed = Vec::with_capacity(expected.len());
        for _ in 0..expected.len() {
            tailed.push(
                tail.next_message()
                    .expect("tail read")
                    .expect("retained message")
                    .seq,
            );
        }
        assert_eq!(tailed, expected);
    }
}
