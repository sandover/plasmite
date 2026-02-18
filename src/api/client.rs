//! Purpose: Define the public API client surface for local pool resolution.
//! Exports: `PoolRef`, `LocalClient`, and local pool lifecycle operations.
//! Role: Stable boundary for bindings; mirrors CLI resolution rules.
//! Invariants: Pool resolution matches `spec/v0/SPEC.md` and is additive-only in v0.
//! Invariants: Remote pool refs are accepted but rejected at runtime in v0.
#![allow(clippy::result_large_err)]

use super::validation::validate_pool_state_report;
use super::{ValidationIssue, ValidationReport};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::{Pool, PoolInfo, PoolOptions};
use crate::pool_paths::{PoolNameResolveError, default_pool_dir, resolve_named_pool_path};
use std::path::{Path, PathBuf};

pub type ApiResult<T> = Result<T, Error>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PoolRef {
    Name(String),
    Path(PathBuf),
    Uri(String),
}

impl PoolRef {
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name(name.into())
    }

    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self::Path(path.into())
    }

    pub fn uri(uri: impl Into<String>) -> Self {
        Self::Uri(uri.into())
    }

    fn describe(&self) -> String {
        match self {
            PoolRef::Name(name) => name.clone(),
            PoolRef::Path(path) => path.to_string_lossy().to_string(),
            PoolRef::Uri(uri) => uri.clone(),
        }
    }

    fn resolve_local_path(&self, pool_dir: &Path) -> ApiResult<PathBuf> {
        match self {
            PoolRef::Name(name) => resolve_name(name, pool_dir),
            PoolRef::Path(path) => Ok(path.clone()),
            PoolRef::Uri(_) => Err(Error::new(ErrorKind::Usage)
                .with_message("remote pool refs are not supported in v0")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalClient {
    pool_dir: PathBuf,
}

impl LocalClient {
    pub fn new() -> Self {
        Self {
            pool_dir: default_pool_dir(),
        }
    }

    pub fn with_pool_dir(mut self, pool_dir: impl Into<PathBuf>) -> Self {
        self.pool_dir = pool_dir.into();
        self
    }

    pub fn pool_dir(&self) -> &Path {
        &self.pool_dir
    }

    pub fn create_pool(&self, pool_ref: &PoolRef, options: PoolOptions) -> ApiResult<PoolInfo> {
        let path = pool_ref.resolve_local_path(&self.pool_dir)?;
        let pool = Pool::create(&path, options)?;
        pool.info()
    }

    pub fn open_pool(&self, pool_ref: &PoolRef) -> ApiResult<Pool> {
        let path = pool_ref.resolve_local_path(&self.pool_dir)?;
        Pool::open(&path)
    }

    pub fn pool_info(&self, pool_ref: &PoolRef) -> ApiResult<PoolInfo> {
        let path = pool_ref.resolve_local_path(&self.pool_dir)?;
        let pool = Pool::open(&path)?;
        pool.info()
    }

    pub fn list_pools(&self) -> ApiResult<Vec<PoolInfo>> {
        let mut pools = Vec::new();
        let entries = std::fs::read_dir(&self.pool_dir).map_err(|err| {
            Error::new(map_io_error_kind(&err))
                .with_message("failed to read pool directory")
                .with_path(&self.pool_dir)
                .with_source(err)
        })?;

        for entry in entries {
            let entry = entry.map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read pool directory entry")
                    .with_path(&self.pool_dir)
                    .with_source(err)
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("plasmite") {
                continue;
            }
            let pool = Pool::open(&path)?;
            pools.push(pool.info()?);
        }

        Ok(pools)
    }

    pub fn delete_pool(&self, pool_ref: &PoolRef) -> ApiResult<()> {
        let path = pool_ref.resolve_local_path(&self.pool_dir)?;
        std::fs::remove_file(&path).map_err(|err| {
            Error::new(map_io_error_kind(&err))
                .with_message("failed to delete pool")
                .with_path(&path)
                .with_source(err)
        })
    }

    pub fn validate_pool(&self, pool_ref: &PoolRef) -> ApiResult<ValidationReport> {
        let path = pool_ref.resolve_local_path(&self.pool_dir)?;
        let pool = match Pool::open(&path) {
            Ok(pool) => pool,
            Err(err) if err.kind() == ErrorKind::Usage => {
                let message = err
                    .message()
                    .unwrap_or("unsupported pool format")
                    .to_string();
                let mut report = ValidationReport::corrupt(
                    path.clone(),
                    ValidationIssue {
                        code: "format".to_string(),
                        message,
                        seq: None,
                        offset: None,
                    },
                    None,
                );
                if let Some(hint) = err.hint() {
                    report.remediation_hints = vec![hint.to_string()];
                }
                return Ok(report.with_pool_ref(pool_ref.describe()));
            }
            Err(err) => return Err(err),
        };
        let header = pool.header_from_mmap()?;
        let report = validate_pool_state_report(header, pool.mmap(), &path)
            .with_pool_ref(pool_ref.describe());
        Ok(report)
    }
}

impl Default for LocalClient {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_name(name: &str, pool_dir: &Path) -> ApiResult<PathBuf> {
    resolve_named_pool_path(name, pool_dir).map_err(map_pool_name_resolve_error)
}

fn map_io_error_kind(err: &std::io::Error) -> ErrorKind {
    match err.kind() {
        std::io::ErrorKind::NotFound => ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorKind::Permission,
        _ => ErrorKind::Io,
    }
}

fn map_pool_name_resolve_error(err: PoolNameResolveError) -> Error {
    match err {
        PoolNameResolveError::ContainsPathSeparator => {
            Error::new(ErrorKind::Usage).with_message("pool name must not contain path separators")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalClient, PoolRef, resolve_name};
    use std::path::PathBuf;

    #[test]
    fn poolref_name_resolves_extension() {
        let pool_dir = PathBuf::from(".scratch/pools");
        let path = resolve_name("chat", &pool_dir).expect("path");
        assert_eq!(path, PathBuf::from(".scratch/pools/chat.plasmite"));
    }

    #[test]
    fn poolref_name_keeps_suffix() {
        let pool_dir = PathBuf::from(".scratch/pools");
        let path = resolve_name("chat.plasmite", &pool_dir).expect("path");
        assert_eq!(path, PathBuf::from(".scratch/pools/chat.plasmite"));
    }

    #[test]
    fn poolref_name_rejects_slash() {
        let pool_dir = PathBuf::from(".scratch/pools");
        let err = resolve_name("foo/bar", &pool_dir).expect_err("err");
        assert_eq!(err.kind(), super::ErrorKind::Usage);
    }

    #[cfg(windows)]
    #[test]
    fn poolref_name_rejects_backslash_on_windows() {
        let pool_dir = PathBuf::from(".scratch/pools");
        let err = resolve_name(r"foo\bar", &pool_dir).expect_err("err");
        assert_eq!(err.kind(), super::ErrorKind::Usage);
    }

    #[test]
    fn local_client_defaults_pool_dir() {
        let client = LocalClient::new();
        assert!(client.pool_dir().to_string_lossy().contains(".plasmite"));
    }

    #[test]
    fn poolref_uri_is_usage_error() {
        let client = LocalClient::new();
        let err = client
            .pool_info(&PoolRef::uri("tcp://example"))
            .expect_err("err");
        assert_eq!(err.kind(), super::ErrorKind::Usage);
    }
}
