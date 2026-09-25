//! CLI helpers for server commands.

use crate::ColorMode;
use crate::ServeRunArgs;
use crate::cli::output::emit_json;
use crate::serve;
use crate::serve_init;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use serde_json::json;
use std::io;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::Path;

use super::output_support::display_handoff_path_from_path;
use super::output_support::display_pool_dir_for_humans;
use super::output_support::format_bytes;
use super::support::read_token_file;

pub(crate) fn emit_serve_init_human(result: &serve_init::ServeInitResult) {
    let token_path = Path::new(&result.token_file);
    let cert_path = Path::new(&result.tls_cert);
    let key_path = Path::new(&result.tls_key);
    let (output_dir, token_label, cert_label, key_label) =
        serve_init_artifact_labels(token_path, cert_path, key_path);
    let token_file = display_handoff_path_from_path(token_path);
    let tls_cert = display_handoff_path_from_path(cert_path);
    let tls_key = display_handoff_path_from_path(key_path);
    let bind = result.bind;
    let client_host = url_host_component(&bind.ip().to_string());
    let port = bind.port();
    let (headline, files_heading) = if result.overwrote_existing {
        ("Secure serving re-initialized.", "Files overwritten:")
    } else {
        ("Secure serving initialized.", "Files created:")
    };

    println!("{headline}");
    if !bind.ip().is_loopback() {
        println!(
            "Clients on your network can read and write your pools over HTTPS after you start the server."
        );
    } else {
        println!(
            "These artifacts support local HTTPS. For network access, re-run init with --bind set to your host IP."
        );
    }
    println!();
    if let Some(output_dir) = output_dir {
        println!("  Output directory: {output_dir}");
        println!();
    }
    println!("  {files_heading}");
    println!("    token   {token_label}");
    println!("    cert    {cert_label}");
    println!("    key     {key_label}");
    println!();
    println!("  Fingerprint (share this with clients to verify the cert):");
    println!("    {}", result.tls_fingerprint);
    println!();
    println!("  Start serving your pools:");
    println!();
    println!("    pls serve \\");
    println!("      --bind {bind} \\");
    if !bind.ip().is_loopback() {
        println!("      --allow-non-loopback \\");
    }
    println!("      --token-file {token_file} \\");
    println!("      --tls-cert {tls_cert} \\");
    println!("      --tls-key {tls_key}");
    println!();
    if !bind.ip().is_loopback() {
        println!("  From another machine, read and write pools by URL:");
    } else {
        println!("  On this machine, read and write pools by URL:");
    }
    println!();
    println!("    pls feed https://{client_host}:{port}/demo \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-ca {tls_cert} \\");
    println!("      '{{\"hello\":\"world\"}}'");
    println!();
    println!("    pls follow https://{client_host}:{port}/demo \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-ca {tls_cert} --tail 10");
    println!();
    println!("  MCP endpoint for agent clients:");
    println!("    https://{client_host}:{port}/mcp");
    println!();
    println!("  Or with curl:");
    println!("    TOKEN=$(cat {token_file})");
    println!("    curl -k -H \"Authorization: Bearer $TOKEN\" \\");
    println!("      https://{client_host}:{port}/v0/pools/demo/tail?timeout_ms=5000");
    println!();
    println!("  The token is in the file, not printed here. Share the token");
    println!("  and fingerprint with collaborators out-of-band (e.g. paste");
    println!("  in a DM). Clients use the fingerprint to verify the cert");
    println!("  on first connect.");
}

pub(crate) fn url_host_component(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        return format!("[{host}]");
    }
    host.to_string()
}

pub(crate) fn serve_init_artifact_labels(
    token_path: &Path,
    cert_path: &Path,
    key_path: &Path,
) -> (Option<String>, String, String, String) {
    let common_parent = token_path.parent().and_then(|parent| {
        if cert_path.parent() == Some(parent) && key_path.parent() == Some(parent) {
            Some(parent)
        } else {
            None
        }
    });
    if let Some(parent) = common_parent {
        return (
            Some(display_pool_dir_for_humans(parent)),
            display_artifact_name(token_path),
            display_artifact_name(cert_path),
            display_artifact_name(key_path),
        );
    }
    (
        None,
        display_handoff_path_from_path(token_path),
        display_handoff_path_from_path(cert_path),
        display_handoff_path_from_path(key_path),
    )
}

pub(crate) fn display_artifact_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| display_handoff_path_from_path(path))
}

pub(crate) fn emit_serve_startup_guidance(config: &serve::ServeConfig) {
    if !io::stderr().is_terminal() {
        return;
    }
    for line in build_serve_startup_lines(config) {
        eprintln!("{line}");
    }
}

pub(crate) fn build_serve_startup_lines(config: &serve::ServeConfig) -> Vec<String> {
    let tls_enabled = serve_tls_enabled(config);
    let scheme = serve_scheme(config);
    let host = display_host(config.bind.ip());
    let base_url = format!("{scheme}://{host}:{}", config.bind.port());
    let web_ui_url = format!("{base_url}/ui");
    let mcp_url = format!("{base_url}/mcp");
    let append_url = format!("{base_url}/v0/pools/demo/append");
    let curl_tls_flag = if config.tls_self_signed { " -k" } else { "" };
    let scope = serve_scope(config.bind.ip());
    let auth = if config.token.is_some() {
        "bearer"
    } else {
        "none"
    };
    let tls = if config.tls_self_signed {
        "self-signed"
    } else if tls_enabled {
        "on"
    } else {
        "off"
    };
    let access = match config.access_mode {
        serve::AccessMode::ReadOnly => "read-only",
        serve::AccessMode::WriteOnly => "write-only",
        serve::AccessMode::ReadWrite => "read-write",
    };
    let cors = if config.cors_allowed_origins.is_empty() {
        "same-origin"
    } else {
        "allowlist"
    };

    let mut feed_cmd = format!("pls feed {base_url}/demo");
    let mut follow_cmd = format!("pls follow {base_url}/demo");
    if config.token.is_some() {
        if config.token_file_used {
            feed_cmd.push_str(" --token-file <token-file>");
            follow_cmd.push_str(" --token-file <token-file>");
        } else {
            feed_cmd.push_str(" --token <token>");
            follow_cmd.push_str(" --token <token>");
        }
    }
    if tls_enabled {
        feed_cmd.push_str(" --tls-ca <tls-cert>");
        follow_cmd.push_str(" --tls-ca <tls-cert>");
    }
    feed_cmd.push_str(" '{\"hello\":\"world\"}'");
    follow_cmd.push_str(" --tail 10");

    let mut lines = vec![
        format!("Serving pools on {base_url} ({scope})"),
        String::new(),
        format!("  UI:   {web_ui_url}"),
        format!("  MCP:  {mcp_url}"),
        format!("  Auth: {auth}    TLS: {tls}    Access: {access}    CORS: {cors}"),
    ];

    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        lines.push(format!("  Fingerprint: {fingerprint}"));
    }

    lines.push(String::new());
    lines.push("Try it:".to_string());
    lines.push(String::new());
    lines.push(format!("  {feed_cmd}"));
    lines.push(format!("  {follow_cmd}"));
    lines.push(String::new());
    if config.token.is_some() && config.token_file_used {
        lines.push("  TOKEN=$(cat <token-file>)".to_string());
    }
    let auth_header = if config.token.is_some() {
        if config.token_file_used {
            " -H \"Authorization: Bearer $TOKEN\""
        } else {
            " -H 'Authorization: Bearer <token>'"
        }
    } else {
        ""
    };
    lines.push(format!(
        "  curl{curl_tls_flag} -sS -X POST{auth_header} -H 'content-type: application/json' \\"
    ));
    lines.push("    --data '{\"hello\":\"world\"}' \\".to_string());
    lines.push(format!("    '{append_url}'"));
    lines.push(String::new());
    lines.push("Press Ctrl-C to stop.".to_string());

    if config.token.is_some() && config.token_file_used {
        lines.push(String::new());
        lines.push(
            "The token is in the file, not printed here. Share token and fingerprint out-of-band."
                .to_string(),
        );
    }

    if config.tls_self_signed {
        lines.push("Self-signed TLS: clients should trust the cert with --tls-ca.".to_string());
    }
    if config.bind.ip().is_unspecified() {
        lines.push(String::new());
        lines.push(
            "Replace YOUR-HOST/127.0.0.1 with your host IP or DNS name for remote clients."
                .to_string(),
        );
    }
    lines
}

pub(crate) fn emit_serve_check_report(
    config: &serve::ServeConfig,
    color_mode: ColorMode,
    json: bool,
) {
    if !json {
        for line in build_serve_check_lines(config) {
            println!("{line}");
        }
        return;
    }

    let tls_enabled = serve_tls_enabled(config);
    let base_url = format!(
        "{}://{}:{}",
        serve_scheme(config),
        display_host(config.bind.ip()),
        config.bind.port()
    );
    let auth_mode = if config.token.is_some() {
        if config.token_file_used {
            "bearer token (--token-file)"
        } else {
            "bearer token (--token)"
        }
    } else {
        "none"
    };
    let tls_mode = if config.tls_self_signed {
        "self-signed"
    } else if tls_enabled {
        "enabled"
    } else {
        "disabled"
    };
    let access_mode = match config.access_mode {
        serve::AccessMode::ReadOnly => "read-only",
        serve::AccessMode::WriteOnly => "write-only",
        serve::AccessMode::ReadWrite => "read-write",
    };
    let cors_origins = config.cors_allowed_origins.clone();

    emit_json(
        json!({
            "check": {
                "status": "valid",
                "listen": config.bind.to_string(),
                "base_url": base_url,
                "web_ui": format!("{base_url}/ui"),
                "web_ui_pool": format!("{base_url}/ui/pools/demo"),
                "mcp": format!("{base_url}/mcp"),
                "auth": auth_mode,
                "tls": tls_mode,
                "tls_fingerprint": config.tls_fingerprint,
                "access": access_mode,
                "cors_allowed_origins": cors_origins,
                "limits": {
                    "max_body_bytes": config.max_body_bytes,
                    "max_tail_timeout_ms": config.max_tail_timeout_ms,
                    "max_tail_concurrency": config.max_concurrent_tails
                }
            }
        }),
        color_mode,
    );
}

pub(crate) fn build_serve_check_lines(config: &serve::ServeConfig) -> Vec<String> {
    let tls_enabled = serve_tls_enabled(config);
    let base_url = format!(
        "{}://{}:{}",
        serve_scheme(config),
        display_host(config.bind.ip()),
        config.bind.port()
    );
    let auth = if config.token.is_some() {
        "bearer token"
    } else {
        "none"
    };
    let tls = if config.tls_self_signed {
        "self-signed"
    } else if tls_enabled {
        "on"
    } else {
        "off"
    };
    let access = match config.access_mode {
        serve::AccessMode::ReadOnly => "access: read-only",
        serve::AccessMode::WriteOnly => "access: write-only",
        serve::AccessMode::ReadWrite => "access: read-write",
    };
    let access = access.strip_prefix("access: ").unwrap_or(access);
    let cors = if config.cors_allowed_origins.is_empty() {
        "same-origin"
    } else {
        "allowlist"
    };
    let mut lines = vec![
        "Configuration valid.".to_string(),
        String::new(),
        format!(
            "  Bind:   {} ({})",
            config.bind,
            serve_scope(config.bind.ip())
        ),
        format!("  MCP:    {base_url}/mcp"),
        format!("  Auth: {auth}    TLS: {tls}    Access: {access}    CORS: {cors}"),
        format!(
            "  Limits: body {}, timeout {}, concurrency {}",
            format_bytes(config.max_body_bytes),
            format_timeout_ms(config.max_tail_timeout_ms),
            config.max_concurrent_tails
        ),
    ];
    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        lines.push(format!("  Fingerprint: {fingerprint}"));
    }
    lines.push(String::new());
    lines.push("Start with: pls serve".to_string());

    lines
}

pub(crate) fn serve_scope(ip: std::net::IpAddr) -> &'static str {
    if ip.is_loopback() {
        "loopback only"
    } else if ip.is_unspecified() {
        "all interfaces"
    } else {
        "network reachable"
    }
}

pub(crate) fn serve_scheme(config: &serve::ServeConfig) -> &'static str {
    if serve_tls_enabled(config) {
        "https"
    } else {
        "http"
    }
}

pub(crate) fn serve_tls_enabled(config: &serve::ServeConfig) -> bool {
    config.tls_self_signed || (config.tls_cert.is_some() && config.tls_key.is_some())
}

pub(crate) fn serve_config_from_run_args(
    run: ServeRunArgs,
    pool_dir: &Path,
) -> Result<serve::ServeConfig, Error> {
    let bind: SocketAddr = run.bind.parse().map_err(|_| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid bind address")
            .with_hint("Use a host:port value like 127.0.0.1:9700.")
    })?;
    if run.token.is_some() && run.token_file.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--token cannot be combined with --token-file")
            .with_hint("Use --token for dev, or run `plasmite serve init` and use the generated --token-file for safer deployments."));
    }
    let (token, token_file_used) = if let Some(path) = run.token_file {
        (Some(read_token_file(&path)?), true)
    } else {
        (run.token, false)
    };
    let tls_self_signed_material = if run.tls_self_signed {
        Some(serve::prepare_self_signed_tls(bind.ip())?)
    } else {
        None
    };
    let tls_fingerprint = if let Some(material) = &tls_self_signed_material {
        Some(material.fingerprint.clone())
    } else if let Some(cert_path) = run.tls_cert.as_ref() {
        Some(serve::tls_fingerprint_from_cert_path(cert_path)?)
    } else {
        None
    };
    Ok(serve::ServeConfig {
        bind,
        pool_dir: pool_dir.to_path_buf(),
        token,
        cors_allowed_origins: run.cors_origin,
        access_mode: run.access.into(),
        allow_non_loopback: run.allow_non_loopback,
        insecure_no_tls: run.insecure_no_tls,
        token_file_used,
        tls_cert: run.tls_cert,
        tls_key: run.tls_key,
        tls_self_signed: run.tls_self_signed,
        tls_self_signed_material,
        tls_fingerprint,
        max_body_bytes: run.max_body_bytes,
        max_tail_timeout_ms: run.max_tail_timeout_ms,
        max_concurrent_tails: run.max_tail_concurrency,
    })
}

pub(crate) fn format_timeout_ms(timeout_ms: u64) -> String {
    if timeout_ms.is_multiple_of(1000) {
        return format!("{}s", timeout_ms / 1000);
    }
    format!("{timeout_ms}ms")
}

pub(crate) fn display_host(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(addr) => {
            if addr.is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                addr.to_string()
            }
        }
        std::net::IpAddr::V6(addr) => {
            let shown = if addr.is_unspecified() {
                "::1".to_string()
            } else {
                addr.to_string()
            };
            format!("[{shown}]")
        }
    }
}
