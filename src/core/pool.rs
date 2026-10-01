//! Purpose: Manage pool files (create/open), mmap access, locking, and append application.
//! Exports: `Pool`, `PoolOptions`, `AppendOptions`, `Durability`, `PoolHeader`, `Bounds`,
//! `PoolInfo`, `SeqOffsetCache`.
//! Role: IO boundary for the core: owns file handles/mmap and delegates planning to `plan`.
//! Invariants: All mutations hold an exclusive append lock across processes.
//! Invariants: Append writes mark frames `Writing` -> payload -> `Committed`; header persists last.
//! Invariants: Mapped lengths fit isize; validated headers keep offsets within that mapping.
//! Invariants: Header size is fixed (4096) and validated on open and refresh.
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
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
                .with_message("file_size must exceed header and index size")
                .with_hint(format!(
                    "Choose a file size greater than {ring_offset} bytes."
                )));
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

    pub(crate) fn validate(&self, actual_file_size: u64) -> Result<(), Error> {
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
        if self.ring_offset.checked_add(self.ring_size) != Some(self.file_size) {
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

// Rust slices and pointer offsets must fit the process's signed address space.
// Check before creating a file or mapping it, so all later u64 -> usize offsets fit.
fn validate_mapped_size(file_size: u64) -> Result<(), Error> {
    let maximum = isize::MAX as u64;
    if file_size > maximum {
        return Err(Error::new(ErrorKind::Usage)
            .with_message(format!(
                "pool size exceeds this process's {maximum}-byte mapping limit"
            ))
            .with_hint("Choose a smaller pool or use a 64-bit Plasmite build."));
    }
    Ok(())
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
    snapshot_lock: Mutex<()>,
}

impl Pool {
    pub fn create(path: impl AsRef<Path>, options: PoolOptions) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        validate_mapped_size(options.file_size).map_err(|err| err.with_path(&path))?;
        let index_capacity = options.resolved_index_capacity();
        let header = PoolHeader::new(options.file_size, index_capacity)?;

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
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| {
                let kind = map_io_error_kind(&err);
                let message = match kind {
                    ErrorKind::NotFound => "parent directory not found",
                    ErrorKind::AlreadyExists => "pool already exists",
                    ErrorKind::Permission => "failed to create pool file (permission denied)",
                    _ => "failed to create pool file",
                };
                let error = Error::new(kind)
                    .with_message(message)
                    .with_path(&path)
                    .with_source(err);
                if kind == ErrorKind::AlreadyExists {
                    error.with_hint("Choose a different name or delete the existing pool first.")
                } else {
                    error
                }
            })?;

        file.set_len(options.file_size).map_err(|err| {
            let kind = map_io_error_kind(&err);
            Error::new(kind)
                .with_message("failed to size pool file")
                .with_path(&path)
                .with_source(err)
        })?;

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
            snapshot_lock: Mutex::new(()),
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

        validate_mapped_size(actual_size).map_err(|err| err.with_path(&path))?;
        FileExt::lock_shared(&file).map_err(|err| {
            Error::new(lock_error_kind(&err))
                .with_path(&path)
                .with_source(err)
        })?;
        let snapshot = (|| {
            let header = read_header(&mut file, &path)?;
            header.validate(actual_size)?;
            let mmap = unsafe {
                MmapMut::map_mut(&file)
                    .map_err(|err| Error::new(ErrorKind::Io).with_path(&path).with_source(err))?
            };
            Ok::<_, Error>((header, mmap))
        })();
        let unlock = FileExt::unlock(&file).map_err(|err| {
            Error::new(lock_error_kind(&err))
                .with_path(&path)
                .with_source(err)
        });
        let (header, mmap) = snapshot?;
        unlock?;

        Ok(Self {
            path,
            file,
            mmap,
            header,
            snapshot_lock: Mutex::new(()),
        })
    }

    pub fn header(&self) -> PoolHeader {
        self.header
    }

    pub fn header_from_mmap(&self) -> Result<PoolHeader, Error> {
        let _lock = self.read_lock()?;
        self.header_locked()
    }

    pub(crate) fn header_locked(&self) -> Result<PoolHeader, Error> {
        let header = PoolHeader::decode(&self.mmap[0..HEADER_SIZE])?;
        header.validate(self.mmap.len() as u64)?;
        Ok(header)
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

    pub(crate) fn malformed_frame_error(&self, offset: usize) -> Error {
        Error::new(ErrorKind::Corrupt)
            .with_message("malformed frame in the retained ring")
            .with_path(&self.path)
            .with_offset(offset as u64)
            .with_hint("Run `plasmite doctor` to inspect the pool.")
    }

    pub(crate) fn read_lock(&self) -> Result<ReadLock<'_>, Error> {
        // flock belongs to the open file description. Serializing snapshots on
        // this handle prevents one thread from unlocking another thread's read.
        let snapshot = self.snapshot_lock.lock().map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("pool snapshot lock is unavailable")
        })?;
        FileExt::lock_shared(&self.file).map_err(|err| {
            Error::new(lock_error_kind(&err))
                .with_path(&self.path)
                .with_source(err)
        })?;
        Ok(ReadLock {
            file: &self.file,
            _snapshot: snapshot,
        })
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

    pub fn get(&self, seq: u64) -> Result<crate::core::cursor::FrameRef, Error> {
        let _lock = self.read_lock()?;
        let header = self.header_locked()?;
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

        self.get_by_scan(seq, header, None)
    }

    fn get_via_index(&self, header: PoolHeader, seq: u64) -> Option<crate::core::cursor::FrameRef> {
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
                Some(frame.into_owned())
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
    ) -> Result<crate::core::cursor::FrameRef, Error> {
        let _lock = self.read_lock()?;
        let header = self.header_locked()?;
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
                    return Ok(frame.into_owned());
                }
            }
            cache.remove(seq);
        }

        self.get_by_scan(seq, header, Some(cache))
    }

    // The caller holds the shared lock. Skipped payloads remain private borrowed
    // views; only the selected message is copied across the public boundary.
    fn get_by_scan(
        &self,
        seq: u64,
        header: PoolHeader,
        mut cache: Option<&mut SeqOffsetCache>,
    ) -> Result<crate::core::cursor::FrameRef, Error> {
        let ring_offset = header.ring_offset as usize;
        let ring_size = header.ring_size as usize;
        let mut offset = header.tail_off as usize;
        let mut last_seq = 0;
        loop {
            match crate::core::cursor::read_frame_at(self.mmap(), ring_offset, ring_size, offset)? {
                crate::core::cursor::ReadResult::Message { frame, next_off } => {
                    if frame.seq < header.oldest_seq
                        || frame.seq > header.newest_seq
                        || (last_seq != 0 && frame.seq <= last_seq)
                    {
                        return Err(self.malformed_frame_error(offset));
                    }
                    if let Some(cache) = cache.as_deref_mut() {
                        cache.insert(frame.seq, offset);
                    }
                    if frame.seq == seq {
                        return Ok(frame.into_owned());
                    }
                    if frame.seq > seq {
                        return Err(Error::new(ErrorKind::NotFound)
                            .with_message("message not found")
                            .with_seq(seq));
                    }
                    offset = next_off;
                    last_seq = frame.seq;
                }
                crate::core::cursor::ReadResult::Wrap => {
                    if offset == 0 {
                        return Err(self.malformed_frame_error(offset));
                    }
                    offset = 0;
                }
                crate::core::cursor::ReadResult::FellBehind => {
                    return Err(self.malformed_frame_error(offset));
                }
            }
        }
    }

    /// Hold an exclusive pool lock until the returned guard is dropped.
    ///
    /// This guard uses its own file description, so other locks cannot release
    /// it. Do not call a pool read or append while retaining the guard on the
    /// same thread: those operations wait for the exclusive guard to be dropped.
    pub fn append_lock(&self) -> Result<AppendLock, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|err| {
                Error::new(map_io_error_kind(&err))
                    .with_path(&self.path)
                    .with_source(err)
            })?;
        let same = same_file_identity(&self.file, &file).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_path(&self.path)
                .with_source(err)
        })?;
        if !same {
            return Err(Error::new(ErrorKind::NotFound)
                .with_message("pool path now refers to a different file")
                .with_path(&self.path)
                .with_hint("Open the pool again before acquiring an explicit append lock."));
        }
        lock_for_append(file, &self.path)
    }

    // Ordinary append has exclusive Rust access to Pool, so its original file
    // description cannot have a concurrent same-handle read. Public guards use
    // independent descriptions and therefore still exclude this lock.
    fn append_operation_lock(&self) -> Result<AppendLock, Error> {
        let file = self.file.try_clone().map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_path(&self.path)
                .with_source(err)
        })?;
        lock_for_append(file, &self.path)
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
        let lock = self.append_operation_lock()?;
        // Refresh header after acquiring the lock to avoid stale state across processes.
        self.header = self.header_locked()?;
        let seq = self.append_locked(payload, options)?;
        drop(lock);
        // Notification is optional wake-up work after committed storage publication.
        let _ = notify::post_for_path(&self.path);
        Ok(seq)
    }

    fn append_locked(&mut self, payload: &[u8], options: AppendOptions) -> Result<u64, Error> {
        self.append_locked_with_flush(payload, options, flush_mmap_range)
    }

    fn append_locked_with_flush<F>(
        &mut self,
        payload: &[u8],
        options: AppendOptions,
        mut flush_range: F,
    ) -> Result<u64, Error>
    where
        F: FnMut(&MmapMut, usize, usize, &Path, &'static str) -> Result<(), Error>,
    {
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
            flush_range(
                &self.mmap,
                frame_offset,
                plan.frame_len,
                &self.path,
                "failed to flush frame",
            )?;
            if let Some(wrap_head) = plan.wrap_offset {
                let wrap_start = ring_offset + wrap_head;
                flush_range(
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
                flush_range(
                    &self.mmap,
                    index_start,
                    index_len,
                    &self.path,
                    "failed to flush index slot",
                )?;
            }
            flush_range(
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

pub(crate) struct ReadLock<'a> {
    file: &'a File,
    _snapshot: MutexGuard<'a, ()>,
}

impl Drop for ReadLock<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(self.file);
    }
}

impl Drop for AppendLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_for_append(file: File, path: &Path) -> Result<AppendLock, Error> {
    file.lock_exclusive().map_err(|err| {
        Error::new(lock_error_kind(&err))
            .with_path(path)
            .with_source(err)
    })?;
    Ok(AppendLock { file })
}

#[cfg(unix)]
fn same_file_identity(first: &File, second: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let first = first.metadata()?;
    let second = second.metadata()?;
    Ok(first.dev() == second.dev() && first.ino() == second.ino())
}

#[cfg(windows)]
fn same_file_identity(first: &File, second: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    fn identity(file: &File) -> io::Result<(u32, u32, u32)> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }
    Ok(identity(first)? == identity(second)?)
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
        io::ErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
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
    use super::{
        AppendOptions, Durability, HEADER_SIZE, Pool, PoolHeader, PoolOptions, SeqOffsetCache,
        apply_append,
    };
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
    use std::time::{Duration, Instant};

    #[test]
    fn oversized_creation_has_no_filesystem_side_effects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parent = dir.path().join("not-created");
        let path = parent.join("pool.plasmite");
        let err = Pool::create(&path, PoolOptions::new(isize::MAX as u64 + 1))
            .err()
            .expect("oversized pool must fail");
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert_eq!(err.path(), Some(path.as_path()));
        assert!(err.message().expect("message").contains("mapping limit"));
        assert!(!parent.exists());
    }

    #[test]
    fn overflowing_ring_extent_returns_corrupt() {
        let mut header = PoolHeader::new(1024 * 1024, 64).expect("header");
        header.ring_size = u64::MAX;
        assert_eq!(
            header
                .validate(header.file_size)
                .expect_err("overflow")
                .kind(),
            ErrorKind::Corrupt
        );
    }

    #[test]
    fn header_refresh_rejects_invalid_extent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("pool");
        let mut header = pool.header();
        header.ring_size = u64::MAX;
        pool.mmap[..HEADER_SIZE].copy_from_slice(&header.encode());
        assert_eq!(
            pool.bounds().expect_err("invalid extent").kind(),
            ErrorKind::Corrupt
        );
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn oversized_open_preserves_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oversized.plasmite");
        let original = b"unchanged";
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("file");
        file.write_all(original).expect("sentinel");
        let size = isize::MAX as u64 + 1;
        file.set_len(size).expect("sparse size");
        drop(file);
        let err = Pool::open(&path).err().expect("oversized open must fail");
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert_eq!(fs::metadata(&path).expect("metadata").len(), size);
        let mut file = std::fs::File::open(&path).expect("read file");
        let mut bytes = [0u8; 9];
        std::io::Read::read_exact(&mut file, &mut bytes).expect("read sentinel");
        assert_eq!(&bytes, original);
    }

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
    fn create_existing_pool_returns_already_exists_without_truncating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool =
            Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create original pool");
        pool.append(b"preserve-me").expect("append");
        drop(pool);

        let err = match Pool::create(&path, PoolOptions::new(2 * 1024 * 1024)) {
            Ok(_) => panic!("existing pool must not be replaced"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(err.hint().is_some());

        let reopened = Pool::open(&path).expect("reopen original pool");
        assert_eq!(
            reopened.get(1).expect("original message").payload,
            b"preserve-me"
        );
        assert_eq!(reopened.header().file_size, 1024 * 1024);
    }

    #[test]
    fn invalid_layout_does_not_create_pool_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");

        let err = match Pool::create(&path, PoolOptions::new(1024)) {
            Ok(_) => panic!("invalid layout must fail"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(!path.exists());
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
        let doc = Lite3DocRef::new(&frame.payload);
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
        let doc = Lite3DocRef::new(&frame.payload);
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
    fn sequence_exhaustion_preserves_last_frame_and_storage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 2048).with_index_capacity(0))
            .expect("create");
        pool.append(b"before").expect("first append");
        let mut header = pool.header_from_mmap().expect("header");
        let start = header.ring_offset as usize + header.tail_off as usize;
        let mut frame =
            FrameHeader::decode(&pool.mmap[start..start + FRAME_HEADER_LEN]).expect("frame header");
        frame.seq = u64::MAX - 1;
        pool.mmap[start..start + FRAME_HEADER_LEN].copy_from_slice(&frame.encode());
        header.oldest_seq = u64::MAX - 1;
        header.newest_seq = u64::MAX - 1;
        super::write_pool_header(&mut pool.mmap, &header);

        assert_eq!(pool.append(b"last").expect("final sequence"), u64::MAX);
        assert_eq!(pool.get(u64::MAX).expect("last frame").payload, b"last");
        let before = pool.mmap.to_vec();
        let before_header = pool.header_from_mmap().expect("final header");
        let err = pool
            .append(b"cannot append")
            .expect_err("sequence exhausted");
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(
            err.message()
                .expect("message")
                .contains("sequence space exhausted")
        );
        assert_eq!(
            pool.header_from_mmap().expect("unchanged header"),
            before_header
        );
        assert_eq!(&pool.mmap[..], before.as_slice());
        assert_eq!(
            pool.get(u64::MAX).expect("retained last frame").payload,
            b"last"
        );
    }

    #[test]
    fn reads_cross_short_ring_padding_after_wrap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + 1024).with_index_capacity(0))
            .expect("create");
        // 64-byte header + 128-byte payload + 8-byte marker = 200 bytes.
        // Five frames leave 24 padding bytes, too short for a wrap marker.
        for _ in 0..6 {
            pool.append(&[b'x'; 128]).expect("append");
        }
        assert_eq!(pool.get(6).expect("scan across padding").seq, 6);
        let mut cache = SeqOffsetCache::new(8);
        assert_eq!(
            pool.get_with_cache(6, &mut cache).expect("cached scan").seq,
            6
        );
        let mut cursor = crate::core::cursor::Cursor::new();
        for seq in 2..=6 {
            let crate::core::cursor::CursorResult::Message(frame) =
                cursor.next(&pool).expect("cursor")
            else {
                panic!("expected retained message {seq}")
            };
            assert_eq!(frame.seq, seq);
        }
        assert_eq!(
            cursor.next(&pool).expect("at head"),
            crate::core::cursor::CursorResult::WouldBlock
        );
        assert_eq!(
            pool.ring_layout(8)
                .expect("layout")
                .frames
                .expect("frames")
                .len(),
            5
        );
    }

    #[test]
    fn notify_reader_child() {
        use crate::api::{PoolApiExt, TailOptions};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let Some(path) = std::env::var_os("PLASMITE_NOTIFY_READER_PATH") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let reader = Pool::open(&path).expect("independent reader");
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut options = TailOptions::new();
        options.poll_interval = Duration::from_millis(5);
        options.timeout = Some(Duration::from_secs(3));
        options.cancel = Some(Arc::clone(&cancelled));
        let mut tail = reader.tail(options);
        // The separate process registers its waiter before any append.
        fs::write(path.with_extension("ready"), b"ready").expect("ready");
        for expected in 1..=128 {
            let message = tail.next_message().expect("tail").expect("message");
            assert_eq!(message.seq, expected);
            assert_eq!(message.data["n"].as_u64(), Some(expected));
        }
        fs::write(path.with_extension("received"), b"received").expect("received");
        let cancel_path = path.with_extension("cancel");
        let cancel_flag = Arc::clone(&cancelled);
        let watcher = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !cancel_path.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(
                cancel_path.exists(),
                "parent must cancel the waiting reader"
            );
            cancel_flag.store(true, Ordering::Release);
        });
        assert!(tail.next_message().expect("cancelled wait").is_none());
        watcher.join().expect("cancel watcher");
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn independent_registered_tail_reads_publications_and_cancels() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notify.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("writer");
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["core::pool::tests::notify_reader_child", "--exact"])
            .env("PLASMITE_NOTIFY_READER_PATH", &path)
            .spawn()
            .expect("reader process");
        let wait_for = |suffix: &str| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !path.with_extension(suffix).exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            path.with_extension(suffix).exists()
        };
        if !wait_for("ready") {
            let _ = child.kill();
            panic!("reader did not register");
        }
        for seq in 1..=128 {
            let payload =
                lite3::encode_message(&[], &serde_json::json!({"n": seq})).expect("encode");
            assert_eq!(writer.append(payload.as_slice()).expect("publish"), seq);
        }
        if !wait_for("received") {
            let _ = child.kill();
            panic!("reader missed published messages");
        }
        fs::write(path.with_extension("cancel"), b"cancel").expect("cancel");
        assert!(child.wait().expect("reader exit").success());
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
    fn get_with_cache_ignores_out_of_bounds_caller_offset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(64 * 1024)).expect("create");
        pool.append(b"original").expect("append");
        let mut cache = SeqOffsetCache::new(4);
        cache.insert(1, usize::MAX);
        let frame = pool.get_with_cache(1, &mut cache).expect("fallback");
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.payload, b"original");
    }

    #[test]
    fn malformed_retained_frames_fail_reads_without_retrying_forever() {
        use crate::core::cursor::Cursor;

        for damage in [
            "magic",
            "payload-length",
            "commit-marker",
            "wrap-at-zero",
            "writing",
            "empty",
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("pool.plasmite");
            let mut pool = Pool::create(&path, PoolOptions::new(64 * 1024).with_index_capacity(0))
                .expect("create");
            let payload = b"original".to_vec();
            pool.append(&payload).expect("append");
            let start = pool.header.ring_offset as usize;
            match damage {
                "magic" => pool.mmap[start..start + 4].copy_from_slice(b"NOPE"),
                "payload-length" => pool.mmap[start + 36] ^= 1,
                "commit-marker" => pool.mmap[start + FRAME_HEADER_LEN + payload.len()] ^= 1,
                "wrap-at-zero" => {
                    let wrap = FrameHeader::new(FrameState::Wrap, 0, 0, 0, 0, 0);
                    pool.mmap[start..start + FRAME_HEADER_LEN].copy_from_slice(&wrap.encode());
                }
                "writing" => pool.mmap[start + 4] = FrameState::Writing as u8,
                "empty" => pool.mmap[start + 4] = FrameState::Empty as u8,
                _ => unreachable!(),
            }
            let mut cursor = Cursor::new();
            assert_eq!(
                cursor.next(&pool).expect_err(damage).kind(),
                ErrorKind::Corrupt
            );
            assert_eq!(pool.get(1).expect_err(damage).kind(), ErrorKind::Corrupt);
            let mut cache = SeqOffsetCache::new(4);
            assert_eq!(
                pool.get_with_cache(1, &mut cache).expect_err(damage).kind(),
                ErrorKind::Corrupt
            );
        }
    }

    #[test]
    fn unpublished_frame_at_head_is_not_visible() {
        use crate::core::cursor::{Cursor, CursorResult};
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(64 * 1024).with_index_capacity(0))
            .expect("create");
        pool.append(b"first").expect("append");
        let start = (pool.header.ring_offset + pool.header.head_off) as usize;
        let unpublished = FrameHeader::new(FrameState::Writing, 0, 2, 2, 0, 0);
        pool.mmap[start..start + FRAME_HEADER_LEN].copy_from_slice(&unpublished.encode());
        let mut cursor = Cursor::new();
        assert!(
            matches!(cursor.next(&pool).expect("retained"), CursorResult::Message(frame) if frame.seq == 1)
        );
        assert_eq!(cursor.next(&pool).expect("head"), CursorResult::WouldBlock);
        assert_eq!(
            pool.get(2).expect_err("unpublished get").kind(),
            ErrorKind::NotFound
        );
        assert_eq!(
            pool.get_with_cache(2, &mut SeqOffsetCache::new(4))
                .expect_err("unpublished cached get")
                .kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn public_append_guard_is_not_downgraded_by_same_pool_reads() {
        use std::sync::{Arc, mpsc};
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(64 * 1024)).expect("create");
        pool.append(b"first").expect("append");
        let mut writer = Pool::open(&path).expect("writer");
        let pool = Arc::new(pool);
        let guard = pool.append_lock().expect("explicit guard");
        let (read_tx, read_rx) = mpsc::channel();
        let reader = Arc::clone(&pool);
        let read_thread = thread::spawn(move || {
            read_tx.send(reader.get(1)).expect("send read");
        });
        let (write_tx, write_rx) = mpsc::channel();
        let write_thread = thread::spawn(move || {
            write_tx
                .send(writer.append(b"second"))
                .expect("send append");
        });
        assert!(matches!(
            read_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(matches!(
            write_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(guard);
        assert_eq!(
            read_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("read completed")
                .expect("read")
                .payload,
            b"first"
        );
        assert_eq!(
            write_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("append completed")
                .expect("append"),
            2
        );
        read_thread.join().expect("reader thread");
        write_thread.join().expect("writer thread");
    }

    #[test]
    fn open_waits_for_header_publication() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(64 * 1024)).expect("create");
        pool.append(b"first").expect("append");
        let guard = pool.append_lock().expect("guard");
        pool.mmap[0..4].copy_from_slice(b"NOPE");
        let (tx, rx) = mpsc::channel();
        let opener = thread::spawn(move || tx.send(Pool::open(&path)).expect("send open"));
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        pool.mmap[0..HEADER_SIZE].copy_from_slice(&pool.header.encode());
        drop(guard);
        let opened = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("open completed")
            .expect("open");
        assert_eq!(opened.get(1).expect("get").payload, b"first");
        opener.join().expect("opener");
    }

    #[test]
    fn public_append_guard_rejects_a_replaced_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(64 * 1024)).expect("create");
        std::fs::rename(&path, dir.path().join("old.plasmite")).expect("rename");
        let _replacement = Pool::create(&path, PoolOptions::new(64 * 1024)).expect("replacement");
        assert!(matches!(pool.append_lock(), Err(error) if error.kind() == ErrorKind::NotFound));
    }

    #[test]
    fn frame_snapshot_waits_for_writer_to_publish() {
        use crate::core::cursor::{Cursor, CursorResult};
        use std::sync::mpsc;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = b"original".to_vec();
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let mut writer = Pool::create(
            &path,
            PoolOptions::new(HEADER_SIZE as u64 + frame_len as u64).with_index_capacity(0),
        )
        .expect("create");
        writer.append(&payload).expect("append first");
        let reader = Pool::open(&path).expect("open reader");
        let (damaged_tx, damaged_rx) = mpsc::channel();
        let (publish_tx, publish_rx) = mpsc::channel();
        let writer_thread = thread::spawn(move || {
            let _lock = writer.append_lock().expect("lock");
            let plan = plan::plan_append(writer.header, &writer.mmap, payload.len()).expect("plan");
            let start = writer.header.ring_offset as usize;
            writer.mmap[start..start + 4].copy_from_slice(b"NOPE");
            damaged_tx.send(()).expect("signal damage");
            publish_rx.recv().expect("wait to publish");
            apply_append(&mut writer.mmap, start, &plan, &payload, 2).expect("publish");
        });
        damaged_rx.recv().expect("wait for damage");
        let (result_tx, result_rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut cursor = Cursor::new();
            let first = cursor
                .next(&reader)
                .map(|result| matches!(result, CursorResult::Message(frame) if frame.seq == 2 && frame.payload == b"original"));
            result_tx.send(first).expect("signal recovery");
        });
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        publish_tx.send(()).expect("publish overwrite");
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("recovery result")
                .expect("read")
        );
        writer_thread.join().expect("writer thread");
        reader_thread.join().expect("reader thread");
    }

    #[test]
    fn returned_frames_keep_their_payload_after_overwrite_and_pool_drop() {
        use crate::core::cursor::{Cursor, CursorResult};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut writer = Pool::create(
            &path,
            PoolOptions::new(HEADER_SIZE as u64 + 80).with_index_capacity(0),
        )
        .expect("create");
        writer.append(b"original").expect("append first");
        let reader = Pool::open(&path).expect("open reader");
        let frame = reader.get(1).expect("get");
        let cached = reader
            .get_with_cache(1, &mut SeqOffsetCache::new(4))
            .expect("cached get");
        let cursor_frame = match Cursor::new().next(&reader).expect("cursor") {
            CursorResult::Message(frame) => frame,
            other => panic!("expected first frame, got {other:?}"),
        };
        writer
            .append(b"replaced")
            .expect("overwrite while snapshots exist");
        drop(reader);
        drop(writer);
        for snapshot in [frame, cached, cursor_frame] {
            assert_eq!(snapshot.seq, 1);
            assert_eq!(snapshot.payload, b"original");
        }
    }

    #[test]
    fn corrupt_middle_frame_and_repeated_sequences_terminate_reads() {
        use crate::core::cursor::{Cursor, CursorResult};

        for damage in ["magic", "sequence"] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("pool.plasmite");
            let mut pool = Pool::create(&path, PoolOptions::new(64 * 1024).with_index_capacity(0))
                .expect("create");
            pool.append(b"original").expect("first");
            pool.append(b"original").expect("second");
            let mut cursor = Cursor::new();
            assert!(matches!(
                cursor.next(&pool).expect("first frame"),
                CursorResult::Message(_)
            ));
            let second = pool.header.ring_offset as usize + 80;
            match damage {
                "magic" => pool.mmap[second..second + 4].copy_from_slice(b"NOPE"),
                "sequence" => {
                    pool.mmap[second + 16..second + 24].copy_from_slice(&1u64.to_le_bytes())
                }
                _ => unreachable!(),
            }
            assert_eq!(
                cursor.next(&pool).expect_err(damage).kind(),
                ErrorKind::Corrupt
            );
            assert_eq!(pool.get(2).expect_err(damage).kind(), ErrorKind::Corrupt);
            assert_eq!(
                pool.get_with_cache(2, &mut SeqOffsetCache::new(4))
                    .expect_err(damage)
                    .kind(),
                ErrorKind::Corrupt
            );
        }
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
            "snapshot-writer" => {
                let mut pool = Pool::open(&path).expect("open");
                for seq in 2u64..=1001 {
                    let written = pool
                        .append_with_options(
                            &seq.to_le_bytes(),
                            AppendOptions::new(seq, Durability::Fast),
                        )
                        .expect("append snapshot fixture");
                    assert_eq!(written, seq);
                }
            }
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
            "flush-ack" => {
                let ack_path = std::env::var("PLASMITE_TEST_ACK").expect("ack path");
                let mut pool = Pool::open(&path).expect("open");
                let seq = pool
                    .append_with_options(b"flush-ack", AppendOptions::new(1, Durability::Flush))
                    .expect("flush append");
                // Write then rename, so the parent sees no file or the whole number,
                // never a file created but not yet written.
                let staged = format!("{ack_path}.tmp");
                fs::write(&staged, seq.to_string()).expect("stage acknowledgement");
                fs::rename(&staged, &ack_path).expect("publish acknowledgement");
                // Wait to be killed, but exit on our own if the parent failed first and
                // will never kill us. The parent gives up after five seconds.
                thread::sleep(Duration::from_secs(60));
                std::process::exit(0);
            }
            "crash-phase" => {
                let phase = std::env::var("PLASMITE_TEST_PHASE").expect("crash phase");
                let mut pool = Pool::open(&path).expect("open");
                let payload = vec![9u8; 240];
                let header = pool.header_from_mmap().expect("header");
                let plan = plan::plan_append(header, &pool.mmap, payload.len()).expect("plan");
                simulate_append_phase(&mut pool, &plan, payload.as_slice(), &phase)
                    .expect("apply crash phase");
                pool.mmap.flush().expect("flush crash phase");
                std::process::exit(91);
            }
            _ => panic!("unknown role"),
        }
    }

    #[test]
    fn owned_snapshots_remain_coherent_with_an_independent_writer_process() {
        use crate::core::cursor::{Cursor, CursorResult};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut initial = Pool::create(
            &path,
            PoolOptions::new(HEADER_SIZE as u64 + 80).with_index_capacity(0),
        )
        .expect("create");
        initial
            .append_with_options(&1u64.to_le_bytes(), AppendOptions::new(1, Durability::Fast))
            .expect("first");
        drop(initial);
        let reader = Pool::open(&path).expect("open reader");
        let retained_snapshot = reader.get(1).expect("snapshot before overwrite");
        let mut child = Command::new(std::env::current_exe().expect("exe"))
            .args([
                "--exact",
                "core::pool::tests::multi_writer_child",
                "--nocapture",
            ])
            .env("PLASMITE_TEST_ROLE", "snapshot-writer")
            .env("PLASMITE_TEST_POOL", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn writer");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut cursor = Cursor::new();
        loop {
            if let Some(status) = child.try_wait().expect("writer status") {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("snapshot writer did not finish");
            }
            match cursor
                .next(&reader)
                .expect("coherent snapshot while writing")
            {
                CursorResult::Message(frame) => {
                    let payload_seq =
                        u64::from_le_bytes(frame.payload.as_slice().try_into().expect("payload"));
                    assert_eq!(frame.seq, payload_seq);
                    assert_eq!(frame.timestamp_ns, payload_seq);
                }
                CursorResult::WouldBlock => thread::yield_now(),
                CursorResult::FellBehind => {}
            }
        }
        let latest = reader.get(1001).expect("final snapshot");
        assert_eq!(latest.payload, 1001u64.to_le_bytes());
        assert_eq!(retained_snapshot.seq, 1);
        assert_eq!(retained_snapshot.payload, 1u64.to_le_bytes());
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
                .arg("core::pool::tests::multi_writer_child")
                .arg("--nocapture")
                .env("PLASMITE_TEST_ROLE", "writer")
                .env("PLASMITE_TEST_POOL", &path_str)
                .env("PLASMITE_TEST_COUNT", "50");
            children.push(cmd.spawn().expect("spawn writer"));
        }

        let mut reader = Command::new(&exe);
        reader
            .arg("--exact")
            .arg("core::pool::tests::multi_writer_child")
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

        let phases = ["write", "commit", "header"];

        for phase in phases {
            let path = dir.path().join(format!("phase-{phase}.plasmite"));
            fs::copy(&base_path, &path).expect("copy");
            let status = Command::new(std::env::current_exe().expect("test executable"))
                .arg("--exact")
                .arg("core::pool::tests::multi_writer_child")
                .arg("--nocapture")
                .env("PLASMITE_TEST_ROLE", "crash-phase")
                .env("PLASMITE_TEST_POOL", &path)
                .env("PLASMITE_TEST_PHASE", phase)
                .status()
                .expect("run crash child");
            assert_eq!(status.code(), Some(91));

            let reopened = Pool::open(&path).expect("reopen");
            let header = reopened.header_from_mmap().expect("header");
            crate::core::validate::validate_pool_state(header, &reopened.mmap).expect("validate");
            assert_eq!(header.newest_seq, if phase == "header" { 3 } else { 2 });
        }
    }

    fn simulate_append_phase(
        pool: &mut Pool,
        plan: &plan::AppendPlan,
        payload: &[u8],
        phase: &str,
    ) -> Result<(), Error> {
        let ring_offset = plan.next_header.ring_offset as usize;
        if let Some(wrap_offset) = plan.wrap_offset {
            super::write_wrap(&mut pool.mmap, ring_offset, wrap_offset)?;
        }

        let header = FrameHeader::new(FrameState::Writing, 0, plan.seq, 0, payload.len() as u32, 0);
        super::write_frame(
            &mut pool.mmap,
            ring_offset,
            plan.frame_offset,
            &header,
            payload,
        )?;
        if phase == "write" {
            return Ok(());
        }

        let mut committed = header;
        committed.state = FrameState::Committed;
        super::write_frame_header(&mut pool.mmap, ring_offset, plan.frame_offset, &committed)?;
        if phase == "commit" {
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
        if phase == "header" {
            return Ok(());
        }
        Err(Error::new(ErrorKind::Usage).with_message("unknown crash phase"))
    }

    #[test]
    fn flush_acknowledgement_survives_forced_child_termination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let ack_path = dir.path().join("ack");
        drop(Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create"));

        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("core::pool::tests::multi_writer_child")
            .arg("--nocapture")
            .env("PLASMITE_TEST_ROLE", "flush-ack")
            .env("PLASMITE_TEST_POOL", &path)
            .env("PLASMITE_TEST_ACK", &ack_path)
            .spawn()
            .expect("spawn flush child");

        let deadline = Instant::now() + Duration::from_secs(5);
        let acknowledged_seq = loop {
            match fs::read_to_string(&ack_path) {
                Ok(raw) => break raw.parse::<u64>().expect("ack sequence"),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    assert!(Instant::now() < deadline, "flush acknowledgement timed out");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("read acknowledgement: {err}"),
            }
        };
        child.kill().expect("force terminate child");
        child.wait().expect("reap child");

        let reopened = Pool::open(&path).expect("reopen");
        assert_eq!(
            reopened
                .get(acknowledged_seq)
                .expect("acknowledged frame")
                .payload,
            b"flush-ack"
        );
    }

    #[test]
    fn flush_failure_is_returned_without_false_acknowledgement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");

        let err = pool
            .append_locked_with_flush(
                b"not-acknowledged",
                AppendOptions::new(1, Durability::Flush),
                |_mmap, _offset, _len, path, _message| {
                    Err(Error::new(ErrorKind::Io)
                        .with_message("injected flush failure")
                        .with_path(path))
                },
            )
            .expect_err("flush failure must be returned");
        assert_eq!(err.kind(), ErrorKind::Io);
        assert!(err.to_string().contains("injected flush failure"));
        drop(pool);

        let reopened = Pool::open(&path).expect("pool remains reopenable");
        let header = reopened.header_from_mmap().expect("header");
        crate::core::validate::validate_pool_state(header, &reopened.mmap).expect("valid state");
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
