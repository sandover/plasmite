// Pool file creation/opening with header validation, mmap, and append locking.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use memmap2::MmapMut;
use libc::{EACCES, EPERM};

use crate::core::error::{Error, ErrorKind};
use crate::core::frame::{self, FrameHeader, FrameState, FRAME_HEADER_LEN};

const MAGIC: [u8; 4] = *b"PLSM";
const VERSION: u32 = 1;
const ENDIANNESS_LE: u8 = 1;
const HEADER_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolHeader {
    pub file_size: u64,
    pub ring_offset: u64,
    pub ring_size: u64,
    pub flags: u64,
    pub head_off: u64,
    pub tail_off: u64,
    pub oldest_seq: u64,
    pub newest_seq: u64,
}

impl PoolHeader {
    fn new(file_size: u64) -> Result<Self, Error> {
        let ring_offset = HEADER_SIZE as u64;
        if file_size <= ring_offset {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("file_size must exceed header size"));
        }
        let ring_size = file_size - ring_offset;
        Ok(Self {
            file_size,
            ring_offset,
            ring_size,
            flags: 0,
            head_off: 0,
            tail_off: 0,
            oldest_seq: 0,
            newest_seq: 0,
        })
    }

    fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4..8].copy_from_slice(&VERSION.to_le_bytes());
        buf[8] = ENDIANNESS_LE;

        write_u64(&mut buf, 16, self.file_size);
        write_u64(&mut buf, 24, self.ring_offset);
        write_u64(&mut buf, 32, self.ring_size);
        write_u64(&mut buf, 40, self.flags);
        write_u64(&mut buf, 48, self.head_off);
        write_u64(&mut buf, 56, self.tail_off);
        write_u64(&mut buf, 64, self.oldest_seq);
        write_u64(&mut buf, 72, self.newest_seq);

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
        if version != VERSION {
            return Err(Error::new(ErrorKind::Corrupt).with_message("unsupported version"));
        }
        if buf[8] != ENDIANNESS_LE {
            return Err(Error::new(ErrorKind::Corrupt).with_message("unsupported endianness"));
        }

        let file_size = read_u64(buf, 16);
        let ring_offset = read_u64(buf, 24);
        let ring_size = read_u64(buf, 32);
        let flags = read_u64(buf, 40);
        let head_off = read_u64(buf, 48);
        let tail_off = read_u64(buf, 56);
        let oldest_seq = read_u64(buf, 64);
        let newest_seq = read_u64(buf, 72);

        Ok(Self {
            file_size,
            ring_offset,
            ring_size,
            flags,
            head_off,
            tail_off,
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
        if self.ring_offset < HEADER_SIZE as u64 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("invalid ring offset"));
        }
        if self.ring_offset + self.ring_size != self.file_size {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring bounds mismatch"));
        }
        if self.ring_size == 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring size is zero"));
        }
        if (self.oldest_seq == 0) != (self.newest_seq == 0) {
            return Err(Error::new(ErrorKind::Corrupt).with_message("invalid seq bounds"));
        }
        if self.oldest_seq > self.newest_seq && self.oldest_seq != 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("seq bounds inverted"));
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

fn read_8(buf: &[u8], offset: usize) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&buf[offset..offset + 8]);
    out
}

fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Copy, Debug)]
pub struct PoolOptions {
    pub file_size: u64,
}

impl PoolOptions {
    pub fn new(file_size: u64) -> Self {
        Self { file_size }
    }
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
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| Error::new(ErrorKind::Io).with_path(&path).with_source(err))?;

        file.set_len(options.file_size)
            .map_err(|err| Error::new(ErrorKind::Io).with_path(&path).with_source(err))?;

        let header = PoolHeader::new(options.file_size)?;
        write_header(&mut file, &header, &path)?;

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

    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|err| Error::new(ErrorKind::Io).with_path(&path).with_source(err))?;

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

    pub fn mmap_len(&self) -> usize {
        self.mmap.len()
    }

    pub fn append_lock(&self) -> Result<AppendLock, Error> {
        let file = self
            .file
            .try_clone()
            .map_err(|err| Error::new(ErrorKind::Io).with_path(&self.path).with_source(err))?;
        file.lock_exclusive()
            .map_err(|err| {
                Error::new(lock_error_kind(&err))
                    .with_path(&self.path)
                    .with_source(err)
            })?;
        Ok(AppendLock { file })
    }

    pub fn append(&mut self, payload: &[u8]) -> Result<u64, Error> {
        let _lock = self.append_lock()?;
        self.append_locked(payload)
    }

    fn append_locked(&mut self, payload: &[u8]) -> Result<u64, Error> {
        if payload.len() > u32::MAX as usize {
            return Err(Error::new(ErrorKind::Usage).with_message("payload too large"));
        }

        let ring_size = self.header.ring_size as usize;
        let max_payload = frame::max_payload(ring_size, FRAME_HEADER_LEN);
        if payload.len() > max_payload {
            return Err(Error::new(ErrorKind::Usage).with_message("payload exceeds ring capacity"));
        }

        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len())
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("frame length overflow"))?;

        if frame_len > ring_size {
            return Err(Error::new(ErrorKind::Usage).with_message("frame larger than ring"));
        }

        let mut head = self.header.head_off as usize;
        let mut tail = self.header.tail_off as usize;
        let mut oldest_seq = self.header.oldest_seq;
        let mut newest_seq = self.header.newest_seq;

        let mut required = required_space(head, frame_len, ring_size);
        while free_space(head, tail, ring_size, oldest_seq) < required {
            let dropped = drop_oldest(
                &mut self.mmap,
                self.header.ring_offset as usize,
                ring_size,
                &mut tail,
                &mut oldest_seq,
            )?;
            if !dropped {
                return Err(Error::new(ErrorKind::Busy).with_message("unable to make space"));
            }
            if oldest_seq == 0 {
                newest_seq = 0;
            }
            required = required_space(head, frame_len, ring_size);
        }

        let remaining = ring_size - head;
        if remaining < frame_len {
            if remaining >= FRAME_HEADER_LEN {
                write_wrap(&mut self.mmap, self.header.ring_offset as usize, head)?;
            }
            head = 0;
        }

        let seq = if newest_seq == 0 { 1 } else { newest_seq + 1 };
        if oldest_seq == 0 {
            oldest_seq = seq;
        }
        newest_seq = seq;

        let header = FrameHeader::new(
            FrameState::Writing,
            0,
            seq,
            0,
            payload.len() as u32,
            0,
        );
        write_frame(&mut self.mmap, self.header.ring_offset as usize, head, &header, payload)?;

        let mut committed = header;
        committed.state = FrameState::Committed;
        write_frame_header(&mut self.mmap, self.header.ring_offset as usize, head, &committed)?;

        let mut new_head = head + frame_len;
        if new_head == ring_size {
            new_head = 0;
        }

        self.header.head_off = new_head as u64;
        self.header.tail_off = tail as u64;
        self.header.oldest_seq = oldest_seq;
        self.header.newest_seq = newest_seq;
        write_pool_header(&mut self.mmap, &self.header);

        Ok(seq)
    }
}

pub struct AppendLock {
    file: File,
}

impl Drop for AppendLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
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

fn read_header(file: &mut File, path: &Path) -> Result<PoolHeader, Error> {
    let mut buf = [0u8; HEADER_SIZE];
    file.seek(SeekFrom::Start(0))
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    file.read_exact(&mut buf)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
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
    let buf = header.encode();
    mmap[0..HEADER_SIZE].copy_from_slice(&buf);
}

fn read_frame_header(mmap: &MmapMut, ring_offset: usize, head: usize) -> Result<FrameHeader, Error> {
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
    Ok(())
}

fn write_wrap(mmap: &mut MmapMut, ring_offset: usize, head: usize) -> Result<(), Error> {
    let header = FrameHeader::new(FrameState::Wrap, 0, 0, 0, 0, 0);
    write_frame_header(mmap, ring_offset, head, &header)
}

fn free_space(head: usize, tail: usize, ring_size: usize, oldest_seq: u64) -> usize {
    let used = used_space(head, tail, ring_size, oldest_seq);
    ring_size.saturating_sub(used)
}

fn used_space(head: usize, tail: usize, ring_size: usize, oldest_seq: u64) -> usize {
    if head == tail {
        return if oldest_seq == 0 { 0 } else { ring_size };
    }
    if head > tail {
        head - tail
    } else {
        ring_size - (tail - head)
    }
}

fn required_space(head: usize, frame_len: usize, ring_size: usize) -> usize {
    let remaining = ring_size - head;
    if remaining >= frame_len {
        frame_len
    } else if remaining >= FRAME_HEADER_LEN {
        frame_len + FRAME_HEADER_LEN
    } else {
        frame_len + remaining
    }
}

fn drop_oldest(
    mmap: &mut MmapMut,
    ring_offset: usize,
    ring_size: usize,
    tail: &mut usize,
    oldest_seq: &mut u64,
) -> Result<bool, Error> {
    if *oldest_seq == 0 {
        return Ok(false);
    }
    let header = read_frame_header(mmap, ring_offset, *tail)?;
    header.validate(ring_size)?;

    match header.state {
        FrameState::Wrap => {
            *tail = 0;
            Ok(true)
        }
        FrameState::Committed => {
            let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, header.payload_len as usize)
                .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("frame length overflow"))?;
            *tail += frame_len;
            if *tail == ring_size {
                *tail = 0;
            }
            *oldest_seq = header.seq + 1;
            Ok(true)
        }
        _ => Err(Error::new(ErrorKind::Corrupt).with_message("invalid tail frame state")),
    }
}

#[cfg(test)]
mod tests {
    use super::{Pool, PoolOptions};
    use crate::core::error::ErrorKind;
    use crate::core::frame::{self, FrameState, FRAME_HEADER_LEN};
    use crate::core::lite3;
    use std::fs::OpenOptions;
    use std::io::{Seek, SeekFrom, Write};

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
    fn corrupt_header_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut file = OpenOptions::new()
            .create(true)
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
    fn mismatched_file_size_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&path)
            .expect("create");
        file.set_len(1024 * 1024).expect("len");
        file.seek(SeekFrom::Start(0)).expect("seek");

        let header = super::PoolHeader::new(512 * 1024).expect("header");
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
                    let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, frame.payload_len as usize)
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

    #[test]
    fn append_wraps_at_ring_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 1}))
            .expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len())
            .expect("frame len");
        let ring_size = frame_len * 3 + FRAME_HEADER_LEN;
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + ring_size as u64))
            .expect("create");
        pool.append(payload.as_slice()).expect("append 1");
        pool.append(payload.as_slice()).expect("append 2");
        pool.append(payload.as_slice()).expect("append 3");
        pool.append(payload.as_slice()).expect("append 4");

        let seqs = collect_seqs(&pool);
        assert_eq!(seqs, vec![2, 3, 4]);

        let wrap_offset = frame_len * 3;
        let frame = super::read_frame_header(
            &pool.mmap,
            pool.header().ring_offset as usize,
            wrap_offset,
        )
        .expect("wrap frame");
        assert_eq!(frame.state, FrameState::Wrap);
    }

    #[test]
    fn append_drops_oldest_when_full() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &serde_json::json!({"x": 2}))
            .expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len())
            .expect("frame len");
        let ring_size = frame_len * 2 + FRAME_HEADER_LEN;
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + ring_size as u64))
            .expect("create");
        pool.append(payload.as_slice()).expect("append 1");
        pool.append(payload.as_slice()).expect("append 2");
        pool.append(payload.as_slice()).expect("append 3");

        let seqs = collect_seqs(&pool);
        assert_eq!(seqs, vec![2, 3]);
        assert_eq!(pool.header().oldest_seq, 2);
        assert_eq!(pool.header().newest_seq, 3);
    }
}
