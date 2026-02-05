//! Purpose: Plan append/drop transitions for the ring without performing any I/O.
//! Exports: `plan_append`, `AppendPlan`, `DropStep`, `DropKind`.
//! Role: Pure planning layer used by `pool` to apply deterministic writes to storage.
//! Invariants: No side effects; output depends only on `header`, `storage`, `payload_len`.
//! Invariants: Reads storage only to validate/inspect existing frames when freeing space.
use crate::core::error::{Error, ErrorKind};
use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
use crate::core::pool::PoolHeader;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropKind {
    Padding,
    Wrap,
    Frame { seq: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DropStep {
    pub offset: usize,
    pub len: usize,
    pub kind: DropKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendPlan {
    pub frame_offset: usize,
    pub frame_len: usize,
    pub wrap_offset: Option<usize>,
    pub drops: Vec<DropStep>,
    pub next_header: PoolHeader,
    pub seq: u64,
}

pub fn plan_append(
    header: PoolHeader,
    storage: &[u8],
    payload_len: usize,
) -> Result<AppendPlan, Error> {
    if payload_len > u32::MAX as usize {
        return Err(Error::new(ErrorKind::Usage).with_message("payload too large"));
    }
    let ring_offset = header.ring_offset as usize;
    let ring_size = header.ring_size as usize;
    if ring_size == 0 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("ring size is zero"));
    }
    if ring_offset + ring_size > storage.len() {
        return Err(Error::new(ErrorKind::Corrupt).with_message("ring exceeds storage bounds"));
    }

    let max_payload = frame::max_payload(ring_size, FRAME_HEADER_LEN);
    if payload_len > max_payload {
        return Err(Error::new(ErrorKind::Usage).with_message("payload exceeds ring capacity"));
    }

    let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload_len)
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("frame length overflow"))?;
    if frame_len > ring_size {
        return Err(Error::new(ErrorKind::Usage).with_message("frame larger than ring"));
    }

    let mut head = header.head_off as usize;
    let mut tail = header.tail_off as usize;
    let mut oldest_seq = header.oldest_seq;
    let mut newest_seq = header.newest_seq;
    if head >= ring_size || tail >= ring_size {
        return Err(Error::new(ErrorKind::Corrupt).with_message("head/tail out of range"));
    }
    if oldest_seq == 0 && head != tail {
        return Err(Error::new(ErrorKind::Corrupt).with_message("empty pool head/tail mismatch"));
    }

    let mut drops = Vec::new();
    let mut required = if oldest_seq == 0 {
        frame_len
    } else {
        required_space(head, frame_len, ring_size)
    };
    let wrap_required = ring_size - head < frame_len;
    while free_space(head, tail, ring_size, oldest_seq) < required
        || (wrap_required && oldest_seq != 0 && tail < frame_len)
    {
        let outcome = plan_drop_step(storage, ring_offset, ring_size, head, tail, oldest_seq)?;
        let Some((step, new_tail, new_oldest)) = outcome else {
            return Err(Error::new(ErrorKind::Busy).with_message("unable to make space"));
        };
        drops.push(step);
        tail = new_tail;
        oldest_seq = new_oldest;
        if tail == head {
            oldest_seq = 0;
        }
        required = if oldest_seq == 0 {
            frame_len
        } else {
            required_space(head, frame_len, ring_size)
        };
    }

    let mut wrap_offset = None;
    let remaining = ring_size - head;
    if remaining < frame_len {
        if remaining >= FRAME_HEADER_LEN {
            wrap_offset = Some(head);
        }
        head = 0;
    }
    if oldest_seq == 0 {
        tail = head;
    }

    let seq = if newest_seq == 0 { 1 } else { newest_seq + 1 };
    if oldest_seq == 0 {
        oldest_seq = seq;
    }
    newest_seq = seq;

    let mut new_head = head + frame_len;
    if new_head == ring_size {
        new_head = 0;
    }

    let next_header = PoolHeader {
        head_off: new_head as u64,
        tail_off: tail as u64,
        oldest_seq,
        newest_seq,
        ..header
    };

    Ok(AppendPlan {
        frame_offset: head,
        frame_len,
        wrap_offset,
        drops,
        next_header,
        seq,
    })
}

fn plan_drop_step(
    storage: &[u8],
    ring_offset: usize,
    ring_size: usize,
    head: usize,
    tail: usize,
    oldest_seq: u64,
) -> Result<Option<(DropStep, usize, u64)>, Error> {
    if oldest_seq == 0 {
        return Ok(None);
    }
    let remaining = ring_size.saturating_sub(tail);
    if remaining < FRAME_HEADER_LEN {
        let step = DropStep {
            offset: tail,
            len: remaining,
            kind: DropKind::Padding,
        };
        return Ok(Some((step, 0, if head == 0 { 0 } else { oldest_seq })));
    }

    let frame = read_frame_header(storage, ring_offset, tail)?;
    frame.validate(ring_size)?;

    match frame.state {
        FrameState::Wrap => {
            let step = DropStep {
                offset: tail,
                len: FRAME_HEADER_LEN,
                kind: DropKind::Wrap,
            };
            Ok(Some((step, 0, oldest_seq)))
        }
        FrameState::Committed => {
            let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, frame.payload_len as usize)
                .ok_or_else(|| {
                    Error::new(ErrorKind::Corrupt).with_message("frame length overflow")
                })?;
            let mut new_tail = tail + frame_len;
            if new_tail == ring_size {
                new_tail = 0;
            }
            let step = DropStep {
                offset: tail,
                len: frame_len,
                kind: DropKind::Frame { seq: frame.seq },
            };
            Ok(Some((step, new_tail, frame.seq + 1)))
        }
        _ => Err(Error::new(ErrorKind::Corrupt).with_message("invalid tail frame state")),
    }
}

fn read_frame_header(
    storage: &[u8],
    ring_offset: usize,
    head: usize,
) -> Result<FrameHeader, Error> {
    let start = ring_offset + head;
    let end = start + FRAME_HEADER_LEN;
    FrameHeader::decode(&storage[start..end])
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

#[cfg(test)]
mod tests {
    use super::{DropKind, plan_append};
    use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
    use crate::core::pool::PoolHeader;
    use crate::core::validate;

    const RING_OFFSET: usize = 4096;

    struct XorShift64 {
        state: u64,
    }

    impl XorShift64 {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.state = x;
            x
        }

        fn next_range(&mut self, max: usize) -> usize {
            if max == 0 {
                return 0;
            }
            (self.next_u64() % max as u64) as usize
        }
    }

    fn header_for(
        ring_size: usize,
        head: usize,
        tail: usize,
        oldest_seq: u64,
        newest_seq: u64,
    ) -> PoolHeader {
        PoolHeader {
            file_size: (RING_OFFSET + ring_size) as u64,
            ring_offset: RING_OFFSET as u64,
            ring_size: ring_size as u64,
            flags: 0,
            head_off: head as u64,
            tail_off: tail as u64,
            oldest_seq,
            newest_seq,
        }
    }

    fn write_frame(storage: &mut [u8], offset: usize, header: &FrameHeader, payload_len: usize) {
        let start = RING_OFFSET + offset;
        let end = start + FRAME_HEADER_LEN;
        storage[start..end].copy_from_slice(&header.encode());
        let payload_start = end;
        let payload_end = payload_start + payload_len;
        storage[payload_start..payload_end].fill(0u8);
        if header.state == FrameState::Committed {
            let marker_start = payload_end;
            let marker_end = marker_start + frame::FRAME_COMMIT_MARKER_LEN;
            storage[marker_start..marker_end].copy_from_slice(&frame::FRAME_COMMIT_MARKER);
        }
    }

    fn write_committed_frame(storage: &mut [u8], offset: usize, seq: u64, payload_len: usize) {
        let header = FrameHeader::new(FrameState::Committed, 0, seq, 0, payload_len as u32, 0);
        write_frame(storage, offset, &header, payload_len);
    }

    #[test]
    fn plan_append_empty_pool() {
        let payload_len = 16usize;
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload_len).expect("frame len");
        let ring_size = frame_len * 4;
        let storage = vec![0u8; RING_OFFSET + ring_size];
        let header = header_for(ring_size, 0, 0, 0, 0);

        let plan = plan_append(header, &storage, payload_len).expect("plan");
        assert_eq!(plan.drops.len(), 0);
        assert_eq!(plan.frame_offset, 0);
        assert_eq!(plan.wrap_offset, None);
        assert_eq!(plan.seq, 1);
        assert_eq!(plan.next_header.oldest_seq, 1);
        assert_eq!(plan.next_header.newest_seq, 1);
        assert_eq!(plan.next_header.head_off as usize, frame_len);
        assert_eq!(plan.next_header.tail_off, 0);
    }

    #[test]
    fn plan_append_exact_fit_full_ring() {
        let payload_len = 32usize;
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload_len).expect("frame len");
        let ring_size = frame_len;
        let storage = vec![0u8; RING_OFFSET + ring_size];
        let header = header_for(ring_size, 0, 0, 0, 0);

        let plan = plan_append(header, &storage, payload_len).expect("plan");
        assert_eq!(plan.frame_offset, 0);
        assert_eq!(plan.next_header.head_off, 0);
        assert_eq!(plan.next_header.tail_off, 0);
        assert_eq!(plan.next_header.oldest_seq, 1);
        assert_eq!(plan.next_header.newest_seq, 1);
    }

    #[test]
    fn plan_append_wraps_when_remaining_space_small() {
        let payload_len = 24usize;
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload_len).expect("frame len");
        let ring_size = frame_len * 3 + FRAME_HEADER_LEN;
        let storage = vec![0u8; RING_OFFSET + ring_size];
        let head = ring_size - FRAME_HEADER_LEN;
        let header = header_for(ring_size, head, head, 0, 0);

        let plan = plan_append(header, &storage, payload_len).expect("plan");
        assert_eq!(plan.wrap_offset, Some(head));
        assert_eq!(plan.frame_offset, 0);
        assert_eq!(plan.next_header.head_off as usize, frame_len);
    }

    #[test]
    fn plan_append_drops_oldest_when_full() {
        let payload_len = 16usize;
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, payload_len).expect("frame len");
        let ring_size = frame_len * 2;
        let mut storage = vec![0u8; RING_OFFSET + ring_size];

        let first = FrameHeader::new(FrameState::Committed, 0, 1, 0, payload_len as u32, 0);
        write_frame(&mut storage, 0, &first, payload_len);
        let second = FrameHeader::new(FrameState::Committed, 0, 2, 0, payload_len as u32, 0);
        write_frame(&mut storage, frame_len, &second, payload_len);

        let header = header_for(ring_size, 0, 0, 1, 2);
        let plan = plan_append(header, &storage, payload_len).expect("plan");

        assert_eq!(plan.drops.len(), 1);
        assert_eq!(plan.drops[0].offset, 0);
        assert_eq!(plan.drops[0].kind, DropKind::Frame { seq: 1 });
        assert_eq!(plan.frame_offset, 0);
        assert_eq!(plan.next_header.tail_off as usize, frame_len);
        assert_eq!(plan.next_header.oldest_seq, 2);
        assert_eq!(plan.next_header.newest_seq, 3);
    }

    #[test]
    fn prop_plan_append_invariants() {
        let seeds = [1u64, 7, 42, 99];
        for seed in seeds {
            let mut rng = XorShift64::new(seed);
            let ring_size = (FRAME_HEADER_LEN * 6) + rng.next_range(FRAME_HEADER_LEN * 4);
            let mut storage = vec![0u8; RING_OFFSET + ring_size];
            let mut header = PoolHeader {
                file_size: (RING_OFFSET + ring_size) as u64,
                ring_offset: RING_OFFSET as u64,
                ring_size: ring_size as u64,
                flags: 0,
                head_off: 0,
                tail_off: 0,
                oldest_seq: 0,
                newest_seq: 0,
            };

            for _ in 0..200 {
                let max_payload = frame::max_payload(ring_size, FRAME_HEADER_LEN);
                let mut payload_len = 1 + rng.next_range(max_payload.max(1));
                while let Some(frame_len) = frame::frame_total_len(FRAME_HEADER_LEN, payload_len) {
                    if frame_len <= ring_size {
                        break;
                    }
                    payload_len = payload_len.saturating_sub(1);
                }
                if payload_len == 0 {
                    continue;
                }

                let plan = plan_append(header, &storage, payload_len).expect("plan");
                if let Some(wrap_offset) = plan.wrap_offset {
                    let wrap = FrameHeader::new(FrameState::Wrap, 0, 0, 0, 0, 0);
                    write_frame(&mut storage, wrap_offset, &wrap, 0);
                }
                write_committed_frame(&mut storage, plan.frame_offset, plan.seq, payload_len);
                header = plan.next_header;
                validate::validate_pool_state(header, &storage).expect("validate");
            }
        }
    }
}
