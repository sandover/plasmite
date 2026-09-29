//! Purpose: Report where stored frames sit in a pool's ring, for drawing the ring.
//! Exports: `RingLayout`, `FrameSpan`, `Pool::ring_layout`.
//! Role: Read-only view for the web map; never changes the pool.
//! Invariants: Offsets are byte positions within the ring, from 0 to `ring_size`.
//! Invariants: Returns no frames, rather than a wrong list, when a writer changes
//!   the ring during the walk or the pool holds more frames than asked for.
use crate::core::cursor::{ReadResult, read_frame_at};
use crate::core::error::Error;
use crate::core::frame::{self, FRAME_HEADER_LEN};
use crate::core::pool::Pool;

/// Where a pool's stored messages sit in its ring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RingLayout {
    /// Offset of the oldest stored frame.
    pub tail: u64,
    /// Offset where the next frame will be written.
    pub head: u64,
    /// Stored frames, oldest first. `None` when there are more than asked for,
    /// or when a writer overwrote frames during the walk.
    pub frames: Option<Vec<FrameSpan>>,
}

/// One stored frame: its sequence number, ring offset, and ring bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSpan {
    pub seq: u64,
    pub offset: u64,
    pub len: u64,
}

impl Pool {
    /// Reads the ring's tail and head, and lists up to `max_frames` stored frames.
    pub fn ring_layout(&self, max_frames: u64) -> Result<RingLayout, Error> {
        let header = self.header_from_mmap()?;
        let frames = if header.oldest_seq == 0 {
            Some(Vec::new())
        } else if header.newest_seq - header.oldest_seq >= max_frames {
            None
        } else {
            self.walk_frames(
                header.ring_offset as usize,
                header.ring_size as usize,
                header.tail_off as usize,
                header.oldest_seq,
                header.newest_seq,
            )?
        };
        Ok(RingLayout {
            tail: header.tail_off,
            head: header.head_off,
            frames,
        })
    }

    // Follows frames from the tail. Each frame must carry the next expected
    // sequence number, so an overwrite during the walk ends it with `None`.
    fn walk_frames(
        &self,
        ring_offset: usize,
        ring_size: usize,
        tail: usize,
        oldest: u64,
        newest: u64,
    ) -> Result<Option<Vec<FrameSpan>>, Error> {
        let mut spans = Vec::with_capacity((newest - oldest + 1) as usize);
        let mut offset = tail;
        let mut wrapped = false;
        loop {
            match read_frame_at(self.mmap(), ring_offset, ring_size, offset)? {
                ReadResult::Wrap if !wrapped => {
                    wrapped = true;
                    offset = 0;
                }
                ReadResult::Message { frame, next_off } => {
                    if frame.seq != oldest + spans.len() as u64 {
                        return Ok(None);
                    }
                    let len = frame::frame_total_len(FRAME_HEADER_LEN, frame.payload.len());
                    let Some(len) = len else { return Ok(None) };
                    spans.push(FrameSpan {
                        seq: frame.seq,
                        offset: offset as u64,
                        len: len as u64,
                    });
                    if frame.seq == newest {
                        return Ok(Some(spans));
                    }
                    offset = next_off;
                }
                _ => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::frame::{FRAME_COMMIT_MARKER_LEN, FRAME_HEADER_LEN};
    use crate::core::pool::{Pool, PoolOptions};

    fn pool_with(ring_bytes: u64, payloads: &[usize]) -> (tempfile::TempDir, Pool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut pool = Pool::create(
            dir.path().join("ring.plasmite"),
            PoolOptions::new(4096 + ring_bytes).with_index_capacity(0),
        )
        .expect("create");
        for &size in payloads {
            pool.append(&vec![b'x'; size]).expect("append");
        }
        (dir, pool)
    }

    #[test]
    fn empty_pool_has_no_frames() {
        let (_dir, pool) = pool_with(4096, &[]);
        let layout = pool.ring_layout(64).expect("layout");
        assert_eq!(layout.frames, Some(Vec::new()));
    }

    #[test]
    fn frames_run_from_tail_to_head_in_order() {
        let (_dir, pool) = pool_with(4096, &[10, 200, 40]);
        let layout = pool.ring_layout(64).expect("layout");
        let frames = layout.frames.expect("frames");
        assert_eq!(frames.iter().map(|f| f.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(frames[0].offset, layout.tail);
        for pair in frames.windows(2) {
            assert_eq!(pair[1].offset, pair[0].offset + pair[0].len);
        }
        assert!(frames[1].len > frames[0].len + 150);
        let last = frames.last().expect("last");
        assert_eq!(last.offset + last.len, layout.head);
    }

    #[test]
    fn frames_follow_the_ring_past_the_end() {
        let payload = 512 - FRAME_HEADER_LEN - FRAME_COMMIT_MARKER_LEN;
        let (_dir, pool) = pool_with(4096, &[payload; 11]);
        let layout = pool.ring_layout(64).expect("layout");
        let frames = layout.frames.expect("frames");
        assert_eq!(frames[0].offset, layout.tail);
        assert!(layout.tail > 0, "the ring should have wrapped");
        assert!(frames.iter().any(|f| f.offset < layout.tail));
        let last = frames.last().expect("last");
        assert_eq!(last.offset + last.len, layout.head);
    }

    #[test]
    fn uneven_wrap_keeps_the_oldest_frame_readable() {
        let (_dir, pool) = pool_with(4096, &[300; 15]);
        let layout = pool.ring_layout(64).expect("layout");
        let frames = layout.frames.expect("frames");
        assert!(layout.tail > 0, "the ring should have wrapped");
        assert!(frames.iter().any(|f| f.offset < layout.tail));
        let oldest = pool
            .info()
            .expect("info")
            .bounds
            .oldest_seq
            .expect("oldest");
        assert_eq!(frames.first().expect("first").seq, oldest);
        assert_eq!(pool.get(oldest).expect("read oldest").seq, oldest);
        crate::core::validate::validate_pool_state(pool.header(), pool.mmap()).expect("valid pool");
    }

    #[test]
    fn too_many_frames_are_not_listed() {
        let (_dir, pool) = pool_with(4096, &[10, 10, 10]);
        let layout = pool.ring_layout(2).expect("layout");
        assert_eq!(layout.frames, None);
    }
}
