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
        assert_eq!(output.status.code(), Some(2), "{option} accepted zero");
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(option), "{error}");
        assert!(error.contains("greater than zero"), "{error}");
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
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("invalid local bind address"), "{error}");
}

#[test]
fn tailnet_endpoint_errors_explain_bind_and_origin_without_creating_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("unused");
    for (args, message, hint) in [
        (
            vec!["--remote-bind", "node.tail123.ts.net:9743"],
            "invalid remote bind address",
            "numeric IP:port",
        ),
        (
            vec![
                "--remote-bind",
                "[fd7a:115c:a1e0::abcd]:9743",
                "--shared-address",
                "https://node.tail123.ts.net:9743/events",
            ],
            "--shared-address must be an HTTPS origin",
            "--remote-bind controls the listening interface",
        ),
        (
            vec!["--shared-address", "http://node.tail123.ts.net:9743"],
            "--shared-address must be an HTTPS origin",
            "without a pool path",
        ),
    ] {
        let output = cli()
            .args(["--dir", pool_dir.to_str().expect("pool dir"), "serve"])
            .args(args)
            .output()
            .expect("invalid endpoint");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
        assert!(stderr.contains(hint), "{stderr}");
        assert!(!pool_dir.exists(), "invalid endpoint created server state");
    }
}

#[test]
fn local_responses_include_the_remote_protocol_version() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;

    for (path, version) in [("/healthz", "0"), ("/v2/pools", "2")] {
        let response = ureq::get(&format!("{}{path}", server.local_url)).call()?;
        assert_eq!(response.header("plasmite-version"), Some(version), "{path}");
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

#[test]
fn idle_tls_connections_expire_without_stopping_the_server()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let url = url::Url::parse(&server.remote_url)?;
    let address = ("127.0.0.1", url.port().ok_or("missing TLS port")?);
    let mut connections = Vec::new();
    for _ in 0..160 {
        connections.push(std::net::TcpStream::connect(address)?);
    }
    // Local administration remains available even while the remote budget fills.
    assert_eq!(
        ureq::get(&format!("{}/healthz", server.local_url))
            .call()?
            .status(),
        200
    );
    // The server must close idle handshakes even when the peer keeps its socket open.
    connections[0].set_read_timeout(Some(Duration::from_secs(7)))?;
    let mut byte = [0u8; 1];
    match std::io::Read::read(&mut connections[0], &mut byte) {
        Ok(0) => {}
        Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset) => {}
        result => return Err(format!("idle TLS socket did not expire: {result:?}").into()),
    }
    drop(connections);
    // A fresh authenticated request succeeds after the held sockets expire.
    server.client()?.list_pools()?;
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
fn mcp_stdio_rejects_obsolete_handshake_without_enabling_legacy_requests() {
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
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"test","version":"1"}
        }}),
    );
    let obsolete = read_mcp(&mut stdout);
    assert_eq!(obsolete["id"], 1);
    assert_eq!(obsolete["error"]["code"], -32601);
    let message = obsolete["error"]["message"].as_str().unwrap();
    assert!(message.contains("2026-07-28") && message.contains("server/discover"));

    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    let reply = read_mcp(&mut stdout);
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["error"]["code"], -32602);
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{
            "io.modelcontextprotocol/protocolVersion":"2025-11-25",
            "io.modelcontextprotocol/clientCapabilities":{}
        }}}),
    );
    let reply = read_mcp(&mut stdout);
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["error"]["code"], -32022);
    assert_eq!(reply["error"]["data"]["supported"], json!(["2026-07-28"]));

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

#[test]
fn mcp_stdio_uses_modern_requests_without_handshake() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut child = cli()
        .args(["--dir", temp.path().to_str().unwrap(), "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn MCP server");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut call = |id, method, mut params: Value| {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        });
        send_mcp(
            &mut stdin,
            &json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        );
        read_mcp(&mut stdout)
    };
    let tools = call(1, "tools/list", json!({}));
    assert_eq!(tools["result"]["resultType"], "complete");
    assert_eq!(tools["result"]["tools"][0]["name"], "plasmite_pool_list");
    let discover = call(2, "server/discover", json!({}));
    assert_eq!(
        discover["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );
    assert_eq!(
        discover["result"]["capabilities"],
        json!({"tools":{}, "resources":{}})
    );
    assert_eq!(
        discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "plasmite"
    );

    let feed = call(
        3,
        "tools/call",
        json!({"name":"plasmite_feed", "arguments":{"pool":"project","create":true,"data":{"handoff":"review"}}}),
    );
    assert_eq!(feed["result"]["structuredContent"]["message"]["seq"], 1);
    let fetch = call(
        4,
        "tools/call",
        json!({"name":"plasmite_fetch", "arguments":{"pool":"project","since":"oldest"}}),
    );
    assert_eq!(
        fetch["result"]["structuredContent"]["messages"][0]["data"]["handoff"],
        "review"
    );
    let resource = call(
        5,
        "resources/read",
        json!({"uri":"plasmite:///pools/project"}),
    );
    assert_eq!(
        resource["result"]["contents"][0]["uri"],
        "plasmite:///pools/project"
    );
    assert_eq!(
        call(
            6,
            "resources/read",
            json!({"uri":"plasmite:///pools/missing"})
        )["error"]["code"],
        -32602
    );

    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":7,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01"}}}),
    );
    let unsupported = read_mcp(&mut stdout);
    assert_eq!(unsupported["error"]["code"], -32022);
    assert_eq!(
        unsupported["error"]["data"],
        json!({"requested":"1900-01-01","supported":["2026-07-28"]})
    );
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":8,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}),
    );
    assert_eq!(read_mcp(&mut stdout)["error"]["code"], -32602);
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}),
    );
    assert_eq!(read_mcp(&mut stdout)["error"]["code"], -32602);
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}
    });
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{
            "_meta":meta, "name":"plasmite_follow","arguments":{"pool":"project","timeout_ms":3000}
        }}),
    );
    std::thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":10}}),
    );
    send_mcp(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":11,"method":"ping","params":{"_meta":meta}}),
    );
    assert_eq!(read_mcp(&mut stdout)["id"], 11);
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
