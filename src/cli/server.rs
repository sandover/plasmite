//! Purpose: Run secure serving from the CLI.
//! Exports: `run`.
//! Role: Pass the selected pool directory and serve options to the server runtime.

use super::context::CliContext;
use super::result::CommandResult;
use crate::ServeRunArgs;
use plasmite::api::Error;

pub(super) fn run(run: ServeRunArgs, context: &CliContext) -> Result<CommandResult, Error> {
    crate::secure_serve::run(context.pool_dir(), &run)?;
    Ok(CommandResult::ok())
}
