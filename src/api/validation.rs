//! Purpose: Provide a stable, serializable validation report model.
//! Exports: `ValidationReport`, `ValidationStatus`, `ValidationIssue`.
//! Role: Shared contract for CLI diagnostics, API users, and future servers.
//! Invariants: Reports are additive-only in v0; no heavy payloads are embedded.
//! Invariants: Snapshot paths are optional and only provided on request.

use crate::core::frame::{FRAME_HEADER_LEN, FrameHeader, FrameState};
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
    match crate::core::validate::scan_pool_state(header, mmap) {
        Ok(last_good_seq) => {
            let mut report = ValidationReport::ok(path.to_path_buf()).set_last_good(last_good_seq);
            for warning in spot_check_index_warnings(header, mmap) {
                report.remediation_hints.push(format!("warning: {warning}"));
            }
            report
        }
        Err(err) => ValidationReport::corrupt(
            path.to_path_buf(),
            issue(
                "corrupt",
                err.message().unwrap_or("pool state is invalid"),
                err.seq(),
                err.offset(),
            ),
            err.seq(),
        ),
    }
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

fn spot_check_index_warnings(header: PoolHeader, mmap: &[u8]) -> Vec<String> {
    if header.index_capacity == 0 {
        return Vec::new();
    }

    let ring_offset = header.ring_offset as usize;
    let ring_size = header.ring_size as usize;
    let index_offset = header.index_offset as usize;
    let index_slots = header.index_capacity as usize;
    let index_bytes = index_slots.saturating_mul(16);
    if index_offset + index_bytes > ring_offset {
        return vec!["index bounds overlap ring".to_string()];
    }

    let mut sample_slots = vec![0usize];
    if index_slots > 1 {
        sample_slots.push(index_slots / 2);
        sample_slots.push(index_slots - 1);
    }
    sample_slots.sort_unstable();
    sample_slots.dedup();

    let mut warnings = Vec::new();
    for slot in sample_slots {
        let entry_off = index_offset + slot * 16;
        let seq = u64::from_le_bytes(mmap[entry_off..entry_off + 8].try_into().unwrap_or([0; 8]));
        let offset = u64::from_le_bytes(
            mmap[entry_off + 8..entry_off + 16]
                .try_into()
                .unwrap_or([0; 8]),
        );
        if seq == 0 {
            continue;
        }
        if offset as usize >= ring_size {
            warnings.push(format!(
                "index slot {slot} seq {seq} points outside ring at offset {offset}"
            ));
            continue;
        }
        if ring_size - (offset as usize) < FRAME_HEADER_LEN {
            warnings.push(format!("index slot {slot} seq {seq} is stale or invalid"));
            continue;
        }
        let frame = read_frame_header(mmap, ring_offset, offset as usize);
        match frame {
            Ok(frame) if frame.state == FrameState::Committed && frame.seq == seq => {}
            _ => warnings.push(format!("index slot {slot} seq {seq} is stale or invalid")),
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::{ValidationStatus, validate_pool_state_report};
    use crate::core::frame::FRAME_HEADER_LEN;
    use crate::core::pool::{Pool, PoolOptions};

    #[test]
    fn validation_report_ok_for_empty_pool() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("empty.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        let header = pool.header_from_mmap().expect("header");

        let report = validate_pool_state_report(header, pool.mmap(), &path);
        assert_eq!(report.status, ValidationStatus::Ok);
        assert_eq!(report.issue_count, 0);
        assert!(report.issues.is_empty());
        assert_eq!(report.last_good_seq, None);
        assert_eq!(report.path, path);
    }

    #[test]
    fn validation_report_marks_corrupt_header() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("corrupt.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        let mut header = pool.header_from_mmap().expect("header");
        header.ring_size = 0;

        let report = validate_pool_state_report(header, pool.mmap(), &path);
        assert_eq!(report.status, ValidationStatus::Corrupt);
        assert_eq!(report.issue_count, 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.last_good_seq, None);
        assert_eq!(report.path, path);
    }

    #[test]
    fn validation_report_rejects_missing_commit_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("missing-marker.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        let payload = b"hello";
        pool.append(payload).expect("append");
        let header = pool.header_from_mmap().expect("header");
        let mut bytes = pool.mmap().to_vec();
        let marker = header.ring_offset as usize + FRAME_HEADER_LEN + payload.len();
        bytes[marker] = 0;

        let report = validate_pool_state_report(header, &bytes, &path);
        assert_eq!(report.status, ValidationStatus::Corrupt);
        assert!(report.issues[0].message.contains("commit marker"));
    }

    #[test]
    fn validation_report_rejects_wrong_tail_next_offset() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("wrong-tail-next.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).expect("create");
        pool.append(b"hello").expect("append");
        let mut header = pool.header_from_mmap().expect("header");
        assert_ne!(header.tail_next_off, 0);
        header.tail_next_off = 0;

        let report = validate_pool_state_report(header, pool.mmap(), &path);
        assert_eq!(report.status, ValidationStatus::Corrupt);
        assert!(report.issues[0].message.contains("tail_next mismatch"));
    }

    #[test]
    fn validation_report_rejects_tail_on_wrap_marker() {
        use crate::core::frame::{self, FRAME_HEADER_LEN};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tail-on-wrap.plasmite");
        let frame_len = frame::frame_total_len(FRAME_HEADER_LEN, 5).expect("frame len");
        let ring_size = frame_len * 3 + FRAME_HEADER_LEN;
        let mut pool = Pool::create(
            &path,
            PoolOptions::new(4096 + ring_size as u64).with_index_capacity(0),
        )
        .expect("create");
        for _ in 0..4 {
            pool.append(b"hello").expect("append");
        }

        let mut header = pool.header_from_mmap().expect("header");
        header.tail_off = (frame_len * 3) as u64;
        header.tail_next_off = 0;
        let report = validate_pool_state_report(header, pool.mmap(), &path);
        assert_eq!(report.status, ValidationStatus::Corrupt);
        assert!(
            report.issues[0]
                .message
                .contains("tail frame is not committed")
        );
    }

    #[test]
    fn validation_report_warns_for_invalid_index_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("bad-index.plasmite");
        let mut pool = Pool::create(&path, PoolOptions::new(1024 * 1024).with_index_capacity(1))
            .expect("create");
        pool.append(b"hello").expect("append");
        let header = pool.header_from_mmap().expect("header");
        let mut bytes = pool.mmap().to_vec();
        let slot_offset = header.index_offset as usize + 8;

        bytes[slot_offset..slot_offset + 8].copy_from_slice(&header.ring_size.to_le_bytes());
        let outside = validate_pool_state_report(header, &bytes, &path);
        assert_eq!(outside.status, ValidationStatus::Ok);
        assert!(outside.remediation_hints[0].contains("points outside ring"));

        bytes[slot_offset..slot_offset + 8].copy_from_slice(&8u64.to_le_bytes());
        let stale = validate_pool_state_report(header, &bytes, &path);
        assert_eq!(stale.status, ValidationStatus::Ok);
        assert!(stale.remediation_hints[0].contains("stale or invalid"));

        bytes[slot_offset..slot_offset + 8].copy_from_slice(&(header.ring_size - 1).to_le_bytes());
        let near_end = validate_pool_state_report(header, &bytes, &path);
        assert_eq!(near_end.status, ValidationStatus::Ok);
        assert!(near_end.remediation_hints[0].contains("stale or invalid"));
    }
}
