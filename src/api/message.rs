//! Purpose: Define public message types and append/get/tail helpers for the API.
//! Exports: message envelopes, feed options and readers.
//! Role: Stable message envelope aligned with the CLI contract.
//! Invariants: Message time holds Unix nanoseconds; text uses RFC 3339 UTC.
//! Invariants: Follow streams preserve ordering and avoid unbounded buffering.
#![allow(clippy::result_large_err)]

use crate::api::notify::{NotifyHandle, open_for_pool};
use crate::core::cursor::{Cursor, CursorResult, FrameRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{Lite3DocRef, sys, validate_bytes};
use crate::core::notify::{NotifyError, WaitOutcome};
use crate::core::pool::{AppendOptions, Durability, Pool};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Meta {
    pub tags: Vec<String>,
}

impl Meta {
    /// Decode metadata and validate the entire data payload without building its tree.
    ///
    /// JSON-compatible values use a checked binary walk. Native byte values
    /// retain the legacy base64 export check; neither path skips validation.
    pub fn from_lite3(payload: &[u8]) -> Result<Self, Error> {
        let doc = Lite3DocRef::new(payload);
        let meta = decode_meta(doc)?;
        validate_data(doc)?;
        Ok(meta)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub seq: u64,
    pub time: u64,
    pub meta: Meta,
    pub data: Value,
}

impl Message {
    /// Decode a message payload using the sequence and timestamp of its frame.
    pub fn from_lite3(seq: u64, timestamp_ns: u64, payload: &[u8]) -> Result<Self, Error> {
        let (meta, data) = decode_payload(payload)?;
        Ok(Self {
            seq,
            time: timestamp_ns,
            meta,
            data,
        })
    }

    /// Format the stored instant for a text envelope.
    pub fn time_rfc3339(&self) -> String {
        format_ts(self.time).expect("u64 Unix nanoseconds fit in RFC 3339")
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

/// An inclusive read position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Since {
    Oldest,
    Seq(u64),
    At(u64),
    Ago(Duration),
}

impl std::str::FromStr for Since {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let text = text.trim();
        if text == "oldest" {
            return Ok(Self::Oldest);
        }
        if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
            return text.parse().map(Self::Seq).map_err(|err| {
                Error::new(ErrorKind::Usage)
                    .with_message("sequence is out of range")
                    .with_source(err)
            });
        }
        if let Some(nanos) = crate::since::parse_relative_ns(text) {
            return Ok(Self::Ago(Duration::from_nanos(nanos)));
        }
        crate::since::parse_rfc3339_ns(text)
            .map(Self::At)
            .map_err(|err| {
                Error::new(ErrorKind::Usage)
                    .with_message("since requires a sequence, oldest, duration or RFC 3339 time")
                    .with_source(err)
            })
    }
}

#[derive(Clone, Debug)]
pub(super) enum ReadStart {
    Default,
    Since(Since),
    Last(u64),
}

/// Options shared by follow and fetch. Tags all have to match.
#[derive(Clone, Debug)]
pub struct Read {
    pub(super) start: ReadStart,
    pub(super) tags: Vec<String>,
    pub(super) cancel: Option<Arc<AtomicBool>>,
}

impl Read {
    pub fn new() -> Self {
        Self {
            start: ReadStart::Default,
            tags: Vec::new(),
            cancel: None,
        }
    }

    pub fn since(position: Since) -> Self {
        Self {
            start: ReadStart::Since(position),
            ..Self::new()
        }
    }

    pub fn last(count: u64) -> Self {
        Self {
            start: ReadStart::Last(count),
            ..Self::new()
        }
    }

    pub fn tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }
}

impl Default for Read {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for storing or forwarding one message.
#[derive(Clone, Debug, Default)]
pub struct Feed {
    tags: Option<Vec<String>>,
    durability: Durability,
}

impl Feed {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = Some(tags.into_iter().map(Into::into).collect());
        self
    }

    pub fn durability(mut self, durability: Durability) -> Self {
        self.durability = durability;
        self
    }
}

/// A feed either stores data or forwards a complete message.
pub enum FeedInput<'a> {
    Data(&'a Value),
    Message(&'a Message),
}

impl<'a> From<&'a Value> for FeedInput<'a> {
    fn from(value: &'a Value) -> Self {
        Self::Data(value)
    }
}

impl<'a> From<&'a Message> for FeedInput<'a> {
    fn from(message: &'a Message) -> Self {
        Self::Message(message)
    }
}

impl Feed {
    pub(super) fn resolve<'a>(
        &'a self,
        input: FeedInput<'a>,
    ) -> Result<(&'a Value, &'a [String], Durability), Error> {
        let (data, tags) = match input {
            FeedInput::Data(data) => (data, self.tags.as_deref().unwrap_or_default()),
            FeedInput::Message(message) => {
                if self.tags.is_some() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("tags cannot accompany a forwarded message"));
                }
                (&message.data, message.meta.tags.as_slice())
            }
        };
        if !data.is_object() {
            return Err(Error::new(ErrorKind::Usage).with_message("message data must be an object"));
        }
        Ok((data, tags, self.durability))
    }

    pub(crate) fn encoded_durability(&self) -> Result<Durability, Error> {
        if self.tags.is_some() {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("tags cannot accompany a forwarded message"));
        }
        Ok(self.durability)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedReceipt {
    pub seq: u64,
    pub time: u64,
}

pub struct Follow<'a>(TailReader<'a>);

/// A pull-based stream of independent immutable Lite3 snapshots.
///
/// Retaining a message keeps only that message alive after this tail or pool closes.
pub struct ViewFollow<'a>(TailReader<'a>);

impl Pool {
    /// Store data or forward a message using a new sequence and wall-clock time.
    pub fn feed<'a>(
        &mut self,
        input: impl Into<FeedInput<'a>>,
        options: Feed,
    ) -> Result<Message, Error> {
        let (data, tags, durability) = options.resolve(input.into())?;
        self.append_json_now(data, tags, durability)
    }

    /// Store an application object from JSON text.
    pub fn feed_json(&mut self, json: &[u8], options: Feed) -> Result<Message, Error> {
        let data: Value = serde_json::from_slice(json).map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid JSON object")
                .with_source(err)
        })?;
        self.feed(&data, options)
    }

    /// Forward a complete Lite3 envelope with a fresh identity.
    pub fn feed_lite3(&mut self, bytes: &[u8], options: Feed) -> Result<Message, Error> {
        let durability = options.encoded_durability()?;
        crate::core::lite3::validate_feed_bytes(bytes)?;
        let (meta, data) = decode_payload(bytes)?;
        let FeedReceipt { seq, time } = self.append_prepared(bytes, durability)?;
        Ok(Message {
            seq,
            time,
            meta,
            data,
        })
    }

    /// Forward a complete Lite3 envelope without decoding the stored result.
    pub fn feed_lite3_receipt(
        &mut self,
        bytes: &[u8],
        options: Feed,
    ) -> Result<FeedReceipt, Error> {
        let durability = options.encoded_durability()?;
        crate::core::lite3::validate_feed_bytes(bytes)?;
        self.append_prepared(bytes, durability)
    }

    /// Forward an immutable message view while retaining its encoded payload.
    pub fn feed_view(
        &mut self,
        view: &crate::view::MessageView,
        options: Feed,
    ) -> Result<crate::view::MessageView, Error> {
        let receipt = self.append_prepared(view.bytes(), options.encoded_durability()?)?;
        Ok(view.with_identity(receipt.seq, receipt.time, 0))
    }

    pub(crate) fn append_prepared(
        &mut self,
        payload: &[u8],
        durability: Durability,
    ) -> Result<FeedReceipt, Error> {
        let time = now_ns()?;
        let seq = self.append_with_options(payload, AppendOptions::new(time, durability))?;
        Ok(FeedReceipt { seq, time })
    }

    pub(crate) fn feed_prepared_view(
        &mut self,
        mut view: crate::view::MessageView,
        durability: Durability,
    ) -> Result<crate::view::MessageView, Error> {
        let receipt = self.append_prepared(view.bytes(), durability)?;
        view.seq = receipt.seq;
        view.time = receipt.time;
        view.flags = 0;
        Ok(view)
    }

    /// Open a reader on new messages unless the caller explicitly selects history.
    pub fn follow(&self, read: Read) -> Result<Follow<'_>, Error> {
        self.open_read(read, true).map(Follow)
    }

    /// Open a reader whose last possible sequence is fixed when it opens.
    pub fn fetch(&self, read: Read) -> Result<Follow<'_>, Error> {
        self.open_read(read, false).map(Follow)
    }

    pub fn follow_view(&self, read: Read) -> Result<ViewFollow<'_>, Error> {
        self.open_read(read, true).map(ViewFollow)
    }

    pub fn fetch_view(&self, read: Read) -> Result<ViewFollow<'_>, Error> {
        self.open_read(read, false).map(ViewFollow)
    }

    pub fn follow_lite3(&self, read: Read) -> Result<Lite3Follow<'_>, Error> {
        self.open_read(read, true).map(Lite3Follow)
    }

    pub fn fetch_lite3(&self, read: Read) -> Result<Lite3Follow<'_>, Error> {
        self.open_read(read, false).map(Lite3Follow)
    }

    fn open_read(&self, read: Read, live: bool) -> Result<TailReader<'_>, Error> {
        Ok(TailReader {
            pool: self,
            state: self.read_state(read, live)?,
        })
    }

    /// Finish the same finite HTTP range after the server's time limit.
    #[doc(hidden)]
    pub fn continue_fetch_lite3(&self, read: Read, until: u64) -> Result<Lite3Follow<'_>, Error> {
        Ok(Lite3Follow(TailReader {
            pool: self,
            state: self.read_state_until(read, false, Some(until), false)?,
        }))
    }

    pub(crate) fn read_state(&self, read: Read, live: bool) -> Result<TailState, Error> {
        self.read_state_until(read, live, None, live)
    }

    fn read_state_until(
        &self,
        read: Read,
        live: bool,
        until: Option<u64>,
        notify: bool,
    ) -> Result<TailState, Error> {
        let mut cursor = Cursor::new();
        let (first, upper) = {
            let _lock = self.read_lock()?;
            let header = self.header_locked()?;
            let oldest = header.oldest_seq.max(1);
            let newest = header.newest_seq;
            let next = newest.checked_add(1);
            let first = match read.start {
                ReadStart::Default if live => next,
                ReadStart::Default | ReadStart::Since(Since::Oldest) => Some(oldest),
                ReadStart::Last(0) => next,
                ReadStart::Last(count) => Some(newest.saturating_sub(count - 1).max(oldest)),
                ReadStart::Since(Since::Seq(seq)) => {
                    if seq == 0 || next.is_some_and(|next| seq > next) {
                        return Err(Error::new(ErrorKind::Usage)
                            .with_message("since must be a positive sequence at or before the next unwritten message")
                            .with_seq(seq));
                    }
                    Some(seq)
                }
                ReadStart::Since(Since::At(time)) => {
                    cursor.seek_time_locked(self, header, time)?.or(next)
                }
                ReadStart::Since(Since::Ago(duration)) => {
                    let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
                    let time = now_ns()?.saturating_sub(nanos);
                    cursor.seek_time_locked(self, header, time)?.or(next)
                }
            };
            if let Some(first) = first {
                if first < oldest {
                    return Err(Error::retention_gap(first, oldest));
                }
                if Some(first) == next {
                    cursor.seek_to(header.head_off as usize);
                } else if first > oldest
                    && matches!(
                        read.start,
                        ReadStart::Last(_) | ReadStart::Since(Since::Seq(_))
                    )
                {
                    self.seek_index_locked(&mut cursor, header, first);
                }
            }
            if until.is_some_and(|until| until > newest) {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("invalid fetch continuation boundary"));
            }
            (first, (!live).then_some(until.unwrap_or(newest)))
        };
        Ok(TailState {
            cursor,
            tags: read.tags,
            cancel: read.cancel,
            #[cfg(any(target_os = "linux", windows))]
            cancel_signal: None,
            notify: notify.then(|| open_for_pool(self)).flatten(),
            expected_seq: first,
            terminated: first.is_none()
                || upper.zip(first).is_some_and(|(last, first)| first > last),
            upper,
            failure: None,
        })
    }
}

pub struct Lite3Follow<'a>(TailReader<'a>);

/// A live reader whose caller manages wake-ups and schedules bounded turns.
#[doc(hidden)]
pub struct HostedRead {
    pool: Arc<Pool>,
    state: TailState,
}

impl HostedRead {
    pub fn new(pool: Arc<Pool>, read: Read) -> Result<Self, Error> {
        let state = pool.read_state_until(read, true, None, false)?;
        Ok(Self { pool, state })
    }

    pub fn start_position(pool: &Pool, read: Read) -> Result<Option<u64>, Error> {
        Ok(pool.read_state_until(read, true, None, false)?.expected_seq)
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub fn restart(&mut self, read: Read) -> Result<(), Error> {
        self.state = self.pool.read_state_until(read, true, None, false)?;
        Ok(())
    }

    pub fn position(&self) -> Option<u64> {
        self.state.expected_seq
    }

    pub fn turn(&mut self, remaining: &mut usize) -> Result<ReadStatus<FrameRef>, Error> {
        self.turn_selected(remaining, select_lite3_frame)
    }

    pub fn turn_json(&mut self, remaining: &mut usize) -> Result<ReadStatus<Vec<u8>>, Error> {
        self.turn_selected(remaining, |frame, tags| frame_json(&frame, Some(tags)))
    }

    pub(crate) fn turn_selected<T>(
        &mut self,
        remaining: &mut usize,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<ReadStatus<T>, Error> {
        self.pool.check_current_path()?;
        let outcome = self.state.next_wait_using(
            Some(Duration::ZERO),
            None,
            |cursor, expected| {
                let Some(left) = remaining.checked_sub(1) else {
                    return Ok(CursorResult::WouldBlock);
                };
                match cursor.next_checked(&self.pool, expected)? {
                    CursorResult::WouldBlock => Ok(CursorResult::WouldBlock),
                    result => {
                        *remaining = left;
                        Ok(result)
                    }
                }
            },
            select,
        )?;
        Ok(match outcome {
            TailOutcome::Item(item) => ReadStatus::Message(item),
            TailOutcome::WaitExpired => ReadStatus::Waiting,
            TailOutcome::Finished => ReadStatus::Complete,
            TailOutcome::Cancelled => ReadStatus::Cancelled,
        })
    }
}

struct TailReader<'a> {
    pool: &'a Pool,
    state: TailState,
}

// Shared policy, independent of whether the adapter borrows or owns its Pool.
pub(crate) struct TailState {
    cursor: Cursor,
    tags: Vec<String>,
    cancel: Option<Arc<AtomicBool>>,
    #[cfg(any(target_os = "linux", windows))]
    cancel_signal: Option<Arc<crate::core::notify::CancelSignal>>,
    notify: Option<NotifyHandle>,
    expected_seq: Option<u64>,
    terminated: bool,
    upper: Option<u64>,
    failure: Option<Box<Error>>,
}

pub(crate) enum TailOutcome<T> {
    Item(T),
    WaitExpired,
    Finished,
    Cancelled,
}

impl<'a> TailReader<'a> {
    fn next<T>(
        &mut self,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        self.state.next(self.pool, select)
    }
}

impl TailState {
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) fn with_cancel_signal(
        mut self,
        signal: Arc<crate::core::notify::CancelSignal>,
    ) -> Self {
        self.cancel = Some(signal.flag());
        self.cancel_signal = Some(signal);
        self
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
        self.next_wait_using(
            wait,
            None,
            |cursor, expected| cursor.next_checked(pool, expected),
            select,
        )
    }

    fn next_wait_using<T>(
        &mut self,
        wait: Option<Duration>,
        max_bytes: Option<usize>,
        acquire: impl FnMut(&mut Cursor, Option<u64>) -> Result<CursorResult, Error>,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        if let Some(error) = &self.failure {
            return Err((**error).clone());
        }
        let result = self.poll_cursor(wait, max_bytes, acquire, select);
        // A bad wait or too-small batch budget leaves the reader usable.
        if let Err(error) = &result
            && error.kind() != ErrorKind::Usage
        {
            self.failure = Some(Box::new(error.clone()));
        }
        result
    }

    fn poll_cursor<T>(
        &mut self,
        wait: Option<Duration>,
        max_bytes: Option<usize>,
        mut acquire: impl FnMut(&mut Cursor, Option<u64>) -> Result<CursorResult, Error>,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        // A poll scans available records but never sleeps. Filters and a
        // requested starting sequence must not conceal an already-ready item.
        let poll_only = wait == Some(Duration::ZERO);
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
        let mut polled = false;
        loop {
            if self.cancelled() {
                return Ok(TailOutcome::Cancelled);
            }
            if polled
                && !poll_only
                && wait_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Ok(TailOutcome::WaitExpired);
            }
            polled = true;

            let checkpoint = max_bytes.map(|_| self.cursor.clone());
            match acquire(&mut self.cursor, self.expected_seq)? {
                CursorResult::Message(frame) => {
                    if self.upper.is_some_and(|upper| frame.seq > upper) {
                        self.terminated = true;
                        return Ok(TailOutcome::Finished);
                    }
                    let seq = frame.seq;
                    if !check_sequence(self.expected_seq, seq)? {
                        continue;
                    }
                    let bytes = frame.payload.len();
                    let item = select(frame, &self.tags)?;
                    if let Some(limit) = max_bytes
                        && bytes > limit
                        && item.is_some()
                    {
                        self.cursor = checkpoint.expect("bounded reads save the cursor");
                        return Err(Error::new(ErrorKind::Usage).with_message(format!(
                            "next frame needs {bytes} encoded bytes, above batch budget {limit}"
                        )));
                    }
                    self.expected_seq = seq.checked_add(1);
                    // Sequence space cannot advance after the last u64 value.
                    self.terminated = seq == u64::MAX || self.upper == Some(seq);
                    if let Some(item) = item {
                        return Ok(TailOutcome::Item(item));
                    }
                    if self.terminated {
                        return Ok(TailOutcome::Finished);
                    }
                }
                CursorResult::WouldBlock => {
                    if self.upper.is_some() {
                        self.terminated = true;
                        return Ok(TailOutcome::Finished);
                    }
                    if wait_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Ok(TailOutcome::WaitExpired);
                    }
                    let poll_interval = Duration::from_millis(50);
                    if let Some(notify) = &mut self.notify {
                        #[cfg(any(target_os = "linux", windows))]
                        let wakeable = self.cancel_signal.is_some();
                        #[cfg(target_os = "macos")]
                        let wakeable = false;
                        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                        let interval =
                            if notify.uses_progress() && (self.cancel.is_none() || wakeable) {
                                crate::core::notify::RECOVERY_CHECK
                            } else {
                                poll_interval
                            };
                        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                        let interval = poll_interval;
                        let wait_for = wait_interval(wait_deadline, interval);
                        match notify.wait_for_generation(
                            self.cursor.observed_generation(),
                            wait_for,
                            #[cfg(any(target_os = "linux", windows))]
                            self.cancel_signal.as_deref(),
                        ) {
                            Ok(WaitOutcome::Signaled) | Ok(WaitOutcome::TimedOut) => continue,
                            Err(NotifyError::Unavailable) => self.notify = None,
                            Err(NotifyError::Io(err)) => {
                                let _ = err.kind();
                            }
                        }
                    }
                    std::thread::sleep(wait_interval(wait_deadline, poll_interval));
                }
                CursorResult::FellBehind => {
                    continue;
                }
            }
        }
    }

    pub(crate) fn next_wait_bounded<T>(
        &mut self,
        pool: &Pool,
        wait: Option<Duration>,
        max_bytes: usize,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        if max_bytes == 0 {
            return Err(Error::new(ErrorKind::Usage).with_message("byte bound must be positive"));
        }
        // A size check at the cursor would reject an oversized record before
        // the selector could discard it. Keep the early check for unfiltered reads.
        let filtered = !self.tags.is_empty();
        self.next_wait_using(
            wait,
            filtered.then_some(max_bytes),
            |cursor, expected| {
                let _lock = pool.read_lock()?;
                if filtered {
                    cursor.next_owned_locked(pool, expected)
                } else {
                    cursor.next_locked_bounded(pool, max_bytes, expected)
                }
            },
            select,
        )
    }

    /// Explicitly acquire a bounded group; regular next calls never prefetch.
    pub(crate) fn next_batch_wait<T>(
        &mut self,
        pool: &Pool,
        wait: Option<Duration>,
        max_count: usize,
        max_bytes: usize,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<Vec<T>>, Error> {
        if max_count == 0 || max_bytes == 0 {
            return Err(Error::new(ErrorKind::Usage).with_message("batch bounds must be positive"));
        }
        let cancel = self.cancel.clone();
        let upper = self.upper;
        let filtered = !self.tags.is_empty();
        let mut frames = std::collections::VecDeque::new();
        let first = self.next_wait_using(
            wait,
            filtered.then_some(max_bytes),
            |cursor, expected| {
                // Filtering may reject the first record. Reuse only companions
                // acquired by this explicit call, committing one checkpoint at a time.
                if let Some((frame, checkpoint)) = frames.pop_front() {
                    *cursor = checkpoint;
                    return Ok(CursorResult::Message(frame));
                }
                // One lock covers the first snapshot and all available companions.
                // Conversion never runs inside this lock.
                let _lock = pool.read_lock()?;
                let first = if filtered {
                    cursor.next_owned_locked(pool, expected)?
                } else {
                    cursor.next_locked_bounded(pool, max_bytes, expected)?
                };
                if let CursorResult::Message(frame) = &first {
                    if expected.is_some_and(|expected| frame.seq < expected) {
                        return Ok(first);
                    }
                    let mut used = frame.payload.len();
                    let mut ahead = cursor.clone();
                    while frames.len() + 1 < max_count && used < max_bytes {
                        if cancel
                            .as_ref()
                            .is_some_and(|flag| flag.load(Ordering::Acquire))
                        {
                            break;
                        }
                        match ahead.next_locked_bounded(pool, max_bytes - used, None) {
                            Ok(CursorResult::Message(frame)) => {
                                if upper.is_some_and(|upper| frame.seq > upper) {
                                    break;
                                }
                                used += frame.payload.len();
                                frames.push_back((frame, ahead.clone()));
                            }
                            Ok(CursorResult::FellBehind) => continue,
                            // Return a valid prefix; the next explicit call sees
                            // this error or retries with a fresh byte budget.
                            Ok(CursorResult::WouldBlock) | Err(_) => break,
                        }
                    }
                }
                Ok(first)
            },
            &select,
        )?;
        let first = match first {
            TailOutcome::Item(first) => first,
            TailOutcome::WaitExpired => return Ok(TailOutcome::WaitExpired),
            TailOutcome::Finished => return Ok(TailOutcome::Finished),
            TailOutcome::Cancelled => return Ok(TailOutcome::Cancelled),
        };
        let mut items = vec![first];
        for (frame, checkpoint) in frames {
            if self.terminated || self.cancelled() {
                break;
            }
            let seq = frame.seq;
            let process = match check_sequence(self.expected_seq, seq) {
                Ok(process) => process,
                Err(_) => break,
            };
            let item = if process {
                match select(frame, &self.tags) {
                    Ok(item) => item,
                    Err(_) => break,
                }
            } else {
                None
            };
            self.cursor = checkpoint;
            if process {
                self.expected_seq = seq.checked_add(1);
                self.terminated = seq == u64::MAX || self.upper == Some(seq);
            }
            items.extend(item);
            if self.terminated {
                break;
            }
        }
        Ok(TailOutcome::Item(items))
    }

    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }
}

impl Follow<'_> {
    pub fn next_message(&mut self) -> Result<Option<Message>, Error> {
        self.0.next(|frame, tags| {
            let message = message_from_frame(&frame)?;
            Ok(has_required_tags(&message.meta.tags, tags).then_some(message))
        })
    }
}

impl Iterator for Follow<'_> {
    type Item = Result<Message, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_message().transpose()
    }
}

impl ViewFollow<'_> {
    pub fn next_view(&mut self) -> Result<Option<crate::view::MessageView>, Error> {
        self.0.next(select_view)
    }
}

impl Iterator for ViewFollow<'_> {
    type Item = Result<crate::view::MessageView, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_view().transpose()
    }
}

impl Lite3Follow<'_> {
    pub fn next_frame(&mut self) -> Result<Option<FrameRef>, Error> {
        self.0.next(select_lite3_frame)
    }

    /// Poll the reader without moving its end boundary or imposing a stream timeout.
    #[doc(hidden)]
    pub fn poll(&mut self, wait: Duration) -> Result<ReadPoll, Error> {
        Ok(match self.poll_selected(wait, select_lite3_frame)? {
            ReadStatus::Message(frame) => ReadPoll::Message(frame),
            ReadStatus::Waiting => ReadPoll::Waiting,
            ReadStatus::Complete => ReadPoll::Complete,
            ReadStatus::Cancelled => ReadPoll::Cancelled,
        })
    }

    #[doc(hidden)]
    pub fn poll_json(&mut self, wait: Duration) -> Result<ReadStatus<Vec<u8>>, Error> {
        self.poll_selected(wait, |frame, required| frame_json(&frame, Some(required)))
    }

    pub(crate) fn poll_selected<T>(
        &mut self,
        wait: Duration,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<ReadStatus<T>, Error> {
        let result = self.0.state.next_wait(self.0.pool, Some(wait), select);
        match result {
            Ok(TailOutcome::Item(frame)) => Ok(ReadStatus::Message(frame)),
            Ok(TailOutcome::WaitExpired) => Ok(ReadStatus::Waiting),
            Ok(TailOutcome::Finished) => Ok(ReadStatus::Complete),
            Ok(TailOutcome::Cancelled) => Ok(ReadStatus::Cancelled),
            Err(err) => Err(err),
        }
    }

    /// Return the next unexamined sequence and the opening end of a finite read.
    #[doc(hidden)]
    pub fn position(&self) -> (Option<u64>, Option<u64>) {
        (self.0.state.expected_seq, self.0.state.upper)
    }
}

#[doc(hidden)]
pub enum ReadPoll {
    Message(FrameRef),
    Waiting,
    Complete,
    Cancelled,
}

#[doc(hidden)]
pub enum ReadStatus<T> {
    Message(T),
    Waiting,
    Complete,
    Cancelled,
}

impl ReadPoll {
    #[doc(hidden)]
    pub fn map<T>(self, f: impl FnOnce(FrameRef) -> T) -> ReadStatus<T> {
        match self {
            Self::Message(frame) => ReadStatus::Message(f(frame)),
            Self::Waiting => ReadStatus::Waiting,
            Self::Complete => ReadStatus::Complete,
            Self::Cancelled => ReadStatus::Cancelled,
        }
    }
}

pub(crate) fn select_view(
    frame: FrameRef,
    tags: &[String],
) -> Result<Option<crate::view::MessageView>, Error> {
    let view =
        crate::view::MessageView::from_owned(frame.payload, frame.seq, frame.time, frame.flags)?;
    if tags.is_empty() {
        return Ok(Some(view));
    }
    // Use the checked snapshot's lookup, which honors the last duplicate key.
    let corrupt = || Error::new(ErrorKind::Corrupt).with_message("invalid message tags");
    let labels = view
        .root()
        .get("meta")?
        .ok_or_else(corrupt)?
        .get("tags")?
        .ok_or_else(corrupt)?;
    let labels = labels
        .iter()?
        .map(|item| item?.1.text().ok_or_else(corrupt))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(has_required_tags(&labels, tags).then_some(view))
}

pub(crate) fn frame_matches_tags(frame: &FrameRef, tags: &[String]) -> Result<bool, Error> {
    if tags.is_empty() {
        return Ok(true);
    }
    let meta = Meta::from_lite3(&frame.payload)?;
    Ok(has_required_tags(&meta.tags, tags))
}

pub(crate) fn select_lite3_frame(
    frame: FrameRef,
    tags: &[String],
) -> Result<Option<FrameRef>, Error> {
    if tags.is_empty() {
        let doc = Lite3DocRef::new(&frame.payload);
        let (ofs, count) = meta_tags(doc)?;
        for index in 0..count {
            meta_tag(doc, ofs, index)?;
        }
        validate_data(doc)?;
        Ok(Some(frame))
    } else {
        Ok(frame_matches_tags(&frame, tags)?.then_some(frame))
    }
}

impl Iterator for Lite3Follow<'_> {
    type Item = Result<FrameRef, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
    }
}

fn check_sequence(expected: Option<u64>, available: u64) -> Result<bool, Error> {
    match expected {
        Some(expected) if available > expected => Err(Error::retention_gap(expected, available)),
        Some(expected) => Ok(available == expected),
        None => Ok(true),
    }
}

fn has_required_tags<T: AsRef<str>>(message_tags: &[T], required_tags: &[String]) -> bool {
    required_tags
        .iter()
        .all(|required| message_tags.iter().any(|tag| tag.as_ref() == required))
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

pub(crate) trait PoolApiExt {
    /// Forward a checked snapshot with a fresh identity and system timestamp.
    #[cfg(test)]
    fn append_value<T: serde::Serialize + ?Sized>(
        &mut self,
        data: &T,
        tags: &[&str],
        options: AppendOptions,
    ) -> Result<crate::view::MessageView, Error>
    where
        Self: Sized,
    {
        let view = crate::view::MessageView::encode(data, tags)?;
        self.append_view(&view, options)
    }

    /// Forward an immutable payload, assigning a fresh destination identity.
    #[cfg(test)]
    fn append_view(
        &mut self,
        view: &crate::view::MessageView,
        options: AppendOptions,
    ) -> Result<crate::view::MessageView, Error> {
        let seq = self.append_lite3(view.bytes(), options)?;
        Ok(view.with_identity(seq, options.timestamp_ns, 0))
    }

    /// Copy one snapshot and validate it once, retaining its encoded bytes.
    #[cfg(test)]
    #[cfg(test)]
    fn get_view(&self, seq: u64) -> Result<crate::view::MessageView, Error> {
        let frame = self.get_lite3(seq)?;
        crate::view::MessageView::from_owned(frame.payload, frame.seq, frame.time, frame.flags)
    }

    #[cfg(test)]
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

    /// Append and return only the committed sequence and Unix timestamp in nanoseconds.
    /// The identity comes from this commit; it never requires a later pool read.
    #[cfg(test)]
    fn append_json_identity(
        &mut self,
        data: &Value,
        tags: &[String],
        options: AppendOptions,
    ) -> Result<(u64, u64), Error> {
        let seq = self.append_json(data, tags, options)?.seq;
        Ok((seq, options.timestamp_ns))
    }

    /// Append validated binary input and return its committed numeric identity.
    #[cfg(test)]
    fn append_lite3_identity(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<(u64, u64), Error> {
        let seq = self.append_lite3(payload, options)?;
        Ok((seq, options.timestamp_ns))
    }

    /// Validate and append a pre-encoded Lite3 payload without re-encoding it.
    #[cfg(test)]
    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error>;

    /// Append the original Lite3 bytes and return a receipt prepared from input.
    /// A concurrent overwrite cannot remove the receipt after a successful append.
    #[cfg(test)]
    fn append_lite3_with_receipt(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<Message, Error>;

    /// Append with a generated timestamp and a receipt prepared from input.
    #[cfg(test)]
    fn get_message(&self, seq: u64) -> Result<Message, Error>;

    /// Fetch the raw Lite3 payload for a sequence number.
    #[cfg(test)]
    fn get_lite3(&self, seq: u64) -> Result<FrameRef, Error>;
}

impl PoolApiExt for Pool {
    #[cfg(test)]
    fn append_view(
        &mut self,
        view: &crate::view::MessageView,
        options: AppendOptions,
    ) -> Result<crate::view::MessageView, Error> {
        let seq = self.append_with_options(view.bytes(), options)?;
        Ok(view.with_identity(seq, options.timestamp_ns, 0))
    }

    #[cfg(test)]
    fn append_json_identity(
        &mut self,
        data: &Value,
        tags: &[String],
        options: AppendOptions,
    ) -> Result<(u64, u64), Error> {
        let payload = crate::core::lite3::encode_message(tags, data)?;
        let seq = self.append_with_options(payload.as_slice(), options)?;
        Ok((seq, options.timestamp_ns))
    }

    #[cfg(test)]
    fn append_json(
        &mut self,
        data: &Value,
        tags: &[String],
        options: AppendOptions,
    ) -> Result<Message, Error> {
        let labels: Vec<_> = tags.iter().map(String::as_str).collect();
        let seq = match crate::view::MessageView::encode_legacy_value(data, &labels) {
            Ok(view) => self.append_with_options(view.bytes(), options)?,
            Err(_) => {
                // The older JSON encoder also accepts values outside the
                // native view writer's domain, including large unsigned ints.
                let payload = crate::core::lite3::encode_message(tags, data)?;
                self.append_with_options(payload.as_slice(), options)?
            }
        };
        Ok(Message {
            seq,
            time: options.timestamp_ns,
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
        let labels = tags.iter().map(String::as_str).collect::<Vec<_>>();
        let (receipt, data) = match crate::view::MessageView::encode_legacy_value(data, &labels) {
            Ok(view) => (
                self.append_prepared(view.bytes(), durability)?,
                data.clone(),
            ),
            Err(_) => {
                let payload = crate::core::lite3::encode_message(tags, data)?;
                let (_, stored_data) = decode_payload(payload.as_slice())?;
                (
                    self.append_prepared(payload.as_slice(), durability)?,
                    stored_data,
                )
            }
        };
        let FeedReceipt { seq, time } = receipt;
        Ok(Message {
            seq,
            time,
            meta: Meta {
                tags: tags.to_vec(),
            },
            data,
        })
    }

    #[cfg(test)]
    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error> {
        crate::core::lite3::validate_feed_bytes(payload)?;
        self.append_with_options(payload, options)
    }

    #[cfg(test)]
    fn append_lite3_with_receipt(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<Message, Error> {
        crate::core::lite3::validate_feed_bytes(payload)?;
        let (meta, data) = decode_payload(payload)?;
        let seq = self.append_with_options(payload, options)?;
        Ok(Message {
            seq,
            time: options.timestamp_ns,
            meta,
            data,
        })
    }

    #[cfg(test)]
    fn get_message(&self, seq: u64) -> Result<Message, Error> {
        let frame = self.get(seq)?;
        message_from_frame(&frame)
    }

    #[cfg(test)]
    fn get_lite3(&self, seq: u64) -> Result<FrameRef, Error> {
        self.get(seq)
    }
}

pub(crate) fn message_from_frame(frame: &FrameRef) -> Result<Message, Error> {
    Message::from_lite3(frame.seq, frame.time, &frame.payload)
}

/// Produce the existing wire envelope while borrowing data from this owned frame.
#[doc(hidden)]
pub fn message_json_from_frame(frame: &FrameRef) -> Result<Vec<u8>, Error> {
    frame_json(frame, None).map(|bytes| bytes.expect("unfiltered JSON"))
}

pub(crate) fn select_frame_json(
    frame: FrameRef,
    required: &[String],
) -> Result<Option<Vec<u8>>, Error> {
    frame_json(&frame, (!required.is_empty()).then_some(required))
}

impl<T: serde::Serialize> crate::interface_wire::MessageWire<crate::core::lite3::JsonValue<'_>, T> {
    pub(crate) fn to_json_bytes(&self, estimated_size: usize) -> serde_json::Result<Vec<u8>> {
        if estimated_size < 4096 {
            return serde_json::to_vec(self);
        }
        // Keep the same field order as MessageWire's Serde implementation.
        // The direct writer scans long strings in blocks.
        let mut out = Vec::with_capacity(estimated_size);
        out.extend_from_slice(b"{\"seq\":");
        serde_json::to_writer(&mut out, &self.seq)?;
        out.extend_from_slice(b",\"time\":");
        serde_json::to_writer(&mut out, &self.time)?;
        out.extend_from_slice(b",\"meta\":");
        serde_json::to_writer(&mut out, &self.meta)?;
        out.extend_from_slice(b",\"data\":");
        self.data.write_json(&mut out)?;
        out.push(b'}');
        Ok(out)
    }
}

fn frame_json(frame: &FrameRef, required: Option<&[String]>) -> Result<Option<Vec<u8>>, Error> {
    let doc = Lite3DocRef::new(&frame.payload);
    let (tags_offset, count) = meta_tags(doc)?;
    let mut tags = Vec::with_capacity(count as usize);
    for index in 0..count {
        tags.push(meta_tag(doc, tags_offset, index)?);
    }
    let offset = doc.message_data_offset(ErrorKind::Corrupt)?;
    let data = if let Some(required) = required {
        let data = crate::core::lite3::JsonValue::for_stream(doc, offset)?;
        if !has_required_tags(&tags, required) {
            return Ok(None);
        }
        data
    } else {
        crate::core::lite3::JsonValue::new(doc, offset)?
    };
    let wire = crate::interface_wire::MessageWire::new(
        frame.seq,
        format_frame_time(frame.time)?,
        tags,
        data,
    );
    wire.to_json_bytes(frame.payload.len())
        .map(Some)
        .map_err(payload_json_error)
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
    let offset = doc.message_data_offset(ErrorKind::Corrupt)?;
    // Keep the exporter as the fallback for stored documents outside the direct
    // reader's checks or whose collection counts cannot drive direct traversal.
    let data = crate::core::lite3::JsonValue::decode(doc, offset).or_else(|_| {
        serde_json::from_str(&doc.to_json_at(offset, false)?).map_err(payload_json_error)
    })?;
    #[cfg(test)]
    DATA_TREE_DECODES.with(|count| count.set(count.get() + 1));
    Ok((meta, data))
}

fn payload_json_error(err: serde_json::Error) -> Error {
    Error::new(ErrorKind::Corrupt)
        .with_message("invalid payload json")
        .with_source(err)
}

fn meta_tags(doc: Lite3DocRef<'_>) -> Result<(usize, u32), Error> {
    let meta_ofs = doc
        .key_offset_cstr_at(0, c"meta")
        .map_err(|err| err.with_message("missing meta"))?;
    match doc.bytes().get(meta_ofs) {
        Some(&sys::LITE3_TYPE_OBJECT) => {}
        None | Some(&sys::LITE3_TYPE_INVALID) => {
            return Err(Error::new(ErrorKind::Corrupt).with_message("missing meta"));
        }
        _ => return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object")),
    }
    let tags_ofs = doc
        .key_offset_cstr_at(meta_ofs, c"tags")
        .map_err(|err| err.with_message("missing meta.tags"))?;
    let tags_count = doc
        .count_at(tags_ofs)
        .map_err(|_| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"))?;
    Ok((tags_ofs, tags_count))
}

fn meta_tag<'a>(doc: Lite3DocRef<'a>, ofs: usize, index: u32) -> Result<&'a str, Error> {
    doc.array_str_at(ofs, index)
        .map_err(|_| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array"))
}

fn decode_meta(doc: Lite3DocRef<'_>) -> Result<Meta, Error> {
    let (ofs, count) = meta_tags(doc)?;
    let mut tags = Vec::with_capacity(count as usize);
    for index in 0..count {
        tags.push(meta_tag(doc, ofs, index)?.to_owned());
    }
    Ok(Meta { tags })
}

fn validate_data(doc: Lite3DocRef<'_>) -> Result<(), Error> {
    doc.validate_readability_at(doc.message_data_offset(ErrorKind::Corrupt)?)
}

#[cfg(test)]
fn data_json(doc: Lite3DocRef<'_>) -> Result<String, Error> {
    let data_ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    doc.to_json_at(data_ofs, false)
}

pub(crate) fn now_ns() -> Result<u64, Error> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("time went backwards")
                .with_source(err)
        })?;
    Ok(duration.as_nanos() as u64)
}

pub(crate) fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Iso8601;
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    ts.format(&Iso8601::DEFAULT).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("timestamp format failed")
            .with_source(err)
    })
}

fn format_frame_time(timestamp_ns: u64) -> Result<String, Error> {
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    let (year, month, day) = ts.date().to_calendar_date();
    let (hour, minute, second, nanos) = ts.time().as_hms_nano();
    // The u64 nanosecond range spans 1970 through 2554. Every timestamp
    // fits the same four-digit year and nine-digit fractional second.
    let mut text = *b"0000-00-00T00:00:00.000000000Z";
    fn digits(mut value: u32, out: &mut [u8]) {
        for digit in out.iter_mut().rev() {
            *digit = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    digits(year as u32, &mut text[..4]);
    digits(u32::from(u8::from(month)), &mut text[5..7]);
    digits(day.into(), &mut text[8..10]);
    digits(hour.into(), &mut text[11..13]);
    digits(minute.into(), &mut text[14..16]);
    digits(second.into(), &mut text[17..19]);
    digits(nanos, &mut text[20..29]);
    Ok(String::from_utf8(text.to_vec()).expect("timestamp contains only ASCII"))
}

#[cfg(test)]
mod tests {
    use super::{PoolApiExt, Read, Since, TailOutcome, decode_payload, message_from_frame};
    use crate::core::error::ErrorKind;
    use crate::core::lite3::{encode_message, json_counter_snapshot, reset_json_counters};
    use crate::core::pool::{AppendOptions, Durability, Pool, PoolOptions};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn v2_time_start_keeps_later_sequences_after_a_clock_step() {
        use super::{Read, ReadPoll, Since};

        let dir = tempdir().unwrap();
        let path = dir.path().join("clock.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(65536)).unwrap();
        for time in [10, 30, 20] {
            pool.append_json(
                &json!({"time": time}),
                &[],
                AppendOptions::new(time, Durability::Fast),
            )
            .unwrap();
        }
        let read = Read::since(Since::At(25));
        let messages = pool
            .fetch(read)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|msg| (msg.seq, msg.time))
                .collect::<Vec<_>>(),
            [(2, 30), (3, 20)]
        );

        let reader = Pool::open(&path).unwrap();
        let mut live = reader.follow_lite3(Read::since(Since::At(100))).unwrap();
        pool.append_json(
            &json!({"live": true}),
            &[],
            AppendOptions::new(21, Durability::Fast),
        )
        .unwrap();
        assert!(matches!(
            live.poll(std::time::Duration::ZERO).unwrap(),
            ReadPoll::Message(frame) if frame.seq == 4
        ));
    }

    fn batch_pool(path: &std::path::Path) -> Pool {
        let mut pool = Pool::create(path, PoolOptions::new(1024 * 1024)).unwrap();
        for value in 1..=4 {
            pool.append_json_now(&json!({"value": value}), &[], Durability::Fast)
                .unwrap();
        }
        pool
    }

    #[test]
    fn reads_seek_to_an_indexed_start_and_scan_on_index_miss() {
        use super::{Read, Since};
        use crate::core::cursor::CursorResult;

        let dir = tempdir().unwrap();
        for (slots, first_frame) in [(8, 3), (1, 1), (0, 1)] {
            let mut pool = Pool::create(
                dir.path().join(format!("index-{slots}.plasmite")),
                PoolOptions::new(65536).with_index_capacity(slots),
            )
            .unwrap();
            for value in 1..=4 {
                pool.append_json_now(&json!({ "value": value }), &[], Durability::Fast)
                    .unwrap();
            }
            for read in [Read::since(Since::Seq(3)), Read::last(2)] {
                for live in [false, true] {
                    let mut state = pool.read_state(read.clone(), live).unwrap();
                    assert!(matches!(
                        state.cursor.next(&pool).unwrap(),
                        CursorResult::Message(frame) if frame.seq == first_frame
                    ));
                }
                let seqs = pool
                    .fetch_lite3(read)
                    .unwrap()
                    .map(|frame| frame.unwrap().seq)
                    .collect::<Vec<_>>();
                assert_eq!(seqs, [3, 4]);
            }
        }
    }

    #[test]
    fn follow_selection_uses_one_opening_boundary() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("follow.plasmite");
        let pool = batch_pool(&path);
        let mut writer = Pool::open(&path).unwrap();
        let mut live = pool.follow(Read::new()).unwrap();
        writer
            .append_json_now(&json!({"value": 5}), &[], Durability::Fast)
            .unwrap();
        assert_eq!(live.next_message().unwrap().unwrap().seq, 5);
        let mut finite = pool.fetch(Read::last(2)).unwrap();
        writer
            .append_json_now(&json!({"value": 6}), &[], Durability::Fast)
            .unwrap();
        assert_eq!(
            std::iter::from_fn(|| finite.next_message().transpose())
                .map(|m| m.unwrap().seq)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        let all = pool.fetch_view(Read::since(Since::Oldest)).unwrap();
        assert_eq!(
            all.map(|m| m.unwrap().seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        let mut empty = pool.fetch(Read::last(0)).unwrap();
        assert!(empty.next_message().unwrap().is_none());
        let mut since = pool.fetch(Read::since(Since::At(u64::MAX))).unwrap();
        assert!(since.next_message().unwrap().is_none());
    }

    #[test]
    fn bounded_fetch_reports_loss_of_its_entire_opening_range() {
        use super::Read;
        let directory = tempdir().unwrap();
        let path = directory.path().join("bounded-gap.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(16384)).unwrap();
        for _ in 0..3 {
            writer
                .feed(&serde_json::json!({"n": 0}), Default::default())
                .unwrap();
        }
        let pool = Pool::open(&path).unwrap();
        let mut single = pool.read_state(Read::default(), false).unwrap();
        let mut batch = pool.read_state(Read::default(), false).unwrap();
        for _ in 0..40 {
            writer
                .feed(
                    &serde_json::json!({"padding": "x".repeat(1024)}),
                    Default::default(),
                )
                .unwrap();
        }
        let select = |frame: super::FrameRef, _: &[String]| Ok(Some(frame.seq));
        let one = single.next_wait_bounded(&pool, Some(Duration::ZERO), usize::MAX, select);
        let many = batch.next_batch_wait(&pool, Some(Duration::ZERO), 4, usize::MAX, select);
        for error in [one.err().unwrap(), many.err().unwrap()] {
            assert_eq!(error.kind(), ErrorKind::RetentionGap);
            assert_eq!(error.missing(), Some(1));
            assert!(error.next_seq().unwrap() > 3);
        }
    }

    #[test]
    fn batch_wait_respects_bytes_and_checkpoints() {
        let directory = tempdir().unwrap();
        let pool = batch_pool(&directory.path().join("batch.plasmite"));
        let size = pool.get_lite3(1).unwrap().payload.len();
        let mut state = pool.read_state(Read::since(Since::Seq(1)), false).unwrap();
        let select = |frame: crate::api::FrameRef, _: &[String]| Ok(Some(frame.seq));
        let error = state
            .next_batch_wait(&pool, Some(Duration::ZERO), 16, size - 1, select)
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Usage);
        let TailOutcome::Item(first) = state
            .next_batch_wait(&pool, Some(Duration::ZERO), 16, size * 2, select)
            .unwrap()
        else {
            panic!("batch")
        };
        assert_eq!(first, vec![1, 2]);
        assert!(matches!(
            state
                .next_wait(&pool, Some(Duration::ZERO), select)
                .unwrap(),
            TailOutcome::Item(3)
        ));
        let TailOutcome::Item(last) = state
            .next_batch_wait(&pool, Some(Duration::ZERO), 16, usize::MAX, select)
            .unwrap()
        else {
            panic!("batch")
        };
        assert_eq!(last, vec![4]);
        assert!(matches!(
            state
                .next_wait(&pool, Some(Duration::ZERO), select)
                .unwrap(),
            TailOutcome::Finished
        ));
    }

    #[test]
    fn batch_prefix_leaves_error_record_for_next_call() {
        let directory = tempdir().unwrap();
        let pool = batch_pool(&directory.path().join("batch.plasmite"));
        let mut state = pool.read_state(Read::since(Since::Seq(1)), false).unwrap();
        let select = |frame: crate::api::FrameRef, _: &[String]| {
            if frame.seq == 2 {
                Err(crate::core::error::Error::new(ErrorKind::Corrupt)
                    .with_message("bad second frame"))
            } else {
                Ok(Some(frame.seq))
            }
        };
        let TailOutcome::Item(prefix) = state
            .next_batch_wait(&pool, Some(Duration::ZERO), 4, usize::MAX, select)
            .unwrap()
        else {
            panic!("batch")
        };
        assert_eq!(prefix, vec![1]);
        assert_eq!(
            state
                .next_wait(&pool, Some(Duration::ZERO), select)
                .err()
                .unwrap()
                .kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(state.expected_seq, Some(2));
        assert_eq!(
            state
                .next_wait(&pool, Some(Duration::ZERO), select)
                .err()
                .unwrap()
                .kind(),
            ErrorKind::Corrupt
        );
    }

    #[test]
    fn batch_selection_runs_outside_pool_lock() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("batch.plasmite");
        let pool = batch_pool(&path);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut state = pool.read_state(Read::since(Since::Seq(1)), false).unwrap();
        let TailOutcome::Item(items) = state
            .next_batch_wait(&pool, Some(Duration::ZERO), 4, usize::MAX, |frame, _| {
                file.try_lock()
                    .expect("formatting must release the shared pool lock");
                file.unlock().unwrap();
                Ok(Some(frame.seq))
            })
            .unwrap()
        else {
            panic!("batch")
        };
        assert_eq!(items, vec![1, 2, 3, 4]);
    }

    #[test]
    fn batch_cancellation_stops_available_tail() {
        let directory = tempdir().unwrap();
        let pool = batch_pool(&directory.path().join("batch.plasmite"));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut state = pool
            .read_state(Read::since(Since::Seq(1)).cancel(cancel.clone()), false)
            .unwrap();
        let TailOutcome::Item(items) = state
            .next_batch_wait(&pool, Some(Duration::ZERO), 4, usize::MAX, |frame, _| {
                cancel.store(true, Ordering::Release);
                Ok(Some(frame.seq))
            })
            .unwrap()
        else {
            panic!("batch")
        };
        assert_eq!(items, vec![1]);
        assert!(matches!(
            state
                .next_wait(&pool, Some(Duration::ZERO), |frame, _| Ok(Some(frame.seq)))
                .unwrap(),
            TailOutcome::Cancelled
        ));
    }

    #[test]
    fn borrowed_wire_matches_complete_messages_and_corrupt_payload_errors() {
        let compare = |payload: Vec<u8>| {
            let frame = crate::api::FrameRef {
                seq: u64::MAX,
                time: u64::MAX,
                flags: 0,
                payload,
            };
            let old = message_from_frame(&frame);
            reset_json_counters();
            super::DATA_TREE_DECODES.with(|count| count.set(0));
            let new = super::message_json_from_frame(&frame);
            match (old, new) {
                (Ok(message), Ok(bytes)) => {
                    let wire = crate::interface_wire::MessageWire::new(
                        message.seq,
                        message.time_rfc3339(),
                        message.meta.tags,
                        message.data,
                    );
                    assert_eq!(bytes, serde_json::to_vec(&wire).unwrap());
                }
                (Err(old), Err(new)) => assert_eq!(old.kind(), new.kind()),
                (old, new) => panic!("old/new mismatch: {old:?} / {new:?}"),
            }
        };
        for text in [
            r#"{"meta":{"tags":["α","a\u0000b"]},"data":{"z":2.027716612057172e155,"a":[null,{},true]}}"#,
            r#"{"meta":{"tags":[]},"data":[]}"#,
            r#"{"meta":{"tags":[]},"data":42}"#,
            r#"{"meta":{"tags":[7]},"data":{}}"#,
            r#"{"meta":{"tags":[]}}"#,
            r#"{"data":{}}"#,
        ] {
            compare(
                crate::core::lite3::Lite3Buf::from_json_str(text)
                    .unwrap()
                    .as_slice()
                    .to_vec(),
            );
        }
        let payload = encode_message(&[], &json!({"blob":"borrowed text"})).unwrap();
        compare(payload.as_slice().to_vec());
        assert_eq!(json_counter_snapshot(), (0, 0));
        super::DATA_TREE_DECODES.with(|count| assert_eq!(count.get(), 0));
        let data = payload.as_doc().key_offset("data").unwrap();
        let value = payload.as_doc().key_offset_at(data, "blob").unwrap();
        let mut bytes = payload.as_slice().to_vec();
        bytes[value] = crate::core::lite3::sys::LITE3_TYPE_BYTES;
        compare(bytes);
        let marker = payload
            .as_slice()
            .windows(13)
            .position(|b| b == b"borrowed text")
            .unwrap();
        let mut bytes = payload.as_slice().to_vec();
        bytes[marker] = 0xff;
        compare(bytes);
        compare(payload.as_slice()[..payload.len() - 1].to_vec());
    }

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
        assert_eq!(message.time, 123);
        assert_eq!(message.time_rfc3339(), "1970-01-01T00:00:00.000000123Z");
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
    fn metadata_accessor_avoids_data_tree_and_matches_full_decode_failures() {
        let bytes = encode_message(&["keep".into()], &json!({"nested":[{"x":"y"}]})).unwrap();
        super::DATA_TREE_DECODES.with(|count| count.set(0));
        assert_eq!(
            super::Meta::from_lite3(bytes.as_slice()).unwrap().tags,
            ["keep"]
        );
        super::DATA_TREE_DECODES.with(|count| assert_eq!(count.get(), 0));
        super::Message::from_lite3(1, u64::MAX, bytes.as_slice()).unwrap();
        super::DATA_TREE_DECODES.with(|count| assert_eq!(count.get(), 1));
    }

    #[test]
    fn direct_decode_matches_json_export_and_reparse() {
        use super::{data_json, payload_json_error};
        use crate::core::lite3::{Lite3Buf, Lite3DocRef, sys};
        use serde_json::Value;

        fn compare(bytes: &[u8]) {
            let legacy = data_json(Lite3DocRef::new(bytes))
                .and_then(|json| serde_json::from_str::<Value>(&json).map_err(payload_json_error));
            let direct = decode_payload(bytes).map(|(_, data)| data);
            match (legacy, direct) {
                (Ok(left), Ok(right)) => assert_eq!(
                    serde_json::to_vec(&left).unwrap(),
                    serde_json::to_vec(&right).unwrap()
                ),
                (Err(left), Err(right)) => assert_eq!(left.kind(), right.kind()),
                pair => panic!("decode mismatch: {pair:?}"),
            }
        }

        for data in [
            json!({"array":[]}),
            json!({}),
            json!({"case": [null, true, i64::MIN, i64::MAX, -0.0f64, 1.0f64]}),
            json!({"float": [2.027716612057172e155, f64::MIN_POSITIVE, f64::MAX]}),
            json!({"caption": "Unicode 温度 and NUL\0 and quote\"", "array": (0..1000).collect::<Vec<_>>()}),
            json!({"text": "a".repeat(32768)}),
            json!({"number":42}),
        ] {
            let input = json!({"meta":{"tags":["room"]},"data":data}).to_string();
            let bytes = Lite3Buf::from_json_str(&input).unwrap();
            compare(bytes.as_slice());
        }
        let encoded = encode_message(&[], &json!({"string":"utf8-probe-你好"})).unwrap();
        let data = encoded.as_doc().key_offset("data").unwrap();
        let string = encoded.as_doc().key_offset_at(data, "string").unwrap();
        let mut binary = encoded.as_slice().to_vec();
        binary[string] = sys::LITE3_TYPE_BYTES;
        compare(&binary);
        let mut invalid = encoded.as_slice().to_vec();
        let i = invalid
            .windows(10)
            .position(|bytes| bytes == b"utf8-probe")
            .unwrap();
        invalid[i] = 0xff;
        compare(&invalid);

        // The legacy exporter walks the tree even if its declared count is
        // wrong. Preserve the result without allocating from that count.
        let mut bad_count = encoded.as_slice().to_vec();
        let count_offset = data + 32;
        let word = u32::from_le_bytes(
            bad_count[count_offset..count_offset + 4]
                .try_into()
                .unwrap(),
        );
        bad_count[count_offset..count_offset + 4]
            .copy_from_slice(&(word | (1 << 20)).to_le_bytes());
        reset_json_counters();
        let decoded = decode_payload(&bad_count).unwrap();
        assert_eq!(decoded.1, json!({"string":"utf8-probe-你好"}));
        assert_eq!(json_counter_snapshot(), (0, 1));
        compare(&bad_count);

        let directory = tempdir().unwrap();
        let path = directory.path().join("v1.3.plasmite");
        std::fs::write(
            &path,
            include_bytes!("../../tests/fixtures/pool-v1.3.0.plasmite"),
        )
        .unwrap();
        let pool = Pool::open(&path).unwrap();
        let mut total = 0;
        for result in pool.fetch_lite3(super::Read::new()).unwrap() {
            compare(&result.unwrap().payload);
            total += 1;
        }
        assert!(total > 0);
    }

    #[test]
    fn decode_payload_avoids_json_text() {
        let data = json!({"x": 1});
        let payload = encode_message(&["tag".to_string()], &data).expect("encode");
        reset_json_counters();
        let _ = decode_payload(payload.as_slice()).expect("decode");
        let (full, partial) = json_counter_snapshot();
        assert_eq!(full, 0);
        assert_eq!(partial, 0);
    }

    #[test]
    fn native_feed_preserves_legacy_encoded_bytes() {
        let dir = tempdir().unwrap();
        let mut pool =
            Pool::create(dir.path().join("feed.plasmite"), PoolOptions::new(262144)).unwrap();
        let tags = vec!["room".to_owned(), "温度\0sensor".to_owned()];
        let labels: Vec<_> = tags.iter().map(String::as_str).collect();
        let wide = (0..200)
            .map(|n| (format!("item_{n}"), json!([n, true, null, "温度"])))
            .collect::<serde_json::Map<_, _>>();
        for data in [
            json!({"reading":1,"signed":[i64::MIN,i64::MAX,-0.0,-3.25],"tag":"a\0b"}),
            json!({"values": (0..1000).collect::<Vec<_>>() }),
            json!({"text": "x".repeat(32000), "nested": wide }),
            json!({"legacy_unsigned": u64::MAX}),
            json!({"legacy\0key": 12}),
        ] {
            let expected = encode_message(&tags, &data);
            let receipt = pool.feed(&data, super::Feed::new().tags(tags.clone()));
            match (expected, receipt) {
                (Ok(expected), Ok(receipt)) => {
                    if let Ok(view) = crate::view::MessageView::encode_legacy_value(&data, &labels)
                    {
                        assert_eq!(view.bytes(), expected.as_slice(), "{data}");
                    }
                    let stored = pool
                        .fetch(super::Read::since(super::Since::Seq(receipt.seq)))
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap();
                    assert_eq!(receipt, stored);
                    assert_eq!(pool.get(receipt.seq).unwrap().payload, expected.as_slice());
                }
                (Err(expected), Err(error)) => {
                    assert_eq!(error.kind(), expected.kind());
                    assert_eq!(error.message(), expected.message());
                }
                pair => panic!("feed compatibility mismatch for {data}: {pair:?}"),
            }
        }
        // Ensure the ordinary path reaches the direct writer.
        assert!(
            crate::view::MessageView::encode_legacy_value(&json!({"ok":true}), &labels).is_ok()
        );
    }

    #[test]
    fn large_json_messages_and_receipts_match_the_standard_serializer() {
        use super::{format_ts, message_json_from_frame};
        use crate::core::lite3::ParsedJson;
        use crate::interface_wire::MessageWire;

        let dir = tempdir().unwrap();
        let mut pool =
            Pool::create(dir.path().join("large.plasmite"), PoolOptions::new(65536)).unwrap();
        let escapes = (0..32)
            .map(|code| char::from(code as u8))
            .collect::<String>();
        for prefix in 0..8 {
            let data = json!({
                "log": format!("{}{}\"\\é🔥\n{}", "x".repeat(prefix), escapes, "y".repeat(4096)),
                "samples": [{"short":"a\nb","number":-0.0}, 0.125, null, true],
            });
            let tags = vec!["line\n💡".to_owned()];
            let payload = encode_message(&tags, &data).unwrap();
            let seq = pool.append_with_timestamp(payload.as_slice(), 123).unwrap();
            let frame = pool.get(seq).unwrap();
            assert!(frame.payload.len() >= 4096);
            let time = format_ts(123).unwrap();
            let input = serde_json::to_vec(&data).unwrap();
            let parsed = ParsedJson::parse(&input).unwrap();
            let receipt = MessageWire::new(seq, time.clone(), &tags, parsed.value());
            let expected = serde_json::to_vec(&MessageWire::new(seq, time, &tags, data)).unwrap();
            assert_eq!(message_json_from_frame(&frame).unwrap(), expected);
            assert_eq!(receipt.to_json_bytes(input.len()).unwrap(), expected);
        }
    }

    #[test]
    fn fixed_width_timestamps_match_iso8601() {
        use super::{format_frame_time, format_ts};

        let boundaries = [0, 1, 999_999_999, 1_000_000_000, u64::MAX - 1, u64::MAX];
        let samples = (0..512).map(|n: u64| n.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        for timestamp in boundaries.into_iter().chain(samples) {
            assert_eq!(
                format_frame_time(timestamp).unwrap(),
                format_ts(timestamp).unwrap(),
                "{timestamp}"
            );
        }
    }

    #[test]
    fn json_poll_preserves_wire_bytes_and_checks_filtered_records() {
        use super::{Read, ReadStatus, message_json_from_frame};
        let dir = tempdir().unwrap();
        let mut pool =
            Pool::create(dir.path().join("http.plasmite"), PoolOptions::new(65536)).unwrap();
        let encoded = encode_message(&["keep".to_owned()], &json!({"x":"tag-data-probe"})).unwrap();
        let seq = pool.append_with_timestamp(encoded.as_slice(), 123).unwrap();
        let expected = message_json_from_frame(&pool.get(seq).unwrap()).unwrap();
        let mut json = pool.fetch_lite3(Read::new().tags(["keep"])).unwrap();
        assert!(matches!(
            json.poll_json(Duration::ZERO).unwrap(),
            ReadStatus::Message(bytes) if bytes == expected
        ));
        drop(json);
        let mut corrupt = encoded.as_slice().to_vec();
        let offset = corrupt
            .windows(14)
            .position(|part| part == b"tag-data-probe")
            .unwrap();
        corrupt[offset] = 0xff;
        pool.append_with_timestamp(&corrupt, 124).unwrap();
        let mut filtered = pool.fetch_lite3(Read::new().tags(["absent"])).unwrap();
        assert!(matches!(
            filtered.poll_json(Duration::ZERO),
            Err(error) if error.kind() == ErrorKind::Corrupt
        ));
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
        let mut reader = pool.fetch_lite3(Read::new().tags(["keep"])).unwrap();
        let frame = reader.next_frame().unwrap().unwrap();
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.time, 123);
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
        let options = Read::since(Since::Seq(seq)).tags(["never"]);
        assert_eq!(
            pool.fetch_lite3(options.clone())
                .unwrap()
                .next_frame()
                .unwrap_err()
                .kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(
            pool.fetch(options)
                .unwrap()
                .next_message()
                .unwrap_err()
                .kind(),
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
        let options = Read::since(Since::Seq(u64::MAX));
        let mut raw = pool.fetch_lite3(options.clone()).unwrap();
        assert_eq!(raw.next_frame().unwrap().unwrap().seq, u64::MAX);
        assert!(raw.next_frame().unwrap().is_none());
        let mut decoded = pool.fetch(options).unwrap();
        assert_eq!(decoded.next_message().unwrap().unwrap().seq, u64::MAX);
        assert!(decoded.next_message().unwrap().is_none());
    }

    #[test]
    fn numeric_receipts_keep_exact_commit_identity_across_writers_and_overwrite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identities.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(64 * 1024)).unwrap();
        let mut workers = Vec::new();
        for writer in 0..2_u64 {
            let path = path.clone();
            workers.push(std::thread::spawn(move || {
                let mut pool = Pool::open(path).unwrap();
                let data = json!({"writer": writer, "padding": "x".repeat(4096)});
                let payload = encode_message(&[], &data).unwrap();
                (0..32_u64)
                    .map(|i| {
                        let timestamp = u64::MAX - writer * 32 - i;
                        let options = AppendOptions::new(timestamp, Durability::Fast);
                        let identity = if i % 2 == 0 {
                            pool.append_json_identity(&data, &[], options)
                        } else {
                            pool.append_lite3_identity(payload.as_slice(), options)
                        }
                        .unwrap();
                        assert_eq!(identity.1, timestamp);
                        (identity, writer)
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let mut receipts: Vec<_> = workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect();
        receipts.sort_by_key(|((seq, _), _)| *seq);
        let mut overwritten = 0;
        for (index, ((seq, timestamp), writer)) in receipts.into_iter().enumerate() {
            assert_eq!(seq, index as u64 + 1);
            match pool.get_lite3(seq) {
                Ok(frame) => {
                    assert_eq!(frame.time, timestamp);
                    assert_eq!(message_from_frame(&frame).unwrap().data["writer"], writer);
                }
                Err(error) => {
                    assert_eq!(error.kind(), ErrorKind::NotFound);
                    overwritten += 1;
                }
            }
        }
        assert!(overwritten > 0);
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
        assert_eq!(receipt.time, 123);
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
    fn append_get_follow_lite3() {
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

        let mut tail = pool.fetch_lite3(Read::since(Since::Seq(seq))).unwrap();
        let frame = tail.next_frame().expect("tail").expect("frame");
        assert_eq!(frame.seq, seq);
        assert_eq!(frame.payload, payload.as_slice());
    }

    #[test]
    fn tails_keep_independent_positions_with_shared_notifications() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("shared-wakeup.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(65536)).unwrap();
        let first = Pool::open(&path).unwrap();
        let second = Pool::open(&path).unwrap();
        let mut first_tail = first.follow(Read::new()).unwrap();
        let mut second_tail = second.follow(Read::new()).unwrap();
        let ready = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let one = scope.spawn(|| {
                ready.wait();
                first_tail.next_message().unwrap().unwrap()
            });
            let two = scope.spawn(|| {
                ready.wait();
                second_tail.next_message().unwrap().unwrap()
            });
            ready.wait();
            let receipt = writer
                .append_json_now(&json!({"n": 1}), &[], Durability::Fast)
                .unwrap();
            assert_eq!(one.join().unwrap(), receipt);
            assert_eq!(two.join().unwrap(), receipt);
        });
    }

    #[test]
    fn tails_read_new_messages_with_notifications_disabled() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        let reader = Pool::open(&path).expect("open reader");

        let mut json = reader.follow(Read::new()).unwrap();
        json.0.state.notify = None;
        let mut lite3 = reader.follow_lite3(Read::new()).unwrap();
        lite3.0.state.notify = None;
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
    fn cancellation_stops_a_reader_with_messages_available() {
        let dir = tempdir().unwrap();
        let pool = batch_pool(&dir.path().join("cancel.plasmite"));
        let cancel = Arc::new(AtomicBool::new(false));
        let options = Read::since(Since::Oldest).cancel(cancel.clone());
        let mut json = pool.follow(options.clone()).unwrap();
        let mut lite3 = pool.follow_lite3(options).unwrap();
        assert_eq!(json.next_message().unwrap().unwrap().seq, 1);
        assert_eq!(lite3.next_frame().unwrap().unwrap().seq, 1);
        cancel.store(true, Ordering::Release);
        assert!(json.next_message().unwrap().is_none());
        assert!(lite3.next_frame().unwrap().is_none());
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
    fn mid_follow_gap_cannot_be_hidden_by_tags() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gap.plasmite");
        let mut writer =
            Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0)).unwrap();
        writer
            .append_json_now(&json!({"n":1}), &["keep".into()], Durability::Fast)
            .unwrap();
        let pool = Pool::open(&path).unwrap();
        let mut reader = pool
            .follow_lite3(Read::since(Since::Oldest).tags(["keep"]))
            .unwrap();
        assert_eq!(reader.next_frame().unwrap().unwrap().seq, 1);
        append_until_oldest_after(&mut writer, 2);
        let oldest = writer.info().unwrap().bounds.oldest_seq.unwrap();
        for _ in 0..2 {
            let gap = reader.next_frame().unwrap_err();
            assert_eq!(gap.kind(), ErrorKind::RetentionGap);
            assert_eq!(gap.missing(), Some(2));
            assert_eq!(gap.next_seq(), Some(oldest));
        }
        for position in [Since::At(0), Since::Ago(Duration::from_secs(u64::MAX))] {
            let mut history = pool.fetch_lite3(Read::since(position)).unwrap();
            assert_eq!(history.next_frame().unwrap().unwrap().seq, oldest);
        }
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

        let mut tail = pool.fetch(Read::since(Since::Seq(oldest))).unwrap();
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
