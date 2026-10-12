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
                "--data",
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
            "--data",
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
fn first_sparse_message_reaches_pool_before_stdin_closes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    create_pool(&directory, "heartbeat");

    let mut child = KillOnDrop(
        cmd()
            .args(["--dir", directory.to_str().unwrap(), "feed", "heartbeat"])
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
        .write_all(b"{\"data\":{\"beat\":1},\"meta\":{\"tags\":[\"health\"]}}\n")
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
fn data_mode_preserves_message_named_application_fields_and_old_flags_fail() {
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
    assert!(!ambiguous.status.success());

    let written = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            "events",
            "--data",
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
    assert_eq!(message["seq"], 1);
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
    assert_eq!(tag_without_data.status.code(), Some(2));

    for args in [
        vec!["version", "--json"],
        vec!["version", "--yaml"],
        vec!["version", "--jsonl"],
        vec!["feed", "events", "--in", "json"],
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

#[cfg(unix)]
#[test]
fn broken_pipe_stops_after_the_message_being_written() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    create_pool(&directory, "events");
    let large_data = "x".repeat(256 * 1024);
    let input = format!(
        "{{\"data\":{{\"n\":1}}}}\n{{\"data\":{{\"large\":\"{large_data}\"}}}}\n{{\"data\":{{\"n\":3}}}}\n"
    );

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
