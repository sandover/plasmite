//! Purpose: Serve, remote, and MCP black-box CLI integration tests.

pub mod support;
use support::cli::*;

#[test]
fn serve_rejects_invalid_bind() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let serve = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "serve",
            "--bind",
            "nope",
        ])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
}

#[test]
fn serve_init_help_is_available() {
    let output = cmd()
        .args(["serve", "init", "--help"])
        .output()
        .expect("serve init help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Generate token + TLS artifacts"));
}

#[test]
fn serve_init_rejects_ignored_parent_options() {
    let cases: &[(&[&str], &str)] = &[
        (&["--bind", "127.0.0.1:9701"], "--bind"),
        (&["--access", "read-only"], "--access"),
        (&["--cors-origin", "https://example.com"], "--cors-origin"),
        (&["--token", "secret"], "--token"),
        (&["--token-file", "token.txt"], "--token-file"),
        (&["--tls-cert", "cert.pem"], "--tls-cert"),
        (&["--tls-key", "key.pem"], "--tls-key"),
        (&["--tls-self-signed"], "--tls-self-signed"),
        (&["--allow-non-loopback"], "--allow-non-loopback"),
        (&["--insecure-no-tls"], "--insecure-no-tls"),
        (&["--max-body-bytes", "2"], "--max-body-bytes"),
        (&["--max-tail-timeout-ms", "2"], "--max-tail-timeout-ms"),
        (&["--max-tail-concurrency", "2"], "--max-tail-concurrency"),
    ];

    for (parent_args, option) in cases {
        let mut args = vec!["serve"];
        args.extend_from_slice(parent_args);
        args.push("init");
        let output = cmd().args(args).output().expect("serve init");
        assert_eq!(output.status.code(), Some(2), "{option}");
        let error = parse_error_json(&output.stderr);
        assert_eq!(error["error"]["kind"], "Usage", "{option}");
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(option)),
            "{option}"
        );
        assert!(
            error["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains(option)),
            "{option}"
        );
    }
}

#[test]
fn serve_check_help_describes_parent_option_placement() {
    let output = cmd()
        .args(["serve", "check", "--help"])
        .output()
        .expect("serve check help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("Serve configuration options belong before `check`"));
}

#[test]
fn serve_check_outputs_resolved_config() {
    let output = cmd()
        .args(["serve", "check", "--json"])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    let payload = parse_json(stdout);
    let check = payload.get("check").expect("check");
    let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(status, "valid");
    let base_url = check.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
    assert!(base_url.contains("127.0.0.1:9700"));
    let mcp = check.get("mcp").and_then(|v| v.as_str()).unwrap_or("");
    assert!(mcp.ends_with("/mcp"));
    assert_eq!(check["reachability"], "loopback only");
    assert_eq!(check["transport"], "plaintext");
    assert_eq!(check["authentication"], "none");
    assert_eq!(check["tls_identity"], "none");
}

#[test]
fn serve_check_warns_for_plaintext_network_transport() {
    let output = cmd()
        .args([
            "serve",
            "--bind",
            "0.0.0.0:9700",
            "--allow-non-loopback",
            "--access",
            "read-only",
            "check",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(stdout.contains("WARNING: plaintext network traffic"));
    assert!(stdout.contains("external private-network or tunnel"));
}

#[test]
fn serve_check_identifies_ephemeral_self_signed_tls() {
    let output = cmd()
        .args(["serve", "--tls-self-signed", "check", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload = parse_json(std::str::from_utf8(&output.stdout).unwrap());
    assert_eq!(payload["check"]["transport"], "tls");
    assert_eq!(payload["check"]["tls_identity"], "ephemeral");
    assert_eq!(payload["check"]["tls"], "temporary-self-signed");
}

#[test]
fn serve_check_human_uses_readable_limits_and_fingerprint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut params = CertificateParams::new(vec!["localhost".to_string()]);
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    let cert = Certificate::from_params(params).expect("cert");
    let cert_pem = cert.serialize_pem().expect("cert pem");
    let key_pem = cert.serialize_private_key_pem();
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    std::fs::write(&cert_path, cert_pem).expect("write cert");
    std::fs::write(&key_path, key_pem).expect("write key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = cmd()
        .args([
            "serve",
            "--tls-cert",
            cert_path.to_str().unwrap(),
            "--tls-key",
            key_path.to_str().unwrap(),
            "check",
        ])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("Configuration valid."));
    assert!(stdout.contains("MCP:    https://127.0.0.1:9700/mcp"));
    assert!(stdout.contains("Limits: body 1M, timeout 30s, concurrency 64"));
    assert!(stdout.contains("Fingerprint: SHA256:"));
}

#[test]
fn serve_check_json_includes_tls_fingerprint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut params = CertificateParams::new(vec!["localhost".to_string()]);
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    let cert = Certificate::from_params(params).expect("cert");
    let cert_pem = cert.serialize_pem().expect("cert pem");
    let key_pem = cert.serialize_private_key_pem();
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    std::fs::write(&cert_path, cert_pem).expect("write cert");
    std::fs::write(&key_path, key_pem).expect("write key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = cmd()
        .args([
            "serve",
            "--tls-cert",
            cert_path.to_str().unwrap(),
            "--tls-key",
            key_path.to_str().unwrap(),
            "check",
            "--json",
        ])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let payload = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    let fingerprint = payload
        .get("check")
        .and_then(|v| v.get("tls_fingerprint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(fingerprint.starts_with("SHA256:"));
}

#[test]
fn serve_check_defaults_to_human_output() {
    let output = cmd()
        .args(["serve", "check"])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("Configuration valid."));
}

#[test]
fn serve_check_rejects_invalid_config() {
    let output = cmd()
        .args(["serve", "--bind", "0.0.0.0:0", "check"])
        .output()
        .expect("serve check");
    assert!(!output.status.success());
    let err = parse_error_json(&output.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
}

#[test]
fn serve_init_writes_artifacts_and_next_commands() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out_dir = temp.path().join("serve-init");
    let output = cmd()
        .args([
            "serve",
            "init",
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--bind",
            "0.0.0.0:9700",
            "--host",
            "pools.example.test",
        ])
        .output()
        .expect("serve init");
    assert!(output.status.success());

    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    let payload = parse_json(stdout);
    let artifacts = payload
        .get("init")
        .and_then(|v| v.get("artifact_paths"))
        .expect("artifact_paths");
    let token_file = artifacts
        .get("token_file")
        .and_then(|v| v.as_str())
        .expect("token_file");
    let tls_cert = artifacts
        .get("tls_cert")
        .and_then(|v| v.as_str())
        .expect("tls_cert");
    let tls_key = artifacts
        .get("tls_key")
        .and_then(|v| v.as_str())
        .expect("tls_key");
    assert!(Path::new(token_file).exists());
    assert!(Path::new(tls_cert).exists());
    assert!(Path::new(tls_key).exists());

    let token = std::fs::read_to_string(token_file).expect("read token");
    assert!(!token.trim().is_empty());
    assert!(
        !stdout.contains(token.trim()),
        "token value should not be echoed to stdout"
    );

    let server_commands = payload
        .get("init")
        .and_then(|v| v.get("server_commands"))
        .and_then(|v| v.as_array())
        .expect("server_commands");
    assert!(server_commands.iter().any(|v| {
        v.as_str()
            .unwrap_or("")
            .contains("plasmite serve --bind 0.0.0.0:9700 --allow-non-loopback")
    }));
    let client_commands = payload
        .get("init")
        .and_then(|v| v.get("client_commands"))
        .and_then(|v| v.as_array())
        .expect("client_commands");
    assert!(
        client_commands
            .iter()
            .any(|v| v.as_str().unwrap_or("").contains("plasmite feed")),
        "expected plasmite feed client command"
    );
    let tls_fingerprint = payload
        .get("init")
        .and_then(|v| v.get("tls_fingerprint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(tls_fingerprint.starts_with("SHA256:"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&out_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(token_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(tls_key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn serve_refuses_broad_token_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let token = temp.path().join("token.txt");
    std::fs::write(&token, "not-printed\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = cmd()
        .args(["serve", "--token-file", token.to_str().unwrap(), "check"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = parse_error_json(&output.stderr);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("permissions are too broad")
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("not-printed"));
}

#[test]
fn serve_init_requires_force_for_existing_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out_dir = temp.path().join("serve-init");

    let first = cmd()
        .args([
            "serve",
            "init",
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--host",
            "pools.example.test",
        ])
        .output()
        .expect("first serve init");
    assert!(first.status.success());

    let second = cmd()
        .args([
            "serve",
            "init",
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--host",
            "pools.example.test",
        ])
        .output()
        .expect("second serve init");
    assert!(!second.status.success());
    let err = parse_error_json(&second.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "AlreadyExists");
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("--force"));
}

#[test]
fn concurrent_forced_initializers_serialize_without_mixed_or_scratch_files() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().to_str().unwrap();
    let mut first = cmd();
    first.args([
        "serve",
        "init",
        "--host",
        "pools.example.test",
        "--output-dir",
        out,
        "--force",
    ]);
    let mut second = cmd();
    second.args([
        "serve",
        "init",
        "--host",
        "pools.example.test",
        "--output-dir",
        out,
        "--force",
    ]);
    let mut first = first.spawn().unwrap();
    let mut second = second.spawn().unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    for name in [
        "plasmite-auth-token.txt",
        "plasmite-tls-cert.pem",
        "plasmite-tls-key.pem",
    ] {
        assert!(temp.path().join(name).exists(), "{name}");
    }
    let leftovers: Vec<_> = std::fs::read_dir(temp.path())
        .unwrap()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".staged")
                || name.ends_with(".backup")
                || name.ends_with("transaction.json")
        })
        .collect();
    assert!(leftovers.is_empty(), "transaction leftovers: {leftovers:?}");
}

#[test]
fn serve_init_requires_host_for_wildcard_binds() {
    for bind in ["0.0.0.0:9700", "[::]:9700"] {
        let temp = tempfile::tempdir().unwrap();
        let output = cmd()
            .args([
                "serve",
                "init",
                "--bind",
                bind,
                "--output-dir",
                temp.path().to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{bind}");
        let error = parse_error_json(&output.stderr);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("requires --host")
        );
    }
}

#[test]
fn serve_init_uses_client_host_but_preserves_bind() {
    let temp = tempfile::tempdir().unwrap();
    let output = cmd()
        .args([
            "serve",
            "init",
            "--bind",
            "0.0.0.0:9443",
            "--host",
            "pools.example.test",
            "--output-dir",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload = parse_json(std::str::from_utf8(&output.stdout).unwrap());
    let server = payload["init"]["server_commands"][0].as_str().unwrap();
    assert!(server.contains("--bind 0.0.0.0:9443"));
    let clients = payload["init"]["client_commands"].as_array().unwrap();
    assert!(clients.iter().all(|command| {
        command
            .as_str()
            .unwrap()
            .contains("https://pools.example.test:9443/")
    }));
}

#[test]
fn serve_init_concrete_ipv6_defaults_client_host() {
    let temp = tempfile::tempdir().unwrap();
    let output = cmd()
        .args([
            "serve",
            "init",
            "--bind",
            "[::1]:9443",
            "--output-dir",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload = parse_json(std::str::from_utf8(&output.stdout).unwrap());
    assert!(
        payload["init"]["client_commands"][0]
            .as_str()
            .unwrap()
            .contains("https://[::1]:9443/demo")
    );
}

#[test]
fn serve_init_token_only_writes_one_private_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let output = cmd()
        .args([
            "serve",
            "init",
            "--token-only",
            "--output-dir",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let payload = parse_json(text);
    assert_eq!(payload["init"]["token_only"], true);
    assert!(
        payload["init"]["artifact_paths"]["token_file"]
            .as_str()
            .is_some()
    );
    assert!(payload["init"]["artifact_paths"]["tls_cert"].is_null());
    assert!(payload["init"]["artifact_paths"]["tls_key"].is_null());
    assert!(
        payload["init"]["server_commands"][0]
            .as_str()
            .unwrap()
            .contains("--insecure-no-tls")
    );
    assert!(
        payload["init"]["client_commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!temp.path().join("plasmite-tls-cert.pem").exists());
    assert!(!temp.path().join("plasmite-tls-key.pem").exists());
    let token = std::fs::read_to_string(temp.path().join("plasmite-auth-token.txt")).unwrap();
    assert!(!text.contains(token.trim()));
}

#[test]
fn serve_init_tty_reports_created_and_overwritten() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out_dir = temp.path().join("serve-init");

    let first = cmd_tty(&[
        "--color",
        "never",
        "serve",
        "init",
        "--output-dir",
        out_dir.to_str().unwrap(),
        "--bind",
        "0.0.0.0:9700",
        "--host",
        "pools.example.test",
    ]);
    assert!(first.status.success());
    let first_text = sanitize_tty_text(&first.stdout);
    assert!(first_text.contains("Secure serving initialized."));
    assert!(first_text.contains("Output directory:"));
    assert!(first_text.contains("Files created:"));
    assert!(first_text.contains("    pls serve \\"));
    assert!(first_text.contains("      --bind 0.0.0.0:9700 \\"));
    assert!(first_text.contains("      --allow-non-loopback \\"));
    assert!(first_text.contains("      --token-file "));
    assert!(first_text.contains("      --tls-cert "));
    assert!(first_text.contains("      --tls-key "));
    assert!(!first_text.contains("THIS-HOST"));
    let feed_line = first_text
        .lines()
        .find(|line| line.contains("pls feed https://"))
        .expect("feed line");
    assert!(feed_line.contains(":9700/demo \\"));
    assert!(feed_line.contains("https://pools.example.test:9700/demo"));
    let follow_line = first_text
        .lines()
        .find(|line| line.contains("pls follow https://"))
        .expect("follow line");
    assert!(follow_line.contains(":9700/demo \\"));
    assert!(!first_text.contains("curl -k"));
    assert!(first_text.contains("curl --cacert"));

    let second = cmd_tty(&[
        "--color",
        "never",
        "serve",
        "init",
        "--output-dir",
        out_dir.to_str().unwrap(),
        "--bind",
        "0.0.0.0:9700",
        "--host",
        "pools.example.test",
        "--force",
    ]);
    assert!(second.status.success());
    let second_text = sanitize_tty_text(&second.stdout);
    assert!(second_text.contains("Secure serving re-initialized."));
    assert!(second_text.contains("Files overwritten:"));
}

#[test]
fn serve_rejects_non_loopback_without_allow() {
    let serve = cmd()
        .args(["serve", "--bind", "0.0.0.0:0"])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("--allow-non-loopback"));
}

#[test]
fn serve_non_loopback_write_requires_token_file() {
    let serve = cmd()
        .args([
            "serve",
            "--bind",
            "0.0.0.0:0",
            "--allow-non-loopback",
            "--access",
            "write-only",
            "--insecure-no-tls",
        ])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
    let message = err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(message.contains("--token-file"));
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_non_loopback_write_requires_tls_or_insecure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let token_path = temp.path().join("token.txt");
    std::fs::write(&token_path, "secret").expect("write token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let serve = cmd()
        .args([
            "serve",
            "--bind",
            "0.0.0.0:0",
            "--allow-non-loopback",
            "--access",
            "write-only",
            "--token-file",
            token_path.to_str().unwrap(),
        ])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
    let message = err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(message.contains("TLS"));
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_rejects_token_and_token_file_combination_with_init_hint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let token_path = temp.path().join("token.txt");
    std::fs::write(&token_path, "secret").expect("write token");

    let serve = cmd()
        .args([
            "serve",
            "--token",
            "dev-token",
            "--token-file",
            token_path.to_str().unwrap(),
        ])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_rejects_conflicting_tls_flags_with_init_hint() {
    let serve = cmd()
        .args([
            "serve",
            "--tls-self-signed",
            "--tls-cert",
            "cert.pem",
            "--tls-key",
            "key.pem",
        ])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_rejects_wildcard_cors_origin() {
    let serve = cmd()
        .args(["serve", "--bind", "127.0.0.1:0", "--cors-origin", "*"])
        .output()
        .expect("serve");
    assert!(!serve.status.success());
    let err = parse_error_json(&serve.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
    let message = err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(message.contains("wildcard"));
}

#[test]
fn serve_responses_include_version_header() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let server = ServeProcess::start(&pool_dir);

    let list_url = format!("{}/v0/pools", server.base_url);
    let list = ureq::get(&list_url).call().expect("list");
    assert_eq!(list.header("plasmite-version"), Some("0"));

    let tail_url = format!("{}/v0/pools/demo/tail?timeout_ms=10", server.base_url);
    let tail = ureq::get(&tail_url).call().expect("tail");
    assert_eq!(tail.header("plasmite-version"), Some("0"));

    let health_url = format!("{}/healthz", server.base_url);
    let health = ureq::get(&health_url).call().expect("healthz");
    assert_eq!(health.header("plasmite-version"), Some("0"));
    let body: serde_json::Value =
        serde_json::from_str(&health.into_string().expect("body")).expect("healthz json");
    assert_eq!(body.get("ok").and_then(|v| v.as_bool()), Some(true));
}

#[test]
fn serve_rejects_oversized_body() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let server = ServeProcess::start_with_args(&pool_dir, &["--max-body-bytes", "64"]);
    let append_url = format!("{}/v0/pools/demo/append", server.base_url);
    let payload = json!({
        "data": { "big": "x".repeat(256) },
        "tags": ["oversized"],
        "durability": "fast"
    })
    .to_string();

    match ureq::post(&append_url)
        .set("Content-Type", "application/json")
        .send_string(&payload)
    {
        Ok(_) => panic!("expected 413 for oversized body"),
        Err(ureq::Error::Status(code, _resp)) => {
            assert_eq!(code, 413);
        }
        Err(err) => panic!("request failed: {err:?}"),
    }
}

#[test]
fn serve_rejects_excessive_tail_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let server = ServeProcess::start_with_args(&pool_dir, &["--max-tail-timeout-ms", "5"]);
    let tail_url = format!("{}/v0/pools/demo/tail?timeout_ms=10", server.base_url);
    let start = Instant::now();
    loop {
        match ureq::get(&tail_url).call() {
            Ok(_) => panic!("expected tail timeout rejection"),
            Err(ureq::Error::Status(code, resp)) => {
                assert_eq!(code, 400);
                let body = resp.into_string().expect("body");
                let value: Value = serde_json::from_str(&body).expect("json");
                assert_eq!(value["error"]["kind"], "Usage");
                break;
            }
            Err(ureq::Error::Transport(err)) => {
                if start.elapsed() >= Duration::from_millis(250) {
                    panic!("request failed after startup retry window: {err:?}");
                }
                sleep(Duration::from_millis(25));
            }
        }
    }
}

#[test]
fn serve_tls_allows_healthz_with_trusted_cert() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let _ = ureq::rustls::crypto::ring::default_provider().install_default();

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut params = CertificateParams::new(vec!["localhost".to_string()]);
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    let cert = Certificate::from_params(params).expect("cert");
    let cert_pem = cert.serialize_pem().expect("cert pem");
    let key_pem = cert.serialize_private_key_pem();
    let cert_path = temp.path().join("cert.pem");
    let key_path = temp.path().join("key.pem");
    std::fs::write(&cert_path, cert_pem).expect("write cert");
    std::fs::write(&key_path, key_pem).expect("write key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let server = ServeProcess::start_with_args_and_scheme(
        &pool_dir,
        &[
            "--tls-cert",
            cert_path.to_str().unwrap(),
            "--tls-key",
            key_path.to_str().unwrap(),
        ],
        "https",
    );

    let mut root_store = ureq::rustls::RootCertStore::empty();
    let cert_der = cert.serialize_der().expect("cert der");
    let (added, _) = root_store
        .add_parsable_certificates([ureq::rustls::pki_types::CertificateDer::from(cert_der)]);
    assert_eq!(added, 1);
    let client_config = ureq::rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let agent = ureq::builder().tls_config(Arc::new(client_config)).build();

    let health_url = format!("{}/healthz", server.base_url);
    let response = agent.get(&health_url).call().expect("healthz");
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_str(&response.into_string().expect("body")).expect("json");
    assert_eq!(body.get("ok").and_then(|value| value.as_bool()), Some(true));
}

#[cfg(unix)]
#[test]
fn serve_sigterm_exits_successfully_within_bound() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut server = ServeProcess::start(&pool_dir);

    let started = Instant::now();
    let status = server
        .terminate_and_wait(Duration::from_secs(3))
        .expect("server exits after SIGTERM");
    assert!(status.success(), "unexpected server status: {status}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

// --- Shell completion tests ---

#[test]
fn mcp_stdio_initialize_and_tool_resource_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let mut child = cmd()
        .args(["mcp", "--dir", pool_dir.to_str().expect("pool dir")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    );
    let initialize = mcp_read_response(&mut stdout);
    assert_eq!(initialize["id"], json!(1));
    assert_eq!(initialize["result"]["protocolVersion"], json!("2025-11-25"));
    assert_eq!(
        initialize["result"]["capabilities"]["tools"]["listChanged"],
        json!(false)
    );
    assert_eq!(
        initialize["result"]["capabilities"]["resources"]["listChanged"],
        json!(false)
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );
    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "ping",
            "params": {}
        }),
    );
    let ping = mcp_read_response(&mut stdout);
    assert_eq!(ping["id"], json!(2));
    assert_eq!(ping["result"], json!({}));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 22,
            "method": "tools/list",
            "params": {}
        }),
    );
    let tools_list = mcp_read_response(&mut stdout);
    assert_eq!(tools_list["id"], json!(22));
    let tool_names = tools_list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"plasmite_pool_create"));
    assert!(tool_names.contains(&"plasmite_read"));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "plasmite_pool_create",
                "arguments": { "name": "demo" }
            }
        }),
    );
    let create = mcp_read_response(&mut stdout);
    assert_eq!(create["id"], json!(3));
    assert_eq!(
        create["result"]["structuredContent"]["pool"]["name"],
        json!("demo")
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "plasmite_feed",
                "arguments": {
                    "pool": "demo",
                    "data": {"msg": "hello"},
                    "tags": ["chat"]
                }
            }
        }),
    );
    let feed = mcp_read_response(&mut stdout);
    let seq = feed["result"]["structuredContent"]["message"]["seq"]
        .as_u64()
        .expect("seq");
    assert_eq!(feed["id"], json!(4));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 41,
            "method": "tools/call",
            "params": {
                "name": "plasmite_read",
                "arguments": {
                    "pool": "demo",
                    "after_seq": seq.saturating_sub(1),
                    "since": "1970-01-01T00:00:00Z",
                    "count": 5
                }
            }
        }),
    );
    let read = mcp_read_response(&mut stdout);
    assert_eq!(read["id"], json!(41));
    assert_eq!(
        read["result"]["structuredContent"]["messages"][0]["seq"],
        json!(seq)
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "plasmite_fetch",
                "arguments": {
                    "pool": "demo",
                    "seq": seq
                }
            }
        }),
    );
    let fetch = mcp_read_response(&mut stdout);
    assert_eq!(fetch["id"], json!(42));
    assert_eq!(
        fetch["result"]["structuredContent"]["message"]["data"]["msg"],
        json!("hello")
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "resources/list",
            "params": {}
        }),
    );
    let resources = mcp_read_response(&mut stdout);
    assert_eq!(resources["id"], json!(5));
    assert_eq!(
        resources["result"]["resources"][0]["uri"],
        json!("plasmite:///pools/demo")
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "resources/read",
            "params": {
                "uri": "plasmite:///pools/demo"
            }
        }),
    );
    let resource_read = mcp_read_response(&mut stdout);
    assert_eq!(resource_read["id"], json!(6));
    let text_payload = resource_read["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    let payload = parse_json(text_payload);
    assert_eq!(payload["next_after_seq"], json!(seq));
    assert_eq!(payload["messages"][0]["data"]["msg"], json!("hello"));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "plasmite_pool_delete",
                "arguments": {
                    "pool": "demo"
                }
            }
        }),
    );
    let delete = mcp_read_response(&mut stdout);
    assert_eq!(delete["id"], json!(7));
    assert_ne!(delete["result"]["isError"], json!(true));

    drop(stdin);
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            assert!(status.success(), "mcp exited non-zero: {status}");
            break;
        }
        if start.elapsed() > Duration::from_secs(3) {
            panic!("mcp process did not exit after stdin close");
        }
        sleep(Duration::from_millis(20));
    }
}
