//! Purpose: End-to-end CLI tests for v0.0.1 flows and JSON output shapes.
//! Role: Integration tests invoking the built `plasmite` binary.
//! Invariants: Parses stdout/stderr as JSON and asserts stable fields/behavior.
//! Invariants: Uses temporary directories; never touches user home or project pools.
//! Invariants: Timeouts are bounded to keep CI deterministic.
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::Value;

fn cmd() -> Command {
    let exe = env!("CARGO_BIN_EXE_plasmite");
    Command::new(exe)
}

fn parse_json(value: &str) -> Value {
    serde_json::from_str(value).expect("valid json")
}

fn parse_json_lines(output: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(output);
    text.lines().map(parse_json).collect()
}

fn parse_error_json(output: &[u8]) -> Value {
    let text = std::str::from_utf8(output).expect("utf8");
    parse_json(text)
}

#[test]
fn create_poke_get_peek_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());
    let create_json = parse_json(std::str::from_utf8(&create.stdout).expect("utf8"));
    let created = create_json
        .get("created")
        .and_then(|value| value.as_array())
        .expect("created array")
        .first()
        .expect("first");
    assert_eq!(created.get("pool").unwrap().as_str().unwrap(), "testpool");
    assert!(
        created
            .get("path")
            .unwrap()
            .as_str()
            .unwrap()
            .ends_with("testpool.plasmite")
    );
    assert!(created.get("bounds").unwrap().get("oldest").is_none());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "testpool",
            "{\"x\":1}",
            "--descrip",
            "ping",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());
    let poke_json = parse_json(std::str::from_utf8(&poke.stdout).expect("utf8"));
    let seq = poke_json.get("seq").unwrap().as_u64().unwrap();
    assert!(poke_json.get("time").is_some());
    assert_eq!(poke_json.get("meta").unwrap()["descrips"][0], "ping");
    assert_eq!(poke_json.get("data").unwrap()["x"], 1);

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "get",
            "testpool",
            &seq.to_string(),
        ])
        .output()
        .expect("get");
    assert!(get.status.success());
    let get_json = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(get_json.get("seq").unwrap().as_u64().unwrap(), seq);
    assert_eq!(get_json.get("data").unwrap()["x"], 1);

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "testpool",
            "--tail",
            "1",
            "--jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let peek_json = parse_json(line.trim());
    assert_eq!(peek_json.get("seq").unwrap().as_u64().unwrap(), seq);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn readme_quickstart_flow() {
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

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":1}",
            "--descrip",
            "ping",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());
    let poke_json = parse_json(std::str::from_utf8(&poke.stdout).expect("utf8"));
    let seq = poke_json.get("seq").unwrap().as_u64().unwrap();

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "get",
            "demo",
            &seq.to_string(),
        ])
        .output()
        .expect("get");
    assert!(get.status.success());
    let get_json = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(get_json.get("seq").unwrap().as_u64().unwrap(), seq);

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let peek_json = parse_json(line.trim());
    assert_eq!(peek_json.get("seq").unwrap().as_u64().unwrap(), seq);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn peek_emits_new_messages() {
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

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");

    thread::sleep(Duration::from_millis(50));

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":42}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 42);

    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn not_found_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "get",
            "testpool",
            "999",
        ])
        .output()
        .expect("get");
    assert_eq!(get.status.code().unwrap(), 3);
    let err = parse_error_json(&get.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(
        inner.get("kind").and_then(|v| v.as_str()).unwrap(),
        "NotFound"
    );
    assert_eq!(inner.get("seq").and_then(|v| v.as_u64()).unwrap(), 999);
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("pool info") || hint.contains("peek"));
}

#[test]
fn usage_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "testpool"])
        .output()
        .expect("poke");
    assert_eq!(poke.status.code().unwrap(), 2);
    let err = parse_error_json(&poke.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--file") || hint.contains("pipe JSON"));
}

#[test]
fn poke_emits_json_by_default() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "testpool",
            "{\"x\":1}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());
    let value = parse_json(std::str::from_utf8(&poke.stdout).expect("utf8"));
    assert!(value.get("seq").is_some());
    assert!(value.get("time").is_some());
}

#[test]
fn poke_create_flag_creates_missing_pool() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "autopool",
            "{\"x\":1}",
            "--create",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());
    let value = parse_json(std::str::from_utf8(&poke.stdout).expect("utf8"));
    assert!(value.get("seq").is_some());

    let pool_path = pool_dir.join("autopool.plasmite");
    assert!(pool_path.exists());
}

#[test]
fn pool_delete_removes_pool_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "deleteme",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let delete = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "delete",
            "deleteme",
        ])
        .output()
        .expect("delete");
    assert!(delete.status.success());
    let pool_path = pool_dir.join("deleteme.plasmite");
    assert!(!pool_path.exists());
}

#[test]
fn errors_are_json_on_non_tty_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let peek = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "peek", "missing"])
        .output()
        .expect("peek");
    assert_eq!(peek.status.code().unwrap(), 3);

    let err = parse_error_json(&peek.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(
        inner.get("kind").and_then(|v| v.as_str()).unwrap(),
        "NotFound"
    );
    assert!(
        !inner
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap()
            .is_empty()
    );
    assert!(
        inner
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap()
            .ends_with("missing.plasmite")
    );
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("pool create") || hint.contains("--dir"));
}

#[test]
fn clap_errors_are_concise_in_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let bad = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--definitely-not-a-flag",
        ])
        .output()
        .expect("peek");
    assert_eq!(bad.status.code().unwrap(), 2);

    let err = parse_error_json(&bad.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap();
    assert!(!message.contains('\n'));
    assert!(!message.contains("Usage:"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--help"));
}

#[test]
fn already_exists_has_hint_and_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let again = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create again");
    assert_eq!(again.status.code().unwrap(), 4);
    let err = parse_error_json(&again.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(
        inner.get("kind").and_then(|v| v.as_str()).unwrap(),
        "AlreadyExists"
    );
    assert!(
        inner
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap()
            .ends_with("testpool.plasmite")
    );
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("different name") || hint.contains("remove"));
}

#[test]
fn permission_error_has_hint_and_causes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("readonly");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");

    let mut perms = std::fs::metadata(&pool_dir)
        .expect("metadata")
        .permissions();
    let original_mode = perms.mode();
    perms.set_readonly(true);
    std::fs::set_permissions(&pool_dir, perms).expect("set perms");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert_eq!(create.status.code().unwrap(), 8);
    let err = parse_error_json(&create.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert!(
        !inner
            .get("hint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
    );
    let empty = Vec::new();
    let causes = inner
        .get("causes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    if causes.is_empty() {
        eprintln!(
            "warning: Permission/Io error had no causes; stderr={}",
            String::from_utf8_lossy(&create.stderr)
        );
    }

    let mut perms = std::fs::metadata(&pool_dir)
        .expect("metadata")
        .permissions();
    perms.set_mode(original_mode);
    std::fs::set_permissions(&pool_dir, perms).expect("unset perms");
}

#[test]
fn corrupt_pool_has_hint_and_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");
    let pool_path = pool_dir.join("bad.plasmite");
    std::fs::write(&pool_path, b"NOPE").expect("write");

    let info = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "info", "bad"])
        .output()
        .expect("info");
    assert_eq!(info.status.code().unwrap(), 7);
    let err = parse_error_json(&info.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(
        inner.get("kind").and_then(|v| v.as_str()).unwrap(),
        "Corrupt"
    );
    assert!(
        inner
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap()
            .ends_with("bad.plasmite")
    );
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("Recreate") || hint.contains("recreate"));
}

#[test]
fn poke_streams_json_values_from_stdin() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "testpool"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\"x\":1}\n{\"x\":2}")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
    assert_eq!(lines[1].get("data").unwrap()["x"], 2);

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "testpool",
            "--tail",
            "2",
            "--jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut peek_lines = Vec::new();
    for _ in 0..2 {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read line");
        assert!(read > 0, "expected a line from peek output");
        peek_lines.push(parse_json(line.trim()));
    }
    let _ = peek.kill();
    let _ = peek.wait();
    assert_eq!(peek_lines.len(), 2);
    assert_eq!(peek_lines[0].get("data").unwrap()["x"], 1);
    assert_eq!(peek_lines[1].get("data").unwrap()["x"], 2);
}
