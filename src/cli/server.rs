//! Run foreground servers and manage installed startup jobs.

use super::args::ServeSubcommand;
use super::context::CliContext;
use super::output_support::{display_pool_dir_for_humans, emit_table, human_literal};
use super::result::CommandResult;
use crate::ServeRunArgs;
use crate::serve_service::{self, Status};
use plasmite::api::Error;

pub(super) fn run(
    command: Option<ServeSubcommand>,
    run: ServeRunArgs,
    context: &CliContext,
) -> Result<CommandResult, Error> {
    match command {
        None => crate::secure_serve::run(context.pool_dir(), &run)?,
        Some(ServeSubcommand::Install { run, .. }) => emit_action(
            "install",
            serve_service::install(context.pool_dir(), &run)?,
            context.json_output(),
        ),
        Some(ServeSubcommand::Start { .. }) => emit_action(
            "start",
            serve_service::control(context.pool_dir(), "start")?,
            context.json_output(),
        ),
        Some(ServeSubcommand::Stop { .. }) => emit_action(
            "stop",
            serve_service::control(context.pool_dir(), "stop")?,
            context.json_output(),
        ),
        Some(ServeSubcommand::Restart { .. }) => emit_action(
            "restart",
            serve_service::control(context.pool_dir(), "restart")?,
            context.json_output(),
        ),
        Some(ServeSubcommand::Uninstall { .. }) => emit_action(
            "uninstall",
            serve_service::control(context.pool_dir(), "uninstall")?,
            context.json_output(),
        ),
        Some(ServeSubcommand::Logs { tail, follow, .. }) => serve_service::logs(
            context.pool_dir(),
            tail.unwrap_or(50),
            follow,
            context.json_output(),
        )?,
        Some(ServeSubcommand::Status { all: true, .. }) => {
            let (servers, errors) = serve_service::all()?;
            if context.json_output() {
                println!(
                    "{}",
                    serde_json::to_string(&servers).expect("status serializes")
                );
            } else if servers.is_empty() {
                println!("No Plasmite servers or installed setups.");
            } else {
                let rows = servers
                    .iter()
                    .map(|server| {
                        vec![
                            display_pool_dir_for_humans(&server.pool_dir),
                            if server.managed {
                                "installed"
                            } else {
                                "foreground"
                            }
                            .into(),
                            server.state.clone(),
                            if server.startup { "yes" } else { "no" }.into(),
                            server.remote_url.as_deref().unwrap_or("—").into(),
                        ]
                    })
                    .collect::<Vec<_>>();
                emit_table(
                    &[
                        "DIRECTORY",
                        "TYPE",
                        "STATE",
                        "START AT BOOT",
                        "SHARED HTTPS",
                    ],
                    &rows,
                );
                for server in servers {
                    if let Some(setup) = server.setup {
                        println!(
                            "  {}  Account: {}  Local: {}  HTTPS listener: {}",
                            display_pool_dir_for_humans(&server.pool_dir),
                            human_literal(&setup.account),
                            human_literal(&server.local_url),
                            human_literal(setup.run.remote_bind.as_deref().unwrap_or("—"))
                        );
                    }
                    if let Some(problem) = server.problem {
                        println!(
                            "  {}  {}",
                            display_pool_dir_for_humans(&server.pool_dir),
                            human_literal(&problem)
                        );
                    }
                }
            }
            for error in &errors {
                super::output_support::emit_error(
                    error,
                    context.color_mode(),
                    context.json_output(),
                );
            }
            if !errors.is_empty() {
                return Ok(CommandResult::with_code(1));
            }
        }
        Some(ServeSubcommand::Status { all: false, .. }) => {
            let servers = crate::serve_registry::running()?;
            if context.json_output() {
                println!(
                    "{}",
                    serde_json::to_string(&servers).expect("server details serialize")
                );
            } else if servers.is_empty() {
                println!("No Plasmite servers running.");
            } else {
                let directories: Vec<_> = servers
                    .iter()
                    .map(|server| display_pool_dir_for_humans(&server.pool_dir))
                    .collect();
                let directory_width = directories
                    .iter()
                    .map(String::len)
                    .max()
                    .unwrap_or(0)
                    .max(9);
                let local_width = servers
                    .iter()
                    .map(|server| human_literal(&server.local_url).len())
                    .max()
                    .unwrap_or(0)
                    .max(9);
                let remote_width = servers
                    .iter()
                    .filter_map(|server| server.remote_url.as_ref())
                    .map(|url| human_literal(url).len())
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
                        human_literal(&server.local_url),
                        human_literal(server.remote_url.as_deref().unwrap_or("—")),
                        server.pid
                    );
                }
            }
        }
    }
    Ok(CommandResult::ok())
}

fn emit_action(action: &str, status: Status, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&status).expect("service status serializes")
        );
        return;
    }
    match action {
        "stop" => {
            println!("Stopped {}.", display_pool_dir_for_humans(&status.pool_dir));
            if status.startup {
                println!("Startup remains enabled; this server will start at the next boot.");
            } else {
                println!("Startup is off. Run `serve install` to restore startup at boot.");
            }
        }
        "uninstall" => println!(
            "Uninstalled the server for {}. Pools, certificates, and access keys remain available.",
            display_pool_dir_for_humans(&status.pool_dir)
        ),
        _ => {
            println!(
                "{} {}",
                if status.state == "running" {
                    "Serving"
                } else {
                    "Saved setup for"
                },
                display_pool_dir_for_humans(&status.pool_dir)
            );
            if let Some(setup) = status.setup {
                println!("Account: {}", human_literal(&setup.account));
                println!(
                    "Startup: {}",
                    if status.startup {
                        "at boot, before login"
                    } else {
                        "off"
                    }
                );
                println!("Local: {}/ui", human_literal(&status.local_url));
                println!(
                    "HTTPS listener: {}",
                    human_literal(setup.run.remote_bind.as_deref().unwrap_or("—"))
                );
                if let Some(address) = status.remote_url {
                    println!("Shared: {}", human_literal(&address));
                }
                println!(
                    "Invite someone: plasmite --dir '{}' access invite NAME",
                    human_literal(&status.pool_dir.to_string_lossy().replace('\'', "'\\''"))
                );
            }
        }
    }
}
