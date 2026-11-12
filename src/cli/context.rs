//! Purpose: Carry the small set of process-wide values CLI handlers share.
//! Exports: `CliContext`.
//! Role: Make command dependencies explicit without a service container.

use super::output::OutputMode;
use crate::ColorMode;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct CliContext {
    pool_dir: Option<PathBuf>,
    color_mode: ColorMode,
    output_mode: OutputMode,
}

impl CliContext {
    pub(crate) fn new(
        pool_dir: Option<PathBuf>,
        color_mode: ColorMode,
        output_mode: OutputMode,
    ) -> Self {
        Self {
            pool_dir,
            color_mode,
            output_mode,
        }
    }

    pub(crate) fn pool_dir(&self) -> Result<&Path, plasmite::api::Error> {
        self.optional_pool_dir()
            .ok_or_else(super::support::missing_pool_dir)
    }

    pub(crate) fn optional_pool_dir(&self) -> Option<&Path> {
        self.pool_dir.as_deref()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::support::{resolve_pool_target, resolve_poolref};

    #[test]
    fn missing_home_only_blocks_directory_dependent_commands() {
        let context = CliContext::new(None, ColorMode::Never, OutputMode::Json);
        assert!(context.pool_dir().is_err());
        assert!(resolve_poolref("named", context.optional_pool_dir()).is_err());
        assert_eq!(
            resolve_poolref("./explicit.plasmite", context.optional_pool_dir()).unwrap(),
            PathBuf::from("./explicit.plasmite")
        );
        assert!(
            resolve_pool_target("http://localhost:9700/named", context.optional_pool_dir()).is_ok()
        );
        assert!(
            resolve_pool_target("udp://localhost:9701/named", context.optional_pool_dir()).is_ok()
        );
        super::super::utility::run(super::super::utility::UtilityCommand::Version, &context)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("explicit.plasmite");
        super::super::pool::run(
            crate::PoolCommand::Create {
                names: vec![path.to_str().unwrap().into()],
                size: None,
                index_capacity: None,
            },
            &context,
        )
        .unwrap();
        assert!(path.exists());
        super::super::doctor::run(
            super::super::doctor::DoctorArgs {
                pool: Some(path.to_str().unwrap().into()),
                all: false,
            },
            &context,
        )
        .unwrap();
        super::super::pool::run(
            crate::PoolCommand::Delete {
                names: vec![path.to_str().unwrap().into()],
            },
            &context,
        )
        .unwrap();
        assert!(!path.exists());
    }
}
