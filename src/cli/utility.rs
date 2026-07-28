//! Purpose: Execute small process-oriented CLI commands.
//! Exports: `UtilityCommand`, `run`.
//! Role: Own utility behavior without coupling it to storage commands.

use super::context::CliContext;
use super::output::emit_json;
use super::result::CommandResult;
use plasmite::api::Error;
use serde_json::json;
use std::io::{self, IsTerminal};

pub(super) enum UtilityCommand {
    Version,
}

pub(super) fn run(command: UtilityCommand, context: &CliContext) -> Result<CommandResult, Error> {
    match command {
        UtilityCommand::Version => {
            if io::stdout().is_terminal() {
                println!("plasmite {}", env!("CARGO_PKG_VERSION"));
            } else {
                emit_json(
                    json!({
                        "name": "plasmite",
                        "version": env!("CARGO_PKG_VERSION"),
                    }),
                    context.color_mode(),
                );
            }
            Ok(CommandResult::ok())
        }
    }
}
