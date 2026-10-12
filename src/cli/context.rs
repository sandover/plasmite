//! Purpose: Carry the small set of process-wide values CLI handlers share.
//! Exports: `CliContext`.
//! Role: Make command dependencies explicit without a service container.

use super::output::OutputMode;
use crate::ColorMode;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct CliContext {
    pool_dir: PathBuf,
    color_mode: ColorMode,
    output_mode: OutputMode,
}

impl CliContext {
    pub(crate) fn new(pool_dir: PathBuf, color_mode: ColorMode, output_mode: OutputMode) -> Self {
        Self {
            pool_dir,
            color_mode,
            output_mode,
        }
    }

    pub(crate) fn pool_dir(&self) -> &Path {
        &self.pool_dir
    }

    pub(crate) fn output_mode(&self) -> OutputMode {
        self.output_mode
    }

    pub(crate) fn structured_output(&self) -> bool {
        self.output_mode.is_structured()
    }

    pub(crate) fn color_mode(&self) -> ColorMode {
        if self.structured_output() {
            ColorMode::Never
        } else {
            self.color_mode
        }
    }
}
