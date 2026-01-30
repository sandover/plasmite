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
