//! Purpose: Shared local pool-directory and pool-name path resolution helpers.
//! Exports: `default_pool_dir` and `resolve_named_pool_path`.
//! Role: Keep CLI and API-client path semantics aligned from one source.
//! Invariants: Default pool directory remains `~/.plasmite/pools`.
//! Invariants: Named pool refs must not contain path separators.

use std::path::{Path, PathBuf};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PoolNameResolveError {
    ContainsPathSeparator,
}

pub(crate) fn default_pool_dir() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".plasmite").join("pools")
}

pub(crate) fn resolve_named_pool_path(
    name: &str,
    pool_dir: &Path,
) -> Result<PathBuf, PoolNameResolveError> {
    if name.contains('/') {
        return Err(PoolNameResolveError::ContainsPathSeparator);
    }
    if name.ends_with(".plasmite") {
        return Ok(pool_dir.join(name));
    }
    Ok(pool_dir.join(format!("{name}.plasmite")))
}
