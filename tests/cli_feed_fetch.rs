//! Purpose: Feed and fetch black-box CLI integration tests.

pub mod support;
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
            "--json",
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
            "{\"x\":1}",
            "--tag",
            "ping",
        ])
        .output()
        .expect("feed");
    assert!(feed_out.status.success());
    let feed_json = parse_json(std::str::from_utf8(&feed_out.stdout).expect("utf8"));
    let seq = feed_json.get("seq").unwrap().as_u64().unwrap();
    assert!(feed_json.get("time").is_some());
    assert_eq!(feed_json.get("meta").unwrap()["tags"][0], "ping");
    assert!(feed_json.get("data").is_none());

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "fetch",
            "testpool",
            &seq.to_string(),
        ])
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
            "--jsonl",
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
fn emit_emits_json_by_default() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "testpool",
            "{\"x\":1}",
        ])
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
            "-f",
            input_file.to_str().unwrap(),
        ])
        .output()
        .expect("feed");
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );
    let receipts = parse_json_lines(&emit_out.stdout);
    assert_eq!(receipts.len(), 1);

    let get = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "1"])
        .output()
        .expect("fetch");
    assert!(get.status.success());
    let value = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(value["data"]["x"], 1);
}

#[test]
fn emit_file_jsonl_ingests_multiple_records() {
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

    let input_file = temp.path().join("events.jsonl");
    std::fs::write(&input_file, b"{\"x\":1}\n{\"x\":2}\n").expect("write input");

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--file",
            input_file.to_str().unwrap(),
        ])
        .output()
        .expect("feed");
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );
    let receipts = parse_json_lines(&emit_out.stdout);
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0]["seq"], 1);
    assert_eq!(receipts[1]["seq"], 2);

    let get_one = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "1"])
        .output()
        .expect("fetch one");
    assert!(get_one.status.success());
    let first = parse_json(std::str::from_utf8(&get_one.stdout).expect("utf8"));
    assert_eq!(first["data"]["x"], 1);

    let get_two = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "2"])
        .output()
        .expect("fetch two");
    assert!(get_two.status.success());
    let second = parse_json(std::str::from_utf8(&get_two.stdout).expect("utf8"));
    assert_eq!(second["data"]["x"], 2);
}

#[test]
fn emit_file_auto_handles_multiline_json() {
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

    let input_file = temp.path().join("pretty.json");
    std::fs::write(&input_file, b"{\n  \"x\": 1,\n  \"y\": 2\n}\n").expect("write input");

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "--file",
            input_file.to_str().unwrap(),
        ])
        .output()
        .expect("feed");
    assert!(
        emit_out.status.success(),
        "{}",
        String::from_utf8_lossy(&emit_out.stderr)
    );
    let receipts = parse_json_lines(&emit_out.stdout);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["seq"], 1);

    let get = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "fetch", "demo", "1"])
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
                "{\"x\":1}",
                "--retry",
                "5",
                "--retry-delay",
                "50ms",
            ])
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
fn emit_auto_handles_pretty_json() {
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

    let mut emit_out = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "feed", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
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
    assert!(lines[0].get("data").is_none());
}

#[test]
fn emit_auto_handles_event_stream() {
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

    let mut emit_out = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "feed", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"data: {\"x\":1}\n\ndata: {\"x\":2}\n\n")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].get("seq").is_some());
    assert!(lines[1].get("data").is_none());
}

#[test]
fn emit_auto_detects_json_seq() {
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

    let mut emit_out = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "feed", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"\x1e{\"x\":1}\x1e{\"x\":2}")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].get("seq").is_some());
    assert!(lines[0].get("data").is_none());
}

#[test]
fn emit_auto_skip_reports_oversize() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        let big = "x".repeat(1024 * 1024 + 1);
        let line = format!("{{\"big\":\"{big}\"}}\n");
        stdin.write_all(line.as_bytes()).expect("write stdin");
        stdin.write_all(b"{\"ok\":1}\n").expect("write ok");
    }
    let output = emit_out.wait_with_output().expect("feed output");
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
fn feed_file_tty_emits_human_receipts() {
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
        "--file",
        input_file.to_str().unwrap(),
        "--in",
        "jsonl",
    ]);
    assert!(output.status.success());
    let text = sanitize_tty_text(&output.stdout);
    assert_eq!(text.matches("fed seq=").count(), 2);
    assert!(!text.contains("\"seq\":"));
}

#[test]
fn emit_seq_mode_parses_rs_records() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "-i",
            "seq",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"\x1e{\"x\":1}\x1e{\"x\":2}")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].get("seq").is_some());
    assert!(lines[0].get("data").is_none());
}

#[test]
fn emit_errors_skip_emits_notices_and_nonzero() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
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
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\"x\":1}\nnot-json\n{\"x\":2}\n")
            .expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
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
fn emit_errors_skip_reports_oversize() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
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
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        let big = "x".repeat(1024 * 1024 + 1);
        let line = format!("{{\"big\":\"{big}\"}}\n");
        stdin.write_all(line.as_bytes()).expect("write stdin");
        stdin.write_all(b"{\"ok\":1}\n").expect("write ok");
    }
    let output = emit_out.wait_with_output().expect("feed output");
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
fn emit_in_json_accepts_pretty_json() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "-i",
            "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
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
    assert!(lines[0].get("data").is_none());
}

#[test]
fn emit_in_json_errors_skip_returns_nonzero() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "-i",
            "json",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin.write_all(b"{\"x\":1").expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
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
fn emit_event_stream_flushes_trailing_event() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "-i",
            "auto",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin.write_all(b"data: {\"x\":1}\n").expect("write stdin");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].get("seq").is_some());
    assert!(lines[0].get("data").is_none());
}

#[test]
fn emit_jq_mode_rejects_skip() {
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

    let mut emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "-i",
            "jq",
            "-e",
            "skip",
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("feed");
    {
        let stdin = emit_out.stdin.as_mut().expect("stdin");
        stdin.write_all(b"{\"x\":1}\n{\"x\":2}\n").expect("write");
    }
    let output = emit_out.wait_with_output().expect("feed output");
    assert_eq!(output.status.code().unwrap(), 2);
    let err = parse_error_json(&output.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
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
            "{\"x\":1}",
            "--create",
        ])
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
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert_eq!(output.status.code(), Some(3));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--create"));
    assert!(hint.contains("exact command"));
    assert!(hint.contains("plasmite feed missing --create"));
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
        .output()
        .expect("create");
    assert!(create.status.success());

    let server = ServeProcess::start(&pool_dir);
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
        .output()
        .expect("feed");
    assert!(emit_out.status.success());
    let value = parse_json(std::str::from_utf8(&emit_out.stdout).expect("utf8"));
    assert_eq!(value.get("seq").and_then(|v| v.as_u64()), Some(1));
    assert!(value.get("data").is_none());
    assert_eq!(
        value.get("meta").and_then(|v| v.get("tags")),
        Some(&json!(["ping"]))
    );
}

#[test]
fn emit_remote_url_rejects_api_shaped_path() {
    let output = cmd()
        .args([
            "feed",
            "http://localhost:9170/v0/pools/demo/append",
            "{\"x\":1}",
        ])
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
        .args(["feed", "http://localhost:9170/demo/", "{\"x\":1}"])
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
            "--create",
            "{\"x\":1}",
        ])
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
    assert!(hint.contains("server-side"));
}

#[test]
fn emit_remote_create_rejected() {
    let output = cmd()
        .args([
            "feed",
            "http://localhost:9170/demo",
            "--create",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert_eq!(output.status.code(), Some(2));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("does not support --create"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("server-side"));
}

#[test]
fn emit_remote_url_auth_errors_propagate() {
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

    let server = ServeProcess::start_with_args(&pool_dir, &["--token", "secret-token"]);
    let pool_url = format!("{}/demo", server.base_url);
    let output = cmd()
        .args(["feed", &pool_url, "{\"x\":1}"])
        .output()
        .expect("feed");
    assert!(!output.status.success());
    let err = parse_error_json(&output.stderr);
    assert_eq!(
        err.get("error")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("Permission")
    );
}

#[test]
fn emit_remote_url_accepts_token_and_token_file_flags() {
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

    let server = ServeProcess::start_with_args(&pool_dir, &["--token", "secret-token"]);
    let pool_url = format!("{}/demo", server.base_url);

    let with_token = cmd()
        .args(["feed", &pool_url, "{\"x\":1}", "--token", "secret-token"])
        .output()
        .expect("feed");
    assert!(with_token.status.success());

    let token_file = temp.path().join("token.txt");
    std::fs::write(&token_file, "secret-token\n").expect("write token file");
    let with_token_file = cmd()
        .args([
            "feed",
            &pool_url,
            "{\"x\":2}",
            "--token-file",
            token_file.to_str().unwrap(),
        ])
        .output()
        .expect("feed");
    assert!(with_token_file.status.success());
}

#[test]
fn emit_and_follow_local_reject_remote_auth_tls_flags() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let cert_path = temp.path().join("dev-cert.pem");
    std::fs::write(&cert_path, "not-a-real-cert\n").expect("write cert");

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

    let feed = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
            "--token",
            "devtoken",
        ])
        .output()
        .expect("feed");
    assert_eq!(feed.status.code(), Some(2));
    let feed_err = parse_error_json(&feed.stderr);
    assert_eq!(
        feed_err
            .get("error")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("Usage")
    );

    let follow = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "0",
            "--timeout",
            "100ms",
            "--tls-ca",
            cert_path.to_str().unwrap(),
        ])
        .output()
        .expect("follow");
    assert_eq!(follow.status.code(), Some(2));
    let follow_err = parse_error_json(&follow.stderr);
    assert_eq!(
        follow_err
            .get("error")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str()),
        Some("Usage")
    );
}

#[test]
fn emit_streams_json_values_from_stdin() {
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

    let mut emit_out = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "feed", "testpool"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
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
    assert!(lines[0].get("data").is_none());
    assert!(lines[1].get("data").is_none());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "testpool",
            "--tail",
            "2",
            "--jsonl",
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
