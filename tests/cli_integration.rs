// CLI integration tests for v0.0.1 minimal flows.
use std::process::Command;

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
