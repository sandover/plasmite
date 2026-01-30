// Frame header layout, alignment helpers, and payload validation utilities.
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3;

pub const FRAME_MAGIC: [u8; 4] = *b"FRM1";
pub const FRAME_HEADER_LEN: usize = 64;
pub const MAX_PAYLOAD_ABS: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameState {
    Empty = 0,
    Writing = 1,
    Committed = 2,
    Wrap = 3,
}

impl FrameState {
    fn from_u32(value: u32) -> Result<Self, Error> {
        match value {
            0 => Ok(FrameState::Empty),
            1 => Ok(FrameState::Writing),
            2 => Ok(FrameState::Committed),
            3 => Ok(FrameState::Wrap),
            _ => Err(Error::new(ErrorKind::Corrupt).with_message("invalid frame state")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    pub state: FrameState,
    pub flags: u32,
    pub header_len: u32,
    pub seq: u64,
    pub timestamp_ns: u64,
    pub payload_len: u32,
    pub payload_len_xor: u32,
    pub crc32c: u32,
}

impl FrameHeader {
    pub fn new(
        state: FrameState,
        flags: u32,
        seq: u64,
        timestamp_ns: u64,
        payload_len: u32,
        crc32c: u32,
    ) -> Self {
        Self {
            state,
            flags,
            header_len: FRAME_HEADER_LEN as u32,
            seq,
            timestamp_ns,
            payload_len,
            payload_len_xor: payload_len ^ 0xFFFF_FFFF,
            crc32c,
        }
    }

    pub fn encode(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut buf = [0u8; FRAME_HEADER_LEN];
        buf[0..4].copy_from_slice(&FRAME_MAGIC);
        write_u32(&mut buf, 4, self.state as u32);
        write_u32(&mut buf, 8, self.flags);
        write_u32(&mut buf, 12, self.header_len);
        write_u64(&mut buf, 16, self.seq);
        write_u64(&mut buf, 24, self.timestamp_ns);
        write_u32(&mut buf, 32, self.payload_len);
        write_u32(&mut buf, 36, self.payload_len_xor);
        write_u32(&mut buf, 40, self.crc32c);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        if buf.len() < FRAME_HEADER_LEN {
            return Err(Error::new(ErrorKind::Corrupt).with_message("frame header too small"));
        }
        if buf[0..4] != FRAME_MAGIC {
            return Err(Error::new(ErrorKind::Corrupt).with_message("bad frame magic"));
        }
        let state = FrameState::from_u32(read_u32(buf, 4))?;
        let flags = read_u32(buf, 8);
        let header_len = read_u32(buf, 12);
        let seq = read_u64(buf, 16);
        let timestamp_ns = read_u64(buf, 24);
        let payload_len = read_u32(buf, 32);
        let payload_len_xor = read_u32(buf, 36);
        let crc32c = read_u32(buf, 40);

        Ok(Self {
            state,
            flags,
            header_len,
            seq,
            timestamp_ns,
            payload_len,
            payload_len_xor,
            crc32c,
        })
    }

    pub fn validate(&self, ring_size: usize) -> Result<(), Error> {
        if self.header_len as usize != FRAME_HEADER_LEN {
            return Err(Error::new(ErrorKind::Corrupt).with_message("unexpected header length"));
        }
        if self.payload_len ^ self.payload_len_xor != 0xFFFF_FFFF {
            return Err(Error::new(ErrorKind::Corrupt).with_message("payload length xor mismatch"));
        }
        let max_payload = max_payload(ring_size, self.header_len as usize);
        if self.payload_len as usize > max_payload {
            return Err(Error::new(ErrorKind::Corrupt).with_message("payload length exceeds max"));
        }
        Ok(())
    }
}

pub fn align8(value: usize) -> usize {
    (value + 7) & !7
}

pub fn frame_total_len(header_len: usize, payload_len: usize) -> usize {
    align8(header_len + payload_len)
}

pub fn max_payload(ring_size: usize, header_len: usize) -> usize {
    let ring_cap = ring_size.saturating_sub(header_len);
    ring_cap.min(MAX_PAYLOAD_ABS)
}

pub fn validate_payload(payload: &[u8]) -> Result<(), Error> {
    lite3::validate_bytes(payload)
        .map_err(|err| Error::new(ErrorKind::Corrupt).with_message("invalid lite3 payload").with_source(err))
}

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(read_4(buf, offset))
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(read_8(buf, offset))
}

fn read_4(buf: &[u8], offset: usize) -> [u8; 4] {
    let mut out = [0u8; 4];
    out.copy_from_slice(&buf[offset..offset + 4]);
    out
}

fn read_8(buf: &[u8], offset: usize) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&buf[offset..offset + 8]);
    out
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::{
        align8, frame_total_len, max_payload, validate_payload, FrameHeader, FrameState,
        FRAME_HEADER_LEN, MAX_PAYLOAD_ABS,
    };
    use crate::core::error::ErrorKind;
    use crate::core::lite3::encode_message;
    use serde_json::json;

    #[test]
    fn alignment_is_8_bytes() {
        assert_eq!(align8(0), 0);
        assert_eq!(align8(1), 8);
        assert_eq!(align8(8), 8);
        assert_eq!(align8(9), 16);
    }

    #[test]
    fn frame_total_len_is_aligned() {
        let total = frame_total_len(64, 1);
        assert_eq!(total, 72);
    }

    #[test]
    fn header_round_trip() {
        let header = FrameHeader::new(FrameState::Committed, 0, 42, 100, 12, 0);
        let buf = header.encode();
        let decoded = FrameHeader::decode(&buf).expect("decode");
        assert_eq!(header, decoded);
    }

    #[test]
    fn header_rejects_torn_payload_len() {
        let mut header = FrameHeader::new(FrameState::Committed, 0, 1, 1, 8, 0);
        header.payload_len_xor = 0;
        let err = header.validate(1024).expect_err("should fail");
        assert_eq!(err.kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn header_rejects_oversized_payload() {
        let header = FrameHeader::new(FrameState::Committed, 0, 1, 1, (MAX_PAYLOAD_ABS as u32) + 1, 0);
        let err = header.validate(MAX_PAYLOAD_ABS + FRAME_HEADER_LEN).expect_err("should fail");
        assert_eq!(err.kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn max_payload_is_ring_limited() {
        let max = max_payload(128, FRAME_HEADER_LEN);
        assert_eq!(max, 64);
    }

    #[test]
    fn payload_contract_is_enforced() {
        let data = json!({"ok": true});
        let buf = encode_message(&["event".to_string()], &data).expect("encode");
        validate_payload(buf.as_slice()).expect("valid payload");
    }
}
