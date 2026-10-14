//! Purpose: Feed and fetch black-box CLI integration tests.

pub mod support;
use plasmite::api::{AppendOptions, Durability, Pool, PoolApiExt, PoolOptions};
use support::cli::*;

#[test]
fn feed_with_no_args_prints_help() {
    let output = cmd().args(["feed"]).output().expect("feed");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite feed"));
}

#[test]
fn fetch_with_no_args_prints_help() {
    let output = cmd().args(["fetch"]).output().expect("fetch");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite fetch"));
}

#[test]
fn create_feed_fetch_follow_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "--format",
            "json",
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

    let feed_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "testpool",
            "--data",
            "{\"x\":1}",
            "--tag",
            "ping",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(feed_out.status.success());
    let feed_json = parse_json(std::str::from_utf8(&feed_out.stdout).expect("utf8"));
    let seq = feed_json.get("seq").unwrap().as_u64().unwrap();
    assert!(feed_json.get("time").is_some());
    assert_eq!(feed_json.get("meta").unwrap()["tags"][0], "ping");
    assert_eq!(feed_json["data"], json!({"x": 1}));

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "fetch",
            "testpool",
            &seq.to_string(),
        ])
        .args(["--format", "json"])
        .output()
        .expect("fetch");
    assert!(get.status.success());
    let get_json = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(get_json.get("seq").unwrap().as_u64().unwrap(), seq);
    assert_eq!(get_json.get("data").unwrap()["x"], 1);

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "testpool",
            "--tail",
            "1",
            "--format",
            "json",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let follower_json = parse_json(line.trim());
    assert_eq!(follower_json.get("seq").unwrap().as_u64().unwrap(), seq);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn feed_stdout_contains_the_complete_message() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "testpool",
            "--data",
            "{\"x\":1}",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());
    let value = parse_json(std::str::from_utf8(&emit_out.stdout).expect("utf8"));
    assert!(value.get("seq").is_some());
    assert!(value.get("time").is_some());
}

#[test]
fn emit_short_file_flag_reads_single_json_file() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let input_file = temp.path().join("one.json");
    std::fs::write(&input_file, b"{\"x\":1}\n").expect("write input");

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--data",
            "-f",
            input_file.to_str().unwrap(),
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );
    let messages = parse_json_lines(&emit_out.stdout);
    assert_eq!(messages.len(), 1);

    let get = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "1"])
        .args(["--format", "json"])
        .output()
        .expect("fetch");
    assert!(get.status.success());
    let value = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(value["data"]["x"], 1);
}

#[test]
fn feed_file_reads_multiple_json_records() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let input_file = temp.path().join("events.jsonl");
    std::fs::write(&input_file, b"{\"x\":1}\n{\"x\":2}\n").expect("write input");

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--data",
            "--input-format",
            "json",
            "--file",
            input_file.to_str().unwrap(),
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );
    let messages = parse_json_lines(&emit_out.stdout);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["seq"], 1);
    assert_eq!(messages[1]["seq"], 2);
    assert_eq!(messages[0]["data"]["x"], 1);
    assert_eq!(messages[1]["data"]["x"], 2);

    let get_one = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "1"])
        .args(["--format", "json"])
        .output()
        .expect("fetch one");
    assert!(get_one.status.success());
    let first = parse_json(std::str::from_utf8(&get_one.stdout).expect("utf8"));
    assert_eq!(first["data"]["x"], 1);

    let get_two = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "2"])
        .args(["--format", "json"])
        .output()
        .expect("fetch two");
    assert!(get_two.status.success());
    let second = parse_json(std::str::from_utf8(&get_two.stdout).expect("utf8"));
    assert_eq!(second["data"]["x"], 2);
}

#[test]
fn feed_file_auto_handles_multiline_json_data() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let input_file = temp.path().join("pretty.json");
    std::fs::write(&input_file, b"{\n  \"x\": 1,\n  \"y\": 2\n}\n").expect("write input");

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--data",
            "--file",
            input_file.to_str().unwrap(),
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );
    let messages = parse_json_lines(&emit_out.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["seq"], 1);

    let get = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "1"])
        .args(["--format", "json"])
        .output()
        .expect("fetch");
    assert!(get.status.success());
    let value = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(value["data"]["x"], 1);
    assert_eq!(value["data"]["y"], 2);
}

#[test]
fn emit_retries_when_pool_is_busy() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let pool_path = pool_dir.join("busy.plasmite");
    let file = File::open(&pool_path).expect("open pool");
    file.try_lock_exclusive().expect("try lock");

    let (tx, rx) = mpsc::channel();
    let pool_dir_str = pool_dir.to_str().unwrap().to_string();
    thread::spawn(move || {
        let output = cmd()
            .args([
                "--dir",
                &pool_dir_str,
                "feed",
                "busy",
                "--data",
                "{\"x\":1}",
                "--retry",
                "5",
                "--retry-delay",
                "50ms",
            ])
            .args(["--format", "json"])
            .output()
            .expect("feed");
        let _ = tx.send(output);
    });

    thread::sleep(Duration::from_millis(150));
    fs2::FileExt::unlock(&file).expect("unlock");

    let output = rx.recv_timeout(Duration::from_secs(2)).expect("output");
    assert!(
        output.status.success(),
        "feed failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    assert!(value.get("seq").is_some());
}

#[test]
fn feed_auto_handles_pretty_json_data() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--data",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .args(["--format", "json"])
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\n  \"x\": 1,\n  \"y\": 2\n}\n")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].get("seq").is_some());
    assert_eq!(lines[0]["data"]["x"], 1);
}

#[test]
fn feed_file_tty_emits_complete_json_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let input_file = temp.path().join("events.jsonl");
    std::fs::write(&input_file, "{\"x\":1}\n{\"x\":2}\n").expect("write input");

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

    let output = cmd_tty(&[
        "--color",
        "never",
        "--dir",
        pool_dir.to_str().unwrap(),
        "feed",
        "demo",
        "--data",
        "--file",
        input_file.to_str().unwrap(),
        "--input-format",
        "json",
        "--format",
        "json",
    ]);
    assert!(output.status.success());
    let text = sanitize_tty_text(&output.stdout);
    // macOS `script` writes a literal terminal EOF marker before the child output.
    // Remove only that leading marker so valid message data remains untouched.
    let text = text.strip_prefix("^D").unwrap_or(&text);
    let messages = parse_json_lines(text.as_bytes());
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["data"]["x"], 1);
    assert_eq!(messages[1]["data"]["x"], 2);
}

#[test]
fn feed_json_format_accepts_pretty_json_data() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--data",
            "--input-format",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .args(["--format", "json"])
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\n  \"x\": 1,\n  \"y\": 2\n}\n")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].get("seq").is_some());
    assert_eq!(lines[0]["data"]["x"], 1);
}

#[test]
fn emit_create_flag_creates_missing_pool() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "autopool",
            "--data",
            "{\"x\":1}",
            "--create",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());
    let value = parse_json(std::str::from_utf8(&emit_out.stdout).expect("utf8"));
    assert!(value.get("seq").is_some());

    let pool_path = pool_dir.join("autopool.plasmite");
    assert!(pool_path.exists());
}

#[test]
fn emit_missing_pool_hint_suggests_create() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "missing",
            "--data",
            "{\"x\":1}",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert_eq!(output.status.code(), Some(3));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("exact command"));
    assert!(hint.contains("plasmite pool create missing"));
}

#[test]
fn emit_remote_url_happy_path_appends_message() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let server = ServeProcess::start(&pool_dir);
    let access_home = temp.path().join("access-home");
    let mut connect = cmd()
        .args(["access", "connect", &server.remote_url])
        .env("PLASMITE_ACCESS_HOME", &access_home)
        .stdin(std::process::Stdio::piped())
        .args(["--format", "json"])
        .spawn()
        .expect("connect");
    use std::io::Write;
    writeln!(
        connect.stdin.take().expect("stdin"),
        "{}",
        server.access_key()
    )
    .expect("key");
    let connect = connect.wait_with_output().expect("connect output");
    assert!(connect.status.success());
    let pool_url = format!("{}/demo", server.base_url);
    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            &pool_url,
            "{\"x\":1}",
            "--tag",
            "ping",
        ])
        .env("PLASMITE_ACCESS_HOME", &access_home)
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());
    let value = parse_json(std::str::from_utf8(&emit_out.stdout).expect("utf8"));
    assert_eq!(value.get("seq").and_then(|v| v.as_u64()), Some(1));
    assert!(value["data"].is_object());
    assert_eq!(
        value.get("meta").and_then(|v| v.get("tags")),
        Some(&json!(["ping"]))
    );
    let copied = cmd()
        .args(["feed", &pool_url, &value.to_string(), "--format", "json"])
        .env("PLASMITE_ACCESS_HOME", &access_home)
        .output()
        .expect("copy remote message");
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    let message = parse_json(std::str::from_utf8(&copied.stdout).unwrap());
    assert_eq!(message["seq"], 2);
    assert_eq!(message["data"], value["data"]);
    assert_eq!(message["meta"], value["meta"]);
}

#[test]
fn emit_remote_url_rejects_api_shaped_path() {
    let output = cmd()
        .args([
            "feed",
            "http://localhost:9170/v0/pools/demo/append",
            "--data",
            "{\"x\":1}",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(!output.status.success());
    let err = parse_error_json(&output.stderr);
    assert_eq!(
        err.get("error")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("Usage")
    );
}

#[test]
fn emit_remote_url_rejects_trailing_slash() {
    let output = cmd()
        .args(["feed", "http://localhost:9170/demo/", "--data", "{\"x\":1}"])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(!output.status.success());
    let err = parse_error_json(&output.stderr);
    assert_eq!(
        err.get("error")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("Usage")
    );
}

#[test]
fn emit_remote_url_rejects_create_flag() {
    let output = cmd()
        .args([
            "feed",
            "http://localhost:9170/demo",
            "--data",
            "--create",
            "{\"x\":1}",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert!(!output.status.success());
    let err = parse_error_json(&output.stderr);
    assert_eq!(
        err.get("error")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("Usage")
    );
    let message = err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(message.contains("does not support --create"));
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("Create the pool on the server with pool create."));
}

#[test]
fn emit_remote_create_rejected() {
    let output = cmd()
        .args([
            "feed",
            "http://localhost:9170/demo",
            "--data",
            "--create",
            "{\"x\":1}",
        ])
        .args(["--format", "json"])
        .output()
        .expect("feed");
    assert_eq!(output.status.code(), Some(2));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("does not support --create"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("Create the pool on the server with pool create."));
}

#[test]
fn feed_streams_json_values_from_stdin() {
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "testpool",
            "--data",
            "--input-format",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .args(["--format", "json"])
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\"x\":1}\n{\"x\":2}")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].get("seq").is_some());
    assert!(lines[1].get("seq").is_some());
    assert_eq!(lines[0]["data"]["x"], 1);
    assert_eq!(lines[1]["data"]["x"], 2);

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "testpool",
            "--tail",
            "2",
            "--format",
            "json",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut follower_lines = Vec::new();
    for _ in 0..2 {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read line");
        assert!(read > 0, "expected a line from follow output");
        follower_lines.push(parse_json(line.trim()));
    }
    let _ = follower.kill();
    let _ = follower.wait();
    assert_eq!(follower_lines.len(), 2);
    assert_eq!(follower_lines[0].get("data").unwrap()["x"], 1);
    assert_eq!(follower_lines[1].get("data").unwrap()["x"], 2);
}

const FIXTURE_TIMESTAMP_NS: u64 = 1_600_000_000_123_456_789;

fn create_fixture_pool(pool_dir: &Path, name: &str) -> Pool {
    std::fs::create_dir_all(pool_dir).expect("pool directory");
    Pool::create(
        pool_dir.join(format!("{name}.plasmite")),
        PoolOptions::new(1024 * 1024),
    )
    .expect("create fixture pool")
}

fn append_fixture(pool: &mut Pool) -> (Vec<u8>, String) {
    let tags = vec!["fixture".to_string(), "binary".to_string()];
    let message = pool
        .append_json(
            &json!({"text": "before\0after\nnext line", "nested": {"value": 7}}),
            &tags,
            AppendOptions::new(FIXTURE_TIMESTAMP_NS, Durability::Fast),
        )
        .expect("append fixture");
    assert_eq!(message.seq, 1);
    let bytes = pool.get_lite3(message.seq).expect("fixture Lite3").payload;
    (bytes, message.time)
}

fn append_marker(pool: &mut Pool) {
    pool.append_json(
        &json!({"marker": true}),
        &[],
        AppendOptions::new(FIXTURE_TIMESTAMP_NS - 1, Durability::Fast),
    )
    .expect("append marker");
}

fn fetch_output(pool_dir: &Path, pool: &str, seq: &str, options: &[&str]) -> std::process::Output {
    let mut command = cmd();
    command
        .arg("--dir")
        .arg(pool_dir)
        .args(["fetch", pool, seq])
        .args(options);
    command.output().expect("fetch")
}

#[test]
fn fetch_defaults_to_json_and_pretty_and_raw_lite3_are_explicit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut pool = create_fixture_pool(&pool_dir, "source");
    let (payload, _) = append_fixture(&mut pool);
    drop(pool);

    let default = fetch_output(&pool_dir, "source", "1", &[]);
    let pretty = fetch_output(&pool_dir, "source", "1", &["--format", "pretty"]);
    assert!(default.status.success());
    assert!(pretty.status.success());
    let readable = parse_json(std::str::from_utf8(&default.stdout).expect("readable utf8"));
    assert_eq!(readable["seq"], 1);
    assert_eq!(readable["data"]["text"], "before\0after\nnext line");
    let pretty_value = parse_json(std::str::from_utf8(&pretty.stdout).expect("pretty utf8"));
    assert_eq!(pretty_value, readable);
    assert!(pretty.stdout.iter().filter(|byte| **byte == b'\n').count() > 1);

    let format_json = fetch_output(&pool_dir, "source", "1", &["--format", "json"]);
    assert!(format_json.status.success());
    let machine = parse_json(std::str::from_utf8(&format_json.stdout).expect("json utf8"));
    assert_eq!(machine["meta"]["tags"], json!(["fixture", "binary"]));

    let raw = fetch_output(&pool_dir, "source", "1", &["--format", "lite3-raw"]);
    assert!(raw.status.success());
    assert_eq!(raw.stderr, b"");
    assert_eq!(raw.stdout, payload);
}

#[test]
fn feed_lite3_file_preserves_bytes_tags_and_assigns_fresh_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut source = create_fixture_pool(&pool_dir, "source");
    let (payload, source_time) = append_fixture(&mut source);
    drop(source);

    let mut target = create_fixture_pool(&pool_dir, "target");
    append_marker(&mut target);
    drop(target);

    let input_file = temp.path().join("message.lite3");
    std::fs::write(&input_file, &payload).expect("write Lite3 fixture");
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool directory"),
            "feed",
            "target",
            "--input-format",
            "lite3-raw",
            "--file",
            input_file.to_str().expect("input path"),
            "--format",
            "json",
        ])
        .output()
        .expect("feed Lite3 file");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = parse_json(std::str::from_utf8(&output.stdout).expect("receipt utf8"));
    assert_eq!(receipt["seq"], 2);
    assert_ne!(receipt["time"].as_str(), Some(source_time.as_str()));
    assert_eq!(receipt["meta"]["tags"], json!(["fixture", "binary"]));
    assert!(receipt["data"].is_object());

    let raw = fetch_output(&pool_dir, "target", "2", &["--format", "lite3-raw"]);
    assert!(raw.status.success());
    assert_eq!(raw.stdout, payload);
    let message = fetch_output(&pool_dir, "target", "2", &["--format", "json"]);
    assert!(message.status.success());
    let message = parse_json(std::str::from_utf8(&message.stdout).expect("message utf8"));
    assert_eq!(message["meta"]["tags"], json!(["fixture", "binary"]));
    assert_eq!(message["data"]["text"], "before\0after\nnext line");
}

#[test]
fn feed_lite3_file_dash_reads_one_binary_document_from_stdin() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut source = create_fixture_pool(&pool_dir, "source");
    let (payload, _) = append_fixture(&mut source);
    drop(source);
    drop(create_fixture_pool(&pool_dir, "target"));

    let mut feed = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool directory"),
            "feed",
            "target",
            "--input-format",
            "lite3-raw",
            "--file",
            "-",
            "--format",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("feed stdin");
    feed.stdin
        .take()
        .expect("feed stdin")
        .write_all(&payload)
        .expect("write Lite3 stdin");
    let output = feed.wait_with_output().expect("feed output");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = parse_json(std::str::from_utf8(&output.stdout).expect("receipt utf8"));
    assert_eq!(receipt["seq"], 1);

    let raw = fetch_output(&pool_dir, "target", "1", &["--format", "lite3-raw"]);
    assert!(raw.status.success());
    assert_eq!(raw.stdout, payload);
}

#[test]
fn native_fetch_pipe_feeds_the_same_lite3_bytes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut source = create_fixture_pool(&pool_dir, "source");
    let (payload, _) = append_fixture(&mut source);
    drop(source);
    let mut target = create_fixture_pool(&pool_dir, "target");
    append_marker(&mut target);
    drop(target);

    let mut fetch = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool directory"),
            "fetch",
            "source",
            "1",
            "--format",
            "lite3-raw",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("fetch raw Lite3");
    let input = Stdio::from(fetch.stdout.take().expect("fetch stdout"));
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool directory"),
            "feed",
            "target",
            "--input-format",
            "lite3-raw",
            "--format",
            "json",
        ])
        .stdin(input)
        .stdout(Stdio::piped())
        .output()
        .expect("feed raw pipe");
    let fetch_status = fetch.wait().expect("fetch status");
    assert!(fetch_status.success());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = parse_json(std::str::from_utf8(&output.stdout).expect("receipt utf8"));
    assert_eq!(receipt["seq"], 2);

    let raw = fetch_output(&pool_dir, "target", "2", &["--format", "lite3-raw"]);
    assert!(raw.status.success());
    assert_eq!(raw.stdout, payload);
}

#[test]
fn http_fetch_and_feed_match_local_lite3_bytes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut source = create_fixture_pool(&pool_dir, "source");
    let (payload, _) = append_fixture(&mut source);
    drop(source);
    let mut target = create_fixture_pool(&pool_dir, "target");
    append_marker(&mut target);
    drop(target);

    let local = fetch_output(&pool_dir, "source", "1", &["--format", "lite3-raw"]);
    assert!(local.status.success());
    assert_eq!(local.stdout, payload);

    let server = ServeProcess::start_with_args_and_scheme(&pool_dir, &[], "http");
    let source_url = format!("{}/source", server.base_url);
    let target_url = format!("{}/target", server.base_url);
    let remote = cmd()
        .args(["fetch", &source_url, "1", "--format", "lite3-raw"])
        .output()
        .expect("HTTP fetch Lite3");
    assert!(
        remote.status.success(),
        "{}",
        String::from_utf8_lossy(&remote.stderr)
    );
    assert_eq!(remote.stdout, local.stdout);

    let input_file = temp.path().join("remote-input.lite3");
    std::fs::write(&input_file, &payload).expect("write remote Lite3 fixture");
    let feed = cmd()
        .args([
            "feed",
            &target_url,
            "--input-format",
            "lite3-raw",
            "--file",
            input_file.to_str().expect("input path"),
            "--format",
            "json",
        ])
        .output()
        .expect("HTTP feed Lite3");
    assert!(
        feed.status.success(),
        "{}",
        String::from_utf8_lossy(&feed.stderr)
    );
    let receipt = parse_json(std::str::from_utf8(&feed.stdout).expect("receipt utf8"));
    assert_eq!(receipt["seq"], 2);
    assert_eq!(receipt["meta"]["tags"], json!(["fixture", "binary"]));

    let local_target = fetch_output(&pool_dir, "target", "2", &["--format", "lite3-raw"]);
    let remote_target = cmd()
        .args(["fetch", &target_url, "2", "--format", "lite3-raw"])
        .output()
        .expect("HTTP fetch appended Lite3");
    assert!(local_target.status.success());
    assert!(remote_target.status.success());
    assert_eq!(local_target.stdout, payload);
    assert_eq!(remote_target.stdout, payload);
}

#[test]
fn feed_lite3_rejects_invalid_options_before_pool_side_effects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let input_file = temp.path().join("input.lite3");
    std::fs::write(&input_file, b"not needed").expect("write input");
    let directory = pool_dir.to_str().expect("pool directory");
    let input_path = input_file.to_str().expect("input path");

    let cases: [(&str, &[&str]); 3] = [
        (
            "inline",
            &[
                "--create",
                "--input-format",
                "lite3-raw",
                "{\"x\":1}",
                "--format",
                "json",
            ],
        ),
        (
            "tagged",
            &[
                "--create",
                "--input-format",
                "lite3-raw",
                "--tag",
                "extra",
                "--file",
                input_path,
                "--format",
                "json",
            ],
        ),
        (
            "skip",
            &[
                "--create",
                "--input-format",
                "lite3-raw",
                "--errors",
                "skip",
                "--file",
                input_path,
                "--format",
                "json",
            ],
        ),
    ];
    for (name, args) in cases {
        let output = cmd()
            .args(["--dir", directory, "feed", name])
            .args(args)
            .output()
            .expect("invalid Lite3 feed");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let error = parse_error_json(&output.stderr);
        assert_eq!(error["error"]["kind"], "Usage");
        assert!(
            !pool_dir.join(format!("{name}.plasmite")).exists(),
            "invalid flags created pool {name}"
        );
    }
    assert!(
        !pool_dir.exists(),
        "invalid flags created the pool directory"
    );

    let missing_pool = cmd()
        .args([
            "--dir",
            directory,
            "feed",
            "missing",
            "--input-format",
            "lite3-raw",
            "--tag",
            "extra",
            "--file",
            input_path,
            "--format",
            "json",
        ])
        .output()
        .expect("invalid feed with missing pool");
    assert_eq!(missing_pool.status.code(), Some(2));
    let error = parse_error_json(&missing_pool.stderr);
    assert_eq!(error["error"]["kind"], "Usage");
    assert!(
        !pool_dir.exists(),
        "invalid flags opened or created the pool directory"
    );
}

#[test]
fn remote_binary_create_rejects_before_waiting_for_stdin() {
    let mut child = cmd()
        .args([
            "feed",
            "http://127.0.0.1:1/unused",
            "--create",
            "--input-format",
            "lite3-raw",
            "--format",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("invalid remote create");
    let status = wait_within(
        &mut child,
        Duration::from_secs(2),
        "remote --create validation",
    );
    assert_eq!(status.code(), Some(2));
    let output = child.wait_with_output().expect("error output");
    assert!(output.stdout.is_empty());
    let error = parse_error_json(&output.stderr);
    assert_eq!(error["error"]["kind"], "Usage");
}

#[test]
fn feed_lite3_raw_allows_empty_stream_and_rejects_invalid_payloads() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let mut source = create_fixture_pool(&pool_dir, "source");
    let (payload, _) = append_fixture(&mut source);
    drop(source);
    drop(create_fixture_pool(&pool_dir, "target"));

    let mut truncated = payload.clone();
    truncated.pop();
    let doc = plasmite::api::Lite3DocRef::new(&payload);
    let data = doc.key_offset("data").expect("data offset");
    let text = doc.key_offset_at(data, "text").expect("text offset");
    let mut invalid_utf8 = payload.clone();
    assert_eq!(invalid_utf8[text + 5], b'b');
    invalid_utf8[text + 5] = 0xff;
    let empty_file = temp.path().join("empty.lite3");
    std::fs::write(&empty_file, []).expect("write empty input");
    let empty = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool directory"),
            "feed",
            "target",
            "--input-format",
            "lite3-raw",
            "--file",
            empty_file.to_str().expect("empty path"),
        ])
        .output()
        .expect("feed empty stream");
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    assert!(empty.stdout.is_empty());

    let invalid = [
        ("truncated", truncated),
        ("corrupt", vec![0x01, 0x02, 0x03]),
        ("nested-invalid-utf8", invalid_utf8),
    ];

    for (name, bytes) in invalid {
        let input_file = temp.path().join(format!("{name}.lite3"));
        std::fs::write(&input_file, bytes).expect("write malformed Lite3");
        let output = cmd()
            .args([
                "--dir",
                pool_dir.to_str().expect("pool directory"),
                "feed",
                "target",
                "--input-format",
                "lite3-raw",
                "--file",
                input_file.to_str().expect("input path"),
                "--format",
                "json",
            ])
            .output()
            .expect("feed malformed Lite3");
        assert!(!output.status.success(), "accepted {name} Lite3 payload");
    }

    let input_file = temp.path().join("valid.lite3");
    std::fs::write(&input_file, &payload).expect("write valid Lite3");
    let accepted = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool directory"),
            "feed",
            "target",
            "--input-format",
            "lite3-raw",
            "--file",
            input_file.to_str().expect("input path"),
            "--format",
            "json",
        ])
        .output()
        .expect("feed valid Lite3");
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let receipt = parse_json(std::str::from_utf8(&accepted.stdout).expect("receipt utf8"));
    assert_eq!(
        receipt["seq"], 1,
        "invalid payloads advanced the pool bounds"
    );

    let raw = fetch_output(&pool_dir, "target", "1", &["--format", "lite3-raw"]);
    assert!(raw.status.success());
    assert_eq!(raw.stdout, payload);
}

#[test]
fn create_size_implies_creation_for_feed_and_tap_without_changing_existing_pools() {
    for family in ["feed", "tap"] {
        let temp = tempfile::tempdir().unwrap();
        let pool_path = temp.path().join("sized.plasmite");
        let run = |pool: &str, size: Option<&str>| {
            let mut command = cmd();
            command.arg("--dir").arg(temp.path()).args([family, pool]);
            if let Some(size) = size {
                command.args(["--create-size", size]);
            }
            if family == "feed" {
                command.arg("{\"value\":1}");
            } else {
                command.args([
                    "--quiet",
                    "--",
                    env!("CARGO_BIN_EXE_plasmite"),
                    "version",
                    "--format",
                    "json",
                ]);
            }
            command.output().unwrap()
        };
        let missing = run("missing", None);
        assert_eq!(missing.status.code(), Some(3));
        assert!(!temp.path().join("missing.plasmite").exists());
        let invalid = run("invalid-size", Some("bad"));
        assert_eq!(invalid.status.code(), Some(2));
        assert!(!temp.path().join("invalid-size.plasmite").exists());
        let created = run("sized", Some("64K"));
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
        assert_eq!(std::fs::metadata(&pool_path).unwrap().len(), 65536);
        let original = Pool::open(&pool_path).unwrap().get_message(1).unwrap();
        let reopened = run("sized", Some("1M"));
        assert!(
            reopened.status.success(),
            "{}",
            String::from_utf8_lossy(&reopened.stderr)
        );
        assert_eq!(std::fs::metadata(&pool_path).unwrap().len(), 65536);
        let retained = Pool::open(&pool_path).unwrap().get_message(1).unwrap();
        assert_eq!(retained.seq, original.seq);
        assert_eq!(retained.time, original.time);
        assert_eq!(retained.data, original.data);
    }
}

#[test]
fn remote_and_udp_feed_reject_create_size_without_waiting_for_a_server() {
    for target in ["http://localhost:9170/demo", "udp://127.0.0.1:9701/demo"] {
        let output = cmd()
            .args([
                "feed",
                target,
                "--create-size",
                "64K",
                "{}",
                "--format",
                "json",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(parse_error_json(&output.stderr)["error"]["kind"], "Usage");
    }
}
