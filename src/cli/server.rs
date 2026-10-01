//! Purpose: Run secure serving from the CLI.
//! Exports: `run`.
//! Role: Pass the selected pool directory and serve options to the server runtime.

use super::args::ServeSubcommand;
use super::context::CliContext;
use super::result::CommandResult;
use crate::ServeRunArgs;
use plasmite::api::Error;

pub(super) fn run(
    command: Option<ServeSubcommand>,
    run: ServeRunArgs,
    context: &CliContext,
) -> Result<CommandResult, Error> {
    match command {
        Some(ServeSubcommand::Status { json }) => {
            let servers = crate::serve_registry::running()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&servers).expect("server details serialize")
                );
            } else if servers.is_empty() {
                println!("No Plasmite servers running.");
            } else {
                let directories: Vec<_> = servers
                    .iter()
                    .map(|server| {
                        super::output_support::display_pool_dir_for_humans(&server.pool_dir)
                    })
                    .collect();
                let directory_width = directories
                    .iter()
                    .map(String::len)
                    .max()
                    .unwrap_or(0)
                    .max(9);
                let local_width = servers
                    .iter()
                    .map(|server| super::output_support::human_literal(&server.local_url).len())
                    .max()
                    .unwrap_or(0)
                    .max(9);
                let remote_width = servers
                    .iter()
                    .filter_map(|server| server.remote_url.as_ref())
                    .map(|url| super::output_support::human_literal(url).len())
                    .max()
                    .unwrap_or(0)
                    .max(12);
                println!(
                    "{:<directory_width$}  {:<local_width$}  {:<remote_width$}  PID",
                    "DIRECTORY", "LOCAL URL", "SHARED HTTPS"
                );
                for (server, directory) in servers.into_iter().zip(directories) {
                    println!(
                        "{:<directory_width$}  {:<local_width$}  {:<remote_width$}  {}",
                        directory,
                        super::output_support::human_literal(&server.local_url),
                        super::output_support::human_literal(
                            server.remote_url.as_deref().unwrap_or("—")
                        ),
                        server.pid
                    );
                }
            }
        }
        None => crate::secure_serve::run(context.pool_dir(), &run)?,
    }
    Ok(CommandResult::ok())
}
