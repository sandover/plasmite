//! Explicit human and JSON output, and private offline connection discovery.

pub mod support;

use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
    command.env("PLASMITE_ACCESS_HOME", home);
    command
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn access_connect(home: &Path, destination: &str, access_key: &str, json_output: bool) -> Output {
    let mut cmd = command(home);
    cmd.args(["access", "connect", destination]);
    if json_output {
        cmd.arg("--json");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{access_key}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn empty_access_list_is_human_by_default_and_does_not_create_store() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("missing");
    let human = success(command(&home).args(["access", "list"]).output().unwrap());
    assert_eq!(human.stdout, b"No saved connections.\n");
    assert!(human.stderr.is_empty());
    let machine = success(
        command(&home)
            .args(["--color", "always", "access", "list", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(machine.stdout, b"[]\n");
    assert!(machine.stderr.is_empty());
    assert!(!home.exists());
}

#[cfg(unix)]
fn saved_fixture(home: &Path, value: Value) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir(home).unwrap();
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = home.join("connections.json");
    std::fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(unix)]
#[test]
fn access_list_is_sorted_offline_and_never_decodes_or_prints_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("access");
    // These are unreachable reserved domains and deliberately undecodable
    // credentials. Listing must only inspect destination names.
    saved_fixture(
        &home,
        json!({"version": 1, "connections": {
            "https://z.example.invalid/": "secret-z-do-not-print",
            "https://a.example.invalid/": "secret-a-do-not-print"
        }}),
    );
    let human = success(command(&home).args(["access", "list"]).output().unwrap());
    assert_eq!(
        human.stdout,
        b"https://a.example.invalid/\nhttps://z.example.invalid/\n"
    );
    let machine = success(
        command(&home)
            .args(["access", "list", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&machine.stdout).unwrap(),
        json!([
            {"destination": "https://a.example.invalid/"},
            {"destination": "https://z.example.invalid/"}
        ])
    );
    assert!(human.stderr.is_empty());
    assert!(machine.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn access_list_rejects_symlinked_store_and_file() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("access");
    let target = temp.path().join("target");
    saved_fixture(&target, json!({"version": 1, "connections": {}}));
    symlink(&target, &home).unwrap();
    let output = command(&home)
        .args(["access", "list", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "Permission");
    std::fs::remove_file(&home).unwrap();
    std::fs::create_dir(&home).unwrap();
    symlink(
        target.join("connections.json"),
        home.join("connections.json"),
    )
    .unwrap();
    let output = command(&home)
        .args(["access", "list", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "Permission");
}

#[cfg(unix)]
#[test]
fn access_list_rejects_unknown_versions_and_credential_bearing_destinations() {
    for fixture in [
        json!({"version": 2, "connections": {}}),
        json!({"version": 1, "connections": {"https://user:private-secret@example.invalid/": "encoded-secret"}}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("access");
        saved_fixture(&home, fixture);
        let output = command(&home)
            .args(["access", "list", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["kind"], "Corrupt");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
    }
}

#[test]
fn feed_receipts_are_human_by_default_and_explicit_json_ignores_color() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("events.plasmite");
    success(
        command(temp.path())
            .args(["pool", "create"])
            .arg(&pool)
            .output()
            .unwrap(),
    );
    let human = success(
        command(temp.path())
            .arg("feed")
            .arg(&pool)
            .arg("{\"value\":1}")
            .output()
            .unwrap(),
    );
    assert!(String::from_utf8_lossy(&human.stdout).starts_with("fed seq=1 at "));
    let machine = success(
        command(temp.path())
            .args(["--color", "always", "feed"])
            .arg(&pool)
            .args(["{\"value\":2}", "--json"])
            .output()
            .unwrap(),
    );
    let receipt: Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(receipt["seq"], 2);
    assert!(!machine.stdout.contains(&0x1b));
    assert_eq!(
        machine.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
}

#[test]
fn disconnect_reports_local_result_in_selected_format() {
    let temp = tempfile::tempdir().unwrap();
    let destination = "https://offline.example.invalid/";
    let human = success(
        command(temp.path())
            .args(["access", "disconnect", destination])
            .output()
            .unwrap(),
    );
    assert!(String::from_utf8_lossy(&human.stdout).starts_with("Removed saved credentials for "));
    let machine = success(
        command(temp.path())
            .args(["access", "disconnect", destination, "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&machine.stdout).unwrap(),
        json!({"destination": destination, "credentials_saved": false})
    );
}

#[test]
fn access_connect_prints_direct_http_oauth_setup_for_humans_and_json() {
    let temp = tempfile::tempdir().unwrap();
    let server = support::server::TestServer::try_start_oauth(temp.path()).unwrap();
    let destination = format!("{}/", server.remote_url.trim_end_matches('/'));
    let endpoint = format!("{}/mcp", server.remote_url.trim_end_matches('/'));
    let human = success(access_connect(
        &temp.path().join("human"),
        &destination,
        server.access_key(),
        false,
    ));
    let human_text = String::from_utf8(human.stdout).unwrap();
    assert!(human_text.contains("claude mcp add --scope user --transport http plasmite"));
    assert!(human_text.contains("codex mcp add plasmite --url"));
    assert!(human_text.contains(&endpoint));
    assert!(human_text.contains("enter the access key in its browser approval page"));
    assert!(human_text.contains("does not sign in the MCP client"));
    assert!(!human_text.contains("mcp --remote"));

    let machine = success(access_connect(
        &temp.path().join("machine"),
        &destination,
        server.access_key(),
        true,
    ));
    let value: Value = serde_json::from_slice(&machine.stdout).unwrap();
    let commands = value["mcp_setup_commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    assert!(
        commands[0]
            .as_str()
            .unwrap()
            .contains("claude mcp add --scope user --transport http plasmite")
    );
    assert!(
        commands[1]
            .as_str()
            .unwrap()
            .contains("codex mcp add plasmite --url")
    );
    assert!(
        commands
            .iter()
            .all(|command| command.as_str().unwrap().contains(&endpoint))
    );
    assert!(
        commands
            .iter()
            .all(|command| !command.as_str().unwrap().contains("mcp --remote"))
    );
}

#[test]
fn skipped_feed_records_keep_selected_notice_format_and_failure_status() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("events.plasmite");
    let data = temp.path().join("records.jsonl");
    std::fs::write(&data, b"{\"ok\":true}\ninvalid-json\n").unwrap();
    success(
        command(temp.path())
            .args(["pool", "create"])
            .arg(&pool)
            .output()
            .unwrap(),
    );
    for json_output in [false, true] {
        let mut cmd = command(temp.path());
        cmd.arg("feed")
            .arg(&pool)
            .arg("--file")
            .arg(&data)
            .args(["--in", "jsonl", "--errors", "skip"]);
        if json_output {
            cmd.arg("--json");
        }
        let output = cmd.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        if json_output {
            let notices = stderr
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(notices.len(), 2);
            assert_eq!(notices[0]["notice"]["kind"], "ingest_skip");
            assert_eq!(notices[1]["notice"]["kind"], "ingest_summary");
            assert_eq!(notices[1]["notice"]["details"]["failed"], 1);
        } else {
            assert!(stderr.contains("notice: Skipped invalid JSON."));
            assert!(stderr.contains("notice: Finished with 1 skipped record."));
        }
    }
}

#[cfg(unix)]
#[test]
fn explicit_access_json_remains_parseable_in_terminal_with_forced_color() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("access");
    let exe = env!("CARGO_BIN_EXE_plasmite");
    let mut cmd = Command::new("script");
    cmd.env("PLASMITE_ACCESS_HOME", &home);
    #[cfg(target_os = "linux")]
    {
        let quoted_exe = format!("'{}'", exe.replace('\'', "'\\''"));
        cmd.args([
            "-q",
            "-e",
            "-c",
            &format!("{quoted_exe} --color always access list --json"),
            "/dev/null",
        ]);
    }
    #[cfg(not(target_os = "linux"))]
    cmd.args([
        "-q",
        "/dev/null",
        exe,
        "--color",
        "always",
        "access",
        "list",
        "--json",
    ]);
    let output = success(cmd.output().unwrap());
    // macOS script echoes EOF as ^D followed by two backspaces. Apply
    // those terminal edits before inspecting the command's visible output.
    let mut text = String::new();
    for character in String::from_utf8(output.stdout).unwrap().chars() {
        match character {
            '\u{8}' => {
                text.pop();
            }
            '\r' | '\u{4}' => {}
            _ => text.push(character),
        }
    }
    assert_eq!(
        serde_json::from_str::<Value>(text.trim()).unwrap(),
        json!([])
    );
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn human_feed_tags_are_literal_while_json_preserves_tag_data() {
    let temp = tempfile::tempdir().unwrap();
    let pool = temp.path().join("events.plasmite");
    let label = "雪\x1b]52;c;value\x07\trow\nnext";
    success(
        command(temp.path())
            .args(["pool", "create"])
            .arg(&pool)
            .output()
            .unwrap(),
    );
    let human = success(
        command(temp.path())
            .arg("feed")
            .arg(&pool)
            .args(["{}", "--tag", label])
            .output()
            .unwrap(),
    );
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("tags: 雪\\u{1b}]52;c;value\\u{7}\\trow\\nnext"));
    assert_eq!(text.lines().count(), 1);
    assert!(!text.contains('\x1b'));
    let machine = success(
        command(temp.path())
            .arg("feed")
            .arg(&pool)
            .args(["{}", "--tag", label, "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        machine.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    let receipt: Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(receipt["meta"]["tags"][0], label);
}

#[cfg(unix)]
#[test]
fn remote_pool_labels_and_saved_access_names_are_literal_in_human_tables() {
    use plasmite::api::{LocalClient, PoolOptions, PoolRef};
    use support::server::TestServer;
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("pools");
    std::fs::create_dir(&directory).unwrap();
    let label = "evil\x1b]52;c;value\x07\trow\nnext";
    LocalClient::new()
        .with_pool_dir(&directory)
        .create_pool(&PoolRef::name(label), PoolOptions::new(1024 * 1024))
        .unwrap();
    // Exercise a persisted name too: newly issued names reject controls, while
    // older saved metadata still needs literal presentation at the boundary.
    let server = TestServer::start(&directory);
    drop(server);
    let key_file = directory.join(".plasmite-serve/keys.json");
    let mut records: Value = serde_json::from_slice(&std::fs::read(&key_file).unwrap()).unwrap();
    records[0]["name"] = json!(label);
    std::fs::write(&key_file, serde_json::to_vec(&records).unwrap()).unwrap();
    let server = TestServer::start(&directory);
    let expected = "evil\\u{1b}]52;c;value\\u{7}\\trow\\nnext";
    let pools = success(
        command(temp.path())
            .args(["pool", "list", &server.local_url])
            .output()
            .unwrap(),
    );
    let keys = success(
        command(temp.path())
            .arg("--dir")
            .arg(&directory)
            .args(["access", "keys"])
            .output()
            .unwrap(),
    );
    for output in [&pools, &keys] {
        let text = std::str::from_utf8(&output.stdout).unwrap();
        assert!(text.contains(expected), "human label should be literal");
        assert!(
            !text
                .chars()
                .any(|character| character.is_control() && character != '\n')
        );
    }
    let pools = success(
        command(temp.path())
            .args(["pool", "list", &server.local_url, "--json"])
            .output()
            .unwrap(),
    );
    let keys = success(
        command(temp.path())
            .arg("--dir")
            .arg(&directory)
            .args(["access", "keys", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        pools.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    assert_eq!(keys.stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
    let pools: Value = serde_json::from_slice(&pools.stdout).unwrap();
    let keys: Value = serde_json::from_slice(&keys.stdout).unwrap();
    assert_eq!(pools["pools"][0]["name"], label);
    assert_eq!(keys["keys"][0]["name"], label);
}
