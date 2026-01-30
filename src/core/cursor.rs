// Cursor iteration with stable snapshot validation for overwriteable ring buffers.
use crate::core::error::{Error, ErrorKind};
use crate::core::frame::{self, FrameHeader, FrameState, FRAME_HEADER_LEN};
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
}

impl Cursor {
    pub fn new() -> Self {
        Self { next_off: 0 }
    }

    pub fn next<'a>(&mut self, pool: &'a Pool) -> Result<CursorResult<'a>, Error> {
        let header = pool.header_from_mmap()?;
        if header.oldest_seq == 0 {
            return Ok(CursorResult::WouldBlock);
        }

        let ring_size = header.ring_size as usize;
        if ring_size == 0 {
            return Err(Error::new(ErrorKind::Corrupt).with_message("ring size is zero"));
        }

        let tail = header.tail_off as usize;
        let head = header.head_off as usize;
        if !offset_in_range(self.next_off, tail, head, header.oldest_seq) {
            self.next_off = tail;
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
                    return Ok(CursorResult::FellBehind);
                }
                ReadResult::Message { frame, next_off } => {
                    self.next_off = next_off;
                    return Ok(CursorResult::Message(frame));
                }
            }
        }
    }
}

enum ReadResult<'a> {
    Message { frame: FrameRef<'a>, next_off: usize },
    Wrap,
    WouldBlock,
    FellBehind,
}

fn read_frame_at<'a>(
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

    let h2 = match FrameHeader::decode(&mmap[start..end]) {
        Ok(header) => header,
        Err(_) => return Ok(ReadResult::FellBehind),
    };
    if !headers_match(&h1, &h2) {
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

fn headers_match(a: &FrameHeader, b: &FrameHeader) -> bool {
    a.state == b.state
        && a.flags == b.flags
        && a.header_len == b.header_len
        && a.seq == b.seq
        && a.timestamp_ns == b.timestamp_ns
        && a.payload_len == b.payload_len
        && a.payload_len_xor == b.payload_len_xor
        && a.crc32c == b.crc32c
}

fn offset_in_range(offset: usize, tail: usize, head: usize, oldest_seq: u64) -> bool {
    if oldest_seq == 0 {
        return false;
    }
    if head == tail {
        return offset == tail;
    }
    if tail < head {
        offset >= tail && offset < head
    } else {
        offset >= tail || offset < head
    }
}

#[cfg(test)]
mod tests {
    use super::{read_frame_at, Cursor, CursorResult, ReadResult};
    use crate::core::frame::{self, FrameHeader, FrameState, FRAME_HEADER_LEN};
    use crate::core::lite3;
    use crate::core::pool::{Pool, PoolOptions};
    use serde_json::json;

    #[test]
    fn unstable_header_is_rejected() {
        let payload = lite3::encode_message(&[], &json!({"x": 1})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len + FRAME_HEADER_LEN;
        let mut buf = vec![0u8; ring_size];

        let header = FrameHeader::new(
            FrameState::Committed,
            0,
            1,
            1,
            payload.len() as u32,
            0,
        );
        let start = 0;
        buf[start..start + FRAME_HEADER_LEN].copy_from_slice(&header.encode());
        buf[start + FRAME_HEADER_LEN..start + FRAME_HEADER_LEN + payload.len()]
            .copy_from_slice(payload.as_slice());

        let result = read_frame_at_with_hook(&mut buf, 0, ring_size, 0, |bytes| {
            bytes[16] ^= 0xFF;
        })
        .expect("read");
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
    fn cursor_resyncs_on_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pool.plasmite");
        let payload = lite3::encode_message(&[], &json!({"x": 2})).expect("payload");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload.len()).expect("len");
        let ring_size = frame_len + FRAME_HEADER_LEN;
        let mut pool = Pool::create(&path, PoolOptions::new(4096 + ring_size as u64)).expect("create");

        pool.append(payload.as_slice()).expect("append 1");
        let mut cursor = Cursor::new();
        let result = cursor.next(&pool).expect("next");
        assert!(matches!(result, CursorResult::Message(_)));

        pool.append(payload.as_slice()).expect("append 2");
        cursor.next_off = 0;
        let result = cursor.next(&pool).expect("next");
        assert!(matches!(result, CursorResult::FellBehind));
        let result = cursor.next(&pool).expect("next");
        assert!(matches!(result, CursorResult::Message(_)));
    }

    fn read_frame_at_with_hook(
        mmap: &mut [u8],
        ring_offset: usize,
        ring_size: usize,
        offset: usize,
        hook: impl FnOnce(&mut [u8]),
    ) -> Result<ReadResult<'_>, crate::core::error::Error> {
        let start = ring_offset + offset;
        let end = start + FRAME_HEADER_LEN;
        let h1 = FrameHeader::decode(&mmap[start..end]).expect("header");
        let payload_start = start + FRAME_HEADER_LEN;
        let payload_end = payload_start + h1.payload_len as usize;
        let _payload = &mmap[payload_start..payload_end];
        hook(mmap);
        let h2 = FrameHeader::decode(&mmap[start..end]).expect("header");
        if !super::headers_match(&h1, &h2) {
            return Ok(ReadResult::FellBehind);
        }
        read_frame_at(mmap, ring_offset, ring_size, offset)
    }
}
