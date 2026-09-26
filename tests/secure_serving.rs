//! Secure serve command, listener, shutdown, and stdio MCP integration checks.

#[allow(dead_code)]
#[path = "support/server.rs"]
mod server;

use serde_json::{Value, json};
use server::TestServer;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ureq::rustls::pki_types::pem::PemObject;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_plasmite"))
}

fn send_mcp(stdin: &mut impl Write, request: &Value) {
    serde_json::to_writer(&mut *stdin, request).expect("write MCP request");
    stdin.write_all(b"\n").expect("write request newline");
    stdin.flush().expect("flush MCP request");
}

fn read_mcp(stdout: &mut BufReader<impl std::io::Read>) -> Value {
    let mut line = String::new();
    assert!(stdout.read_line(&mut line).expect("read MCP response") > 0);
    serde_json::from_str(line.trim()).expect("valid MCP response")
}

fn cli_error(output: &std::process::Output) -> Value {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("CLI emits a JSON error")
}

#[test]
fn serve_rejects_invalid_bind_and_nonpositive_limits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    for option in [
        "--max-body-bytes",
        "--max-tail-timeout-ms",
        "--max-tail-concurrency",
    ] {
        let output = cli()
            .args([
                "--dir",
                pool_dir.to_str().expect("pool dir"),
                "serve",
                option,
                "0",
            ])
            .output()
            .expect("serve with invalid limit");
        assert!(!output.status.success(), "{option} accepted zero");
        let error = cli_error(&output);
        assert_eq!(error["error"]["kind"], "Usage");
    }

    let output = cli()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool dir"),
            "serve",
            "--bind",
            "nope",
        ])
        .output()
        .expect("serve with invalid bind");
    assert!(!output.status.success());
    let error = cli_error(&output);
    assert_eq!(error["error"]["kind"], "Usage");
}

#[test]
fn local_responses_include_the_remote_protocol_version() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;

    for path in ["/healthz", "/v0/pools"] {
        let response = ureq::get(&format!("{}{path}", server.local_url)).call()?;
        assert_eq!(response.header("plasmite-version"), Some("0"));
    }
    Ok(())
}

#[test]
fn remote_tls_health_uses_the_configured_certificate() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let pool_dir = temp.path().join("pools");
    let server = TestServer::try_start(&pool_dir)?;

    let serve_dir = pool_dir.join(".plasmite-serve");
    let identity: Value = serde_json::from_slice(&std::fs::read(serve_dir.join("identity.json"))?)?;
    let certificate_name = identity["cert_file"]
        .as_str()
        .expect("certificate filename");
    let pem = std::fs::read(serve_dir.join(certificate_name))?;
    let certificates = ureq::rustls::pki_types::CertificateDer::pem_slice_iter(&pem)
        .collect::<Result<Vec<_>, _>>()?;
    let mut roots = ureq::rustls::RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(certificates);
    assert_eq!(added, 1);
    let tls = ureq::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let agent = ureq::builder().tls_config(Arc::new(tls)).build();

    let health = agent
        .get(&format!("{}/healthz", server.remote_url))
        .call()?;
    assert_eq!(health.status(), 200);
    let body: Value = serde_json::from_str(&health.into_string()?)?;
    assert_eq!(body["ok"], json!(true));
    Ok(())
}

#[cfg(unix)]
#[test]
fn serve_exits_successfully_after_sigterm() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut server = TestServer::try_start(temp.path())?;

    let started = Instant::now();
    let status = server.terminate_and_wait(Duration::from_secs(3))?;
    assert!(status.success(), "unexpected server status: {status}");
    assert!(started.elapsed() < Duration::from_secs(3));
    Ok(())
}

#[test]
fn mcp_stdio_uses_the_2025_handshake_and_shared_pool_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut child = cli()
        .args(["--dir", pool_dir.to_str().expect("pool dir"), "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn MCP server");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    send_mcp(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{
                "protocolVersion":"2025-11-25",
                "capabilities":{},
                "clientInfo":{"name":"test","version":"1"}
            }
        }),
    );
    let initialized = read_mcp(&mut stdout);
    assert_eq!(initialized["id"], json!(1));
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-11-25")
    );
    assert_eq!(
        initialized["result"]["capabilities"]["tools"]["listChanged"],
        json!(false)
    );
    assert_eq!(
        initialized["result"]["capabilities"]["resources"]["subscribe"],
        json!(false)
    );

    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    send_mcp(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":2,"method":"tools/list","params":{}
        }),
    );
    assert_eq!(
        read_mcp(&mut stdout)["result"]["tools"][0]["name"],
        json!("plasmite_pool_list")
    );

    send_mcp(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{
                "name":"plasmite_pool_create",
                "arguments":{"name":"demo"}
            }
        }),
    );
    let created = read_mcp(&mut stdout);
    assert_eq!(
        created["result"]["structuredContent"]["pool"]["name"],
        json!("demo")
    );

    drop(stdin);
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("wait for MCP process") {
            assert!(status.success(), "MCP exited with {status}");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "MCP did not exit after stdin closed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
