//! Purpose: Follow and duplex black-box CLI integration tests.

pub mod support;
use support::cli::*;

#[test]
fn duplex_with_no_args_prints_help() {
    let output = cmd().args(["duplex"]).output().expect("duplex");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite duplex"));
    assert!(stderr.contains("Send and follow from one command"));
}

#[test]
fn follow_with_no_args_prints_help() {
    let output = cmd().args(["follow"]).output().expect("follow");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite follow"));
}

#[test]
fn follow_emits_new_messages() {
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

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--one",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");

    let stdout = follower.stdout.take().expect("stdout");
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = line_tx.send(line);
    });

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":42}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let line = line_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow output");
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 42);
    let status = follower.wait().expect("follow wait");
    assert!(status.success(), "follow status={status:?}");
}

#[test]
fn follow_one_exits_after_first_match() {
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

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--jsonl",
            "--tail",
            "1",
            "--one",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");

    thread::sleep(Duration::from_millis(50));

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let stdout = follower.stdout.take().expect("stdout");
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = line_tx.send(line);
    });

    let line = line_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 1);

    // Send a second message after the first is observed so `--one` ordering
    // is deterministic without relying on fixed startup sleeps.
    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let status = follower.wait().expect("wait");
        let _ = exit_tx.send(status);
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow exit");
    assert!(status.success());
}

#[test]
fn follow_tail_one_emits_nth_match() {
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

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "2",
            "--jsonl",
            "--one",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");

    thread::sleep(Duration::from_millis(50));

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let stdout = follower.stdout.take().expect("stdout");
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = line_tx.send(line);
    });

    let line = line_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let status = follower.wait().expect("wait");
        let _ = exit_tx.send(status);
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow exit");
    assert!(status.success());
}

#[test]
fn follow_timeout_exits_when_no_output() {
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
            "follow",
            "demo",
            "--jsonl",
            "--timeout",
            "500ms",
        ])
        .output()
        .expect("follow");
    assert_eq!(output.status.code().unwrap(), 124);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn follow_timeout_on_tty_prints_message() {
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

    let output = cmd_tty(&[
        "--color",
        "never",
        "--dir",
        pool_dir.to_str().unwrap(),
        "follow",
        "demo",
        "--one",
        "--timeout",
        "200ms",
    ]);
    assert_eq!(output.status.code(), Some(124));
    let text = sanitize_tty_text(&output.stdout);
    assert!(text.contains("No messages received (timed out after 200ms)."));
}

#[test]
fn follow_timeout_with_one_exits_on_message() {
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
            .args(["--dir", &pool_dir_str, "feed", "demo", "{\"x\":1}"])
            .output();
    });

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--jsonl",
            "--one",
            "--timeout",
            "5s",
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("data").unwrap()["x"], 1);
}

#[test]
fn follow_data_only_jsonl_emits_payload() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--data-only",
            "--one",
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("x").unwrap().as_i64().unwrap(), 1);
    assert!(lines[0].get("data").is_none());
}

#[test]
fn follow_data_only_where_filters_envelope() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
            "--tag",
            "drop",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
            "--tag",
            "keep",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--data-only",
            "--one",
            "--where",
            r#".meta.tags[]? == "keep""#,
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].get("x").unwrap().as_i64().unwrap(), 2);
    assert!(lines[0].get("data").is_none());
}

#[test]
fn follow_data_only_pretty_emits_payload() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":3}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--format",
            "pretty",
            "--data-only",
            "--one",
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let value = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    assert_eq!(value.get("x").unwrap().as_i64().unwrap(), 3);
    assert!(value.get("data").is_none());
}

#[test]
fn follow_where_filters_messages() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
            "--tag",
            "drop",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
            "--tag",
            "keep",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "10",
            "--jsonl",
            "--where",
            r#".meta.tags[]? == "keep""#,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_tag_filters_messages() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
            "--tag",
            "drop",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
            "--tag",
            "keep",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "10",
            "--jsonl",
            "--tag",
            "keep",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_tag_and_where_compose_with_and() {
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

    let events = [
        (r#"{"service":"billing","level":"warn"}"#, "keep"),
        (r#"{"service":"billing","level":"error"}"#, "keep"),
        (r#"{"service":"payments","level":"error"}"#, "keep"),
    ];
    for (payload, tag) in events {
        let emit_out = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "demo",
                payload,
                "--tag",
                tag,
            ])
            .output()
            .expect("feed");
        assert!(emit_out.status.success());
    }

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "10",
            "--jsonl",
            "--tag",
            "keep",
            "--where",
            r#".data.level == "error""#,
            "--where",
            r#".data.service == "billing""#,
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value["data"]["service"], "billing");
    assert_eq!(value["data"]["level"], "error");
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_where_multiple_predicates_and() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"level\":1}",
            "--tag",
            "alpha",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"level\":2}",
            "--tag",
            "alpha",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "10",
            "--jsonl",
            "--where",
            r#".meta.tags[]? == "alpha""#,
            "--where",
            ".data.level >= 2",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["level"], 2);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_where_invalid_expression_is_usage_error() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--where",
            "not valid jq",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code().unwrap(), 2);
    let err = parse_error_json(&follower.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
}

#[test]
fn follow_where_non_boolean_expression_is_usage_error() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--where",
            ".data",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code().unwrap(), 2);
    let err = parse_error_json(&follower.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
}

#[test]
fn follow_where_with_since_emits_matches() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":5}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--since",
            "1h",
            "--jsonl",
            "--where",
            ".data.x == 5",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 5);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_where_with_format_pretty_emits_matches() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
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
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let reader = BufReader::new(stdout);
    let value = read_json_value(reader);
    assert_eq!(value.get("data").unwrap()["x"], 1);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_where_with_quiet_drops_suppresses_notice() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
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
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 1);
    let _ = follower.kill();
    let output = follower.wait_with_output().expect("wait");
    assert!(
        output.stderr.is_empty(),
        "expected no drop notices on stderr"
    );
}

#[test]
fn follow_where_multiple_predicates_with_since() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
            "--tag",
            "alpha",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
            "--tag",
            "alpha",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--since",
            "1h",
            "--jsonl",
            "--where",
            r#".meta.tags[]? == "alpha""#,
            "--where",
            ".data.x == 2",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").unwrap()["x"], 2);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_since_future_exits_empty() {
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

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--since",
            "2999-01-01T00:00:00Z",
        ])
        .output()
        .expect("follow");
    assert!(follower.status.success());
    assert!(follower.stdout.is_empty());
}

#[test]
fn follow_since_future_missing_pool_reports_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "missing",
            "--since",
            "2999-01-01T00:00:00Z",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code(), Some(3));
    let err = parse_error_json(&follower.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
}

#[test]
fn follow_format_jsonl_matches_jsonl_alias() {
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

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut fmt = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--format",
            "jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow format");
    let fmt_stdout = fmt.stdout.take().expect("stdout");
    let fmt_line = read_line_with_timeout(fmt_stdout, Duration::from_secs(2));
    assert!(!fmt_line.is_empty(), "expected a line from follow output");
    let fmt_line = fmt_line.trim_end();
    assert!(!fmt_line.contains('\n'));
    let _ = parse_json(fmt_line);
    let _ = fmt.kill();
    let _ = fmt.wait();

    let mut alias = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow jsonl");
    let alias_stdout = alias.stdout.take().expect("stdout");
    let alias_line = read_line_with_timeout(alias_stdout, Duration::from_secs(2));
    assert!(!alias_line.is_empty(), "expected a line from follow output");
    let alias_line = alias_line.trim_end();
    assert!(!alias_line.contains('\n'));
    let _ = parse_json(alias_line);
    let _ = alias.kill();
    let _ = alias.wait();
}

#[test]
fn follow_emits_drop_notice_on_stderr() {
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

    let mut follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--jsonl",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("follow");

    let stdout = follower.stdout.take().expect("stdout");
    let stderr = follower.stderr.take().expect("stderr");

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
        let emit_out = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "demo",
                &format!("{{\"x\":{i},\"pad\":\"{payload}\"}}"),
            ])
            .output()
            .expect("feed");
        if !emit_out.status.success() {
            let stderr = String::from_utf8_lossy(&emit_out.stderr);
            panic!("feed failed at {i}: {stderr}");
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
    assert_eq!(notice.get("cmd").and_then(|v| v.as_str()), Some("follow"));
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

    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_rejects_conflicting_output_flags() {
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

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--jsonl",
            "--format",
            "jsonl",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code().unwrap(), 2);
    let err = parse_error_json(&follower.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--format jsonl") || hint.contains("--jsonl"));
}

#[test]
fn follow_create_flag_creates_missing_pool() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "autopool",
            "--create",
            "--timeout",
            "20ms",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code(), Some(124));
    assert!(pool_dir.join("autopool.plasmite").exists());
}

#[test]
fn follow_missing_pool_has_actionable_hint() {
    // This assertion currently checks the fallback exact command hint text for missing pools.
    let output = cmd().args(["follow", "-n", "1"]).output().expect("follow");
    assert_eq!(output.status.code().unwrap(), 2);
    let err = parse_error_json(&output.stderr);
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("pool ref"));
    assert!(hint.contains("plasmite follow chat -n 1"));
}

#[test]
fn follow_missing_pool_hint_suggests_create() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "follow", "missing"])
        .output()
        .expect("follow");
    assert_eq!(output.status.code(), Some(3));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--create"));
    assert!(hint.contains("exact command"));
    assert!(hint.contains("plasmite follow missing --create"));
}

#[test]
fn follow_remote_url_rejects_create_flag() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "http://127.0.0.1:65535/demo",
            "--create",
        ])
        .output()
        .expect("follow");
    assert_eq!(output.status.code(), Some(2));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("does not support --create"));
}

#[test]
fn follow_remote_tls_ca_and_skip_verify_work() {
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

    let feed = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(feed.status.success());

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
    let pool_url = format!("{}/demo", server.base_url);

    let trusted = cmd()
        .args([
            "follow",
            &pool_url,
            "--tail",
            "1",
            "--one",
            "--jsonl",
            "--timeout",
            "2s",
            "--tls-ca",
            cert_path.to_str().unwrap(),
        ])
        .output()
        .expect("follow");
    assert!(
        trusted.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&trusted.stderr)
    );

    let skipped = cmd()
        .args([
            "follow",
            &pool_url,
            "--tail",
            "1",
            "--one",
            "--jsonl",
            "--timeout",
            "2s",
            "--tls-skip-verify",
        ])
        .output()
        .expect("follow");
    assert!(
        skipped.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&skipped.stderr)
    );
}

#[test]
fn follow_remote_url_happy_path_reads_recent_messages() {
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

    let first = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(first.status.success());
    let second = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":2}",
        ])
        .output()
        .expect("feed");
    assert!(second.status.success());

    let server = ServeProcess::start(&pool_dir);
    let pool_url = format!("{}/demo", server.base_url);
    let follower = cmd()
        .args([
            "follow",
            &pool_url,
            "--tail",
            "1",
            "--one",
            "--jsonl",
            "--timeout",
            "2s",
        ])
        .output()
        .expect("follow");
    assert!(
        follower.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&follower.stderr)
    );
    let value = parse_json(std::str::from_utf8(&follower.stdout).expect("utf8").trim());
    assert_eq!(value.get("seq").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(value.get("data").and_then(|v| v.get("x")), Some(&json!(2)));
}

#[test]
fn follow_remote_url_supports_tag_filter() {
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

    for (payload, tag) in [("{\"x\":1}", "drop"), ("{\"x\":2}", "keep")] {
        let emit_out = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "demo",
                payload,
                "--tag",
                tag,
            ])
            .output()
            .expect("feed");
        assert!(emit_out.status.success());
    }

    let server = ServeProcess::start(&pool_dir);
    let pool_url = format!("{}/demo", server.base_url);
    let mut follower = cmd()
        .args([
            "follow", &pool_url, "--tail", "10", "--jsonl", "--tag", "keep",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let value = parse_json(line.trim());
    assert_eq!(value.get("data").and_then(|v| v.get("x")), Some(&json!(2)));
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn follow_remote_url_rejects_api_shaped_path() {
    let output = cmd()
        .args([
            "follow",
            "http://localhost:9170/v0/pools/demo/tail",
            "--jsonl",
        ])
        .output()
        .expect("follow");
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
fn follow_remote_url_rejects_since_and_replay() {
    let pool_url = "http://localhost:9170/demo";

    let since = cmd()
        .args(["follow", pool_url, "--since", "5m"])
        .output()
        .expect("follow");
    assert!(!since.status.success());
    let since_err = parse_error_json(&since.stderr);
    let since_message = since_err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(since_message.contains("does not support --since"));

    let replay = cmd()
        .args(["follow", pool_url, "--tail", "5", "--replay", "1"])
        .output()
        .expect("follow");
    assert!(!replay.status.success());
    let replay_err = parse_error_json(&replay.stderr);
    let replay_message = replay_err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(replay_message.contains("does not support --replay"));
}

#[test]
fn follow_remote_url_rejects_future_since() {
    let pool_url = "http://localhost:9170/demo";

    let since = cmd()
        .args(["follow", pool_url, "--since", "2999-01-01T00:00:00Z"])
        .output()
        .expect("follow");
    assert_eq!(since.status.code(), Some(2));
    let since_err = parse_error_json(&since.stderr);
    let since_kind = since_err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(since_kind, "Usage");
    let since_message = since_err
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(since_message.contains("does not support --since"));
}

#[test]
fn follow_remote_url_timeout_returns_124() {
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
    let follower = cmd()
        .args(["follow", &pool_url, "--jsonl", "--timeout", "150ms"])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code(), Some(124));
}

#[test]
fn duplex_non_tty_echoes_followed_messages_without_self_echo() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "chat",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let seed_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "chat",
            "{\"from\":\"bob\",\"msg\":\"seed\"}",
        ])
        .output()
        .expect("seed feed");
    assert!(seed_out.status.success());

    let mut duplex = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "duplex",
            "chat",
            "--create",
            "--me",
            "alice",
            "--tail",
            "1",
            "--jsonl",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("duplex");
    let stdout = duplex.stdout.take().expect("duplex stdout");
    let (line_tx, line_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).unwrap_or(0);
            if read == 0 {
                break;
            }
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let first_line = line_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("expected seed line");
    let first_value = parse_json(first_line.trim());
    assert_eq!(first_value.get("data").unwrap()["from"], "bob");
    {
        let stdin = duplex.stdin.as_mut().expect("duplex stdin");
        stdin
            .write_all(b"{\"from\":\"alice\",\"msg\":\"reply\"}\n")
            .expect("write stdin");
    }
    assert!(
        line_rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "expected self-suppression to avoid echoing alice message"
    );
    let _ = duplex.stdin.take();
    let status = duplex.wait().expect("duplex wait");
    assert_eq!(status.code(), Some(0), "unexpected duplex exit code");

    let follow_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "chat",
            "--tail",
            "2",
            "--jsonl",
            "--timeout",
            "150ms",
        ])
        .output()
        .expect("follow");
    assert!(
        follow_out.status.code() == Some(0) || follow_out.status.code() == Some(124),
        "unexpected follow exit code: {:?}",
        follow_out.status.code()
    );
    let follow_lines = parse_json_lines(&follow_out.stdout);
    assert_eq!(follow_lines.len(), 2);
    assert_eq!(follow_lines[0].get("data").unwrap()["from"], "bob");
    assert_eq!(follow_lines[1].get("data").unwrap()["from"], "alice");
}

#[test]
fn duplex_remote_url_rejects_create_flag() {
    let output = cmd()
        .args([
            "duplex",
            "http://127.0.0.1:65535/chat",
            "--create",
            "--me",
            "alice",
        ])
        .output()
        .expect("duplex");
    assert_eq!(output.status.code(), Some(2));
    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("does not support --create"));
}

#[test]
fn duplex_remote_url_rejects_since_even_when_future() {
    let output = cmd()
        .args([
            "duplex",
            "http://127.0.0.1:65535/chat",
            "--since",
            "2999-01-01T00:00:00Z",
        ])
        .output()
        .expect("duplex");
    assert_eq!(output.status.code(), Some(2));
    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("does not support --since"));
}

#[test]
fn duplex_since_future_missing_pool_reports_not_found() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "duplex",
            "missing",
            "--since",
            "2999-01-01T00:00:00Z",
        ])
        .output()
        .expect("duplex");
    assert_eq!(output.status.code(), Some(3));
    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
}

#[test]
fn duplex_remote_happy_path_sends_and_reads() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "chat",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let seed_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "chat",
            "{\"from\":\"bob\",\"msg\":\"seed\"}",
        ])
        .output()
        .expect("seed feed");
    assert!(seed_out.status.success());

    let server = ServeProcess::start(&pool_dir);
    let pool_url = format!("{}/chat", server.base_url);
    let mut duplex = cmd()
        .args([
            "duplex", &pool_url, "--me", "alice", "--tail", "1", "--jsonl",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("duplex");
    let stdout = duplex.stdout.take().expect("duplex stdout");
    let first_line = read_line_with_timeout(stdout, Duration::from_secs(2));
    let first_value = parse_json(first_line.trim());
    assert_eq!(first_value.get("data").unwrap()["from"], "bob");

    {
        let stdin = duplex.stdin.as_mut().expect("duplex stdin");
        stdin
            .write_all(b"{\"from\":\"alice\",\"msg\":\"remote-reply\"}\n")
            .expect("write stdin");
    }
    let _ = duplex.stdin.take();
    let status = duplex.wait().expect("duplex wait");
    assert_eq!(status.code(), Some(0), "unexpected duplex exit code");

    let follow_out = cmd()
        .args([
            "follow",
            &pool_url,
            "--tail",
            "2",
            "--jsonl",
            "--timeout",
            "200ms",
        ])
        .output()
        .expect("follow");
    assert!(
        follow_out.status.code() == Some(0) || follow_out.status.code() == Some(124),
        "unexpected follow exit code: {:?}",
        follow_out.status.code()
    );
    let follow_lines = parse_json_lines(&follow_out.stdout);
    assert_eq!(follow_lines.len(), 2);
    assert_eq!(follow_lines[0].get("data").unwrap()["from"], "bob");
    assert_eq!(follow_lines[1].get("data").unwrap()["from"], "alice");
}

#[test]
fn follow_replay_emits_messages_in_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rp"])
        .output()
        .expect("create");
    for i in 1..=3 {
        cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "rp",
                &format!("{{\"i\":{i}}}"),
            ])
            .output()
            .expect("feed");
        sleep(Duration::from_millis(20));
    }

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rp",
            "--tail",
            "100",
            "--replay",
            "0",
            "--jsonl",
        ])
        .output()
        .expect("follow --replay");
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["data"]["i"], 1);
    assert_eq!(messages[1]["data"]["i"], 2);
    assert_eq!(messages[2]["data"]["i"], 3);
}

#[test]
fn follow_replay_tail_limits_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpt"])
        .output()
        .expect("create");
    for i in 1..=5 {
        cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "rpt",
                &format!("{{\"i\":{i}}}"),
            ])
            .output()
            .expect("feed");
    }

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpt",
            "--tail",
            "2",
            "--replay",
            "0",
            "--jsonl",
        ])
        .output()
        .expect("follow --replay");
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["data"]["i"], 4);
    assert_eq!(messages[1]["data"]["i"], 5);
}

#[test]
fn follow_replay_respects_speed_timing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rps"])
        .output()
        .expect("create");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rps",
            "{\"i\":1}",
        ])
        .output()
        .expect("feed");
    sleep(Duration::from_millis(200));
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rps",
            "{\"i\":2}",
        ])
        .output()
        .expect("feed");

    let start = Instant::now();
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rps",
            "--tail",
            "100",
            "--replay",
            "1",
            "--jsonl",
        ])
        .output()
        .expect("follow --replay");
    let elapsed = start.elapsed();
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 2);
    assert!(
        elapsed >= Duration::from_millis(150),
        "replay at 1x should wait ~200ms between messages, took {elapsed:?}"
    );
}

#[test]
fn follow_replay_speed_2x_halves_delay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "rps2",
        ])
        .output()
        .expect("create");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rps2",
            "{\"i\":1}",
        ])
        .output()
        .expect("feed");
    sleep(Duration::from_millis(400));
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rps2",
            "{\"i\":2}",
        ])
        .output()
        .expect("feed");

    let start = Instant::now();
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rps2",
            "--tail",
            "100",
            "--replay",
            "2",
            "--jsonl",
        ])
        .output()
        .expect("follow --replay");
    let elapsed = start.elapsed();
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 2);
    assert!(
        elapsed >= Duration::from_millis(150),
        "replay at 2x of 400ms gap should wait ~200ms, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(2200),
        "replay at 2x should be faster than 1x, took {elapsed:?}"
    );
}

#[test]
fn follow_replay_rejects_without_tail_or_since() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpz"])
        .output()
        .expect("create");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpz",
            "--replay",
            "1",
        ])
        .output()
        .expect("follow --replay");
    assert!(!output.status.success());
}

#[test]
fn follow_replay_rejects_negative_speed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpn"])
        .output()
        .expect("create");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpn",
            "--tail",
            "5",
            "--replay",
            "-1",
        ])
        .output()
        .expect("follow --replay");
    assert!(!output.status.success());
}

#[test]
fn follow_replay_where_filters_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpw"])
        .output()
        .expect("create");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rpw",
            r#"{"level":"info"}"#,
        ])
        .output()
        .expect("feed");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rpw",
            r#"{"level":"error"}"#,
        ])
        .output()
        .expect("feed");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rpw",
            r#"{"level":"info"}"#,
        ])
        .output()
        .expect("feed");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpw",
            "--tail",
            "100",
            "--replay",
            "0",
            "--jsonl",
            "--where",
            r#".data.level == "error""#,
        ])
        .output()
        .expect("follow --replay");
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["data"]["level"], "error");
}

#[test]
fn follow_replay_one_exits_after_first_message() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpo"])
        .output()
        .expect("create");
    for i in 1..=3 {
        cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "rpo",
                &format!("{{\"i\":{i}}}"),
            ])
            .output()
            .expect("feed");
    }

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpo",
            "--tail",
            "100",
            "--replay",
            "0",
            "--jsonl",
            "--one",
        ])
        .output()
        .expect("follow --replay");
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["data"]["i"], 1);
}

#[test]
fn follow_replay_empty_pool_exits_ok() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpe"])
        .output()
        .expect("create");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpe",
            "--tail",
            "100",
            "--replay",
            "0",
            "--jsonl",
        ])
        .output()
        .expect("follow --replay");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn follow_replay_data_only_emits_payload() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rpd"])
        .output()
        .expect("create");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rpd",
            r#"{"x":42}"#,
        ])
        .output()
        .expect("feed");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rpd",
            "--tail",
            "100",
            "--replay",
            "0",
            "--jsonl",
            "--data-only",
        ])
        .output()
        .expect("follow --replay");
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0], json!({"x": 42}));
    assert!(messages[0].get("seq").is_none());
}

#[test]
fn follow_replay_zero_speed_emits_without_delay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "rp0"])
        .output()
        .expect("create");
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rp0",
            "{\"i\":1}",
        ])
        .output()
        .expect("feed");
    sleep(Duration::from_millis(200));
    cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "rp0",
            "{\"i\":2}",
        ])
        .output()
        .expect("feed");

    let start = Instant::now();
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "rp0",
            "--tail",
            "100",
            "--replay",
            "0",
            "--jsonl",
        ])
        .output()
        .expect("follow --replay 0");
    let elapsed = start.elapsed();
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 2);
    assert!(
        elapsed < Duration::from_millis(300),
        "--replay 0 should emit without delay, took {elapsed:?}"
    );
}
