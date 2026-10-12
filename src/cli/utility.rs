//! Purpose: Execute small process-oriented CLI commands.
//! Exports: `UtilityCommand`, `run`.
//! Role: Own utility behavior without coupling it to storage commands.

use super::context::CliContext;
use super::output::emit_output;
use super::result::CommandResult;
use crate::Cli;
use crate::mcp_stdio;
use clap::CommandFactory;
use clap_complete::aot::Shell;
use plasmite::api::Error;
use serde_json::json;

pub(super) enum UtilityCommand {
    Version,
    Completion { shell: Shell },
    Mcp,
}

pub(super) fn run(command: UtilityCommand, context: &CliContext) -> Result<CommandResult, Error> {
    let output_mode = context.output_mode();
    match command {
        UtilityCommand::Version => {
            if !output_mode.is_structured() {
                super::output::write_stdout(
                    format!("plasmite {}\n", env!("PLASMITE_BUILD_VERSION")).as_bytes(),
                )?;
            } else {
                emit_output(
                    json!({
                        "name": "plasmite",
                        "version": env!("PLASMITE_BUILD_VERSION"),
                    }),
                    output_mode,
                )?;
            }
            Ok(CommandResult::ok())
        }
        UtilityCommand::Completion { shell } => {
            let mut command = Cli::command();
            let mut script = Vec::new();
            clap_complete::aot::generate(shell, &mut command, "plasmite", &mut script);
            super::output::write_stdout(&script)?;
            Ok(CommandResult::ok())
        }
        UtilityCommand::Mcp => {
            mcp_stdio::serve(context.pool_dir().to_path_buf())?;
            Ok(CommandResult::ok())
        }
    }
}
