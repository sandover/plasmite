//! Purpose: End-to-end CLI tests for v0.0.1 flows and JSON output shapes.
//! Role: Integration tests invoking the built `plasmite` binary.
//! Invariants: Parses stdout/stderr as JSON and asserts stable fields/behavior.
//! Invariants: Uses temporary directories; never touches user home or project pools.
//! Invariants: Timeouts are bounded to keep CI deterministic.
use std::fs::File;
use std::io::Write;
use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::{thread::sleep, time::Instant};

use fs2::FileExt;
use rcgen::{Certificate, CertificateParams, SanType};
use serde_json::{Value, json};

fn cmd() -> Command {
    let exe = env!("CARGO_BIN_EXE_plasmite");
    Command::new(exe)
}

struct ServeProcess {
    child: Child,
    base_url: String,
}

impl ServeProcess {
    fn start(pool_dir: &std::path::Path) -> Self {
        Self::start_with_args(pool_dir, &[])
    }

    fn start_with_args(pool_dir: &std::path::Path, extra_args: &[&str]) -> Self {
        Self::start_with_args_and_scheme(pool_dir, extra_args, "http")
    }

    fn start_with_args_and_scheme(
        pool_dir: &std::path::Path,
        extra_args: &[&str],
        scheme: &str,
    ) -> Self {
        let port = pick_port().expect("port");
        let bind = format!("127.0.0.1:{port}");
        let base_url = format!("{scheme}://{bind}");

        let mut command = cmd();
        command.args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "serve",
            "--bind",
            &bind,
        ]);
        if !extra_args.is_empty() {
            command.args(extra_args);
        }
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve");

        wait_for_server(bind.parse().expect("addr")).expect("server ready");

        Self { child, base_url }
    }
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_json(value: &str) -> Value {
    serde_json::from_str(value).expect("valid json")
}

fn parse_json_lines(output: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(output);
    text.lines().map(parse_json).collect()
}

fn read_json_value<R: Read>(reader: R) -> Value {
    let mut stream = serde_json::Deserializer::from_reader(reader).into_iter::<Value>();
    stream.next().expect("json value").expect("valid json")
}

fn parse_error_json(output: &[u8]) -> Value {
    let text = std::str::from_utf8(output).expect("utf8");
    parse_json(text)
}

fn parse_notice_json(line: &str) -> Value {
    parse_json(line.trim())
}

fn pick_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn wait_for_server(addr: SocketAddr) -> std::io::Result<()> {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "server did not start in time",
            ));
        }
        sleep(Duration::from_millis(20));
    }
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
    assert_eq!(created.get("name").unwrap().as_str().unwrap(), "testpool");
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
            "--size",
            "128K",
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
fn peek_one_exits_after_first_match() {
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
            "--one",
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
            "{\"x\":1}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":2}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let stdout = peek.stdout.take().expect("stdout");
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = line_tx.send(line);
    });

    let line = line_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 1);

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let status = peek.wait().expect("wait");
        let _ = exit_tx.send(status);
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("peek exit");
    assert!(status.success());
}

#[test]
fn peek_tail_one_emits_nth_match() {
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
            "--tail",
            "2",
            "--jsonl",
            "--one",
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
            "{\"x\":1}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":2}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let stdout = peek.stdout.take().expect("stdout");
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = line_tx.send(line);
    });

    let line = line_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let status = peek.wait().expect("wait");
        let _ = exit_tx.send(status);
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("peek exit");
    assert!(status.success());
}

#[test]
fn peek_timeout_exits_when_no_output() {
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

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--jsonl",
            "--timeout",
            "500ms",
        ])
        .output()
        .expect("peek");
    assert_eq!(output.status.code().unwrap(), 124);
    assert!(output.stdout.is_empty());
}

#[test]
fn peek_timeout_with_one_exits_on_message() {
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

    let pool_dir_str = pool_dir.to_str().unwrap().to_string();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        let _ = cmd()
            .args(["--dir", &pool_dir_str, "poke", "demo", "{\"x\":1}"])
            .output();
    });

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--jsonl",
            "--one",
            "--timeout",
            "5s",
        ])
        .output()
        .expect("peek");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn peek_data_only_jsonl_emits_payload() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--data-only",
            "--one",
        ])
        .output()
        .expect("peek");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("x").unwrap().as_i64().unwrap(), 1);
    assert!(lines[0].get("data").is_none());
}

#[test]
fn peek_data_only_where_filters_envelope() {
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
            "drop",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":2}",
            "--descrip",
            "keep",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--data-only",
            "--one",
            "--where",
            r#".meta.descrips[]? == "keep""#,
        ])
        .output()
        .expect("peek");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("x").unwrap().as_i64().unwrap(), 2);
    assert!(lines[0].get("data").is_none());
}

#[test]
fn peek_data_only_pretty_emits_payload() {
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
            "{\"x\":3}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--format",
            "pretty",
            "--data-only",
            "--one",
        ])
        .output()
        .expect("peek");
    assert!(output.status.success());
    let value = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    assert_eq!(value.get("x").unwrap().as_i64().unwrap(), 3);
    assert!(value.get("data").is_none());
}

#[test]
fn peek_where_filters_messages() {
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
            "drop",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":2}",
            "--descrip",
            "keep",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "10",
            "--jsonl",
            "--where",
            r#".meta.descrips[]? == "keep""#,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn peek_where_multiple_predicates_and() {
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
            "{\"level\":1}",
            "--descrip",
            "alpha",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"level\":2}",
            "--descrip",
            "alpha",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "10",
            "--jsonl",
            "--where",
            r#".meta.descrips[]? == "alpha""#,
            "--where",
            ".data.level >= 2",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["level"], 2);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn peek_where_invalid_expression_is_usage_error() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--where",
            "not valid jq",
        ])
        .output()
        .expect("peek");
    assert_eq!(peek.status.code().unwrap(), 2);
    let err = parse_error_json(&peek.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
}

#[test]
fn peek_where_non_boolean_expression_is_usage_error() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--where",
            ".data",
        ])
        .output()
        .expect("peek");
    assert_eq!(peek.status.code().unwrap(), 2);
    let err = parse_error_json(&peek.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
}

#[test]
fn peek_where_with_since_emits_matches() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":5}",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--since",
            "1h",
            "--jsonl",
            "--where",
            ".data.x == 5",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 5);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn peek_where_with_format_pretty_emits_matches() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--format",
            "pretty",
            "--where",
            ".data.x == 1",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let reader = BufReader::new(stdout);
    let value = read_json_value(reader);
    assert_eq!(value.get("data").unwrap()["x"], 1);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn peek_where_with_quiet_drops_suppresses_notice() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--where",
            ".data.x == 1",
            "--quiet-drops",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 1);
    let _ = peek.kill();
    let output = peek.wait_with_output().expect("wait");
    assert!(
        output.stderr.is_empty(),
        "expected no drop notices on stderr"
    );
}

#[test]
fn peek_where_multiple_predicates_with_since() {
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
            "alpha",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "{\"x\":2}",
            "--descrip",
            "alpha",
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--since",
            "1h",
            "--jsonl",
            "--where",
            r#".meta.descrips[]? == "alpha""#,
            "--where",
            ".data.x == 2",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);
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
fn poke_retries_when_pool_is_busy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "busy",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let pool_path = pool_dir.join("busy.plasmite");
    let file = File::open(&pool_path).expect("open pool");
    file.lock_exclusive().expect("lock");

    let (tx, rx) = mpsc::channel();
    let pool_dir_str = pool_dir.to_str().unwrap().to_string();
    thread::spawn(move || {
        let output = cmd()
            .args([
                "--dir",
                &pool_dir_str,
                "poke",
                "busy",
                "{\"x\":1}",
                "--retry",
                "5",
                "--retry-delay",
                "50ms",
            ])
            .output()
            .expect("poke");
        let _ = tx.send(output);
    });

    thread::sleep(Duration::from_millis(150));
    fs2::FileExt::unlock(&file).expect("unlock");

    let output = rx.recv_timeout(Duration::from_secs(2)).expect("output");
    assert!(
        output.status.success(),
        "poke failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    assert!(value.get("seq").is_some());
}

#[test]
fn color_always_colorizes_pretty_stdout() {
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

    let info = cmd()
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "demo",
        ])
        .output()
        .expect("info");
    assert!(info.status.success());
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(stdout.contains("\u{1b}[36m\"name\"\u{1b}[0m"));
}

#[test]
fn color_never_does_not_emit_ansi() {
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

    let info = cmd()
        .args([
            "--color",
            "never",
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "demo",
        ])
        .output()
        .expect("info");
    assert!(info.status.success());
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(!stdout.contains("\u{1b}["));
}

#[test]
fn color_always_does_not_color_jsonl() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut peek = cmd()
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--format",
            "jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek jsonl");
    let stdout = peek.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let line = line.trim_end();
    assert!(!line.contains("\u{1b}["));
    let _ = parse_json(line);
    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn poke_auto_handles_pretty_json() {
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

    let mut poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\n  \"x\": 1,\n  \"y\": 2\n}\n")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn poke_auto_handles_event_stream() {
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

    let mut poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"data: {\"x\":1}\n\ndata: {\"x\":2}\n\n")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].get("data").unwrap()["x"], 2);
}

#[test]
fn poke_auto_detects_json_seq() {
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

    let mut poke = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "poke", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"\x1e{\"x\":1}\x1e{\"x\":2}")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn poke_auto_skip_reports_oversize() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        let big = "x".repeat(1024 * 1024 + 1);
        let line = format!("{{\"big\":\"{big}\"}}\n");
        stdin.write_all(line.as_bytes()).expect("write stdin");
        stdin.write_all(b"{\"ok\":1}\n").expect("write ok");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert_eq!(output.status.code().unwrap(), 1);
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    let notices = parse_json_lines(&output.stderr);
    let oversize = notices.iter().find(|value| {
        value
            .get("notice")
            .and_then(|v| v.get("details"))
            .and_then(|v| v.get("error_kind"))
            .and_then(|v| v.as_str())
            == Some("Oversize")
    });
    assert!(oversize.is_some());
}

#[test]
fn poke_seq_mode_parses_rs_records() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "seq",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"\x1e{\"x\":1}\x1e{\"x\":2}")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn poke_errors_skip_emits_notices_and_nonzero() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "jsonl",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\"x\":1}\nnot-json\n{\"x\":2}\n")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert_eq!(output.status.code().unwrap(), 1);
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);

    let notices = parse_json_lines(&output.stderr);
    assert!(notices.len() >= 2);
    let first = notices[0]
        .get("notice")
        .and_then(|v| v.as_object())
        .expect("notice");
    assert_eq!(
        first.get("kind").and_then(|v| v.as_str()),
        Some("ingest_skip")
    );
    let summary = notices
        .iter()
        .find(|value| {
            value
                .get("notice")
                .and_then(|v| v.get("kind"))
                .and_then(|v| v.as_str())
                == Some("ingest_summary")
        })
        .expect("summary");
    assert!(summary.get("notice").is_some());
}

#[test]
fn poke_errors_skip_reports_oversize() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "jsonl",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        let big = "x".repeat(1024 * 1024 + 1);
        let line = format!("{{\"big\":\"{big}\"}}\n");
        stdin.write_all(line.as_bytes()).expect("write stdin");
        stdin.write_all(b"{\"ok\":1}\n").expect("write ok");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert_eq!(output.status.code().unwrap(), 1);
    let notices = parse_json_lines(&output.stderr);
    let oversize = notices.iter().find(|value| {
        value
            .get("notice")
            .and_then(|v| v.get("details"))
            .and_then(|v| v.get("error_kind"))
            .and_then(|v| v.as_str())
            == Some("Oversize")
    });
    assert!(oversize.is_some());
}

#[test]
fn poke_in_json_accepts_pretty_json() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\n  \"x\": 1,\n  \"y\": 2\n}\n")
            .expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn poke_in_json_errors_skip_returns_nonzero() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "json",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin.write_all(b"{\"x\":1").expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert_eq!(output.status.code().unwrap(), 1);
    let notices = parse_json_lines(&output.stderr);
    assert!(notices.iter().any(|value| {
        value
            .get("notice")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str())
            == Some("ingest_skip")
    }));
}

#[test]
fn poke_event_stream_flushes_trailing_event() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "auto",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin.write_all(b"data: {\"x\":1}\n").expect("write stdin");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn poke_jq_mode_rejects_skip() {
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

    let mut poke = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "poke",
            "demo",
            "-i",
            "jq",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("poke");
    {
        let stdin = poke.stdin.as_mut().expect("stdin");
        stdin.write_all(b"{\"x\":1}\n{\"x\":2}\n").expect("write");
    }
    let output = poke.wait_with_output().expect("poke output");
    assert_eq!(output.status.code().unwrap(), 2);
    let err = parse_error_json(&output.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
}

#[test]
fn pool_list_lists_pools_sorted_by_name() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "beta",
            "alpha",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let list = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "list"])
        .output()
        .expect("list");
    assert!(list.status.success());

    let value = parse_json(std::str::from_utf8(&list.stdout).expect("utf8"));
    let pools = value
        .get("pools")
        .and_then(|v| v.as_array())
        .expect("pools array");
    assert_eq!(pools.len(), 2);
    assert_eq!(pools[0].get("name").and_then(|v| v.as_str()), Some("alpha"));
    assert_eq!(pools[1].get("name").and_then(|v| v.as_str()), Some("beta"));
}

#[test]
fn peek_since_future_exits_empty() {
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

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--since",
            "2999-01-01T00:00:00Z",
        ])
        .output()
        .expect("peek");
    assert!(peek.status.success());
    assert!(peek.stdout.is_empty());
}

#[test]
fn peek_format_jsonl_matches_jsonl_alias() {
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
        ])
        .output()
        .expect("poke");
    assert!(poke.status.success());

    let mut fmt = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--format",
            "jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("peek format");
    let fmt_stdout = fmt.stdout.take().expect("stdout");
    let mut fmt_reader = BufReader::new(fmt_stdout);
    let mut fmt_line = String::new();
    let read = fmt_reader.read_line(&mut fmt_line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let fmt_line = fmt_line.trim_end();
    assert!(!fmt_line.contains('\n'));
    let _ = parse_json(fmt_line);
    let _ = fmt.kill();
    let _ = fmt.wait();

    let mut alias = cmd()
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
        .expect("peek jsonl");
    let alias_stdout = alias.stdout.take().expect("stdout");
    let mut alias_reader = BufReader::new(alias_stdout);
    let mut alias_line = String::new();
    let read = alias_reader.read_line(&mut alias_line).expect("read line");
    assert!(read > 0, "expected a line from peek output");
    let alias_line = alias_line.trim_end();
    assert!(!alias_line.contains('\n'));
    let _ = parse_json(alias_line);
    let _ = alias.kill();
    let _ = alias.wait();
}

#[test]
fn peek_emits_drop_notice_on_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "--size",
            "1M",
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
        .stderr(Stdio::piped())
        .spawn()
        .expect("peek");

    let stdout = peek.stdout.take().expect("stdout");
    let stderr = peek.stderr.take().expect("stderr");

    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).unwrap_or(0);
            if read == 0 {
                break;
            }
            if !line.trim().is_empty() {
                let _ = tx.send(line.clone());
                break;
            }
        }
    });

    for i in 0..200u64 {
        let payload = "a".repeat(8192);
        let poke = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "poke",
                "demo",
                &format!("{{\"x\":{i},\"pad\":\"{payload}\"}}"),
            ])
            .output()
            .expect("poke");
        if !poke.status.success() {
            let stderr = String::from_utf8_lossy(&poke.stderr);
            panic!("poke failed at {i}: {stderr}");
        }
    }

    let notice_line = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("drop notice");
    let notice_json = parse_notice_json(&notice_line);
    let notice = notice_json
        .get("notice")
        .and_then(|v| v.as_object())
        .expect("notice object");
    assert_eq!(notice.get("kind").and_then(|v| v.as_str()), Some("drop"));
    assert_eq!(notice.get("cmd").and_then(|v| v.as_str()), Some("peek"));
    assert_eq!(notice.get("pool").and_then(|v| v.as_str()), Some("demo"));
    let details = notice
        .get("details")
        .and_then(|v| v.as_object())
        .expect("details");
    let dropped = details
        .get("dropped_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(dropped > 0);
    assert!(details.get("last_seen_seq").is_some());
    assert!(details.get("next_seen_seq").is_some());

    let _ = peek.kill();
    let _ = peek.wait();
}

#[test]
fn peek_rejects_conflicting_output_flags() {
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

    let peek = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--format",
            "jsonl",
        ])
        .output()
        .expect("peek");
    assert_eq!(peek.status.code().unwrap(), 2);
    let err = parse_error_json(&peek.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--format jsonl") || hint.contains("--jsonl"));
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
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "peek",
            "missing",
        ])
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
fn doctor_reports_ok_as_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "doctorpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let doctor = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "doctor", "doctorpool"])
        .output()
        .expect("doctor");
    assert!(doctor.status.success());
    let output = parse_json(std::str::from_utf8(&doctor.stdout).expect("utf8"));
    let reports = output
        .get("reports")
        .and_then(|v| v.as_array())
        .expect("reports array");
    assert_eq!(reports.len(), 1);
    let report = reports[0].as_object().expect("report object");
    assert_eq!(report.get("status").and_then(|v| v.as_str()), Some("ok"));
}

#[test]
fn doctor_reports_corrupt_and_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");
    let pool_path = pool_dir.join("bad.plasmite");
    std::fs::write(&pool_path, b"NOPE").expect("write");

    let doctor = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "doctor", "bad"])
        .output()
        .expect("doctor");
    assert_eq!(doctor.status.code().unwrap(), 7);
    let output = parse_json(std::str::from_utf8(&doctor.stdout).expect("utf8"));
    let reports = output
        .get("reports")
        .and_then(|v| v.as_array())
        .expect("reports array");
    let report = reports[0].as_object().expect("report object");
    assert_eq!(
        report.get("status").and_then(|v| v.as_str()),
        Some("corrupt")
    );
}

#[test]
fn doctor_all_reports_mixed_ok_and_corrupt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "ok"])
        .output()
        .expect("create");
    assert!(create.status.success());

    std::fs::create_dir_all(&pool_dir).expect("mkdir");
    let pool_path = pool_dir.join("bad.plasmite");
    std::fs::write(&pool_path, b"NOPE").expect("write");

    let doctor = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "doctor", "--all"])
        .output()
        .expect("doctor");
    assert_eq!(doctor.status.code().unwrap(), 7);
    let output = parse_json(std::str::from_utf8(&doctor.stdout).expect("utf8"));
    let reports = output
        .get("reports")
        .and_then(|v| v.as_array())
        .expect("reports array");
    let statuses = reports
        .iter()
        .filter_map(|report| report.get("status").and_then(|v| v.as_str()))
        .collect::<Vec<_>>();
    assert!(statuses.contains(&"ok"));
    assert!(statuses.contains(&"corrupt"));
}

#[test]
fn doctor_missing_pool_reports_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let doctor = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "doctor", "missing"])
        .output()
        .expect("doctor");
    assert!(!doctor.status.success());
    let err = parse_error_json(&doctor.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "NotFound");
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
}

#[test]
fn serve_non_loopback_write_requires_tls_or_insecure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let token_path = temp.path().join("token.txt");
    std::fs::write(&token_path, "secret").expect("write token");

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
        "descrips": ["oversized"],
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
    match ureq::get(&tail_url).call() {
        Ok(_) => panic!("expected tail timeout rejection"),
        Err(ureq::Error::Status(code, resp)) => {
            assert_eq!(code, 400);
            let body = resp.into_string().expect("body");
            let value: Value = serde_json::from_str(&body).expect("json");
            assert_eq!(value["error"]["kind"], "Usage");
        }
        Err(err) => panic!("request failed: {err:?}"),
    }
}

#[test]
fn serve_tls_allows_healthz_with_trusted_cert() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let _ = ureq::rustls::crypto::aws_lc_rs::default_provider().install_default();

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
