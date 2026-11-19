//! Purpose: Define stable error kinds and rich errors shared across core + CLI.
//! Exports: `ErrorKind`, `Error`, `to_exit_code`.
//! Role: Central error vocabulary for JSON stderr surfaces and exit code mapping.
//! Invariants: `ErrorKind` names are part of the 2.0 contract; change with care.
//! Invariants: `Error` may carry optional context (path/seq/offset) + a source error.
use std::error::Error as StdError;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Internal,
    Usage,
    NotFound,
    AlreadyExists,
    Busy,
    Permission,
    Corrupt,
    Io,
    RetentionGap,
}

#[derive(Clone, Debug)]
pub struct Error {
    kind: ErrorKind,
    message: Option<Box<str>>,
    hint: Option<Box<str>>,
    path: Option<PathBuf>,
    seq: Option<u64>,
    detail: ErrorDetail,
    source: Option<Arc<dyn StdError + Send + Sync>>,
}

#[derive(Clone, Copy, Debug)]
enum ErrorDetail {
    None,
    Offset(u64),
    NextSeq(u64),
}

impl Error {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            message: None,
            hint: None,
            path: None,
            seq: None,
            detail: ErrorDetail::None,
            source: None,
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    pub fn seq(&self) -> Option<u64> {
        self.seq
    }

    pub fn offset(&self) -> Option<u64> {
        match self.detail {
            ErrorDetail::Offset(offset) => Some(offset),
            _ => None,
        }
    }

    /// First requested message that the pool no longer retains.
    pub fn missing(&self) -> Option<u64> {
        (self.kind == ErrorKind::RetentionGap)
            .then_some(self.seq)
            .flatten()
    }

    /// First sequence the pool retained when it detected a gap.
    pub fn next_seq(&self) -> Option<u64> {
        match self.detail {
            ErrorDetail::NextSeq(next_seq) => Some(next_seq),
            _ => None,
        }
    }

    #[doc(hidden)]
    pub fn retention_gap(missing: u64, next_seq: u64) -> Self {
        let last_missing = next_seq.saturating_sub(1);
        let mut error = Self::new(ErrorKind::RetentionGap)
            .with_message(format!(
                "messages {missing}-{last_missing} are no longer retained; the next available message is {next_seq}"
            ))
            .with_hint("Reopen at the next available sequence or rebuild from an authoritative source.")
            .with_seq(missing);
        error.detail = ErrorDetail::NextSeq(next_seq);
        error
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into().into_boxed_str());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into().into_boxed_str());
        self
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_seq(mut self, seq: u64) -> Self {
        self.seq = Some(seq);
        self
    }

    pub fn with_offset(mut self, offset: u64) -> Self {
        self.detail = ErrorDetail::Offset(offset);
        self
    }

    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Arc::new(source));
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kind)?;
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        if let Some(path) = &self.path {
            write!(f, " (path: {})", path.display())?;
        }
        if let Some(seq) = self.seq {
            write!(f, " (seq: {seq})")?;
        }
        if let Some(offset) = self.offset() {
            write!(f, " (offset: {offset})")?;
        }
        Ok(())
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn StdError + 'static))
    }
}

pub fn to_exit_code(kind: ErrorKind) -> i32 {
    match kind {
        ErrorKind::Internal => 1,
        ErrorKind::Usage => 2,
        ErrorKind::NotFound => 3,
        ErrorKind::AlreadyExists => 4,
        ErrorKind::Busy => 5,
        ErrorKind::Permission => 6,
        ErrorKind::Corrupt => 7,
        ErrorKind::Io => 8,
        ErrorKind::RetentionGap => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorKind, to_exit_code};

    #[test]
    fn exit_code_mapping_is_stable() {
        let cases = [
            (ErrorKind::Internal, 1),
            (ErrorKind::Usage, 2),
            (ErrorKind::NotFound, 3),
            (ErrorKind::AlreadyExists, 4),
            (ErrorKind::Busy, 5),
            (ErrorKind::Permission, 6),
            (ErrorKind::Corrupt, 7),
            (ErrorKind::Io, 8),
            (ErrorKind::RetentionGap, 9),
        ];

        for (kind, code) in cases {
            assert_eq!(to_exit_code(kind), code);
        }
    }
}
