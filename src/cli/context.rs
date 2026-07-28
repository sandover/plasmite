//! Purpose: Carry the small set of process-wide values CLI handlers share.
//! Exports: `CliContext`.
//! Role: Make command dependencies explicit without a service container.

use crate::ColorMode;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct CliContext {
    pool_dir: PathBuf,
    color_mode: ColorMode,
}

impl CliContext {
    pub(crate) fn new(pool_dir: PathBuf, color_mode: ColorMode) -> Self {
        Self {
            pool_dir,
            color_mode,
        }
    }

    pub(crate) fn pool_dir(&self) -> &Path {
        &self.pool_dir
    }

    pub(crate) fn color_mode(&self) -> ColorMode {
        self.color_mode
    }
}
