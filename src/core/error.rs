//! Purpose: Define stable error kinds and rich errors shared across core + CLI.
//! Exports: `ErrorKind`, `Error`, `to_exit_code`.
//! Role: Central error vocabulary for JSON stderr surfaces and exit code mapping.
//! Invariants: `ErrorKind` names are part of the v0.0.1 contract; change with care.
//! Invariants: `Error` may carry optional context (path/seq/offset) + a source error.
use std::error::Error as StdError;
use std::fmt;
use std::path::PathBuf;

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
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: Option<String>,
    hint: Option<String>,
    path: Option<PathBuf>,
    seq: Option<u64>,
    offset: Option<u64>,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl Error {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            message: None,
            hint: None,
            path: None,
            seq: None,
            offset: None,
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
        self.offset
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
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
        self.offset = Some(offset);
        self
    }

    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
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
        if let Some(offset) = self.offset {
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
        ];

        for (kind, code) in cases {
            assert_eq!(to_exit_code(kind), code);
        }
    }
}
