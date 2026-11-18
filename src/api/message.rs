//! Purpose: Define public message types and append/get/tail helpers for the API.
//! Exports: `Message`, `Meta` (including validated `Meta::from_lite3`), `FollowOptions`, `Follow`, `Lite3Follow`.
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

#[derive(Clone, Debug)]
pub struct FollowOptions {
    /// New messages by default; select retained history explicitly.
    #[cfg(test)]
    pub start: super::FollowStart,
    /// Continue after history. False stops at the opening upper bound; alone it selects all history.
    #[cfg(test)]
    pub live: bool,
    pub max_messages: Option<usize>,
    pub tags: Vec<String>,
    pub poll_interval: Duration,
    #[cfg(test)]
    pub timeout: Option<Duration>,
    #[cfg(test)]
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

impl FollowOptions {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            start: super::FollowStart::New,
            #[cfg(test)]
            live: true,
            max_messages: None,
            tags: Vec::new(),
            poll_interval: Duration::from_millis(50),
            #[cfg(test)]
            timeout: None,
            #[cfg(test)]
            notify: true,
            gap_policy: GapPolicy::Continue,
            cancel: None,
        }
    }
}

impl Default for FollowOptions {
    fn default() -> Self {
        Self::new()
    }
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
    pub(super) until: Option<u64>,
}

impl Read {
    pub fn new() -> Self {
        Self {
            start: ReadStart::Default,
            tags: Vec::new(),
            cancel: None,
            until: None,
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

    /// Keep the original end boundary when an HTTP stream continues.
    #[doc(hidden)]
    pub fn until(mut self, seq: u64) -> Self {
        self.until = Some(seq);
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

    pub(super) fn encoded_durability(&self) -> Result<Durability, Error> {
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
        self.append_lite3_with_receipt_now(bytes, options.encoded_durability()?)
    }

    /// Forward a complete Lite3 envelope without decoding the stored result.
    pub fn feed_lite3_receipt(
        &mut self,
        bytes: &[u8],
        options: Feed,
    ) -> Result<FeedReceipt, Error> {
        let (seq, time) = self.append_lite3_identity_now(bytes, options.encoded_durability()?)?;
        Ok(FeedReceipt { seq, time })
    }

    /// Forward an immutable message view while retaining its encoded payload.
    pub fn feed_view(
        &mut self,
        view: &crate::view::MessageView,
        options: Feed,
    ) -> Result<crate::view::MessageView, Error> {
        self.append_view_now(view, options.encoded_durability()?)
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

    pub(crate) fn read_state(&self, read: Read, live: bool) -> Result<TailState, Error> {
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
                            .with_message("since must be a positive sequence at or before the next unwritten message"));
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
            if read.until.is_some_and(|until| live || until > newest) {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("invalid fetch continuation boundary"));
            }
            (first, (!live).then_some(read.until.unwrap_or(newest)))
        };
        let options = FollowOptions {
            #[cfg(test)]
            live,
            tags: read.tags,
            gap_policy: GapPolicy::Error,
            cancel: read.cancel,
            ..FollowOptions::new()
        };
        Ok(TailState {
            cursor,
            options,
            seen: 0,
            deadline: None,
            notify: live.then(|| open_for_pool(self)).flatten(),
            expected_seq: first,
            terminated: first.is_none()
                || upper.zip(first).is_some_and(|(last, first)| first > last),
            upper,
            since_ns: None,
            startup_error: None,
            check_retention_before_reading: true,
        })
    }

    /// Follow validated owned views without constructing JSON or native data trees.
    #[cfg(test)]
    pub(crate) fn follow_views(&self, options: FollowOptions) -> ViewFollow<'_> {
        ViewFollow(TailReader::new(self, options))
    }
}

pub struct Lite3Follow<'a>(TailReader<'a>);

struct TailReader<'a> {
    pool: &'a Pool,
    state: TailState,
}

// Shared policy, independent of whether the adapter borrows or owns its Pool.
pub(crate) struct TailState {
    cursor: Cursor,
    options: FollowOptions,
    seen: usize,
    deadline: Option<Instant>,
    notify: Option<NotifyHandle>,
    expected_seq: Option<u64>,
    terminated: bool,
    upper: Option<u64>,
    since_ns: Option<u64>,
    startup_error: Option<Error>,
    check_retention_before_reading: bool,
}

pub(crate) enum TailOutcome<T> {
    Item(T),
    WaitExpired,
    Finished,
    Cancelled,
}

impl<'a> TailReader<'a> {
    #[cfg(test)]
    fn new(pool: &'a Pool, options: FollowOptions) -> Self {
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
    #[cfg(test)]
    pub(crate) fn new(pool: &Pool, options: FollowOptions) -> Self {
        let deadline = options.timeout.map(|duration| Instant::now() + duration);
        let mut cursor = Cursor::new();
        let mut expected_seq = None;
        let mut upper = None;
        let mut since_ns = None;
        let mut terminated = false;
        let mut startup_error = None;
        match pool.header_from_mmap() {
            Ok(header) => {
                let oldest = header.oldest_seq.max(1);
                let newest = header.newest_seq;
                use super::FollowStart;
                expected_seq = Some(match options.start {
                    FollowStart::New if !options.live => oldest,
                    FollowStart::New => {
                        cursor.seek_to(header.head_off as usize);
                        terminated = newest == u64::MAX;
                        newest.saturating_add(1).max(1)
                    }
                    FollowStart::Last(count) => {
                        if count == 0 {
                            newest.saturating_add(1).max(1)
                        } else {
                            newest.saturating_sub(count - 1).max(oldest)
                        }
                    }
                    FollowStart::Since(time) => {
                        since_ns = Some(time);
                        oldest
                    }
                    FollowStart::Ago(nanos) => {
                        match now_ns() {
                            Ok(now) => since_ns = Some(now.saturating_sub(nanos)),
                            Err(err) => startup_error = Some(err),
                        }
                        oldest
                    }
                    FollowStart::Sequence(0) => oldest,
                    FollowStart::Sequence(seq) => seq,
                });
                if !options.live {
                    upper = Some(newest);
                    terminated |= newest == 0 || expected_seq.is_some_and(|seq| seq > newest);
                }
            }
            Err(error) => startup_error = Some(error),
        }
        let notify = if options.notify {
            open_for_pool(pool)
        } else {
            None
        };
        Self {
            cursor,
            options,
            seen: 0,
            deadline,
            notify,
            expected_seq,
            terminated,
            upper,
            since_ns,
            startup_error,
            check_retention_before_reading: false,
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
        let check_retention = self.check_retention_before_reading;
        let result = self.next_wait_using(
            wait,
            |cursor, expected| {
                cursor.next_checked(pool, if check_retention { expected } else { None })
            },
            select,
        );
        if result.is_err() && check_retention {
            self.terminated = true;
        }
        result
    }

    fn next_wait_using<T>(
        &mut self,
        wait: Option<Duration>,
        mut acquire: impl FnMut(&mut Cursor, Option<u64>) -> Result<CursorResult, Error>,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        if let Some(error) = self.startup_error.take() {
            self.terminated = true;
            return Err(error);
        }
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

            if polled
                && !poll_only
                && wait_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Ok(TailOutcome::WaitExpired);
            }
            polled = true;

            match acquire(&mut self.cursor, self.expected_seq)? {
                CursorResult::Message(frame) => {
                    if self.upper.is_some_and(|upper| frame.seq > upper) {
                        self.terminated = true;
                        return Ok(TailOutcome::Finished);
                    }
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
                    self.terminated = frame.seq == u64::MAX || self.upper == Some(frame.seq);
                    if self.since_ns.is_some_and(|since| frame.time < since) {
                        if self.terminated {
                            return Ok(TailOutcome::Finished);
                        }
                        continue;
                    }
                    if let Some(item) = select(frame, &self.options.tags)? {
                        self.seen += 1;
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
                    let poll_interval = if self.options.cancel.is_some() {
                        self.options.poll_interval.min(Duration::from_millis(50))
                    } else {
                        self.options.poll_interval
                    };
                    let wait_for =
                        wait_interval(wait_deadline, wait_interval(self.deadline, poll_interval));
                    if let Some(notify) = &mut self.notify {
                        match notify
                            .wait_for_generation(self.cursor.observed_generation(), wait_for)
                        {
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
        self.next_wait_using(
            wait,
            |cursor, expected| {
                let _lock = pool.read_lock()?;
                cursor.next_locked_bounded(pool, max_bytes, expected)
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
        let copy_count = self.options.max_messages.map_or(max_count, |max| {
            max_count.min(max.saturating_sub(self.seen))
        });
        let cancel = self.options.cancel.clone();
        let deadline = self.deadline;
        let upper = self.upper;
        let mut frames = std::collections::VecDeque::new();
        let first = self.next_wait_using(
            wait,
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
                let first = cursor.next_locked_bounded(pool, max_bytes, expected)?;
                if let CursorResult::Message(frame) = &first {
                    if expected.is_some_and(|expected| frame.seq < expected) {
                        return Ok(first);
                    }
                    let mut used = frame.payload.len();
                    let mut ahead = cursor.clone();
                    while frames.len() + 1 < copy_count && used < max_bytes {
                        if cancel
                            .as_ref()
                            .is_some_and(|flag| flag.load(Ordering::Acquire))
                            || deadline.is_some_and(|limit| Instant::now() >= limit)
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
            if self.terminated || self.batch_stopped() {
                break;
            }
            let mut expected = self.expected_seq;
            let process = match observe_sequence(&mut expected, frame.seq, self.options.gap_policy)
            {
                Ok(process) => process,
                Err(_) => break,
            };
            let terminal = frame.seq == u64::MAX || self.upper == Some(frame.seq);
            let item = if process {
                match select(frame, &self.options.tags) {
                    Ok(item) => item,
                    Err(_) => break,
                }
            } else {
                None
            };
            self.cursor = checkpoint;
            self.expected_seq = expected;
            if process {
                self.terminated = terminal;
            }
            if let Some(item) = item {
                items.push(item);
                self.seen += 1;
            }
            if self.terminated {
                break;
            }
        }
        Ok(TailOutcome::Item(items))
    }

    fn batch_stopped(&self) -> bool {
        self.options
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
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
        self.0.next(|frame, required| {
            let view = crate::view::MessageView::from_owned(
                frame.payload,
                frame.seq,
                frame.time,
                frame.flags,
            )?;
            if !required.is_empty() {
                let tags = view.root().get("meta")?.unwrap().get("tags")?.unwrap();
                for required in required {
                    let mut found = false;
                    for entry in tags.iter()? {
                        if entry?.1.text() == Some(required.as_str()) {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        return Ok(None);
                    }
                }
            }
            Ok(Some(view))
        })
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
        let result = self
            .0
            .state
            .next_wait(self.0.pool, Some(wait), select_lite3_frame);
        match result {
            Ok(TailOutcome::Item(frame)) => Ok(ReadPoll::Message(frame)),
            Ok(TailOutcome::WaitExpired) => Ok(ReadPoll::Waiting),
            Ok(TailOutcome::Finished) => Ok(ReadPoll::Complete),
            Ok(TailOutcome::Cancelled) => Ok(ReadPoll::Cancelled),
            Err(err) => {
                self.0.state.terminated = true;
                Err(err)
            }
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
        let meta = Meta::from_lite3(&frame.payload)?;
        Ok(has_required_tags(&meta.tags, tags).then_some(frame))
    }
}

impl Iterator for Lite3Follow<'_> {
    type Item = Result<FrameRef, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
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
        return Err(Error::retention_gap(expected, available_seq));
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

pub(crate) trait PoolApiExt {
    /// Forward a checked snapshot with a fresh identity and system timestamp.
    fn append_view_now(
        &mut self,
        view: &crate::view::MessageView,
        durability: Durability,
    ) -> Result<crate::view::MessageView, Error> {
        self.append_view(view, AppendOptions::new(now_ns()?, durability))
    }

    /// Encode native data once and retain the committed immutable encoding.
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
    fn get_view(&self, seq: u64) -> Result<crate::view::MessageView, Error> {
        let frame = self.get_lite3(seq)?;
        crate::view::MessageView::from_owned(frame.payload, frame.seq, frame.time, frame.flags)
    }

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
    fn append_lite3_identity(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<(u64, u64), Error> {
        let seq = self.append_lite3(payload, options)?;
        Ok((seq, options.timestamp_ns))
    }

    fn append_lite3_identity_now(
        &mut self,
        payload: &[u8],
        durability: Durability,
    ) -> Result<(u64, u64), Error> {
        self.append_lite3_identity(payload, AppendOptions::new(now_ns()?, durability))
    }

    /// Validate and append a pre-encoded Lite3 payload without re-encoding it.
    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error>;

    /// Append the original Lite3 bytes and return a receipt prepared from input.
    /// A concurrent overwrite cannot remove the receipt after a successful append.
    fn append_lite3_with_receipt(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<Message, Error> {
        let (meta, data) = decode_payload(payload)?;
        let seq = self.append_lite3(payload, options)?;
        Ok(Message {
            seq,
            time: options.timestamp_ns,
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

    #[cfg(test)]
    fn get_message(&self, seq: u64) -> Result<Message, Error>;

    /// Fetch the raw Lite3 payload for a sequence number.
    fn get_lite3(&self, seq: u64) -> Result<FrameRef, Error>;

    #[doc(hidden)]
    #[cfg(test)]
    fn follow_legacy(&self, options: FollowOptions) -> Follow<'_>;

    /// Follow frames without JSON decoding.
    #[doc(hidden)]
    #[cfg(test)]
    fn follow_lite3_legacy(&self, options: FollowOptions) -> Lite3Follow<'_>;
}

impl PoolApiExt for Pool {
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
        let timestamp_ns = now_ns()?;
        let options = AppendOptions::new(timestamp_ns, durability);
        self.append_json(data, tags, options)
    }

    fn append_lite3(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error> {
        validate_bytes(payload)?;
        self.append_with_options(payload, options)
    }

    #[cfg(test)]
    fn get_message(&self, seq: u64) -> Result<Message, Error> {
        let frame = self.get(seq)?;
        message_from_frame(&frame)
    }

    fn get_lite3(&self, seq: u64) -> Result<FrameRef, Error> {
        self.get(seq)
    }

    #[cfg(test)]
    fn follow_legacy(&self, options: FollowOptions) -> Follow<'_> {
        Follow(TailReader::new(self, options))
    }

    #[cfg(test)]
    fn follow_lite3_legacy(&self, options: FollowOptions) -> Lite3Follow<'_> {
        Lite3Follow(TailReader::new(self, options))
    }
}

pub(crate) fn message_from_frame(frame: &FrameRef) -> Result<Message, Error> {
    Message::from_lite3(frame.seq, frame.time, &frame.payload)
}

/// Produce the existing wire envelope while borrowing data from this owned frame.
#[doc(hidden)]
pub fn message_json_from_frame(frame: &FrameRef) -> Result<Vec<u8>, Error> {
    let doc = Lite3DocRef::new(&frame.payload);
    let (tags_offset, count) = meta_tags(doc)?;
    let mut tags = Vec::with_capacity(count as usize);
    for index in 0..count {
        tags.push(meta_tag(doc, tags_offset, index)?);
    }
    let offset = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    let data = crate::core::lite3::JsonValue::new(doc, offset)?;
    let wire =
        crate::interface_wire::MessageWire::new(frame.seq, format_ts(frame.time)?, tags, data);
    serde_json::to_vec(&wire).map_err(payload_json_error)
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
    let offset = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
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
    Ok((tags_ofs, tags_count))
}

fn meta_tag<'a>(doc: Lite3DocRef<'a>, ofs: usize, index: u32) -> Result<&'a str, Error> {
    let item_type = doc.array_item_type(ofs, index).map_err(|_| {
        Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
    })?;
    if item_type != sys::LITE3_TYPE_STRING {
        return Err(Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array"));
    }
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
    let ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    doc.validate_readability_at(ofs)
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

#[cfg(test)]
mod tests {
    use super::{
        FollowOptions, GapPolicy, PoolApiExt, TailOutcome, TailState, decode_payload,
        message_from_frame,
    };
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
        use crate::api::FollowStart;
        let directory = tempdir().unwrap();
        let path = directory.path().join("follow.plasmite");
        let pool = batch_pool(&path);
        let mut writer = Pool::open(&path).unwrap();
        let mut live = pool.follow_legacy(FollowOptions::default());
        writer
            .append_json_now(&json!({"value": 5}), &[], Durability::Fast)
            .unwrap();
        assert_eq!(live.next_message().unwrap().unwrap().seq, 5);
        let mut finite = pool.follow_legacy(FollowOptions {
            start: FollowStart::Last(2),
            live: false,
            ..Default::default()
        });
        writer
            .append_json_now(&json!({"value": 6}), &[], Durability::Fast)
            .unwrap();
        assert_eq!(
            std::iter::from_fn(|| finite.next_message().transpose())
                .map(|m| m.unwrap().seq)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        let all = pool.follow_views(FollowOptions {
            start: FollowStart::all(),
            live: false,
            ..Default::default()
        });
        assert_eq!(
            all.map(|m| m.unwrap().seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        let mut empty = pool.follow_legacy(FollowOptions {
            start: FollowStart::Last(0),
            live: false,
            ..Default::default()
        });
        assert!(empty.next_message().unwrap().is_none());
        let mut since = pool.follow_legacy(FollowOptions {
            start: FollowStart::Since(u64::MAX),
            live: false,
            ..Default::default()
        });
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
        let mut state = TailState::new(
            &pool,
            FollowOptions {
                start: crate::api::FollowStart::Sequence(1),
                max_messages: Some(4),
                ..Default::default()
            },
        );
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
        let mut state = TailState::new(
            &pool,
            FollowOptions {
                start: crate::api::FollowStart::Sequence(1),
                ..Default::default()
            },
        );
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
        assert!(matches!(
            state
                .next_wait(&pool, Some(Duration::ZERO), select)
                .unwrap(),
            TailOutcome::Item(3)
        ));
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
        let mut state = TailState::new(
            &pool,
            FollowOptions {
                start: crate::api::FollowStart::Sequence(1),
                ..Default::default()
            },
        );
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
        let mut state = TailState::new(
            &pool,
            FollowOptions {
                start: crate::api::FollowStart::Sequence(1),
                cancel: Some(cancel.clone()),
                ..Default::default()
            },
        );
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
            json!([]),
            json!({}),
            json!({"case": [null, true, i64::MIN, i64::MAX, -0.0f64, 1.0f64]}),
            json!({"float": [2.027716612057172e155, f64::MIN_POSITIVE, f64::MAX]}),
            json!({"caption": "Unicode 温度 and NUL\0 and quote\"", "array": (0..1000).collect::<Vec<_>>()}),
            json!({"text": "a".repeat(32768)}),
            json!(42),
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
    fn raw_tail_checks_readability_and_tags_without_a_data_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool =
            Pool::create(dir.path().join("raw.plasmite"), PoolOptions::new(65536)).unwrap();
        let payload =
            encode_message(&["keep".into()], &json!({"nested": [{"text": "original"}]})).unwrap();
        pool.append_with_timestamp(payload.as_slice(), 123).unwrap();
        super::DATA_TREE_DECODES.with(|count| count.set(0));
        let options = FollowOptions {
            tags: vec!["keep".into()],
            max_messages: Some(1),
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        let frame = pool
            .follow_lite3_legacy(options)
            .next_frame()
            .unwrap()
            .unwrap();
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
        let options = FollowOptions {
            start: crate::api::FollowStart::Sequence(seq),
            tags: vec!["never".into()],
            ..Default::default()
        };
        assert_eq!(
            pool.follow_lite3_legacy(options.clone())
                .next_frame()
                .unwrap_err()
                .kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(
            pool.follow_legacy(options)
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
        let options = FollowOptions {
            start: crate::api::FollowStart::Sequence(u64::MAX),
            ..Default::default()
        };
        let mut raw = pool.follow_lite3_legacy(options.clone());
        assert_eq!(raw.next_frame().unwrap().unwrap().seq, u64::MAX);
        assert!(raw.next_frame().unwrap().is_none());
        let mut decoded = pool.follow_legacy(options);
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

        let mut options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        options.start = crate::api::FollowStart::Sequence(seq);
        options.max_messages = Some(1);
        let mut tail = pool.follow_lite3_legacy(options);
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
        let options = FollowOptions {
            start: crate::api::FollowStart::Sequence(1),
            timeout: Some(Duration::from_secs(1)),
            ..Default::default()
        };
        let mut first_tail = first.follow_legacy(options.clone());
        let mut second_tail = second.follow_legacy(options);
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

        let mut options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        options.notify = false;
        options.timeout = Some(Duration::from_secs(2));
        let mut json = reader.follow_legacy(options.clone());
        let mut lite3 = reader.follow_lite3_legacy(options);
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

        let mut options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        options.start = crate::api::FollowStart::Sequence(first.seq);
        options.max_messages = Some(1);
        options.tags = vec!["keep".to_string()];
        let mut raw = pool.follow_lite3_legacy(options.clone());
        let mut tail = pool.follow_legacy(options);
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
        let mut options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        options.cancel = Some(cancel.clone());
        let mut json = pool.follow_legacy(options.clone());
        let mut lite3 = pool.follow_lite3_legacy(options);
        assert_eq!(json.next_message().unwrap().unwrap().seq, 1);
        assert_eq!(lite3.next_frame().unwrap().unwrap().seq, 1);
        cancel.store(true, Ordering::Release);
        assert!(json.next_message().unwrap().is_none());
        assert!(lite3.next_frame().unwrap().is_none());

        let mut expired = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        expired.timeout = Some(Duration::ZERO);
        assert!(
            pool.follow_legacy(expired.clone())
                .next_message()
                .unwrap()
                .is_none()
        );
        assert!(
            pool.follow_lite3_legacy(expired)
                .next_frame()
                .unwrap()
                .is_none()
        );
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

        let mut strict_options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        strict_options.start = crate::api::FollowStart::Sequence(first.seq);
        strict_options.gap_policy = GapPolicy::Error;
        let mut strict_lite3 = strict_reader.follow_lite3_legacy(strict_options.clone());
        let mut strict = strict_reader.follow_legacy(strict_options);
        assert_eq!(
            strict
                .next_message()
                .expect("first strict read")
                .expect("first message")
                .seq,
            first.seq
        );
        assert_eq!(strict_lite3.next_frame().unwrap().unwrap().seq, first.seq);

        let mut continuing_options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        continuing_options.start = crate::api::FollowStart::Sequence(first.seq);
        let mut continuing = continuing_reader.follow_legacy(continuing_options);
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
        let mut strict_options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        strict_options.start = crate::api::FollowStart::Sequence(first.seq);
        strict_options.tags = vec!["never".to_string()];
        strict_options.gap_policy = GapPolicy::Error;
        let mut strict = strict_reader.follow_legacy(strict_options);
        let err = strict.next_message().expect_err("stale start must fail");
        assert_eq!(err.kind(), ErrorKind::RetentionGap);
        assert_eq!(err.seq(), Some(first.seq));

        let implicit_reader = Pool::open(&path).expect("open implicit reader");
        let mut implicit_options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        implicit_options.max_messages = Some(1);
        implicit_options.gap_policy = GapPolicy::Error;
        let mut implicit = implicit_reader.follow_legacy(implicit_options);
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
        let mut options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        options.start = crate::api::FollowStart::Sequence(first.seq);
        options.gap_policy = GapPolicy::Error;
        let mut tail = reader.follow_lite3_legacy(options);
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

        let mut options = FollowOptions {
            start: crate::api::FollowStart::all(),
            ..Default::default()
        };
        options.start = crate::api::FollowStart::Sequence(oldest);
        options.max_messages = Some(expected.len());
        options.gap_policy = GapPolicy::Error;
        options.notify = false;
        let mut tail = pool.follow_legacy(options);
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
