// Pool and ring validation helpers plus debug-only assertions.
// Full scans are for explicit validation; hot paths use tail-only checks.
// Snapshot output is opt-in and written to .scratch/ on failure.
use crate::core::error::{Error, ErrorKind};
use crate::core::frame::{self, FrameHeader, FrameState, FRAME_HEADER_LEN};
use crate::core::pool::PoolHeader;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SNAPSHOT_DIR: &str = ".scratch";
const SNAPSHOT_PREFIX: &str = "pool-snapshot-";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotMode {
    Disabled,
    OnFailure,
}

pub fn validate_frame_header(header: &FrameHeader, ring_size: usize) -> Result<(), Error> {
    header.validate(ring_size)
}

pub fn validate_pool_state(header: PoolHeader, mmap: &[u8]) -> Result<(), Error> {
    if header.ring_size == 0 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("ring size is zero"));
    }
    let ring_offset = header.ring_offset as usize;
    let ring_size = header.ring_size as usize;
    if ring_offset + ring_size > mmap.len() {
        return Err(Error::new(ErrorKind::Corrupt).with_message("ring exceeds mmap bounds"));
    }
    let head = header.head_off as usize;
    let tail = header.tail_off as usize;
    if head >= ring_size || tail >= ring_size {
        return Err(Error::new(ErrorKind::Corrupt).with_message("head/tail out of range"));
    }
    if header.oldest_seq == 0 {
        if header.head_off != header.tail_off {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("empty pool head/tail mismatch"));
        }
        return Ok(());
    }
    if header.newest_seq < header.oldest_seq {
        return Err(Error::new(ErrorKind::Corrupt).with_message("seq bounds inverted"));
    }

    let mut offset = tail;
    let mut expected_seq = header.oldest_seq;
    let max_frames = ring_size / FRAME_HEADER_LEN + 1;
    let mut steps = 0usize;

    loop {
        if steps > max_frames {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("scan exceeded ring capacity"));
        }
        if ring_size - offset < FRAME_HEADER_LEN {
            offset = 0;
            steps += 1;
            continue;
        }
        let frame = read_frame_header(mmap, ring_offset, offset)?;
        validate_frame_header(&frame, ring_size)?;
        match frame.state {
            FrameState::Wrap => {
                offset = 0;
                steps += 1;
                continue;
            }
            FrameState::Committed => {}
            _ => {
                return Err(Error::new(ErrorKind::Corrupt)
                    .with_message("unexpected frame state"));
            }
        }

        if frame.seq != expected_seq {
            return Err(Error::new(ErrorKind::Corrupt).with_message("seq mismatch"));
        }

        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, frame.payload_len as usize)
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("frame length overflow"))?;
        if offset + frame_len > ring_size {
            return Err(Error::new(ErrorKind::Corrupt).with_message("frame exceeds ring"));
        }
        let mut next_off = offset + frame_len;
        if next_off == ring_size {
            next_off = 0;
        }

        if expected_seq == header.newest_seq {
            if next_off != head {
                return Err(Error::new(ErrorKind::Corrupt)
                    .with_message("head offset mismatch"));
            }
            break;
        }

        expected_seq += 1;
        offset = next_off;
        steps += 1;
    }

    Ok(())
}

pub fn debug_assert_pool_state(header: PoolHeader, mmap: &[u8]) {
    debug_assert_pool_state_with_snapshot(header, mmap, SnapshotMode::Disabled);
}

pub fn debug_assert_pool_state_with_snapshot(
    header: PoolHeader,
    mmap: &[u8],
    snapshot: SnapshotMode,
) {
    if !cfg!(debug_assertions) {
        return;
    }
    if let Err(err) = validate_pool_state(header, mmap) {
        let snapshot_path = if snapshot == SnapshotMode::OnFailure {
            write_snapshot(header, mmap)
        } else {
            None
        };
        if let Some(path) = snapshot_path {
            panic!("pool state invariant failed: {err} (snapshot: {})", path.display());
        }
        panic!("pool state invariant failed: {err}");
    }
}

pub fn debug_assert_tail_committed(
    mmap: &[u8],
    ring_offset: usize,
    ring_size: usize,
    tail: usize,
    oldest_seq: u64,
) {
    if !cfg!(debug_assertions) || oldest_seq == 0 {
        return;
    }
    if tail >= ring_size {
        panic!("tail offset out of bounds");
    }
    let header = read_frame_header(mmap, ring_offset, tail).unwrap_or_else(|err| {
        let start = ring_offset + tail;
        let end = start + 4;
        let magic = &mmap[start..end];
        panic!("tail frame header decode failed: {err}; magic={magic:?}");
    });
    validate_frame_header(&header, ring_size).expect("tail frame header validation failed");
    if header.state != FrameState::Committed {
        panic!("tail frame is not committed");
    }
    if header.seq != oldest_seq {
        panic!("tail seq mismatch");
    }
}

fn read_frame_header(mmap: &[u8], ring_offset: usize, head: usize) -> Result<FrameHeader, Error> {
    let start = ring_offset + head;
    let end = start + FRAME_HEADER_LEN;
    FrameHeader::decode(&mmap[start..end])
}

fn write_snapshot(header: PoolHeader, mmap: &[u8]) -> Option<PathBuf> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis();
    let dir = Path::new(SNAPSHOT_DIR);
    fs::create_dir_all(dir).ok()?;
    let filename = format!("{SNAPSHOT_PREFIX}{timestamp}-{}.txt", std::process::id());
    let path = dir.join(filename);
    let mut file = File::create(&path).ok()?;

    let _ = writeln!(file, "timestamp_ms={timestamp}");
    let _ = writeln!(
        file,
        "header file_size={} ring_offset={} ring_size={} head_off={} tail_off={} oldest_seq={} newest_seq={}",
        header.file_size,
        header.ring_offset,
        header.ring_size,
        header.head_off,
        header.tail_off,
        header.oldest_seq,
        header.newest_seq
    );

    let ring_offset = header.ring_offset as usize;
    let ring_size = header.ring_size as usize;
    let _ = write_frame_snapshot(&mut file, "tail", mmap, ring_offset, ring_size, header.tail_off as usize);
    let _ = write_frame_snapshot(&mut file, "head", mmap, ring_offset, ring_size, header.head_off as usize);

    Some(path)
}

fn write_frame_snapshot(
    file: &mut File,
    label: &str,
    mmap: &[u8],
    ring_offset: usize,
    ring_size: usize,
    offset: usize,
) -> std::io::Result<()> {
    if ring_offset + ring_size > mmap.len() {
        return writeln!(file, "{label}: ring out of bounds");
    }
    if offset >= ring_size {
        return writeln!(file, "{label}: offset out of range ({offset})");
    }
    let start = ring_offset + offset;
    let end = start + FRAME_HEADER_LEN;
    if end > ring_offset + ring_size {
        return writeln!(file, "{label}: header exceeds ring (offset={offset})");
    }
    let magic = &mmap[start..start + 4];
    match FrameHeader::decode(&mmap[start..end]) {
        Ok(header) => {
            let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, header.payload_len as usize);
            writeln!(
                file,
                "{label}: state={:?} seq={} payload_len={} frame_len={:?} magic={magic:?}",
                header.state,
                header.seq,
                header.payload_len,
                frame_len
            )
        }
        Err(err) => writeln!(file, "{label}: decode_error={err} magic={magic:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{debug_assert_pool_state_with_snapshot, SnapshotMode, SNAPSHOT_PREFIX};
    use crate::core::frame::FRAME_HEADER_LEN;
    use crate::core::pool::PoolHeader;
    use std::collections::HashSet;
    use std::fs;

    fn snapshot_set() -> HashSet<String> {
        let dir = std::path::Path::new(super::SNAPSHOT_DIR);
        let Ok(entries) = fs::read_dir(dir) else {
            return HashSet::new();
        };
        entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(SNAPSHOT_PREFIX))
            .collect()
    }

    #[test]
    fn snapshot_written_on_validation_failure() {
        if !cfg!(debug_assertions) {
            return;
        }
        let ring_size = FRAME_HEADER_LEN * 2;
        let header = PoolHeader {
            file_size: ring_size as u64,
            ring_offset: 0,
            ring_size: ring_size as u64,
            flags: 0,
            head_off: 0,
            tail_off: 0,
            oldest_seq: 1,
            newest_seq: 1,
        };
        let mmap = vec![0u8; ring_size];
        let before = snapshot_set();
        let result = std::panic::catch_unwind(|| {
            debug_assert_pool_state_with_snapshot(header, &mmap, SnapshotMode::OnFailure);
        });
        assert!(result.is_err());
        let after = snapshot_set();
        let new_files: Vec<_> = after.difference(&before).cloned().collect();
        assert!(!new_files.is_empty());
        for name in new_files {
            let _ = fs::remove_file(std::path::Path::new(super::SNAPSHOT_DIR).join(name));
        }
    }
}
