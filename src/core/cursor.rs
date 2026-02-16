//! Purpose: Iterate committed frames in the ring with overwrite safety and minimal scanning.
//! Exports: `Cursor`, `CursorResult`, `FrameRef`.
//! Role: Read-side API used by CLI commands (fetch/follow) without exposing raw offsets.
//! Invariants: Never returns `Writing` or invalid frames; treats them as non-visible.
//! Invariants: Detects overwrite (fell-behind) and resynchronizes to the current tail.
use crate::core::error::{Error, ErrorKind};
use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
use crate::core::pool::Pool;

#[derive(Debug, PartialEq)]
pub enum CursorResult<'a> {
    Message(FrameRef<'a>),
    WouldBlock,
    FellBehind,
}

#[derive(Debug, PartialEq)]
pub struct FrameRef<'a> {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub flags: u32,
    pub payload: &'a [u8],
}

#[derive(Debug)]
pub struct Cursor {
    next_off: usize,
    last_seq: u64,
}

impl Cursor {
    pub fn new() -> Self {
        Self {
            next_off: 0,
            last_seq: 0,
        }
    }

    pub fn seek_to(&mut self, offset: usize) {
        self.next_off = offset;
        self.last_seq = 0;
    }

    pub fn next<'a>(&mut self, pool: &'a Pool) -> Result<CursorResult<'a>, Error> {
        let header = pool.header_from_mmap()?;
        if header.oldest_seq == 0 {
            return Ok(CursorResult::WouldBlock);
        }

        if self.last_seq != 0 && self.last_seq >= header.newest_seq {
            return Ok(CursorResult::WouldBlock);
        }

        let ring_size = header.ring_size as usize;
        if ring_size == 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring size is zero"));
        }

        let tail = header.tail_off as usize;
        let head = header.head_off as usize;
        let is_full = head == tail && header.oldest_seq != 0;
        if !is_full && self.next_off == head {
            return Ok(CursorResult::WouldBlock);
        }

        if self.next_off >= ring_size
            || !offset_in_range(self.next_off, tail, head, ring_size, header.oldest_seq)
        {
            self.next_off = tail;
            self.last_seq = 0;
            return Ok(CursorResult::FellBehind);
        }

        loop {
            let read = read_frame_at(
                pool.mmap(),
                header.ring_offset as usize,
                ring_size,
                self.next_off,
            )?;
            match read {
                ReadResult::Wrap => {
                    self.next_off = 0;
                    continue;
                }
                ReadResult::WouldBlock => return Ok(CursorResult::WouldBlock),
                ReadResult::FellBehind => {
                    self.next_off = tail;
                    self.last_seq = 0;
                    return Ok(CursorResult::FellBehind);
                }
                ReadResult::Message { frame, next_off } => {
                    self.next_off = next_off;
                    self.last_seq = frame.seq;
                    return Ok(CursorResult::Message(frame));
                }
            }
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) enum ReadResult<'a> {
    Message {
        frame: FrameRef<'a>,
        next_off: usize,
    },
    Wrap,
    WouldBlock,
    FellBehind,
}

pub(crate) fn read_frame_at<'a>(
    mmap: &'a [u8],
    ring_offset: usize,
    ring_size: usize,
    offset: usize,
) -> Result<ReadResult<'a>, Error> {
    let start = ring_offset + offset;
    let end = start + FRAME_HEADER_LEN;
    if end > ring_offset + ring_size {
        return Ok(ReadResult::FellBehind);
    }

    let h1 = match FrameHeader::decode(&mmap[start..end]) {
        Ok(header) => header,
        Err(_) => return Ok(ReadResult::FellBehind),
    };

    match h1.state {
        FrameState::Wrap => return Ok(ReadResult::Wrap),
        FrameState::Committed => {}
        _ => return Ok(ReadResult::WouldBlock),
    }

    if h1.validate(ring_size).is_err() {
        return Ok(ReadResult::FellBehind);
    }

    let frame_len = match frame::frame_total_len(FRAME_HEADER_LEN, h1.payload_len as usize) {
        Some(len) => len,
        None => return Ok(ReadResult::FellBehind),
    };
    if offset + frame_len > ring_size {
        return Ok(ReadResult::FellBehind);
    }

    let payload_start = start + FRAME_HEADER_LEN;
    let payload_end = payload_start + h1.payload_len as usize;
    let payload = &mmap[payload_start..payload_end];

    let marker_start = payload_end;
    let marker_end = marker_start + frame::FRAME_COMMIT_MARKER_LEN;
    if &mmap[marker_start..marker_end] != frame::FRAME_COMMIT_MARKER.as_slice() {
        return Ok(ReadResult::FellBehind);
    }

    let mut next_off = offset + frame_len;
    if next_off == ring_size {
        next_off = 0;
    }

    Ok(ReadResult::Message {
        frame: FrameRef {
            seq: h1.seq,
            timestamp_ns: h1.timestamp_ns,
            flags: h1.flags,
            payload,
        },
        next_off,
    })
}

fn offset_in_range(
    offset: usize,
    tail: usize,
    head: usize,
    ring_size: usize,
    oldest_seq: u64,
) -> bool {
    if oldest_seq == 0 {
        return false;
    }
    if offset >= ring_size {
        return false;
    }
    if head == tail {
        // Full-ring case: there is no empty "head" boundary to exclude.
        // We rely on seq-based stopping (`last_seq >= newest_seq`) to avoid cycling forever.
        return true;
    }
    if tail < head {
        offset >= tail && offset < head
    } else {
        offset >= tail || offset < head
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, CursorResult, ReadResult, read_frame_at};
    use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
    use crate::core::lite3;
    use crate::core::pool::{Pool, PoolOptions};
    use serde_json::json;

    #[test]
    fn missing_commit_marker_is_rejected() {
        let payload = lite3::encode_message(&[], &json!({"x": 1})).expect("payload");
        let ring_size = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let mut buf = vec![0u8; ring_size];

        let header = FrameHeader::new(FrameState::Committed, 0, 1, 1, payload.len() as u32, 0);
        buf[0..FRAME_HEADER_LEN].copy_from_slice(&header.encode());
        buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + payload.len()].copy_from_slice(payload.as_slice());
        // Intentionally omit the commit marker.

        let result = read_frame_at(&buf, 0, ring_size, 0).expect("read");
        assert!(matches!(result, ReadResult::FellBehind));
    }

    #[test]
    fn wrap_marker_is_reported() {
        let ring_size = FRAME_HEADER_LEN * 2;
        let mut buf = vec![0u8; ring_size];
        let header = FrameHeader::new(FrameState::Wrap, 0, 0, 0, 0, 0);
        buf[0..FRAME_HEADER_LEN].copy_from_slice(&header.encode());

        let result = read_frame_at(&buf, 0, ring_size, 0).expect("read");
        assert!(matches!(result, ReadResult::Wrap));
    }

    #[test]
    fn invalid_magic_falls_behind() {
        let ring_size = FRAME_HEADER_LEN * 2;
        let mut buf = vec![0u8; ring_size];
        buf[0..4].copy_from_slice(b"NOPE");

        let result = read_frame_at(&buf, 0, ring_size, 0).expect("read");
        assert!(matches!(result, ReadResult::FellBehind));
    }

    #[test]
    fn writing_state_would_block() {
        let payload = lite3::encode_message(&[], &json!({"x": 1})).expect("payload");
        let ring_size = FRAME_HEADER_LEN + payload.len();
        let mut buf = vec![0u8; ring_size];
        let header = FrameHeader::new(FrameState::Writing, 0, 1, 1, payload.len() as u32, 0);
        buf[0..FRAME_HEADER_LEN].copy_from_slice(&header.encode());
        buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + payload.len()].copy_from_slice(payload.as_slice());

        let result = read_frame_at(&buf, 0, ring_size, 0).expect("read");
        assert!(matches!(result, ReadResult::WouldBlock));
    }

    #[test]
    fn header_length_mismatch_falls_behind() {
        let payload = lite3::encode_message(&[], &json!({"x": 1})).expect("payload");
        let ring_size = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let mut buf = vec![0u8; ring_size];
        let mut header = FrameHeader::new(FrameState::Committed, 0, 1, 1, payload.len() as u32, 0);
        header.header_len = 0;
        buf[0..FRAME_HEADER_LEN].copy_from_slice(&header.encode());
        write_payload_and_marker(&mut buf, payload.as_slice());

        let result = read_frame_at(&buf, 0, ring_size, 0).expect("read");
        assert!(matches!(result, ReadResult::FellBehind));
    }

    #[test]
    fn payload_length_xor_mismatch_falls_behind() {
        let payload = lite3::encode_message(&[], &json!({"x": 1})).expect("payload");
        let ring_size = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let mut buf = vec![0u8; ring_size];
        let mut header = FrameHeader::new(FrameState::Committed, 0, 1, 1, payload.len() as u32, 0);
        header.payload_len_xor ^= 0xFF;
        buf[0..FRAME_HEADER_LEN].copy_from_slice(&header.encode());
        write_payload_and_marker(&mut buf, payload.as_slice());

        let result = read_frame_at(&buf, 0, ring_size, 0).expect("read");
        assert!(matches!(result, ReadResult::FellBehind));
    }

    #[test]
    fn cursor_resyncs_on_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &json!({"x": 2})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len + FRAME_HEADER_LEN;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");
        pool.append(payload.as_slice()).expect("append 1");
        let mut cursor = Cursor::new();
        let result = cursor.next(&pool).expect("next");
        assert!(matches!(result, CursorResult::Message(_)));

        pool.append(payload.as_slice()).expect("append 2");
        cursor.next_off = ring_size + 8;
        let result = cursor.next(&pool).expect("next");
        assert!(matches!(result, CursorResult::FellBehind));
        let result = cursor.next(&pool).expect("next");
        assert!(matches!(result, CursorResult::Message(_)));
    }

    fn write_payload_and_marker(buf: &mut [u8], payload: &[u8]) {
        let payload_start = FRAME_HEADER_LEN;
        let payload_end = payload_start + payload.len();
        buf[payload_start..payload_end].copy_from_slice(payload);
        let marker_start = payload_end;
        let marker_end = marker_start + frame::FRAME_COMMIT_MARKER_LEN;
        buf[marker_start..marker_end].copy_from_slice(&frame::FRAME_COMMIT_MARKER);
    }
}
