//! Purpose: Represent a completed CLI command independently of process exit.
//! Exports: `CommandResult`.
//! Role: Let handlers select an exit code while `main` owns process termination.

#[derive(Copy, Clone, Debug)]
pub(crate) struct CommandResult {
    pub(crate) exit_code: i32,
}

impl CommandResult {
    pub(crate) fn ok() -> Self {
        Self { exit_code: 0 }
    }

    pub(crate) fn with_code(exit_code: i32) -> Self {
        Self { exit_code }
    }
}
