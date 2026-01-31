// CLI integration tests for v0.0.1 minimal flows.
use std::process::{Command, Stdio};
use std::io::Write;

use serde_json::Value;

fn cmd() -> Command {
    let exe = env!("CARGO_BIN_EXE_plasmite");
    Command::new(exe)
}

fn parse_json(value: &str) -> Value {
    serde_json::from_str(value).expect("valid json")
}

fn parse_json_line(output: &[u8]) -> Value {
    let text = String::from_utf8_lossy(output);
    let line = text.lines().next().expect("json line");
    parse_json(line)
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
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
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
    assert!(created.get("path").unwrap().as_str().unwrap().ends_with("testpool.plasmite"));
    assert!(created.get("bounds").unwrap().get("oldest").is_none());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "testpool",
            "--print",
            "--descrip",
            "ping",
            "--data-json",
            "{\"x\":1}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());
    let poke_json = parse_json(std::str::from_utf8(&poke.stdout).expect("utf8"));
    assert_eq!(poke_json.get("pool").unwrap().as_str().unwrap(), "testpool");
    let seq = poke_json.get("seq").unwrap().as_u64().unwrap();
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

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "testpool",
            "--tail",
            "1",
        ])
        .output()
        .expect("peek");
    assert!(peek.status.success());
    let peek_json = parse_json_line(&peek.stdout);
    assert_eq!(peek_json.get("seq").unwrap().as_u64().unwrap(), seq);
}

#[test]
fn readme_quickstart_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "demo"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "--print",
            "--descrip",
            "ping",
            "--data-json",
            "{\"x\":1}",
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

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
        ])
        .output()
        .expect("peek");
    assert!(peek.status.success());
    let peek_json = parse_json_line(&peek.stdout);
    assert_eq!(peek_json.get("seq").unwrap().as_u64().unwrap(), seq);
}

#[test]
fn not_found_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
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
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "NotFound");
    assert_eq!(inner.get("seq").and_then(|v| v.as_u64()).unwrap(), 999);
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("pool bounds") || hint.contains("peek"));
}

#[test]
fn usage_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "testpool"])
        .output()
        .expect("poke");
    assert_eq!(poke.status.code().unwrap(), 2);
    let err = parse_error_json(&poke.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--data-json") || hint.contains("pipe JSON"));
}

#[test]
fn poke_is_silent_without_print() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "testpool",
            "--data-json",
            "{\"x\":1}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());
    assert!(poke.stdout.is_empty());
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
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "NotFound");
    assert!(inner.get("message").and_then(|v| v.as_str()).unwrap().len() > 0);
    assert!(inner.get("path").and_then(|v| v.as_str()).unwrap().ends_with("missing.plasmite"));
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
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
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
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let again = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
        .output()
        .expect("create again");
    assert_eq!(again.status.code().unwrap(), 4);
    let err = parse_error_json(&again.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "AlreadyExists");
    assert!(inner.get("path").and_then(|v| v.as_str()).unwrap().ends_with("testpool.plasmite"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("different name") || hint.contains("remove"));
}

#[test]
fn permission_error_has_hint_and_causes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("readonly");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");

    let mut perms = std::fs::metadata(&pool_dir).expect("metadata").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&pool_dir, perms).expect("set perms");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
        .output()
        .expect("create");
    assert_eq!(create.status.code().unwrap(), 8);
    let err = parse_error_json(&create.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
    assert!(inner.get("hint").and_then(|v| v.as_str()).unwrap_or("").len() > 0);
    let empty = Vec::new();
    let causes = inner.get("causes").and_then(|v| v.as_array()).unwrap_or(&empty);
    if causes.is_empty() {
        eprintln!("warning: Permission/Io error had no causes; stderr={}", String::from_utf8_lossy(&create.stderr));
    }

    let mut perms = std::fs::metadata(&pool_dir).expect("metadata").permissions();
    perms.set_readonly(false);
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
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "bad",
        ])
        .output()
        .expect("info");
    assert_eq!(info.status.code().unwrap(), 7);
    let err = parse_error_json(&info.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Corrupt");
    assert!(inner.get("path").and_then(|v| v.as_str()).unwrap().ends_with("bad.plasmite"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("Recreate") || hint.contains("recreate"));
}

#[test]
fn poke_streams_json_values_from_stdin() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "testpool"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "testpool", "--print"])
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

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "testpool",
            "--tail",
            "2",
            "--jsonl",
        ])
        .output()
        .expect("peek");
    assert!(peek.status.success());
    let peek_lines = parse_json_lines(&peek.stdout);
    assert_eq!(peek_lines.len(), 2);
    assert_eq!(peek_lines[0].get("data").unwrap()["x"], 1);
    assert_eq!(peek_lines[1].get("data").unwrap()["x"], 2);
}
