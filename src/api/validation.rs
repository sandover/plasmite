//! Purpose: Provide a stable, serializable validation report model.
//! Exports: `ValidationReport`, `ValidationStatus`, `ValidationIssue`.
//! Role: Shared contract for CLI diagnostics, API users, and future servers.
//! Invariants: Reports are additive-only in v0; no heavy payloads are embedded.
//! Invariants: Snapshot paths are optional and only provided on request.

use crate::core::frame::{self, FRAME_HEADER_LEN, FrameHeader, FrameState};
use crate::core::pool::PoolHeader;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationStatus {
    Ok,
    Corrupt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    pub seq: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationReport {
    pub pool_ref: Option<String>,
    pub path: PathBuf,
    pub status: ValidationStatus,
    pub last_good_seq: Option<u64>,
    pub issues: Vec<ValidationIssue>,
    pub issue_count: usize,
    pub remediation_hints: Vec<String>,
    pub snapshot_path: Option<PathBuf>,
}

impl ValidationReport {
    pub fn ok(path: PathBuf) -> Self {
        Self {
            pool_ref: None,
            path,
            status: ValidationStatus::Ok,
            last_good_seq: None,
            issues: Vec::new(),
            issue_count: 0,
            remediation_hints: Vec::new(),
            snapshot_path: None,
        }
    }

    pub fn corrupt(path: PathBuf, issue: ValidationIssue, last_good_seq: Option<u64>) -> Self {
        let remediation_hints = vec![
            "Pool appears corrupt. Consider recreating it or running diagnostics.".to_string(),
        ];
        Self {
            pool_ref: None,
            path,
            status: ValidationStatus::Corrupt,
            last_good_seq,
            issues: vec![issue],
            issue_count: 1,
            remediation_hints,
            snapshot_path: None,
        }
    }

    pub fn with_pool_ref(mut self, pool_ref: impl Into<String>) -> Self {
        self.pool_ref = Some(pool_ref.into());
        self
    }

    pub fn with_snapshot(mut self, path: impl Into<PathBuf>) -> Self {
        self.snapshot_path = Some(path.into());
        self
    }

    pub fn set_issues(mut self, issues: Vec<ValidationIssue>) -> Self {
        self.issue_count = issues.len();
        self.issues = issues;
        self.status = if self.issue_count == 0 {
            ValidationStatus::Ok
        } else {
            ValidationStatus::Corrupt
        };
        self
    }

    fn set_last_good(mut self, seq: Option<u64>) -> Self {
        self.last_good_seq = seq;
        self
    }
}

pub(crate) fn validate_pool_state_report(
    header: PoolHeader,
    mmap: &[u8],
    path: &Path,
) -> ValidationReport {
    let ring_offset = header.ring_offset as usize;
    let ring_size = header.ring_size as usize;

    if ring_size == 0 {
        return ValidationReport::corrupt(
            path.to_path_buf(),
            issue("corrupt", "ring size is zero", None, None),
            None,
        );
    }
    if ring_offset + ring_size > mmap.len() {
        return ValidationReport::corrupt(
            path.to_path_buf(),
            issue("corrupt", "ring exceeds mmap bounds", None, None),
            None,
        );
    }
    let head = header.head_off as usize;
    let tail = header.tail_off as usize;
    if head >= ring_size || tail >= ring_size {
        return ValidationReport::corrupt(
            path.to_path_buf(),
            issue("corrupt", "head/tail out of range", None, None),
            None,
        );
    }
    if header.oldest_seq == 0 {
        if header.head_off != header.tail_off {
            return ValidationReport::corrupt(
                path.to_path_buf(),
                issue("corrupt", "empty pool head/tail mismatch", None, None),
                None,
            );
        }
        return ValidationReport::ok(path.to_path_buf());
    }
    if header.newest_seq < header.oldest_seq {
        return ValidationReport::corrupt(
            path.to_path_buf(),
            issue("corrupt", "seq bounds inverted", None, None),
            None,
        );
    }

    let mut offset = tail;
    let mut expected_seq = header.oldest_seq;
    let max_frames = ring_size / FRAME_HEADER_LEN + 1;
    let mut steps = 0usize;
    let mut last_good_seq = None;

    loop {
        if steps > max_frames {
            return ValidationReport::corrupt(
                path.to_path_buf(),
                issue(
                    "corrupt",
                    "scan exceeded ring capacity",
                    last_good_seq,
                    None,
                ),
                last_good_seq,
            );
        }
        if ring_size - offset < FRAME_HEADER_LEN {
            offset = 0;
            steps += 1;
            continue;
        }
        let frame = match read_frame_header(mmap, ring_offset, offset) {
            Ok(frame) => frame,
            Err(err) => {
                return ValidationReport::corrupt(
                    path.to_path_buf(),
                    issue(
                        "corrupt",
                        &format!("frame header decode failed: {err}"),
                        last_good_seq,
                        Some(offset as u64),
                    ),
                    last_good_seq,
                );
            }
        };
        if let Err(err) = frame.validate(ring_size) {
            return ValidationReport::corrupt(
                path.to_path_buf(),
                issue(
                    "corrupt",
                    &format!("frame header invalid: {err}"),
                    last_good_seq,
                    Some(offset as u64),
                ),
                last_good_seq,
            );
        }
        match frame.state {
            FrameState::Wrap => {
                offset = 0;
                steps += 1;
                continue;
            }
            FrameState::Committed => {}
            _ => {
                return ValidationReport::corrupt(
                    path.to_path_buf(),
                    issue(
                        "corrupt",
                        "unexpected frame state",
                        last_good_seq,
                        Some(offset as u64),
                    ),
                    last_good_seq,
                );
            }
        }

        if frame.seq != expected_seq {
            return ValidationReport::corrupt(
                path.to_path_buf(),
                issue(
                    "corrupt",
                    "seq mismatch",
                    last_good_seq,
                    Some(offset as u64),
                ),
                last_good_seq,
            );
        }

        let frame_len = match frame::frame_total_len(FRAME_HEADER_LEN, frame.payload_len as usize) {
            Some(len) => len,
            None => {
                return ValidationReport::corrupt(
                    path.to_path_buf(),
                    issue(
                        "corrupt",
                        "frame length overflow",
                        last_good_seq,
                        Some(offset as u64),
                    ),
                    last_good_seq,
                );
            }
        };
        if offset + frame_len > ring_size {
            return ValidationReport::corrupt(
                path.to_path_buf(),
                issue(
                    "corrupt",
                    "frame exceeds ring",
                    last_good_seq,
                    Some(offset as u64),
                ),
                last_good_seq,
            );
        }
        let mut next_off = offset + frame_len;
        if next_off == ring_size {
            next_off = 0;
        }

        last_good_seq = Some(frame.seq);

        if expected_seq == header.newest_seq {
            if next_off != head {
                return ValidationReport::corrupt(
                    path.to_path_buf(),
                    issue(
                        "corrupt",
                        "head offset mismatch",
                        last_good_seq,
                        Some(offset as u64),
                    ),
                    last_good_seq,
                );
            }
            break;
        }

        expected_seq += 1;
        offset = next_off;
        steps += 1;
    }

    ValidationReport::ok(path.to_path_buf()).set_last_good(last_good_seq)
}

fn issue(code: &str, message: &str, seq: Option<u64>, offset: Option<u64>) -> ValidationIssue {
    ValidationIssue {
        code: code.to_string(),
        message: message.to_string(),
        seq,
        offset,
    }
}

fn read_frame_header(mmap: &[u8], ring_offset: usize, head: usize) -> Result<FrameHeader, String> {
    let start = ring_offset + head;
    let end = start + FRAME_HEADER_LEN;
    FrameHeader::decode(&mmap[start..end]).map_err(|err| err.to_string())
}
