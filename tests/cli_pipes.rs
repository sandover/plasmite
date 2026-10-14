//! Purpose: Verify default CLI message pipes, replay, and stream failure behavior.
pub mod support;
use support::cli::*;

fn create_pool(directory: &Path, name: &str) {
    let output = cmd()
        .args([
            "--dir",
            directory.to_str().expect("pool directory"),
            "pool",
            "create",
            name,
        ])
        .output()
        .expect("create pool");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn feed_stdin(directory: &Path, pool: &str, input: &[u8]) -> std::process::Output {
    let mut child = cmd()
        .args([
            "--dir",
            directory.to_str().expect("pool directory"),
            "feed",
            pool,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("feed");
    child
        .stdin
        .take()
        .expect("feed stdin")
        .write_all(input)
        .expect("write feed input");
    child.wait_with_output().expect("feed output")
}

fn follow_tail(directory: &Path, pool: &str, count: &str) -> std::process::Output {
    cmd()
        .args([
            "--dir",
            directory.to_str().expect("pool directory"),
            "follow",
            pool,
            "--tail",
            count,
            "--no-follow",
        ])
        .output()
        .expect("follow")
}

#[test]
fn default_messages_copy_edit_save_reload_and_chain() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    for name in ["source", "reloaded", "chained", "edited"] {
        create_pool(&directory, name);
    }

    for temperature in [20, 21] {
        let input = format!("{{\"temperature\":{temperature}}}");
        let output = cmd()
            .args([
                "--dir",
                directory.to_str().unwrap(),
                "feed",
                "source",
                &input,
                "--tag",
                "sensor",
            ])
            .output()
            .expect("feed source data");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let source = follow_tail(&directory, "source", "2");
    assert!(
        source.status.success(),
        "{}",
        String::from_utf8_lossy(&source.stderr)
    );
    let source_messages = parse_json_lines(&source.stdout);
    assert_eq!(source_messages.len(), 2);
    assert_eq!(source_messages[0]["data"]["temperature"], 20);
    assert_eq!(source_messages[1]["meta"]["tags"], json!(["sensor"]));

    let saved = temp.path().join("history.jsonl");
    std::fs::write(&saved, &source.stdout).expect("save complete messages");
    let seed = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "reloaded",
            "{\"seed\":true}",
            "--tag",
            "prior",
        ])
        .output()
        .expect("seed reload pool");
    assert!(seed.status.success());

    let reload = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "reloaded",
            "--file",
            saved.to_str().unwrap(),
        ])
        .output()
        .expect("reload saved messages");
    assert!(
        reload.status.success(),
        "{}",
        String::from_utf8_lossy(&reload.stderr)
    );
    let reloaded = parse_json_lines(&reload.stdout);
    assert_eq!(reloaded.len(), 2);
    assert_eq!(reloaded[0]["seq"], 2);
    assert_eq!(reloaded[0]["data"]["temperature"], 20);
    assert_eq!(reloaded[0]["meta"]["tags"], json!(["sensor"]));
    assert_eq!(reloaded[1]["seq"], 3);

    let copied = follow_tail(&directory, "reloaded", "2");
    assert!(copied.status.success());
    let chained = feed_stdin(&directory, "chained", &copied.stdout);
    assert!(
        chained.status.success(),
        "{}",
        String::from_utf8_lossy(&chained.stderr)
    );
    let chained_messages = parse_json_lines(&chained.stdout);
    assert_eq!(chained_messages.len(), 2);
    assert_eq!(chained_messages[0]["seq"], 1);
    assert_eq!(chained_messages[0]["data"]["temperature"], 20);
    assert_eq!(chained_messages[1]["meta"]["tags"], json!(["sensor"]));

    if Command::new("jq").arg("--version").output().is_ok() {
        let mut jq = Command::new("jq")
            .args(["-c", ".data.temperature += 1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start jq");
        jq.stdin
            .take()
            .expect("jq stdin")
            .write_all(&source.stdout)
            .expect("write jq input");
        let edited = jq.wait_with_output().expect("jq output");
        assert!(edited.status.success());

        let fed = feed_stdin(&directory, "edited", &edited.stdout);
        assert!(
            fed.status.success(),
            "{}",
            String::from_utf8_lossy(&fed.stderr)
        );
        let messages = parse_json_lines(&fed.stdout);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["data"]["temperature"], 21);
        assert_eq!(messages[1]["data"]["temperature"], 22);
        assert_eq!(messages[0]["meta"]["tags"], json!(["sensor"]));
    }
}

#[test]
fn first_sparse_object_reaches_pool_before_stdin_closes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    create_pool(&directory, "heartbeat");

    let mut child = KillOnDrop(
        cmd()
            .args([
                "--dir",
                directory.to_str().unwrap(),
                "feed",
                "heartbeat",
                "--tag",
                "health",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start feed"),
    );
    child
        .0
        .stdin
        .as_mut()
        .expect("feed stdin")
        .write_all(b"{\"beat\":1}\n")
        .expect("write first message");
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .flush()
        .expect("flush stdin");

    let stdout = child.0.stdout.take().expect("feed stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    let first = parse_json(line.trim());
    assert_eq!(first["seq"], 1);
    assert_eq!(first["data"]["beat"], 1);
    assert_eq!(first["meta"]["tags"], json!(["health"]));
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "feed should still await stdin"
    );

    child.0.stdin.take();
    let status = wait_within(
        &mut child.0,
        Duration::from_secs(5),
        "feed after stdin closes",
    );
    assert!(status.success());
}

#[test]
fn message_named_application_fields_are_preserved_and_old_flags_fail() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    create_pool(&directory, "events");
    let application = json!({
        "seq": 900,
        "time": "application-time",
        "meta": {"tags": ["inside"]},
        "data": {"temperature": 23},
        "application": true
    });
    let text = serde_json::to_string(&application).unwrap();

    let ambiguous = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "events",
            &text,
        ])
        .output()
        .expect("feed without data mode");
    assert!(ambiguous.status.success());
    assert_eq!(
        parse_json(std::str::from_utf8(&ambiguous.stdout).unwrap())["data"],
        application
    );

    let written = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "events",
            &text,
            "--tag",
            "outer",
        ])
        .output()
        .expect("feed application data");
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    let message = parse_json(std::str::from_utf8(&written.stdout).expect("utf8"));
    assert_eq!(message["seq"], 2);
    assert_eq!(message["meta"]["tags"], json!(["outer"]));
    assert_eq!(message["data"], application);

    let tag_without_data = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "events",
            "--tag",
            "orphan",
            "{\"x\":1}",
        ])
        .output()
        .expect("tag without data mode");
    assert!(tag_without_data.status.success());
    assert_eq!(
        parse_json(std::str::from_utf8(&tag_without_data.stdout).unwrap())["meta"]["tags"],
        json!(["orphan"])
    );

    for args in [
        vec!["version", "--json"],
        vec!["version", "--yaml"],
        vec!["version", "--jsonl"],
        vec!["feed", "events", "--in", "json"],
        vec!["feed", "events", "--data"],
        vec!["feed", "events", "--data-only"],
        vec!["feed", "events", "--errors", "skip"],
    ] {
        let output = cmd()
            .arg("--dir")
            .arg(&directory)
            .args(&args)
            .output()
            .expect("run removed-option check");
        assert_eq!(output.status.code(), Some(2), "args={args:?}");
    }
}

#[test]
fn removed_data_flag_is_rejected_before_local_or_network_work() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("unused");
    for command in ["feed", "duplex"] {
        for target in [
            "missing",
            "http://127.0.0.1:1/missing",
            "udp://127.0.0.1:1/missing",
        ] {
            let output = cmd()
                .arg("--dir")
                .arg(&directory)
                .args([command, target, "--data"])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{command} {target}");
            let error = String::from_utf8(output.stderr).unwrap();
            assert!(error.contains("unexpected argument '--data'"), "{error}");
            assert!(!directory.exists());
        }
    }
    for command in ["feed", "duplex"] {
        let help = cmd().args([command, "--help"]).output().unwrap();
        assert!(help.status.success());
        assert!(!String::from_utf8(help.stdout).unwrap().contains("--data"));
    }
}

#[cfg(unix)]
#[test]
fn broken_pipe_stops_after_the_message_being_written() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    create_pool(&directory, "events");
    let large_data = "x".repeat(256 * 1024);
    let input = format!("{{\"n\":1}}\n{{\"large\":\"{large_data}\"}}\n{{\"n\":3}}\n");

    let mut child = KillOnDrop(
        cmd()
            .args(["--dir", directory.to_str().unwrap(), "feed", "events"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start feed"),
    );
    let stdin = child.0.stdin.take().expect("feed stdin");
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        let result = stdin.write_all(input.as_bytes());
        drop(stdin);
        result
    });

    let stdout = child.0.stdout.take().expect("feed stdout");
    let first_line = read_line_with_timeout(stdout, Duration::from_secs(5));
    assert_eq!(parse_json(first_line.trim())["data"]["n"], 1);
    let _ = writer.join().expect("stdin writer");
    let status = wait_within(
        &mut child.0,
        Duration::from_secs(10),
        "feed after broken pipe",
    );
    assert!(
        status.success(),
        "broken pipe should end successfully: {status:?}"
    );

    let second = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "fetch",
            "events",
            "2",
            "--format",
            "json",
        ])
        .output()
        .expect("fetch second message");
    assert!(
        second.status.success(),
        "the failed output write follows its append"
    );
    let third = cmd()
        .args(["--dir", directory.to_str().unwrap(), "fetch", "events", "3"])
        .output()
        .expect("fetch third message");
    assert!(
        !third.status.success(),
        "feed must stop after the broken pipe"
    );
}

#[test]
fn ordinary_partial_and_reserved_objects_use_the_same_rule_for_all_sources() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("pools");
    create_pool(&directory, "events");
    for application in [
        json!({"x": 1}),
        json!({"data": {"x": 2}}),
        json!({"data": {}, "meta": {"tags": ["application"]}}),
    ] {
        let text = application.to_string();
        let output = feed_stdin(&directory, "events", text.as_bytes());
        assert!(output.status.success());
        let message = parse_json(std::str::from_utf8(&output.stdout).unwrap());
        assert_eq!(message["data"], application);
        assert_eq!(message["meta"]["tags"], json!([]));
    }
    let reserved = json!({"seq": 900, "time": "2000-01-01T00:00:00Z",
        "meta": {"tags": ["copied"]}, "data": {"value": 4}});
    let file = temp.path().join("message.json");
    std::fs::write(&file, reserved.to_string()).unwrap();
    let copied = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "events",
            "--file",
            file.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(copied.status.success());
    let message = parse_json(std::str::from_utf8(&copied.stdout).unwrap());
    assert_eq!(message["seq"], 4);
    assert_ne!(message["time"], reserved["time"]);
    assert_eq!(message["data"], reserved["data"]);
    assert_eq!(message["meta"], reserved["meta"]);
    let inline = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "events",
            &reserved.to_string(),
        ])
        .output()
        .unwrap();
    assert!(inline.status.success());
    assert_eq!(
        parse_json(std::str::from_utf8(&inline.stdout).unwrap())["data"],
        reserved["data"]
    );

    let yaml = b"---\nx: 5\n...\n---\nseq: 900\ntime: '2000-01-01T00:00:00Z'\nmeta: {tags: [yaml]}\ndata: {x: 6}\n...\n";
    let output = feed_stdin(&directory, "events", yaml);
    assert!(output.status.success());
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages[0]["data"], json!({"x": 5}));
    assert_eq!(messages[1]["data"], json!({"x": 6}));
    assert_eq!(messages[1]["meta"]["tags"], json!(["yaml"]));
}

#[test]
fn invalid_reserved_message_stops_after_prior_success() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("pools");
    create_pool(&directory, "events");
    let output = feed_stdin(&directory, "events", b"{\"ok\":1}\n{\"seq\":null,\"time\":\"bad\",\"meta\":{\"tags\":[]},\"data\":{}}\n{\"later\":true}\n");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse_json_lines(&output.stdout).len(), 1);
    let history = follow_tail(&directory, "events", "10");
    let messages = parse_json_lines(&history.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["data"], json!({"ok": 1}));
}

#[test]
fn actual_feed_follow_fetch_pipes_preserve_data_and_tags() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("pools");
    for name in ["source", "destination"] {
        create_pool(&directory, name);
    }
    for args in [
        vec!["feed", "source", "--tag", "pipe", "{\"x\":1}"],
        vec!["follow", "source", "--tail", "1", "--no-follow"],
        vec!["fetch", "source", "1"],
    ] {
        let mut source = cmd()
            .arg("--dir")
            .arg(&directory)
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let copied = cmd()
            .arg("--dir")
            .arg(&directory)
            .args(["feed", "destination"])
            .stdin(Stdio::from(source.stdout.take().unwrap()))
            .output()
            .unwrap();
        assert!(source.wait().unwrap().success());
        assert!(
            copied.status.success(),
            "{}",
            String::from_utf8_lossy(&copied.stderr)
        );
        let message = parse_json(std::str::from_utf8(&copied.stdout).unwrap());
        assert_eq!(message["data"], json!({"x": 1}));
        assert_eq!(message["meta"]["tags"], json!(["pipe"]));
    }
    assert_eq!(
        parse_json_lines(&follow_tail(&directory, "destination", "3").stdout).len(),
        3
    );
}
