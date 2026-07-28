//! Purpose: Own CLI execution boundaries below argument parsing.
//! Exports: `CliContext`, `CommandResult`, and `dispatch`.
//! Role: Route parsed commands to cohesive command-family modules.
//! Invariants: Command modules depend on explicit context and output helpers.

mod context;
mod doctor;
pub(super) mod output;
mod pool;
mod result;
mod utility;

pub(super) use context::CliContext;
pub(super) use result::CommandResult;

use crate::Command;
use plasmite::api::Error;

pub(super) fn dispatch(command: Command, context: CliContext) -> Result<CommandResult, Error> {
    match command {
        Command::Version => utility::run(utility::UtilityCommand::Version, &context),
        Command::Doctor { pool, all, json } => {
            doctor::run(doctor::DoctorArgs { pool, all, json }, &context)
        }
        Command::Pool { command } => pool::run(command, &context),
        command => crate::command_dispatch::dispatch_command(
            command,
            context.pool_dir().to_path_buf(),
            context.color_mode(),
        ),
    }
}
