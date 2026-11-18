//! Purpose: Define the public API client surface for local pool resolution.
//! Exports: `PoolRef`, `LocalClient`, and local pool lifecycle operations.
//! Role: Stable boundary for bindings; mirrors CLI resolution rules.
//! Invariants: Pool resolution matches `spec/v0/SPEC.md` and is additive-only in v0.
//! Invariants: Remote pool refs are accepted but rejected at runtime in v0.
#![allow(clippy::result_large_err)]

use super::validation::validate_pool_state_report;
use super::{
    ValidationIssue, ValidationMessageStats, ValidationObservation, ValidationReport,
    ValidationStatus,
};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::{Pool, PoolInfo, PoolOptions};
use crate::pool_paths::{PoolNameResolveError, default_pool_dir, resolve_named_pool_path};
use std::path::{Path, PathBuf};

pub type ApiResult<T> = Result<T, Error>;

pub const DEFAULT_POOL_SIZE: u64 = 1024 * 1024;

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
                .with_message("a LocalClient requires a local pool reference")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalClient {
    pool_dir: Option<PathBuf>,
}

impl LocalClient {
    pub fn new() -> Self {
        Self {
            pool_dir: default_pool_dir(),
        }
    }

    pub fn with_pool_dir(mut self, pool_dir: impl Into<PathBuf>) -> Self {
        self.pool_dir = Some(pool_dir.into());
        self
    }

    /// Return the selected directory, or require an explicit directory when no home exists.
    pub fn pool_dir(&self) -> ApiResult<&Path> {
        self.pool_dir.as_deref().ok_or_else(|| {
            Error::new(ErrorKind::Usage)
                .with_message("cannot determine the user home directory")
                .with_hint("Set an absolute home directory or supply an explicit pool directory.")
        })
    }

    fn resolve_pool_path(&self, pool_ref: &PoolRef) -> ApiResult<PathBuf> {
        match pool_ref {
            PoolRef::Path(path) => Ok(path.clone()),
            PoolRef::Uri(_) => Err(Error::new(ErrorKind::Usage)
                .with_message("a LocalClient requires a local pool reference")),
            PoolRef::Name(_) => pool_ref.resolve_local_path(self.pool_dir()?),
        }
    }

    pub fn create_pool(&self, pool_ref: &PoolRef, options: PoolOptions) -> ApiResult<Pool> {
        let path = self.resolve_pool_path(pool_ref)?;
        Pool::create(&path, options)
    }

    pub fn open_pool(&self, pool_ref: &PoolRef, create: bool) -> ApiResult<Pool> {
        let path = self.resolve_pool_path(pool_ref)?;
        match Pool::open(&path) {
            Err(error) if create && error.kind() == ErrorKind::NotFound => {
                match Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE)) {
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => Pool::open(&path),
                    result => result,
                }
            }
            result => result,
        }
    }

    pub fn pool_info(&self, pool_ref: &PoolRef) -> ApiResult<PoolInfo> {
        self.open_pool(pool_ref, false)?.info()
    }

    pub fn list_pools(&self) -> ApiResult<Vec<PoolInfo>> {
        let mut pools = Vec::new();
        let pool_dir = self.pool_dir()?;
        let entries = match std::fs::read_dir(pool_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(pools),
            Err(err) => {
                return Err(Error::new(map_io_error_kind(&err))
                    .with_message("failed to read pool directory")
                    .with_path(pool_dir)
                    .with_source(err));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read pool directory entry")
                    .with_path(pool_dir)
                    .with_source(err)
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("plasmite") {
                continue;
            }
            // A pool deleted after the directory read is no longer part of the list.
            let pool = match Pool::open(&path) {
                Ok(pool) => pool,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            pools.push(pool.info()?);
        }

        Ok(pools)
    }

    pub fn delete_pool(&self, pool_ref: &PoolRef) -> ApiResult<()> {
        let path = self.resolve_pool_path(pool_ref)?;
        crate::core::pool::delete_pool_file(&path)
    }

    pub fn validate_pool(&self, pool_ref: &PoolRef) -> ApiResult<ValidationReport> {
        Ok(self.validate_pool_observation(pool_ref)?.report)
    }

    /// Capture retained message facts under the validation scan's shared lock.
    /// Later appends, deletion or replacement do not update this observation.
    pub fn validate_pool_observation(
        &self,
        pool_ref: &PoolRef,
    ) -> ApiResult<ValidationObservation> {
        let path = self.resolve_pool_path(pool_ref)?;
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
                return Ok(ValidationObservation {
                    report: report.with_pool_ref(pool_ref.describe()),
                    messages: None,
                });
            }
            Err(err) => return Err(err),
        };
        let _lock = pool.read_lock()?;
        let header = pool.header_locked()?;
        let report = validate_pool_state_report(header, pool.mmap(), &path)
            .with_pool_ref(pool_ref.describe());
        let messages = if report.status == ValidationStatus::Ok {
            let (oldest_seq, newest_seq, count) = if header.oldest_seq == 0 {
                (None, None, 0)
            } else {
                (
                    Some(header.oldest_seq),
                    Some(header.newest_seq),
                    header.newest_seq - header.oldest_seq + 1,
                )
            };
            Some(ValidationMessageStats {
                count,
                oldest_seq,
                newest_seq,
            })
        } else {
            None
        };
        Ok(ValidationObservation { report, messages })
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
        PoolNameResolveError::InvalidName => Error::new(ErrorKind::Usage)
            .with_message("pool name must be nonempty and must not contain NUL or a path prefix"),
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
    fn missing_home_requires_directory_only_for_named_operations() {
        let client = LocalClient { pool_dir: None };
        assert_eq!(
            client.list_pools().unwrap_err().kind(),
            super::ErrorKind::Usage
        );
        assert!(
            client
                .resolve_pool_path(&super::PoolRef::name("work"))
                .is_err()
        );
        assert_eq!(
            client
                .resolve_pool_path(&super::PoolRef::path("./work.plasmite"))
                .unwrap(),
            PathBuf::from("./work.plasmite")
        );
        assert_eq!(
            client.with_pool_dir("./chosen").pool_dir().unwrap(),
            std::path::Path::new("./chosen")
        );
    }

    #[test]
    fn local_client_defaults_pool_dir() {
        let client = LocalClient::new();
        assert!(
            client
                .pool_dir()
                .unwrap()
                .to_string_lossy()
                .contains(".plasmite")
        );
    }

    #[test]
    fn pool_admin_lists_only_pools_and_reports_deleted_paths() {
        let temp = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(temp.path());
        let keep = PoolRef::name("keep");
        let delete = PoolRef::name("delete");
        for pool in [&keep, &delete] {
            client
                .create_pool(pool, super::PoolOptions::new(1024 * 1024))
                .unwrap();
        }
        std::fs::write(temp.path().join("notes.txt"), b"ignore this").unwrap();

        let info = client.pool_info(&keep).unwrap();
        assert_eq!(info.file_size, 1024 * 1024);
        assert_eq!(info.bounds.oldest_seq, None);
        assert_eq!(info.bounds.newest_seq, None);
        assert_eq!(client.list_pools().unwrap().len(), 2);

        client.delete_pool(&delete).unwrap();
        let listed = client.list_pools().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, info.path);
        for error in [
            client.pool_info(&delete).unwrap_err(),
            client.delete_pool(&delete).unwrap_err(),
        ] {
            assert_eq!(error.kind(), super::ErrorKind::NotFound);
            assert_eq!(
                error.path(),
                Some(temp.path().join("delete.plasmite").as_path())
            );
        }
    }

    #[test]
    fn list_pools_returns_empty_when_directory_does_not_exist() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pool_dir = temp.path().join("missing");
        let client = LocalClient::new().with_pool_dir(&pool_dir);

        assert_eq!(client.list_pools().expect("list pools").len(), 0);
        assert!(!pool_dir.exists());
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
