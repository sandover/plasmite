//! Purpose: Follow and duplex black-box CLI integration tests.

pub mod support;
use support::cli::*;

fn connect_to_server(server: &ServeProcess, home: &std::path::Path) {
    let mut connect = cmd()
        .args(["access", "connect", &server.remote_url])
        .env("PLASMITE_ACCESS_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("connect");
    writeln!(
        connect.stdin.take().expect("stdin"),
        "{}",
        server.access_key()
    )
    .expect("key");
    assert!(wait_within(&mut connect, Duration::from_secs(60), "access connect").success());
}

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
            "--last",
            "1",
            "--format",
            "json",
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
    let status = wait_within(&mut follower, Duration::from_secs(60), "follow --one");
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
            "--format",
            "json",
            "--last",
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
        let status = wait_within(&mut follower, Duration::from_secs(60), "finite follower");
        let _ = exit_tx.send(status);
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow exit");
    assert!(status.success());
}

#[test]
fn follow_tail_one_exits_after_first_live_match() {
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
            "--last",
            "2",
            "--format",
            "json",
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
    assert_eq!(value.get("data").unwrap()["x"], 1);

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let status = wait_within(&mut follower, Duration::from_secs(60), "finite follower");
        let _ = exit_tx.send(status);
    });
    let status = exit_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("follow exit");
    assert!(status.success());
}

#[test]
fn follow_json_emits_complete_message() {
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
            "--last",
            "1",
            "--format",
            "json",
            "--one",
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["data"]["x"], 1);
    assert_eq!(lines[0]["meta"]["tags"], json!([]));
}

#[test]
fn follow_json_preserves_filtered_message_tags() {
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
            "--last",
            "1",
            "--format",
            "json",
            "--one",
            "--tag",
            "keep",
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let lines = parse_json_lines(&output.stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["data"]["x"], 2);
    assert_eq!(lines[0]["meta"]["tags"], json!(["keep"]));
}

#[test]
fn follow_json_emits_complete_message_with_explicit_format() {
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
            "--last",
            "1",
            "--format",
            "json",
            "--one",
        ])
        .output()
        .expect("follow");
    assert!(output.status.success());
    let value = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    assert_eq!(value["data"]["x"], 3);
    assert_eq!(value["seq"], 1);
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
            "--last",
            "10",
            "--format",
            "json",
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
            "fetch",
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
        .args(["--format", "json"])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code(), Some(3));
    let err = parse_error_json(&follower.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
}

#[test]
fn follow_defaults_to_json_when_stdout_is_redirected() {
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
            "fetch",
            "demo",
            "--last",
            "1",
        ])
        .output()
        .expect("follow default output");
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["seq"], 1);
    assert_eq!(messages[0]["data"]["x"], 1);
    assert_eq!(messages[0]["meta"]["tags"], json!([]));
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
        .args(["--format", "json"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let client = plasmite::api::LocalClient::new().with_pool_dir(&pool_dir);
    let mut pool = client
        .open_pool(&plasmite::api::PoolRef::name("demo"), false)
        .expect("pool");
    let payload = "a".repeat(8192);
    pool.feed(
        &serde_json::json!({"x": "first", "pad": payload}),
        plasmite::api::Feed::default(),
    )
    .expect("first message");

    let mut follower = KillOnDrop(
        cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "follow",
                "demo",
                "--format",
                "json",
                "--last",
                "1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("follow"),
    );

    let stdout = follower.0.stdout.take().expect("stdout");
    let stderr = follower.0.stderr.take().expect("stderr");

    let (attached_tx, attached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let _ = attached_tx.send(());
        // Hold stdout until the ring wraps. Pipe backpressure then forces the
        // follower behind; timing and process startup cannot decide the outcome.
        if resume_rx.recv().is_err() {
            return;
        }
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
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

    attached_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("follower attached");

    for i in 0..400u64 {
        pool.feed(
            &serde_json::json!({"x": i, "pad": payload}),
            plasmite::api::Feed::default(),
        )
        .expect("append");
    }
    resume_tx.send(()).expect("resume reader");

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

    drop(follower);
}

#[test]
fn follow_rejects_removed_jsonl_format_value() {
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

    let follower = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--last",
            "1",
            "--format",
            "jsonl",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code().unwrap(), 2);
    assert!(
        String::from_utf8_lossy(&follower.stderr).contains("jsonl"),
        "expected an unsupported format error: {}",
        String::from_utf8_lossy(&follower.stderr)
    );
}

#[test]
fn follow_missing_pool_has_actionable_hint() {
    let output = cmd()
        .args(["follow", "-n", "1"])
        .args(["--format", "json"])
        .output()
        .expect("follow");
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
    let pool_dir = temp.path().join("pools with spaces");

    let output = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "follow", "missing"])
        .args(["--format", "json"])
        .output()
        .expect("follow");
    assert_eq!(output.status.code(), Some(3));

    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--create"));
    assert!(hint.contains("existing invocation"));
    assert_eq!(
        inner["path"],
        pool_dir.join("missing.plasmite").to_str().unwrap()
    );
    assert!(!pool_dir.exists());
}

#[test]
fn follow_remote_url_creates_missing_pool_and_returns_one_match() {
    let temp = tempfile::tempdir().expect("tempdir");
    let server = ServeProcess::start(temp.path());
    let pool_url = format!("{}/demo", server.local_url);
    let mut follow = KillOnDrop(
        cmd()
            .args([
                "follow", &pool_url, "--create", "--since", "oldest", "--one", "--format", "json",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("follow"),
    );
    let path = temp.path().join("demo.plasmite");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut pool = loop {
        if let Ok(pool) = plasmite::api::Pool::open(&path) {
            break pool;
        }
        assert!(
            Instant::now() < deadline,
            "follow did not create the remote pool"
        );
        sleep(Duration::from_millis(20));
    };
    pool.feed(&json!({"marker": true}), plasmite::api::Feed::new())
        .expect("feed");
    let line = read_line_with_timeout(
        follow.0.stdout.take().expect("stdout"),
        Duration::from_secs(5),
    );
    assert_eq!(
        parse_json_lines(line.as_bytes())[0]["data"],
        json!({"marker": true})
    );
    assert!(wait_within(&mut follow.0, Duration::from_secs(5), "remote follow --one").success());
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
    let access_home = temp.path().join("access-home");
    connect_to_server(&server, &access_home);
    let pool_url = format!("{}/demo", server.base_url);
    let follower = cmd()
        .args([
            "follow", &pool_url, "--last", "1", "--one", "--format", "json",
        ])
        .env("PLASMITE_ACCESS_HOME", &access_home)
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
    let access_home = temp.path().join("access-home");
    connect_to_server(&server, &access_home);
    let pool_url = format!("{}/demo", server.base_url);
    let mut follower = cmd()
        .args([
            "follow", &pool_url, "--last", "10", "--format", "json", "--tag", "keep",
        ])
        .env("PLASMITE_ACCESS_HOME", &access_home)
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
            "http://localhost:9170/v0/pools/demo/follow",
            "--format",
            "json",
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
fn fetch_remote_by_time() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir_all(&pool_dir).expect("pool directory");
    let client = plasmite::api::LocalClient::new().with_pool_dir(&pool_dir);
    client
        .create_pool(
            &plasmite::api::PoolRef::name("demo"),
            plasmite::api::PoolOptions::new(1024 * 1024),
        )
        .expect("create");
    let mut pool = client
        .open_pool(&plasmite::api::PoolRef::name("demo"), false)
        .expect("pool");
    pool.feed(&json!({"x": 1}), plasmite::api::Feed::default())
        .expect("append");
    let server = ServeProcess::start(&pool_dir);
    let access_home = temp.path().join("access-home");
    connect_to_server(&server, &access_home);
    let pool_url = format!("{}/demo", server.remote_url);
    let since = cmd()
        .args(["fetch", &pool_url, "--since", "5m", "--format", "json"])
        .env("PLASMITE_ACCESS_HOME", &access_home)
        .output()
        .expect("follow");
    assert!(
        since.status.success(),
        "{}",
        String::from_utf8_lossy(&since.stderr)
    );
    let messages = parse_json_lines(&since.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["data"]["x"], 1);
    let future = cmd()
        .args([
            "fetch",
            &pool_url,
            "--since",
            "2999-01-01T00:00:00Z",
            "--format",
            "json",
        ])
        .env("PLASMITE_ACCESS_HOME", &access_home)
        .output()
        .expect("future follow");
    assert!(future.status.success());
    assert!(future.stdout.is_empty());
}

#[test]
fn duplex_remote_url_accepts_future_since() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir_all(&pool_dir).expect("pool directory");
    let client = plasmite::api::LocalClient::new().with_pool_dir(&pool_dir);
    client
        .create_pool(
            &plasmite::api::PoolRef::name("chat"),
            plasmite::api::PoolOptions::new(1024 * 1024),
        )
        .expect("create");
    let server = ServeProcess::start(&pool_dir);
    let access_home = temp.path().join("access-home");
    connect_to_server(&server, &access_home);
    let pool_url = format!("{}/chat", server.remote_url);
    let mut duplex = cmd()
        .args([
            "duplex",
            &pool_url,
            "--timeout",
            "300ms",
            "--since",
            "2999-01-01T00:00:00Z",
            "--format",
            "json",
        ])
        .env("PLASMITE_ACCESS_HOME", &access_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("duplex");
    writeln!(
        duplex.stdin.take().expect("stdin"),
        "{{\"from\":\"alice\",\"msg\":\"hello\"}}"
    )
    .expect("send JSON");
    let output = duplex.wait_with_output().expect("duplex output");
    assert!(
        output.status.code() == Some(124),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["data"], json!({"from":"alice","msg":"hello"}));
    let pool = client
        .open_pool(&plasmite::api::PoolRef::name("chat"), false)
        .expect("pool");
    let frame = pool.get(1).unwrap();
    let message =
        plasmite::api::Message::from_lite3(frame.seq, frame.time, &frame.payload).unwrap();
    assert_eq!(message.data, json!({"from":"alice", "msg":"hello"}));
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
        .args(["--format", "json"])
        .output()
        .expect("duplex");
    assert_eq!(output.status.code(), Some(3));
    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
}

#[test]
fn duplex_https_sends_complete_messages_and_receives_after_eof() {
    let temp = tempfile::tempdir().unwrap();
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir(&pool_dir).unwrap();
    let mut pool = plasmite::api::Pool::create(
        pool_dir.join("events.plasmite"),
        plasmite::api::PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    pool.feed(&json!({"seed":true}), plasmite::api::Feed::default())
        .unwrap();
    let server = ServeProcess::start(&pool_dir);
    let access_home = temp.path().join("access-home");
    connect_to_server(&server, &access_home);
    let target = format!("{}/events", server.base_url);
    let mut child = KillOnDrop(
        cmd()
            .args([
                "duplex",
                &target,
                "--last",
                "1",
                "--timeout",
                "200ms",
                "--format",
                "json",
            ])
            .env("PLASMITE_ACCESS_HOME", &access_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    writeln!(child.0.stdin.take().unwrap(), "{}", json!({"seq":900,"time":"2000-01-01T00:00:00Z","meta":{"tags":["copied"]},"data":{"from":"alice","msg":"application"}})).unwrap();
    assert_eq!(
        wait_within(&mut child.0, Duration::from_secs(5), "HTTPS duplex").code(),
        Some(124)
    );
    let mut output = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    let messages = parse_json_lines(&output);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["data"], json!({"seed":true}));
    assert_eq!(
        messages[1]["data"],
        json!({"from":"alice","msg":"application"})
    );
    assert_eq!(messages[1]["meta"]["tags"], json!(["copied"]));
    let frame = pool.get(2).unwrap();
    let message =
        plasmite::api::Message::from_lite3(frame.seq, frame.time, &frame.payload).unwrap();
    assert_eq!(message.data, messages[1]["data"]);
}

#[test]
fn binary_fetch_validates_before_filtering() {
    use plasmite::api::{AppendOptions, Durability, Pool, PoolOptions, lite3::Lite3Buf};
    let temp = tempfile::tempdir().unwrap();
    let payload =
        Lite3Buf::from_json_str(r#"{"meta":{"tags":[7]},"data":{"nested":[1,2]}}"#).unwrap();
    let mut pool =
        Pool::create(temp.path().join("broken.plasmite"), PoolOptions::new(65536)).unwrap();
    pool.append_with_options(payload.as_slice(), AppendOptions::new(1, Durability::Fast))
        .unwrap();
    drop(pool);
    let server = ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http");
    for reference in ["broken".to_owned(), format!("{}/broken", server.local_url)] {
        let raw = cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(["fetch", &reference, "--format", "lite3"])
            .output()
            .unwrap();
        assert!(!raw.status.success());
        assert!(String::from_utf8_lossy(&raw.stderr).contains("meta.tags must be string array"));
        for extra in [
            vec!["--tag", "absent"],
            vec!["--tag", "absent", "--since", "oldest"],
        ] {
            let filtered = cmd()
                .arg("--dir")
                .arg(temp.path())
                .args(["fetch", &reference, "--format", "lite3"])
                .args(extra)
                .output()
                .unwrap();
            assert!(!filtered.status.success());
            assert!(
                String::from_utf8_lossy(&filtered.stderr)
                    .contains("meta.tags must be string array")
            );
        }
    }
}
