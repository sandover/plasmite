//! Purpose: Execute small process-oriented CLI commands.
//! Exports: `UtilityCommand`, `run`.
//! Role: Own utility behavior without coupling it to storage commands.

use super::context::CliContext;
use super::output::emit_json;
use super::result::CommandResult;
use crate::Cli;
use crate::mcp_stdio;
use clap::CommandFactory;
use clap_complete::aot::Shell;
use plasmite::api::Error;
use serde_json::json;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

pub(super) enum UtilityCommand {
    Version,
    Completion {
        shell: Shell,
    },
    Mcp {
        pool_dir: Option<PathBuf>,
        remote: Option<String>,
    },
}

pub(super) fn run(command: UtilityCommand, context: &CliContext) -> Result<CommandResult, Error> {
    match command {
        UtilityCommand::Version => {
            if io::stdout().is_terminal() {
                println!("plasmite {}", env!("PLASMITE_BUILD_VERSION"));
            } else {
                emit_json(
                    json!({
                        "name": "plasmite",
                        "version": env!("PLASMITE_BUILD_VERSION"),
                    }),
                    context.color_mode(),
                );
            }
            Ok(CommandResult::ok())
        }
        UtilityCommand::Completion { shell } => {
            let mut command = Cli::command();
            clap_complete::aot::generate(shell, &mut command, "plasmite", &mut io::stdout());
            Ok(CommandResult::ok())
        }
        UtilityCommand::Mcp { pool_dir, remote } => {
            if let Some(remote) = remote {
                mcp_stdio::serve_remote(remote)?;
            } else {
                let pool_dir = pool_dir.unwrap_or_else(|| context.pool_dir().to_path_buf());
                mcp_stdio::serve(pool_dir)?;
            }
            Ok(CommandResult::ok())
        }
    }
}
