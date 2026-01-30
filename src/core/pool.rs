// Pool file creation/opening with header validation, mmap, and append locking.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use memmap2::MmapMut;
use libc::{EACCES, EPERM};

use crate::core::error::{Error, ErrorKind};

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

        Ok(Self {
            file_size,
            ring_offset,
            ring_size,
            flags,
            head_off,
            tail_off,
        })
    }

    fn validate(&self, actual_file_size: u64) -> Result<(), Error> {
        if self.file_size == 0 || self.file_size > actual_file_size {
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

    pub fn append_lock(&self) -> Result<AppendLock<'_>, Error> {
        self.file
            .lock_exclusive()
            .map_err(|err| {
                Error::new(lock_error_kind(&err))
                    .with_path(&self.path)
                    .with_source(err)
            })?;
        Ok(AppendLock { file: &self.file })
    }
}

pub struct AppendLock<'a> {
    file: &'a File,
}

impl<'a> Drop for AppendLock<'a> {
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

#[cfg(test)]
mod tests {
    use super::{Pool, PoolOptions};
    use crate::core::error::ErrorKind;
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
}
