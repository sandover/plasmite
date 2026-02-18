//! Purpose: Manage pool files (create/open), mmap access, locking, and append application.
//! Exports: `Pool`, `PoolOptions`, `AppendOptions`, `Durability`, `PoolHeader`, `Bounds`,
//! `PoolInfo`, `SeqOffsetCache`.
//! Role: IO boundary for the core: owns file handles/mmap and delegates planning to `plan`.
//! Invariants: All mutations hold an exclusive append lock across processes.
//! Invariants: Append writes mark frames `Writing` -> payload -> `Committed`; header persists last.
//! Invariants: Header size is fixed (4096) and validated strictly on open.
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use libc::{EACCES, EPERM};
use memmap2::MmapMut;

use crate::core::error::{Error, ErrorKind};
use crate::core::format;
use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
use crate::core::notify;
use crate::core::plan;
use crate::core::validate;

const MAGIC: [u8; 4] = *b"PLSM";
const ENDIANNESS_LE: u8 = 1;
const HEADER_SIZE: usize = 4096;
const INDEX_SLOT_BYTES: u64 = 16;
const MAX_AUTO_INDEX_CAPACITY: u64 = 65_536;
const MIN_RING_SIZE_FOR_INDEX: u64 = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolHeader {
    pub file_size: u64,
    pub index_offset: u64,
    pub index_capacity: u32,
    pub ring_offset: u64,
    pub ring_size: u64,
    pub flags: u64,
    pub head_off: u64,
    pub tail_off: u64,
    pub tail_next_off: u64,
    pub oldest_seq: u64,
    pub newest_seq: u64,
}

impl PoolHeader {
    fn new(file_size: u64, index_capacity: u32) -> Result<Self, Error> {
        let index_offset = HEADER_SIZE as u64;
        let index_bytes = (index_capacity as u64)
            .checked_mul(INDEX_SLOT_BYTES)
            .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("index capacity too large"))?;
        let ring_offset = index_offset
            .checked_add(index_bytes)
            .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("index capacity too large"))?;
        if file_size <= ring_offset {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("file_size must exceed header and index size"));
        }
        let ring_size = file_size - ring_offset;
        Ok(Self {
            file_size,
            index_offset,
            index_capacity,
            ring_offset,
            ring_size,
            flags: 0,
            head_off: 0,
            tail_off: 0,
            tail_next_off: 0,
            oldest_seq: 0,
            newest_seq: 0,
        })
    }

    fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4..8].copy_from_slice(&format::POOL_FORMAT_VERSION.to_le_bytes());
        buf[8] = ENDIANNESS_LE;

        write_u64(&mut buf, 16, self.file_size);
        write_u64(&mut buf, 24, self.index_offset);
        write_u32(&mut buf, 32, self.index_capacity);
        write_u64(&mut buf, 40, self.ring_offset);
        write_u64(&mut buf, 48, self.ring_size);
        write_u64(&mut buf, 56, self.flags);
        write_u64(&mut buf, 64, self.head_off);
        write_u64(&mut buf, 72, self.tail_off);
        write_u64(&mut buf, 80, self.tail_next_off);
        write_u64(&mut buf, 88, self.oldest_seq);
        write_u64(&mut buf, 96, self.newest_seq);

        buf
    }

    fn decode(buf: &[u8]) -> Result<Self, Error> {
        if buf.len() < HEADER_SIZE {
            return Err(Error::new(ErrorKind::Corrupt).with_message("header too small"));
        }
        if buf[0..4] != MAGIC {
            return Err(Error::new(ErrorKind::Corrupt).with_message("bad magic"));
        }
        let version = u32::from_le_bytes(read_4(buf, 4));
        if version != format::POOL_FORMAT_VERSION {
            return Err(format::pool_version_error(version));
        }
        if buf[8] != ENDIANNESS_LE {
            return Err(Error::new(ErrorKind::Corrupt).with_message("unsupported endianness"));
        }

        let file_size = read_u64(buf, 16);
        let index_offset = read_u64(buf, 24);
        let index_capacity = read_u32(buf, 32);
        let ring_offset = read_u64(buf, 40);
        let ring_size = read_u64(buf, 48);
        let flags = read_u64(buf, 56);
        let head_off = read_u64(buf, 64);
        let tail_off = read_u64(buf, 72);
        let tail_next_off = read_u64(buf, 80);
        let oldest_seq = read_u64(buf, 88);
        let newest_seq = read_u64(buf, 96);

        Ok(Self {
            file_size,
            index_offset,
            index_capacity,
            ring_offset,
            ring_size,
            flags,
            head_off,
            tail_off,
            tail_next_off,
            oldest_seq,
            newest_seq,
        })
    }

    fn validate(&self, actual_file_size: u64) -> Result<(), Error> {
        if self.file_size == 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("invalid file size"));
        }
        if self.file_size != actual_file_size {
            return Err(Error::new(ErrorKind::Corrupt).with_message("invalid file size"));
        }
        if self.index_offset != HEADER_SIZE as u64 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("invalid index offset"));
        }
        let index_bytes = (self.index_capacity as u64)
            .checked_mul(INDEX_SLOT_BYTES)
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("index size overflow"))?;
        let expected_ring_offset = self
            .index_offset
            .checked_add(index_bytes)
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("ring offset overflow"))?;
        if self.ring_offset < HEADER_SIZE as u64 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("invalid ring offset"));
        }
        if self.ring_offset != expected_ring_offset {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring offset mismatch"));
        }
        if self.ring_offset + self.ring_size != self.file_size {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring bounds mismatch"));
        }
        if self.ring_size == 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring size is zero"));
        }
        let ring_size = self.ring_size;
        if self.head_off >= ring_size
            || self.tail_off >= ring_size
            || self.tail_next_off >= ring_size
        {
            return Err(Error::new(ErrorKind::Corrupt).with_message("header offset out of range"));
        }
        if self.head_off % 8 != 0 || self.tail_off % 8 != 0 || self.tail_next_off % 8 != 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("header offset not aligned"));
        }
        // Empty pool is indicated by oldest_seq == 0. newest_seq is monotonic and may be non-zero
        // even when the pool is empty (e.g. after overwriting all messages).
        if self.oldest_seq != 0 && self.oldest_seq > self.newest_seq {
            return Err(Error::new(ErrorKind::Corrupt).with_message("seq bounds inverted"));
        }
        if self.oldest_seq == 0
            && (self.head_off != self.tail_off || self.tail_next_off != self.tail_off)
        {
            return Err(
                Error::new(ErrorKind::Corrupt).with_message("empty header offsets mismatch")
            );
        }
        Ok(())
    }
}

fn read_4(buf: &[u8], offset: usize) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(&buf[offset..offset + 4]);
    out
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(read_8(buf, offset))
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    let mut out = [0u8; 4];
    out.copy_from_slice(&buf[offset..offset + 4]);
    u32::from_le_bytes(out)
}

fn read_8(buf: &[u8], offset: usize) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&buf[offset..offset + 8]);
    out
}

fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Copy, Debug)]
pub struct PoolOptions {
    pub file_size: u64,
    pub index_capacity: Option<u32>,
}

impl PoolOptions {
    pub fn new(file_size: u64) -> Self {
        Self {
            file_size,
            index_capacity: None,
        }
    }

    pub fn with_index_capacity(mut self, index_capacity: u32) -> Self {
        self.index_capacity = Some(index_capacity);
        self
    }

    fn resolved_index_capacity(&self) -> u32 {
        if let Some(explicit) = self.index_capacity {
            return explicit;
        }
        let candidate = (self.file_size / 256).min(MAX_AUTO_INDEX_CAPACITY);
        if candidate == 0 {
            return 0;
        }
        let usable_for_index = self
            .file_size
            .saturating_sub(HEADER_SIZE as u64 + MIN_RING_SIZE_FOR_INDEX);
        let max_by_budget = usable_for_index / INDEX_SLOT_BYTES;
        candidate.min(max_by_budget) as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    Fast,
    Flush,
}

#[derive(Clone, Copy, Debug)]
pub struct AppendOptions {
    pub timestamp_ns: u64,
    pub durability: Durability,
}

impl AppendOptions {
    pub fn new(timestamp_ns: u64, durability: Durability) -> Self {
        Self {
            timestamp_ns,
            durability,
        }
    }
}

impl Default for AppendOptions {
    fn default() -> Self {
        Self {
            timestamp_ns: 0,
            durability: Durability::Fast,
        }
    }
}

/// Bounded LRU cache mapping sequence numbers to ring offsets.
/// Use with `Pool::get_with_cache`; the cache is optional and must be passed explicitly.
#[derive(Debug, Clone)]
pub struct SeqOffsetCache {
    max_entries: usize,
    entries: HashMap<u64, usize>,
    order: VecDeque<u64>,
}

impl SeqOffsetCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub fn get(&mut self, seq: u64) -> Option<usize> {
        let offset = *self.entries.get(&seq)?;
        self.touch(seq);
        Some(offset)
    }

    pub fn insert(&mut self, seq: u64, offset: usize) {
        if self.max_entries == 0 {
            return;
        }
        if let std::collections::hash_map::Entry::Occupied(mut entry) = self.entries.entry(seq) {
            entry.insert(offset);
            self.touch(seq);
            return;
        }

        if self.entries.len() == self.max_entries {
            if let Some(evict) = self.order.pop_back() {
                self.entries.remove(&evict);
            }
        }

        self.entries.insert(seq, offset);
        self.order.push_front(seq);
    }

    pub fn remove(&mut self, seq: u64) {
        if self.entries.remove(&seq).is_none() {
            return;
        }
        if let Some(index) = self.order.iter().position(|item| *item == seq) {
            self.order.remove(index);
        }
    }

    fn touch(&mut self, seq: u64) {
        if let Some(index) = self.order.iter().position(|item| *item == seq) {
            self.order.remove(index);
        }
        self.order.push_front(seq);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bounds {
    pub oldest_seq: Option<u64>,
    pub newest_seq: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolInfo {
    pub path: PathBuf,
    pub file_size: u64,
    pub index_offset: u64,
    pub index_capacity: u32,
    pub index_size_bytes: u64,
    pub ring_offset: u64,
    pub ring_size: u64,
    pub bounds: Bounds,
    pub metrics: Option<PoolMetrics>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolMetrics {
    pub message_count: u64,
    pub seq_span: u64,
    pub utilization: PoolUtilization,
    pub age: PoolAgeMetrics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolUtilization {
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub used_percent_hundredths: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolAgeMetrics {
    pub oldest_time: Option<String>,
    pub newest_time: Option<String>,
    pub oldest_age_ms: Option<u64>,
    pub newest_age_ms: Option<u64>,
}

pub struct Pool {
    path: PathBuf,
    file: File,
    mmap: MmapMut,
    header: PoolHeader,
}

impl Pool {
    pub fn create(path: impl AsRef<Path>, options: PoolOptions) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();

        // Creating a pool is a mutating operation; ensure the parent directory exists so
        // API/binding users don't need to `mkdir -p` for common first-run flows.
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|err| {
                let kind = map_io_error_kind(&err);
                let message = match kind {
                    ErrorKind::Permission => "failed to create pool directory (permission denied)",
                    _ => "failed to create pool directory",
                };
                Error::new(kind)
                    .with_message(message)
                    .with_path(parent)
                    .with_source(err)
            })?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| {
                let kind = map_io_error_kind(&err);
                let message = match kind {
                    ErrorKind::NotFound => "parent directory not found",
                    ErrorKind::Permission => "failed to create pool file (permission denied)",
                    _ => "failed to create pool file",
                };
                Error::new(kind)
                    .with_message(message)
                    .with_path(&path)
                    .with_source(err)
            })?;

        file.set_len(options.file_size).map_err(|err| {
            let kind = map_io_error_kind(&err);
            Error::new(kind)
                .with_message("failed to size pool file")
                .with_path(&path)
                .with_source(err)
        })?;

        let index_capacity = options.resolved_index_capacity();
        let header = PoolHeader::new(options.file_size, index_capacity)?;
        write_header(&mut file, &header, &path)?;

        let mmap = unsafe {
            MmapMut::map_mut(&file).map_err(|err| {
                let kind = map_io_error_kind(&err);
                Error::new(kind)
                    .with_message("failed to mmap pool file")
                    .with_path(&path)
                    .with_source(err)
            })?
        };

        let mut pool = Self {
            path,
            file,
            mmap,
            header,
        };
        let index_start = pool.header.index_offset as usize;
        let index_end = pool.header.ring_offset as usize;
        pool.mmap[index_start..index_end].fill(0);
        Ok(pool)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| {
                let err_kind = err.kind();
                let mut error = Error::new(map_io_error_kind(&err))
                    .with_path(&path)
                    .with_source(err);
                if err_kind == io::ErrorKind::NotFound {
                    error = error.with_message("not found");
                }
                error
            })?;

        let actual_size = file
            .metadata()
            .map(|meta| meta.len())
            .map_err(|err| Error::new(ErrorKind::Io).with_path(&path).with_source(err))?;

        let header = read_header(&mut file, &path)?;
        header.validate(actual_size)?;

        let mmap = unsafe {
            MmapMut::map_mut(&file)
                .map_err(|err| Error::new(ErrorKind::Io).with_path(&path).with_source(err))?
        };

        Ok(Self {
            path,
            file,
            mmap,
            header,
        })
    }

    pub fn header(&self) -> PoolHeader {
        self.header
    }

    pub fn header_from_mmap(&self) -> Result<PoolHeader, Error> {
        PoolHeader::decode(&self.mmap[0..HEADER_SIZE])
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub fn mmap_len(&self) -> usize {
        self.mmap.len()
    }

    pub(crate) fn mmap(&self) -> &MmapMut {
        &self.mmap
    }

    pub fn bounds(&self) -> Result<Bounds, Error> {
        let header = self.header_from_mmap()?;
        Ok(bounds_from_header(header))
    }

    pub fn info(&self) -> Result<PoolInfo, Error> {
        let header = self.header_from_mmap()?;
        let bounds = bounds_from_header(header);
        Ok(PoolInfo {
            path: self.path.clone(),
            file_size: header.file_size,
            index_offset: header.index_offset,
            index_capacity: header.index_capacity,
            index_size_bytes: header.index_capacity as u64 * INDEX_SLOT_BYTES,
            ring_offset: header.ring_offset,
            ring_size: header.ring_size,
            bounds,
            metrics: Some(self.metrics_from_header(header, bounds)),
        })
    }

    pub fn get(&self, seq: u64) -> Result<crate::core::cursor::FrameRef<'_>, Error> {
        let mut header = self.header_from_mmap()?;
        let bounds = bounds_from_header(header);
        let (oldest, newest) = match (bounds.oldest_seq, bounds.newest_seq) {
            (Some(oldest), Some(newest)) => (oldest, newest),
            _ => {
                return Err(Error::new(ErrorKind::NotFound)
                    .with_message("message not found")
                    .with_seq(seq));
            }
        };

        if seq < oldest || seq > newest {
            return Err(Error::new(ErrorKind::NotFound)
                .with_message("message not found")
                .with_seq(seq));
        }

        if let Some(frame) = self.get_via_index(header, seq) {
            return Ok(frame);
        }

        let mut cursor = crate::core::cursor::Cursor::new();
        cursor.seek_to(header.tail_off as usize);

        loop {
            match cursor.next(self)? {
                crate::core::cursor::CursorResult::Message(frame) => {
                    if frame.seq == seq {
                        return Ok(frame);
                    }
                    if frame.seq > seq {
                        return Err(Error::new(ErrorKind::NotFound)
                            .with_message("message not found")
                            .with_seq(seq));
                    }
                }
                crate::core::cursor::CursorResult::WouldBlock => {
                    return Err(Error::new(ErrorKind::NotFound)
                        .with_message("message not found")
                        .with_seq(seq));
                }
                crate::core::cursor::CursorResult::FellBehind => {
                    header = self.header_from_mmap()?;
                    if header.oldest_seq != 0 && seq < header.oldest_seq {
                        return Err(Error::new(ErrorKind::NotFound)
                            .with_message("message not found")
                            .with_seq(seq));
                    }
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    }

    fn get_via_index(
        &self,
        header: PoolHeader,
        seq: u64,
    ) -> Option<crate::core::cursor::FrameRef<'_>> {
        let index_capacity = header.index_capacity as u64;
        if index_capacity == 0 {
            return None;
        }

        let slot = (seq % index_capacity) as usize;
        let start = header.index_offset as usize + slot * INDEX_SLOT_BYTES as usize;
        let end = start + INDEX_SLOT_BYTES as usize;
        if end > self.mmap.len() {
            return None;
        }
        let stored_seq = read_u64(self.mmap(), start);
        let stored_offset = read_u64(self.mmap(), start + 8);
        if stored_seq != seq {
            return None;
        }
        let ring_size = header.ring_size as usize;
        if stored_offset as usize >= ring_size {
            return None;
        }
        let ring_offset = header.ring_offset as usize;
        match crate::core::cursor::read_frame_at(
            self.mmap(),
            ring_offset,
            ring_size,
            stored_offset as usize,
        ) {
            Ok(crate::core::cursor::ReadResult::Message { frame, .. }) if frame.seq == seq => {
                Some(frame)
            }
            _ => None,
        }
    }

    /// Fetch a frame using a caller-managed seq->offset cache for faster repeats.
    /// The cache is an optional optimization and must be passed explicitly.
    pub fn get_with_cache(
        &self,
        seq: u64,
        cache: &mut SeqOffsetCache,
    ) -> Result<crate::core::cursor::FrameRef<'_>, Error> {
        let mut header = self.header_from_mmap()?;
        let bounds = bounds_from_header(header);
        let (oldest, newest) = match (bounds.oldest_seq, bounds.newest_seq) {
            (Some(oldest), Some(newest)) => (oldest, newest),
            _ => {
                return Err(Error::new(ErrorKind::NotFound)
                    .with_message("message not found")
                    .with_seq(seq));
            }
        };

        if seq < oldest || seq > newest {
            return Err(Error::new(ErrorKind::NotFound)
                .with_message("message not found")
                .with_seq(seq));
        }

        let ring_offset = header.ring_offset as usize;
        let ring_size = header.ring_size as usize;

        if let Some(offset) = cache.get(seq) {
            let cached =
                crate::core::cursor::read_frame_at(self.mmap(), ring_offset, ring_size, offset);
            if let Ok(crate::core::cursor::ReadResult::Message { frame, .. }) = cached {
                if frame.seq == seq {
                    return Ok(frame);
                }
            }
            cache.remove(seq);
        }

        let mut offset = header.tail_off as usize;
        loop {
            match crate::core::cursor::read_frame_at(self.mmap(), ring_offset, ring_size, offset)? {
                crate::core::cursor::ReadResult::Message { frame, next_off } => {
                    cache.insert(frame.seq, offset);
                    if frame.seq == seq {
                        return Ok(frame);
                    }
                    if frame.seq > seq {
                        return Err(Error::new(ErrorKind::NotFound)
                            .with_message("message not found")
                            .with_seq(seq));
                    }
                    offset = next_off;
                }
                crate::core::cursor::ReadResult::Wrap => {
                    offset = 0;
                }
                crate::core::cursor::ReadResult::WouldBlock => {
                    return Err(Error::new(ErrorKind::NotFound)
                        .with_message("message not found")
                        .with_seq(seq));
                }
                crate::core::cursor::ReadResult::FellBehind => {
                    header = self.header_from_mmap()?;
                    if header.oldest_seq != 0 && seq < header.oldest_seq {
                        return Err(Error::new(ErrorKind::NotFound)
                            .with_message("message not found")
                            .with_seq(seq));
                    }
                    offset = header.tail_off as usize;
                }
            }
        }
    }

    pub fn append_lock(&self) -> Result<AppendLock, Error> {
        let file = self.file.try_clone().map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_path(&self.path)
                .with_source(err)
        })?;
        file.lock_exclusive().map_err(|err| {
            Error::new(lock_error_kind(&err))
                .with_path(&self.path)
                .with_source(err)
        })?;
        Ok(AppendLock { file })
    }

    pub fn append(&mut self, payload: &[u8]) -> Result<u64, Error> {
        self.append_with_options(payload, AppendOptions::default())
    }

    pub fn append_with_timestamp(
        &mut self,
        payload: &[u8],
        timestamp_ns: u64,
    ) -> Result<u64, Error> {
        self.append_with_options(payload, AppendOptions::new(timestamp_ns, Durability::Fast))
    }

    pub fn append_with_options(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
    ) -> Result<u64, Error> {
        let _lock = self.append_lock()?;
        // Refresh header after acquiring the lock to avoid stale state across processes.
        self.header = self.header_from_mmap()?;
        self.append_locked(payload, options)
    }

    fn append_locked(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error> {
        let ring_offset = self.header.ring_offset as usize;
        let ring_size = self.header.ring_size as usize;
        let plan = plan::plan_append(self.header, &self.mmap, payload.len())?;

        apply_append(
            &mut self.mmap,
            ring_offset,
            &plan,
            payload,
            options.timestamp_ns,
        )?;

        self.header = plan.next_header;

        if options.durability == Durability::Flush {
            let frame_offset = ring_offset + plan.frame_offset;
            flush_mmap_range(
                &self.mmap,
                frame_offset,
                plan.frame_len,
                &self.path,
                "failed to flush frame",
            )?;
            if let Some(wrap_head) = plan.wrap_offset {
                let wrap_start = ring_offset + wrap_head;
                flush_mmap_range(
                    &self.mmap,
                    wrap_start,
                    FRAME_HEADER_LEN,
                    &self.path,
                    "failed to flush wrap marker",
                )?;
            }
            if plan.next_header.index_capacity > 0 {
                let (index_start, index_len) = index_slot_range(
                    plan.next_header.index_offset,
                    plan.next_header.index_capacity,
                    plan.seq,
                )
                .ok_or_else(|| {
                    Error::new(ErrorKind::Corrupt).with_message("index slot calculation overflow")
                })?;
                flush_mmap_range(
                    &self.mmap,
                    index_start,
                    index_len,
                    &self.path,
                    "failed to flush index slot",
                )?;
            }
            flush_mmap_range(
                &self.mmap,
                0,
                HEADER_SIZE,
                &self.path,
                "failed to flush header",
            )?;
        }

        validate::debug_assert_tail_committed(
            &self.mmap,
            ring_offset,
            ring_size,
            self.header.tail_off as usize,
            self.header.tail_next_off as usize,
            self.header.oldest_seq,
        );

        let _ = notify::post_for_path(&self.path);

        Ok(plan.seq)
    }

    fn metrics_from_header(&self, header: PoolHeader, bounds: Bounds) -> PoolMetrics {
        let message_count = match (bounds.oldest_seq, bounds.newest_seq) {
            (Some(oldest), Some(newest)) => newest.saturating_sub(oldest).saturating_add(1),
            _ => 0,
        };
        let seq_span = message_count;

        let used_bytes = used_ring_bytes(header);
        let free_bytes = header.ring_size.saturating_sub(used_bytes);
        let used_percent_hundredths = if header.ring_size == 0 {
            0
        } else {
            used_bytes.saturating_mul(10_000) / header.ring_size
        };

        let (oldest_timestamp_ns, newest_timestamp_ns) = self.boundary_timestamps(bounds);
        let now_ns = unix_now_ns();
        let oldest_age_ms = oldest_timestamp_ns.map(|ts| now_ns.saturating_sub(ts) / 1_000_000);
        let newest_age_ms = newest_timestamp_ns.map(|ts| now_ns.saturating_sub(ts) / 1_000_000);

        PoolMetrics {
            message_count,
            seq_span,
            utilization: PoolUtilization {
                used_bytes,
                free_bytes,
                used_percent_hundredths,
            },
            age: PoolAgeMetrics {
                oldest_time: oldest_timestamp_ns.and_then(format_timestamp_ns),
                newest_time: newest_timestamp_ns.and_then(format_timestamp_ns),
                oldest_age_ms,
                newest_age_ms,
            },
        }
    }

    fn boundary_timestamps(&self, bounds: Bounds) -> (Option<u64>, Option<u64>) {
        let (Some(oldest), Some(newest)) = (bounds.oldest_seq, bounds.newest_seq) else {
            return (None, None);
        };
        if oldest == newest {
            let ts = self.frame_timestamp_ns_for_seq(oldest);
            return (ts, ts);
        }
        (
            self.frame_timestamp_ns_for_seq(oldest),
            self.frame_timestamp_ns_for_seq(newest),
        )
    }

    fn frame_timestamp_ns_for_seq(&self, seq: u64) -> Option<u64> {
        self.get(seq).ok().map(|frame| frame.timestamp_ns)
    }
}

pub struct AppendLock {
    file: File,
}

impl Drop for AppendLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_error_kind(err: &io::Error) -> ErrorKind {
    let errno = err.raw_os_error().unwrap_or_default();
    if errno == EACCES || errno == EPERM {
        return ErrorKind::Permission;
    }
    match err.kind() {
        io::ErrorKind::WouldBlock => ErrorKind::Busy,
        io::ErrorKind::PermissionDenied => ErrorKind::Permission,
        _ => ErrorKind::Io,
    }
}

fn map_io_error_kind(err: &io::Error) -> ErrorKind {
    match err.kind() {
        io::ErrorKind::NotFound => ErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => ErrorKind::Permission,
        _ => ErrorKind::Io,
    }
}

fn read_header(file: &mut File, path: &Path) -> Result<PoolHeader, Error> {
    let mut buf = [0u8; HEADER_SIZE];
    file.seek(SeekFrom::Start(0))
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    file.read_exact(&mut buf).map_err(|err| {
        let kind = if err.kind() == io::ErrorKind::UnexpectedEof {
            ErrorKind::Corrupt
        } else {
            ErrorKind::Io
        };
        Error::new(kind).with_path(path).with_source(err)
    })?;
    PoolHeader::decode(&buf)
}

fn write_header(file: &mut File, header: &PoolHeader, path: &Path) -> Result<(), Error> {
    let buf = header.encode();
    file.seek(SeekFrom::Start(0))
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    file.write_all(&buf)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    file.flush()
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    Ok(())
}

fn write_pool_header(mmap: &mut MmapMut, header: &PoolHeader) {
    mmap[0..4].copy_from_slice(&MAGIC);
    mmap[4..8].copy_from_slice(&format::POOL_FORMAT_VERSION.to_le_bytes());
    mmap[8] = ENDIANNESS_LE;
    write_u64(mmap, 16, header.file_size);
    write_u64(mmap, 24, header.index_offset);
    write_u32(mmap, 32, header.index_capacity);
    write_u64(mmap, 40, header.ring_offset);
    write_u64(mmap, 48, header.ring_size);
    write_u64(mmap, 56, header.flags);
    write_u64(mmap, 64, header.head_off);
    write_u64(mmap, 72, header.tail_off);
    write_u64(mmap, 80, header.tail_next_off);
    write_u64(mmap, 88, header.oldest_seq);
    write_u64(mmap, 96, header.newest_seq);
}

fn flush_mmap_range(
    mmap: &MmapMut,
    offset: usize,
    len: usize,
    path: &Path,
    message: &str,
) -> Result<(), Error> {
    mmap.flush_range(offset, len).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message(message)
            .with_path(path)
            .with_source(err)
    })
}

fn bounds_from_header(header: PoolHeader) -> Bounds {
    if header.oldest_seq == 0 {
        Bounds {
            oldest_seq: None,
            newest_seq: None,
        }
    } else {
        Bounds {
            oldest_seq: Some(header.oldest_seq),
            newest_seq: Some(header.newest_seq),
        }
    }
}

fn used_ring_bytes(header: PoolHeader) -> u64 {
    let head = header.head_off;
    let tail = header.tail_off;
    if head == tail {
        return if header.oldest_seq == 0 {
            0
        } else {
            header.ring_size
        };
    }
    if head > tail {
        head - tail
    } else {
        header.ring_size.saturating_sub(tail - head)
    }
}

fn unix_now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

fn format_timestamp_ns(timestamp_ns: u64) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

#[cfg(test)]
fn read_frame_header(
    mmap: &MmapMut,
    ring_offset: usize,
    head: usize,
) -> Result<FrameHeader, Error> {
    let start = ring_offset + head;
    let end = start + FRAME_HEADER_LEN;
    FrameHeader::decode(&mmap[start..end])
}

fn write_frame_header(
    mmap: &mut MmapMut,
    ring_offset: usize,
    head: usize,
    header: &FrameHeader,
) -> Result<(), Error> {
    let start = ring_offset + head;
    let end = start + FRAME_HEADER_LEN;
    mmap[start..end].copy_from_slice(&header.encode());
    Ok(())
}

fn write_frame(
    mmap: &mut MmapMut,
    ring_offset: usize,
    head: usize,
    header: &FrameHeader,
    payload: &[u8],
) -> Result<(), Error> {
    write_frame_header(mmap, ring_offset, head, header)?;
    let payload_start = ring_offset + head + FRAME_HEADER_LEN;
    let payload_end = payload_start + payload.len();
    mmap[payload_start..payload_end].copy_from_slice(payload);
    let marker_start = payload_end;
    let marker_end = marker_start + frame::FRAME_COMMIT_MARKER_LEN;
    mmap[marker_start..marker_end].copy_from_slice(&frame::FRAME_COMMIT_MARKER);
    Ok(())
}

fn write_wrap(mmap: &mut MmapMut, ring_offset: usize, head: usize) -> Result<(), Error> {
    let header = FrameHeader::new(FrameState::Wrap, 0, 0, 0, 0, 0);
    write_frame_header(mmap, ring_offset, head, &header)
}

fn apply_append(
    mmap: &mut MmapMut,
    ring_offset: usize,
    plan: &plan::AppendPlan,
    payload: &[u8],
    timestamp_ns: u64,
) -> Result<(), Error> {
    let expected_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len())
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("frame length overflow"))?;
    if expected_len != plan.frame_len {
        return Err(Error::new(ErrorKind::Corrupt).with_message("append plan length mismatch"));
    }

    if let Some(wrap_offset) = plan.wrap_offset {
        write_wrap(mmap, ring_offset, wrap_offset)?;
    }

    let header = FrameHeader::new(
        FrameState::Writing,
        0,
        plan.seq,
        timestamp_ns,
        payload.len() as u32,
        0,
    );
    write_frame(mmap, ring_offset, plan.frame_offset, &header, payload)?;

    let mut committed = header;
    committed.state = FrameState::Committed;
    write_frame_header(mmap, ring_offset, plan.frame_offset, &committed)?;

    write_index_slot(
        mmap,
        plan.next_header.index_offset,
        plan.next_header.index_capacity,
        plan.seq,
        plan.frame_offset as u64,
    )?;

    write_pool_header(mmap, &plan.next_header);

    Ok(())
}

fn write_index_slot(
    mmap: &mut [u8],
    index_offset: u64,
    index_capacity: u32,
    seq: u64,
    ring_relative_offset: u64,
) -> Result<(), Error> {
    if index_capacity == 0 {
        return Ok(());
    }
    let (start, slot_bytes) =
        index_slot_range(index_offset, index_capacity, seq).ok_or_else(|| {
            Error::new(ErrorKind::Corrupt).with_message("index slot calculation overflow")
        })?;
    let end = start + slot_bytes;
    if end > mmap.len() {
        return Err(Error::new(ErrorKind::Corrupt).with_message("index slot out of bounds"));
    }
    mmap[start..start + 8].copy_from_slice(&seq.to_le_bytes());
    mmap[start + 8..end].copy_from_slice(&ring_relative_offset.to_le_bytes());
    Ok(())
}

fn index_slot_range(index_offset: u64, index_capacity: u32, seq: u64) -> Option<(usize, usize)> {
    if index_capacity == 0 {
        return None;
    }
    let slot = usize::try_from(seq % index_capacity as u64).ok()?;
    let index_offset = usize::try_from(index_offset).ok()?;
    let slot_bytes = usize::try_from(INDEX_SLOT_BYTES).ok()?;
    let start = index_offset.checked_add(slot.checked_mul(slot_bytes)?)?;
    Some((start, slot_bytes))
}

#[cfg(test)]
mod tests {
    use super::{HEADER_SIZE, Pool, PoolHeader, PoolOptions, SeqOffsetCache, apply_append};
    use crate::core::error::{Error, ErrorKind};
    use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
    use crate::core::lite3;
    use crate::core::lite3::Lite3DocRef;
    use crate::core::plan;
    use std::fs;
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn create_and_open_pool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create pool");
        let header = pool.header();
        assert_eq!(header.file_size, 1024 * 1024);

        let reopened = Pool::open(&path).expect("open pool");
        assert_eq!(reopened.header().file_size, 1024 * 1024);
    }

    #[test]
    fn create_auto_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("missing").join("nested");
        let path = nested.join("pool.plasmite");
        assert!(!nested.exists());

        let _pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create pool");
        assert!(nested.exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_reports_permission_when_parent_dir_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let readonly = dir.path().join("readonly");
        fs::create_dir_all(&readonly).expect("mkdir");
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555)).expect("chmod");

        let path = readonly.join("child").join("pool.plasmite");
        let err = match Pool::create(&path, PoolOptions::new(1024 * 1024)) {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert!(
            err.message()
                .unwrap_or_default()
                .contains("failed to create pool directory"),
            "unexpected message: {:?}",
            err.message()
        );
    }

    #[test]
    fn corrupt_header_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(&path)
            .expect("create");
        file.set_len(1024 * 1024).expect("len");
        file.seek(SeekFrom::Start(0)).expect("seek");
        file.write_all(b"NOPE").expect("write");
        file.flush().expect("flush");

        let result = Pool::open(&path);
        match result {
            Ok(_) => panic!("expected corrupt header error"),
            Err(err) => assert_eq!(err.kind(), ErrorKind::Corrupt),
        }
    }

    #[test]
    fn unsupported_version_is_usage_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(&path)
            .expect("create");
        file.set_len(1024 * 1024).expect("len");

        let header = super::PoolHeader::new(1024 * 1024, 0).expect("header");
        let mut buf = header.encode();
        buf[4..8].copy_from_slice(&42u32.to_le_bytes());
        file.seek(SeekFrom::Start(0)).expect("seek");
        file.write_all(&buf).expect("write");
        file.flush().expect("flush");

        let err = match Pool::open(&path) {
            Ok(_) => panic!("expected unsupported version error"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Usage);
        let message = err.message().unwrap_or("");
        assert!(message.contains("42"));
        assert!(message.contains("3"));
    }

    #[test]
    fn validator_accepts_wrap_and_seq_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len * 4;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        for _ in 0..20 {
            pool.append(payload.as_slice()).expect("append");
        }

        let header = pool.header_from_mmap().expect("header");
        crate::core::validate::validate_pool_state(header, &pool.mmap).expect("validate");
    }

    #[test]
    fn validator_rejects_invalid_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len * 4;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        for _ in 0..4 {
            pool.append(payload.as_slice()).expect("append");
        }

        let mut header = pool.header_from_mmap().expect("header");
        header.tail_off = (header.head_off + 8) % header.ring_size;
        super::write_pool_header(&mut pool.mmap, &header);

        let err =
            crate::core::validate::validate_pool_state(header, &pool.mmap).expect_err("invalid");
        assert_eq!(err.kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn validator_rejects_seq_discontinuity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len * 4;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        for _ in 0..3 {
            pool.append(payload.as_slice()).expect("append");
        }

        let header = pool.header_from_mmap().expect("header");
        let ring_offset = header.ring_offset as usize;
        let tail = header.tail_off as usize;
        let mut second_header =
            super::read_frame_header(&pool.mmap, ring_offset, tail + frame_len).expect("frame");
        second_header.seq = header.oldest_seq + 2;
        super::write_frame_header(
            &mut pool.mmap,
            ring_offset,
            tail + frame_len,
            &second_header,
        )
        .expect("write");

        let err =
            crate::core::validate::validate_pool_state(header, &pool.mmap).expect_err("invalid");
        assert_eq!(err.kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn append_uses_tail_only_validator() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len * 8;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        for _ in 0..3 {
            pool.append(payload.as_slice()).expect("append");
        }

        let header = pool.header_from_mmap().expect("header");
        let ring_offset = header.ring_offset as usize;
        let tail = header.tail_off as usize;
        let mut second_header =
            super::read_frame_header(&pool.mmap, ring_offset, tail + frame_len).expect("frame");
        second_header.seq = header.oldest_seq + 2;
        super::write_frame_header(
            &mut pool.mmap,
            ring_offset,
            tail + frame_len,
            &second_header,
        )
        .expect("write");

        let append_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.append(payload.as_slice())
        }));
        assert!(append_result.is_ok());
        assert!(append_result.unwrap().is_ok());
    }

    #[test]
    fn mismatched_file_size_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(&path)
            .expect("create");
        file.set_len(1024 * 1024).expect("len");
        file.seek(SeekFrom::Start(0)).expect("seek");

        let header = super::PoolHeader::new(512 * 1024, 0).expect("header");
        let buf = header.encode();
        file.write_all(&buf).expect("write");
        file.flush().expect("flush");

        let result = Pool::open(&path);
        match result {
            Ok(_) => panic!("expected mismatch error"),
            Err(err) => assert_eq!(err.kind(), ErrorKind::Corrupt),
        }
    }

    #[test]
    fn lock_errors_map_to_expected_kinds() {
        let err = std::io::Error::from_raw_os_error(libc::EAGAIN);
        assert_eq!(super::lock_error_kind(&err), ErrorKind::Busy);

        let err = std::io::Error::from_raw_os_error(libc::EWOULDBLOCK);
        assert_eq!(super::lock_error_kind(&err), ErrorKind::Busy);

        let err = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(super::lock_error_kind(&err), ErrorKind::Permission);

        let err = std::io::Error::from_raw_os_error(libc::EPERM);
        assert_eq!(super::lock_error_kind(&err), ErrorKind::Permission);

        let err = std::io::Error::from_raw_os_error(libc::EBADF);
        assert_eq!(super::lock_error_kind(&err), ErrorKind::Io);
    }

    fn collect_seqs(pool: &Pool) -> Vec<u64> {
        let header = pool.header();
        if header.oldest_seq == 0 {
            return Vec::new();
        }
        let ring_offset = header.ring_offset as usize;
        let ring_size = header.ring_size as usize;
        let mut offset = header.tail_off as usize;
        let mut seqs = Vec::new();

        loop {
            let frame = super::read_frame_header(&pool.mmap, ring_offset, offset).expect("frame");
            match frame.state {
                FrameState::Wrap => {
                    offset = 0;
                    continue;
                }
                FrameState::Committed => {
                    seqs.push(frame.seq);
                    let frame_len =
                        frame::frame_total_len(FRAME_HEADER_LEN, frame.payload_len as usize)
                            .expect("frame len");
                    offset += frame_len;
                    if offset == ring_size {
                        offset = 0;
                    }
                    if offset == header.head_off as usize {
                        break;
                    }
                }
                _ => panic!("unexpected frame state"),
            }
        }
        seqs
    }

    fn apply_model(storage: &mut [u8], plan: &plan::AppendPlan, payload_len: usize) {
        let ring_offset = plan.next_header.ring_offset as usize;
        if let Some(wrap_offset) = plan.wrap_offset {
            let wrap = FrameHeader::new(FrameState::Wrap, 0, 0, 0, 0, 0);
            write_frame_bytes(storage, ring_offset, wrap_offset, &wrap, 0);
        }
        let header = FrameHeader::new(FrameState::Committed, 0, plan.seq, 0, payload_len as u32, 0);
        write_frame_bytes(
            storage,
            ring_offset,
            plan.frame_offset,
            &header,
            payload_len,
        );
        super::write_index_slot(
            storage,
            plan.next_header.index_offset,
            plan.next_header.index_capacity,
            plan.seq,
            plan.frame_offset as u64,
        )
        .expect("write index");
        let encoded = plan.next_header.encode();
        storage[0..HEADER_SIZE].copy_from_slice(&encoded);
    }

    fn write_frame_bytes(
        storage: &mut [u8],
        ring_offset: usize,
        offset: usize,
        header: &FrameHeader,
        payload_len: usize,
    ) {
        let start = ring_offset + offset;
        let end = start + FRAME_HEADER_LEN;
        storage[start..end].copy_from_slice(&header.encode());
        let payload_start = end;
        let payload_end = payload_start + payload_len;
        storage[payload_start..payload_end].fill(0u8);
        let marker_start = payload_end;
        let marker_end = marker_start + frame::FRAME_COMMIT_MARKER_LEN;
        storage[marker_start..marker_end].copy_from_slice(&frame::FRAME_COMMIT_MARKER);
    }

    fn read_index_entry(mmap: &[u8], index_offset: usize, slot: usize) -> (u64, u64) {
        let start = index_offset + slot * 16;
        let mut seq_bytes = [0u8; 8];
        let mut off_bytes = [0u8; 8];
        seq_bytes.copy_from_slice(&mmap[start..start + 8]);
        off_bytes.copy_from_slice(&mmap[start + 8..start + 16]);
        (u64::from_le_bytes(seq_bytes), u64::from_le_bytes(off_bytes))
    }

    fn scan_frames(mmap: &[u8], header: PoolHeader) -> Vec<(usize, u64, u32)> {
        if header.oldest_seq == 0 {
            return Vec::new();
        }
        let ring_offset = header.ring_offset as usize;
        let ring_size = header.ring_size as usize;
        let mut offset = header.tail_off as usize;
        let mut expected_seq = header.oldest_seq;
        let mut frames = Vec::new();

        loop {
            if ring_size - offset < FRAME_HEADER_LEN {
                offset = 0;
                continue;
            }
            let start = ring_offset + offset;
            let end = start + FRAME_HEADER_LEN;
            let frame = FrameHeader::decode(&mmap[start..end]).expect("frame");
            match frame.state {
                FrameState::Wrap => {
                    offset = 0;
                    continue;
                }
                FrameState::Committed => {}
                _ => panic!("unexpected frame state"),
            }
            frames.push((offset, frame.seq, frame.payload_len));

            let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, frame.payload_len as usize)
                .expect("frame len");
            offset += frame_len;
            if offset == ring_size {
                offset = 0;
            }
            if expected_seq == header.newest_seq {
                break;
            }
            expected_seq += 1;
        }

        frames
    }

    #[test]
    fn append_wraps_at_ring_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("frame len");
        let ring_size = frame_len * 3 + FRAME_HEADER_LEN;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");
        pool.append(payload.as_slice()).expect("append 1");
        pool.append(payload.as_slice()).expect("append 2");
        pool.append(payload.as_slice()).expect("append 3");
        pool.append(payload.as_slice()).expect("append 4");

        let seqs = collect_seqs(&pool);
        assert_eq!(seqs, vec![2, 3, 4]);

        let wrap_offset = frame_len * 3;
        let frame =
            super::read_frame_header(&pool.mmap, pool.header().ring_offset as usize, wrap_offset)
                .expect("wrap frame");
        assert_eq!(frame.state, FrameState::Wrap);
    }

    #[test]
    fn append_drops_oldest_when_full() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 2})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("frame len");
        let ring_size = frame_len * 2 + FRAME_HEADER_LEN;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");
        pool.append(payload.as_slice()).expect("append 1");
        pool.append(payload.as_slice()).expect("append 2");
        pool.append(payload.as_slice()).expect("append 3");

        let seqs = collect_seqs(&pool);
        assert_eq!(seqs, vec![2, 3]);
        assert_eq!(pool.header().oldest_seq, 2);
        assert_eq!(pool.header().newest_seq, 3);
    }

    #[test]
    fn append_succeeds_when_notify_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 2048).with_index_capacity(0))
            .expect("create");

        crate::core::notify::force_unavailable_for_tests(true);
        let result = pool.append(payload.as_slice());
        crate::core::notify::force_unavailable_for_tests(false);

        assert!(result.is_ok());
    }

    #[test]
    fn model_apply_matches_plan_on_wrap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let ring_size = 512;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        let payload_a = vec![1u8; 100];
        pool.append(payload_a.as_slice()).expect("append 1");
        pool.append(payload_a.as_slice()).expect("append 2");

        let payload_b = vec![2u8; 200];
        let header = pool.header_from_mmap().expect("header");
        let plan = plan::plan_append(header, &pool.mmap, payload_b.len()).expect("plan");

        let mut model = pool.mmap.to_vec();
        apply_model(&mut model, &plan, payload_b.len());

        apply_append(
            &mut pool.mmap,
            header.ring_offset as usize,
            &plan,
            payload_b.as_slice(),
            0,
        )
        .expect("apply");

        let actual_header = super::PoolHeader::decode(&pool.mmap[0..HEADER_SIZE]).expect("header");
        let model_header = super::PoolHeader::decode(&model[0..HEADER_SIZE]).expect("header");
        assert_eq!(actual_header, plan.next_header);
        assert_eq!(model_header, plan.next_header);
        assert_eq!(
            scan_frames(&model, plan.next_header),
            scan_frames(&pool.mmap, plan.next_header)
        );
    }

    #[test]
    fn model_apply_matches_plan_on_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let ring_size = 512;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        let payload_a = vec![1u8; 100];
        for _ in 0..3 {
            pool.append(payload_a.as_slice()).expect("append");
        }

        let payload_b = vec![3u8; 120];
        let header = pool.header_from_mmap().expect("header");
        let plan = plan::plan_append(header, &pool.mmap, payload_b.len()).expect("plan");

        let mut model = pool.mmap.to_vec();
        apply_model(&mut model, &plan, payload_b.len());

        apply_append(
            &mut pool.mmap,
            header.ring_offset as usize,
            &plan,
            payload_b.as_slice(),
            0,
        )
        .expect("apply");

        let actual_header = super::PoolHeader::decode(&pool.mmap[0..HEADER_SIZE]).expect("header");
        let model_header = super::PoolHeader::decode(&model[0..HEADER_SIZE]).expect("header");
        assert_eq!(actual_header, plan.next_header);
        assert_eq!(model_header, plan.next_header);
        assert_eq!(
            scan_frames(&model, plan.next_header),
            scan_frames(&pool.mmap, plan.next_header)
        );
    }

    #[test]
    fn bounds_and_get_scan_by_seq() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 2048).with_index_capacity(0))
            .expect("create");

        for value in 1..=3 {
            let payload =
                lite3::encode_message(&[], &serde_json::json!({"x": value})).expect("payload");
            pool.append(payload.as_slice()).expect("append");
        }

        let bounds = pool.bounds().expect("bounds");
        assert_eq!(bounds.oldest_seq, Some(1));
        assert_eq!(bounds.newest_seq, Some(3));

        let frame = pool.get(2).expect("get");
        let doc = Lite3DocRef::new(frame.payload);
        let json = doc.to_json(false).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["data"]["x"], 2);

        let err = pool.get(4).expect_err("missing");
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn get_falls_back_when_index_slot_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024).with_index_capacity(2))
            .expect("create");

        for value in 1..=3 {
            let payload =
                lite3::encode_message(&[], &serde_json::json!({"x": value})).expect("payload");
            pool.append(payload.as_slice()).expect("append");
        }

        let frame = pool.get(1).expect("fallback get");
        let doc = Lite3DocRef::new(frame.payload);
        let json = doc.to_json(false).expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["data"]["x"], 1);
    }

    #[test]
    fn get_falls_back_when_index_slot_offset_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024).with_index_capacity(8))
            .expect("create");

        for value in 1..=3 {
            let payload =
                lite3::encode_message(&[], &serde_json::json!({"x": value})).expect("payload");
            pool.append(payload.as_slice()).expect("append");
        }

        let header = pool.header_from_mmap().expect("header");
        let slot = (2 % header.index_capacity as u64) as usize;
        let slot_start = header.index_offset as usize + slot * 16;
        pool.mmap[slot_start..slot_start + 8].copy_from_slice(&2u64.to_le_bytes());
        pool.mmap[slot_start + 8..slot_start + 16].copy_from_slice(&0u64.to_le_bytes());

        let frame = pool.get(2).expect("fallback get");
        assert_eq!(frame.seq, 2);
    }

    #[test]
    fn write_pool_header_partial_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 2048).with_index_capacity(0))
            .expect("create");
        let mut header = pool.header_from_mmap().expect("header");
        header.flags = 1;
        header.head_off = 128;
        header.tail_off = 64;
        header.oldest_seq = 10;
        header.newest_seq = 12;

        pool.mmap[0..HEADER_SIZE].fill(0);
        super::write_pool_header(&mut pool.mmap, &header);

        let decoded = super::PoolHeader::decode(&pool.mmap[0..HEADER_SIZE]).expect("decode");
        assert_eq!(decoded, header);
    }

    #[test]
    fn append_writes_index_slot_with_seq_and_offset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024).with_index_capacity(8))
            .expect("create");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");

        let seq = pool.append(payload.as_slice()).expect("append");
        let header = pool.header_from_mmap().expect("header");
        let slot = (seq % header.index_capacity as u64) as usize;
        let (stored_seq, stored_off) =
            read_index_entry(&pool.mmap, header.index_offset as usize, slot);
        assert_eq!(stored_seq, seq);

        let frame =
            super::read_frame_header(&pool.mmap, header.ring_offset as usize, stored_off as usize)
                .expect("frame");
        assert_eq!(frame.seq, seq);
        assert_eq!(frame.state, FrameState::Committed);
    }

    #[test]
    fn append_overwrites_index_slot_on_collision() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024).with_index_capacity(2))
            .expect("create");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");

        for _ in 0..3 {
            pool.append(payload.as_slice()).expect("append");
        }
        let header = pool.header_from_mmap().expect("header");
        let (slot0_seq, slot0_off) = read_index_entry(&pool.mmap, header.index_offset as usize, 0);
        let (slot1_seq, slot1_off) = read_index_entry(&pool.mmap, header.index_offset as usize, 1);
        assert_eq!(slot0_seq, 2);
        assert_eq!(slot1_seq, 3);

        let frame0 =
            super::read_frame_header(&pool.mmap, header.ring_offset as usize, slot0_off as usize)
                .expect("frame");
        let frame1 =
            super::read_frame_header(&pool.mmap, header.ring_offset as usize, slot1_off as usize)
                .expect("frame");
        assert_eq!(frame0.seq, slot0_seq);
        assert_eq!(frame1.seq, slot1_seq);
    }

    #[test]
    fn get_with_cache_hits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 2048).with_index_capacity(0))
            .expect("create");

        for value in 1..=3 {
            let payload =
                lite3::encode_message(&[], &serde_json::json!({"x": value})).expect("payload");
            pool.append(payload.as_slice()).expect("append");
        }

        let mut cache = SeqOffsetCache::new(8);
        let frame = pool.get_with_cache(2, &mut cache).expect("get");
        assert_eq!(frame.seq, 2);
        assert!(cache.get(2).is_some());

        let cached = pool.get_with_cache(2, &mut cache).expect("cached get");
        assert_eq!(cached.seq, 2);
    }

    #[test]
    fn get_with_cache_stale_entry_falls_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("frame len");
        let ring_size = frame_len * 2 + FRAME_HEADER_LEN;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");

        pool.append(payload.as_slice()).expect("append 1");
        pool.append(payload.as_slice()).expect("append 2");

        let mut cache = SeqOffsetCache::new(8);
        let frame = pool.get_with_cache(1, &mut cache).expect("get");
        assert_eq!(frame.seq, 1);

        pool.append(payload.as_slice()).expect("append 3");

        let err = pool.get_with_cache(1, &mut cache).expect_err("stale");
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn multi_writer_child() {
        let role = std::env::var("PLASMITE_TEST_ROLE").ok();
        let Some(role) = role else {
            return;
        };
        let path = std::env::var("PLASMITE_TEST_POOL").expect("pool path");
        match role.as_str() {
            "writer" => {
                let count: usize = std::env::var("PLASMITE_TEST_COUNT")
                    .expect("count")
                    .parse()
                    .expect("parse count");
                let mut pool = Pool::open(&path).expect("open");
                let payload =
                    lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
                for _ in 0..count {
                    pool.append(payload.as_slice()).expect("append");
                }
            }
            "reader" => {
                let loops: usize = std::env::var("PLASMITE_TEST_LOOPS")
                    .expect("loops")
                    .parse()
                    .expect("parse loops");
                let pool = Pool::open(&path).expect("open");
                let mut cursor = crate::core::cursor::Cursor::new();
                for _ in 0..loops {
                    match cursor.next(&pool) {
                        Ok(crate::core::cursor::CursorResult::WouldBlock) => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Ok(_) => {}
                        Err(err) => panic!("cursor error: {err}"),
                    }
                }
            }
            _ => panic!("unknown role"),
        }
    }

    #[test]
    fn multi_writer_stress() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        drop(pool);

        let exe = std::env::current_exe().expect("exe");
        let path_str = path.to_string_lossy().to_string();
        let mut children = Vec::new();

        for _ in 0..3 {
            let mut cmd = Command::new(&exe);
            cmd.arg("--exact")
                .arg("multi_writer_child")
                .arg("--nocapture")
                .env("PLASMITE_TEST_ROLE", "writer")
                .env("PLASMITE_TEST_POOL", &path_str)
                .env("PLASMITE_TEST_COUNT", "50");
            children.push(cmd.spawn().expect("spawn writer"));
        }

        let mut reader = Command::new(&exe);
        reader
            .arg("--exact")
            .arg("multi_writer_child")
            .arg("--nocapture")
            .env("PLASMITE_TEST_ROLE", "reader")
            .env("PLASMITE_TEST_POOL", &path_str)
            .env("PLASMITE_TEST_LOOPS", "200");
        children.push(reader.spawn().expect("spawn reader"));

        for mut child in children {
            let status = child.wait().expect("wait");
            assert!(status.success());
        }

        let pool = Pool::open(&path).expect("open");
        let header = pool.header_from_mmap().expect("header");
        crate::core::validate::validate_pool_state(header, &pool.mmap).expect("validate");
    }

    #[test]
    fn crash_append_phases_preserve_invariants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base_path = dir.path().join("base.plasmite");
        let mut base = Pool::create(
            &base_path,
            PoolOptions::new(4096 + 1024).with_index_capacity(0),
        )
        .expect("create");
        let payload_a = vec![7u8; 100];
        base.append(payload_a.as_slice()).expect("append 1");
        base.append(payload_a.as_slice()).expect("append 2");
        drop(base);

        let payload_b = vec![9u8; 240];
        let phases = [
            CrashPhase::Wrap,
            CrashPhase::Write,
            CrashPhase::Commit,
            CrashPhase::Header,
        ];

        for phase in phases {
            let path = dir.path().join(format!("phase-{phase:?}.plasmite"));
            fs::copy(&base_path, &path).expect("copy");
            let mut pool = Pool::open(&path).expect("open");
            let header = pool.header_from_mmap().expect("header");
            let plan = plan::plan_append(header, &pool.mmap, payload_b.len()).expect("plan");
            simulate_append_phase(&mut pool, &plan, payload_b.as_slice(), phase).expect("phase");
            drop(pool);

            let reopened = Pool::open(&path).expect("reopen");
            let header = reopened.header_from_mmap().expect("header");
            crate::core::validate::validate_pool_state(header, &reopened.mmap).expect("validate");
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum CrashPhase {
        Wrap,
        Write,
        Commit,
        Header,
    }

    fn simulate_append_phase(
        pool: &mut Pool,
        plan: &plan::AppendPlan,
        payload: &[u8],
        phase: CrashPhase,
    ) -> Result<(), Error> {
        let ring_offset = plan.next_header.ring_offset as usize;
        if let Some(wrap_offset) = plan.wrap_offset {
            super::write_wrap(&mut pool.mmap, ring_offset, wrap_offset)?;
            if matches!(phase, CrashPhase::Wrap) {
                return Ok(());
            }
        }

        let header = FrameHeader::new(FrameState::Writing, 0, plan.seq, 0, payload.len() as u32, 0);
        super::write_frame(
            &mut pool.mmap,
            ring_offset,
            plan.frame_offset,
            &header,
            payload,
        )?;
        if matches!(phase, CrashPhase::Write) {
            return Ok(());
        }

        let mut committed = header;
        committed.state = FrameState::Committed;
        super::write_frame_header(&mut pool.mmap, ring_offset, plan.frame_offset, &committed)?;
        if matches!(phase, CrashPhase::Commit) {
            return Ok(());
        }

        super::write_index_slot(
            &mut pool.mmap,
            plan.next_header.index_offset,
            plan.next_header.index_capacity,
            plan.seq,
            plan.frame_offset as u64,
        )?;

        super::write_pool_header(&mut pool.mmap, &plan.next_header);
        if matches!(phase, CrashPhase::Header) {
            return Ok(());
        }
        Ok(())
    }

    #[test]
    fn bounds_empty_pool_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0))
            .expect("create");

        let bounds = pool.bounds().expect("bounds");
        assert_eq!(bounds.oldest_seq, None);
        assert_eq!(bounds.newest_seq, None);
    }

    #[test]
    fn info_metrics_empty_pool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0))
            .expect("create");

        let info = pool.info().expect("info");
        let metrics = info.metrics.expect("metrics");
        assert_eq!(metrics.message_count, 0);
        assert_eq!(metrics.seq_span, 0);
        assert_eq!(metrics.utilization.used_bytes, 0);
        assert_eq!(metrics.utilization.free_bytes, info.ring_size);
        assert_eq!(metrics.age.oldest_time, None);
        assert_eq!(metrics.age.newest_time, None);
        assert_eq!(metrics.age.oldest_age_ms, None);
        assert_eq!(metrics.age.newest_age_ms, None);
    }

    #[test]
    fn info_metrics_non_empty_pool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 4096).with_index_capacity(0))
            .expect("create");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1})).expect("payload");
        pool.append(payload.as_slice()).expect("append 1");
        pool.append(payload.as_slice()).expect("append 2");

        let info = pool.info().expect("info");
        let metrics = info.metrics.expect("metrics");
        assert_eq!(metrics.message_count, 2);
        assert_eq!(metrics.seq_span, 2);
        assert!(metrics.utilization.used_bytes > 0);
        assert!(metrics.utilization.free_bytes < info.ring_size);
        assert!(metrics.age.oldest_time.is_some());
        assert!(metrics.age.newest_time.is_some());
        assert!(metrics.age.oldest_age_ms.is_some());
        assert!(metrics.age.newest_age_ms.is_some());
    }
}
