//! Run foreground servers and manage installed startup jobs.

use super::args::ServeSubcommand;
use super::context::CliContext;
use super::output::{OutputMode, emit_output};
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
    let output_mode = context.output_mode();
    match command {
        None => crate::secure_serve::run(context.pool_dir(), &run)?,
        Some(ServeSubcommand::Install {
            run,
            no_udp,
            no_udp_follow,
            ..
        }) => emit_action(
            "install",
            serve_service::install(context.pool_dir(), &run, no_udp, no_udp_follow)?,
            output_mode,
        )?,
        Some(ServeSubcommand::Start { .. }) => emit_action(
            "start",
            serve_service::control(context.pool_dir(), "start")?,
            output_mode,
        )?,
        Some(ServeSubcommand::Stop { .. }) => emit_action(
            "stop",
            serve_service::control(context.pool_dir(), "stop")?,
            output_mode,
        )?,
        Some(ServeSubcommand::Restart { .. }) => emit_action(
            "restart",
            serve_service::control(context.pool_dir(), "restart")?,
            output_mode,
        )?,
        Some(ServeSubcommand::Uninstall { .. }) => emit_action(
            "uninstall",
            serve_service::control(context.pool_dir(), "uninstall")?,
            output_mode,
        )?,
        Some(ServeSubcommand::Logs { tail, follow, .. }) => {
            serve_service::logs(context.pool_dir(), tail.unwrap_or(50), follow, output_mode)?
        }
        Some(ServeSubcommand::Status { all: true, .. }) => {
            let (servers, errors) = serve_service::all()?;
            if output_mode.is_structured() {
                emit_output(
                    serde_json::to_value(&servers).expect("status serializes"),
                    output_mode,
                )?;
            } else if servers.is_empty() {
                super::output::write_stdout(b"No Plasmite servers or installed setups.\n")?;
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
                )?;
                for server in servers {
                    if let Some(setup) = server.setup {
                        super::output::write_stdout(
                            format!(
                                "  {}  Account: {}  Local: {}  HTTPS listener: {}\n",
                                display_pool_dir_for_humans(&server.pool_dir),
                                human_literal(&setup.account),
                                human_literal(&server.local_url),
                                human_literal(setup.run.remote_bind.as_deref().unwrap_or("—"))
                            )
                            .as_bytes(),
                        )?;
                    }
                    if let Some(problem) = server.problem {
                        super::output::write_stdout(
                            format!(
                                "  {}  {}\n",
                                display_pool_dir_for_humans(&server.pool_dir),
                                human_literal(&problem)
                            )
                            .as_bytes(),
                        )?;
                    }
                }
            }
            for error in &errors {
                super::output_support::emit_error(error, context.color_mode(), output_mode);
            }
            if !errors.is_empty() {
                return Ok(CommandResult::with_code(1));
            }
        }
        Some(ServeSubcommand::Status { all: false, .. }) => {
            let servers = crate::serve_registry::running()?;
            if output_mode.is_structured() {
                emit_output(
                    serde_json::to_value(&servers).expect("server details serialize"),
                    output_mode,
                )?;
            } else if servers.is_empty() {
                super::output::write_stdout(b"No Plasmite servers running.\n")?;
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
                super::output::write_stdout(
                    format!(
                        "{:<directory_width$}  {:<local_width$}  {:<remote_width$}  PID\n",
                        "DIRECTORY", "LOCAL URL", "SHARED HTTPS"
                    )
                    .as_bytes(),
                )?;
                for (server, directory) in servers.into_iter().zip(directories) {
                    super::output::write_stdout(
                        format!(
                            "{:<directory_width$}  {:<local_width$}  {:<remote_width$}  {}\n",
                            directory,
                            human_literal(&server.local_url),
                            human_literal(server.remote_url.as_deref().unwrap_or("—")),
                            server.pid
                        )
                        .as_bytes(),
                    )?;
                }
            }
        }
    }
    Ok(CommandResult::ok())
}

fn emit_action(action: &str, status: Status, output_mode: OutputMode) -> Result<(), Error> {
    if output_mode.is_structured() {
        emit_output(
            serde_json::to_value(&status).expect("service status serializes"),
            output_mode,
        )?;
        return Ok(());
    }
    match action {
        "stop" => {
            super::output::write_stdout(format!("Stopped {}.\n", display_pool_dir_for_humans(&status.pool_dir)).as_bytes())?;
            if status.startup {
                super::output::write_stdout(b"Startup remains enabled; this server will start at the next boot.\n")?;
            } else {
                super::output::write_stdout(b"Startup is off. Run `serve install` to restore startup at boot.\n")?;
            }
        }
        "uninstall" => super::output::write_stdout(format!(
            "Uninstalled the server for {}. Pools, certificates, and access keys remain available.\n",
            display_pool_dir_for_humans(&status.pool_dir)
        ).as_bytes())?,
        _ => {
            super::output::write_stdout(format!(
                "{} {}\n",
                if status.state == "running" {
                    "Serving"
                } else {
                    "Saved setup for"
                },
                display_pool_dir_for_humans(&status.pool_dir)
            ).as_bytes())?;
            if let Some(setup) = status.setup {
                super::output::write_stdout(format!("Account: {}\n", human_literal(&setup.account)).as_bytes())?;
                super::output::write_stdout(format!(
                    "Startup: {}\n",
                    if status.startup {
                        "at boot, before login"
                    } else {
                        "off"
                    }
                ).as_bytes())?;
                super::output::write_stdout(format!("Local: {}/ui\n", human_literal(&status.local_url)).as_bytes())?;
                super::output::write_stdout(format!(
                    "HTTPS listener: {}\n",
                    human_literal(setup.run.remote_bind.as_deref().unwrap_or("—"))
                ).as_bytes())?;
                if let Some(address) = status.remote_url {
                    super::output::write_stdout(format!("Shared: {}\n", human_literal(&address)).as_bytes())?;
                }
                super::output::write_stdout(format!(
                    "Invite someone: plasmite --dir '{}' access invite NAME\n",
                    human_literal(&status.pool_dir.to_string_lossy().replace('\'', "'\\''"))
                ).as_bytes())?;
            }
        }
    }
    Ok(())
}
