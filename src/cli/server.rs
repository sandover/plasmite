//! Purpose: Adapt serve CLI arguments to server configuration and presentation.
//! Exports: `run`.
//! Role: Keep server runtime internals in `serve` while owning CLI lifecycle choices.

use super::context::CliContext;
use super::output::emit_json;
use super::result::CommandResult;
use crate::serve;
use crate::serve_init;
use crate::{
    AccessModeCli, DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_TAIL_CONCURRENCY,
    DEFAULT_MAX_TAIL_TIMEOUT_MS, ServeRunArgs, ServeSubcommand, emit_serve_check_report,
    emit_serve_init_human, emit_serve_startup_guidance, serve_config_from_run_args,
};
use plasmite::api::{Error, ErrorKind};
use serde_json::json;
use std::io::{self, IsTerminal};
use std::net::SocketAddr;

pub(super) fn run(
    subcommand: Option<ServeSubcommand>,
    run: ServeRunArgs,
    context: &CliContext,
) -> Result<CommandResult, Error> {
    match subcommand {
        Some(ServeSubcommand::Init(args)) => {
            reject_ignored_init_args(&run)?;
            let bind: SocketAddr = args.bind.parse().map_err(|_| {
                Error::new(ErrorKind::Usage)
                    .with_message("invalid bind address")
                    .with_hint("Use a host:port value like 0.0.0.0:9700.")
            })?;
            let config = serve_init::ServeInitConfig {
                output_dir: args.output_dir,
                token_file: args.token_file,
                tls_cert: args.tls_cert,
                tls_key: args.tls_key,
                bind,
                force: args.force,
            };
            let result = serve_init::init(config)?;
            if io::stdout().is_terminal() {
                emit_serve_init_human(&result);
            } else {
                emit_json(
                    json!({
                        "init": {
                            "artifact_paths": {
                                "token_file": result.token_file,
                                "tls_cert": result.tls_cert,
                                "tls_key": result.tls_key,
                            },
                            "tls_fingerprint": result.tls_fingerprint,
                            "server_commands": result.server_commands,
                            "client_commands": result.client_commands,
                            "curl_client_commands": result.curl_client_commands,
                        }
                    }),
                    context.color_mode(),
                );
            }
            Ok(CommandResult::ok())
        }
        Some(ServeSubcommand::Check { json }) => {
            let mut config = serve_config_from_run_args(run, context.pool_dir())?;
            config.cors_allowed_origins = serve::preflight_config(&config)?;
            emit_serve_check_report(&config, context.color_mode(), json);
            Ok(CommandResult::ok())
        }
        None => {
            let config = serve_config_from_run_args(run, context.pool_dir())?;
            emit_serve_startup_guidance(&config);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| {
                    Error::new(ErrorKind::Internal)
                        .with_message("failed to start runtime")
                        .with_source(err)
                })?;
            runtime.block_on(serve::serve(config))?;
            Ok(CommandResult::ok())
        }
    }
}

fn reject_ignored_init_args(run: &ServeRunArgs) -> Result<(), Error> {
    let ignored = if run.bind != "127.0.0.1:9700" {
        Some(("--bind", true))
    } else if !matches!(run.access, AccessModeCli::ReadWrite) {
        Some(("--access", false))
    } else if !run.cors_origin.is_empty() {
        Some(("--cors-origin", false))
    } else if run.token.is_some() {
        Some(("--token", false))
    } else if run.token_file.is_some() {
        Some(("--token-file", true))
    } else if run.tls_cert.is_some() {
        Some(("--tls-cert", true))
    } else if run.tls_key.is_some() {
        Some(("--tls-key", true))
    } else if run.tls_self_signed {
        Some(("--tls-self-signed", false))
    } else if run.allow_non_loopback {
        Some(("--allow-non-loopback", false))
    } else if run.insecure_no_tls {
        Some(("--insecure-no-tls", false))
    } else if run.max_body_bytes != DEFAULT_MAX_BODY_BYTES {
        Some(("--max-body-bytes", false))
    } else if run.max_tail_timeout_ms != DEFAULT_MAX_TAIL_TIMEOUT_MS {
        Some(("--max-tail-timeout-ms", false))
    } else if run.max_tail_concurrency != DEFAULT_MAX_TAIL_CONCURRENCY {
        Some(("--max-tail-concurrency", false))
    } else {
        None
    };

    let Some((option, has_init_equivalent)) = ignored else {
        return Ok(());
    };
    let hint = if has_init_equivalent {
        format!("Place `{option}` after `init`, or remove it from this command.")
    } else {
        format!("Remove `{option}`; it configures `serve` and `serve check`, not `serve init`.")
    };
    Err(Error::new(ErrorKind::Usage)
        .with_message(format!("serve init does not use parent option {option}"))
        .with_hint(hint))
}
