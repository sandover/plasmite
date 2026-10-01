//! Finite history selection and local/remote CLI parity.

pub mod support;
use plasmite::api::{AppendOptions, Durability, Pool, PoolOptions, lite3};
use support::cli::*;

fn seed(dir: &Path, count: u64, payload_bytes: usize) -> Pool {
    std::fs::create_dir_all(dir).expect("pool dir");
    let mut pool = Pool::create(
        dir.join("history.plasmite"),
        PoolOptions::new(4 * 1024 * 1024),
    )
    .expect("pool");
    for seq in 1..=count {
        append(
            &mut pool,
            seq,
            payload_bytes,
            1_600_000_000_000_000_000 + seq * 1_000_000,
        );
    }
    pool
}

fn append(pool: &mut Pool, value: u64, payload_bytes: usize, timestamp: u64) {
    let tags = if value % 2 == 0 {
        vec!["even".into()]
    } else {
        vec!["odd".into()]
    };
    let payload = lite3::encode_message(
        &tags,
        &json!({"i": value, "padding": "x".repeat(payload_bytes)}),
    )
    .expect("encode");
    pool.append_with_options(
        payload.as_slice(),
        AppendOptions::new(timestamp, Durability::Fast),
    )
    .expect("append");
}

fn history(dir: &Path, target: &str, args: &[&str]) -> std::process::Output {
    let mut command = cmd();
    command
        .arg("--dir")
        .arg(dir)
        .args(["follow", target])
        .args(args);
    if !args.contains(&"--timeout") {
        command.args(["--timeout", "5s"]);
    }
    command.output().expect("follow")
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "status={:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn finite_tail_filters_after_selecting_retained_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 5, 0);
    let output = history(
        temp.path(),
        "history",
        &[
            "--tail",
            "2",
            "--no-follow",
            "--json",
            "--timeout",
            "5s",
            "--tag",
            "even",
            "--where",
            ".data.i > 2",
        ],
    );
    assert_success(&output);
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["seq"], 4);
}

#[test]
fn finite_one_returns_first_match_in_selected_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 5, 0);
    let output = history(
        temp.path(),
        "history",
        &["--tail", "4", "--no-follow", "--one", "--json"],
    );
    assert_success(&output);
    assert_eq!(parse_json_lines(&output.stdout)[0]["seq"], 2);
}

#[test]
fn finite_empty_history_exits_without_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 0, 0);
    let output = history(
        temp.path(),
        "history",
        &["--tail", "10", "--no-follow", "--json"],
    );
    assert_success(&output);
    assert!(output.stdout.is_empty());
}

#[test]
fn finite_since_uses_timestamp_and_filters() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 5, 0);
    let output = history(
        temp.path(),
        "history",
        &[
            "--since",
            "2020-09-13T12:26:40.003Z",
            "--no-follow",
            "--json",
            "--timeout",
            "5s",
            "--tag",
            "odd",
        ],
    );
    assert_success(&output);
    assert_eq!(
        parse_json_lines(&output.stdout)
            .iter()
            .map(|m| m["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![3, 5]
    );
    let output = history(
        temp.path(),
        "history",
        &["--since", "1s", "--no-follow", "--json"],
    );
    assert_success(&output);
    assert!(output.stdout.is_empty());
}

#[test]
fn finite_requires_history_selector() {
    let output = cmd()
        .args(["follow", "missing", "--no-follow", "--json"])
        .output()
        .expect("follow");
    assert_eq!(output.status.code(), Some(2));
    let error = parse_error_json(&output.stderr);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("requires --tail or --since")
    );
}

#[test]
fn structured_aliases_remain_uncolored_and_human_default_ignores_tty() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 1, 0);
    for aliases in [vec!["--json"], vec!["--jsonl"], vec!["--format", "jsonl"]] {
        let mut args = vec!["--tail", "1", "--no-follow", "--color", "always"];
        args.extend(aliases);
        let output = history(temp.path(), "history", &args);
        assert_success(&output);
        assert_eq!(parse_json_lines(&output.stdout).len(), 1);
        assert!(!output.stdout.contains(&0x1b));
    }
    let output = history(
        temp.path(),
        "history",
        &["--tail", "1", "--no-follow", "--color", "never"],
    );
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("\n  \""));
}

#[test]
fn replay_selects_tail_before_filtering() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 5, 0);
    let output = history(
        temp.path(),
        "history",
        &["--tail", "2", "--replay", "0", "--json", "--tag", "even"],
    );
    assert_success(&output);
    assert_eq!(
        parse_json_lines(&output.stdout)
            .iter()
            .map(|m| m["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![4]
    );
}

#[test]
fn local_and_remote_finite_reads_share_selection_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 5, 0);
    let server = ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http");
    let target = format!("{}/history", server.base_url);
    for selection in [
        vec!["--tail", "2", "--tag", "even"],
        vec!["--since", "2020-09-13T12:26:40.003Z", "--tag", "odd"],
        vec!["--tail", "5", "--one"],
        vec!["--since", "2999-01-01T00:00:00Z"],
        vec!["--tail", "5", "--where", "false"],
    ] {
        let mut args = selection;
        args.extend(["--no-follow", "--json"]);
        let local = history(temp.path(), "history", &args);
        let remote = history(temp.path(), &target, &args);
        assert_success(&local);
        assert_success(&remote);
        assert_eq!(
            parse_json_lines(&remote.stdout),
            parse_json_lines(&local.stdout)
        );
    }
}

#[test]
fn finite_snapshot_excludes_writes_while_output_is_blocked() {
    for remote in [false, true] {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut pool = seed(temp.path(), 128, 2048);
        let server =
            remote.then(|| ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http"));
        let target = server
            .as_ref()
            .map(|s| format!("{}/history", s.base_url))
            .unwrap_or_else(|| "history".into());
        let mut child = cmd()
            .arg("--dir")
            .arg(temp.path())
            .args([
                "follow",
                &target,
                "--tail",
                "128",
                "--no-follow",
                "--json",
                "--timeout",
                "5s",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("follow");
        let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
        let mut first = String::new();
        reader.read_line(&mut first).expect("first line");
        assert_eq!(parse_json(first.trim())["seq"], 1);
        // The first output proves bounds were captured. Remaining large output
        // fills the pipe while new writes occur before we drain it.
        for i in 129..=160 {
            append(
                &mut pool,
                i,
                2048,
                1_600_000_000_000_000_000 + i * 1_000_000,
            );
        }
        let mut remaining = String::new();
        reader.read_to_string(&mut remaining).expect("drain output");
        let status = child.wait().expect("wait");
        assert!(status.success());
        let messages = parse_json_lines(format!("{first}{remaining}").as_bytes());
        assert_eq!(messages.len(), 128);
        assert_eq!(messages.last().unwrap()["seq"], 128);
    }
}

#[test]
fn finite_retention_gap_reports_exact_missing_history_and_exits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut pool = seed(temp.path(), 128, 2048);
    let mut child = cmd()
        .arg("--dir")
        .arg(temp.path())
        .args([
            "follow",
            "history",
            "--tail",
            "128",
            "--no-follow",
            "--json",
            "--timeout",
            "5s",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("follow");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let mut first = String::new();
    reader.read_line(&mut first).expect("first line");
    assert_eq!(parse_json(first.trim())["seq"], 1);
    // More than the ring capacity removes every remaining snapshot frame while
    // the reader has stopped draining the pipe.
    for i in 129..=300 {
        append(
            &mut pool,
            i,
            32768,
            1_600_000_000_000_000_000 + i * 1_000_000,
        );
    }
    let mut remaining = String::new();
    reader.read_to_string(&mut remaining).expect("drain");
    let status = child.wait().expect("wait");
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_end(&mut stderr)
        .expect("stderr read");
    assert!(
        status.success(),
        "status={status}, stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    let messages = parse_json_lines(format!("{first}{remaining}").as_bytes());
    assert!(
        messages.len() < 128,
        "pipe capacity must hold less than the selected history"
    );
    let dropped: u64 = parse_json_lines(&stderr)
        .iter()
        .map(|n| {
            n["notice"]["details"]["dropped_count"]
                .as_u64()
                .expect("drop count")
        })
        .sum();
    assert_eq!(messages.len() as u64 + dropped, 128);
    assert!(messages.iter().all(|m| m["seq"].as_u64().unwrap() <= 128));
}

#[test]
fn duplex_since_reads_same_local_and_remote_history() {
    for remote in [false, true] {
        let temp = tempfile::tempdir().expect("tempdir");
        seed(temp.path(), 5, 0);
        let server =
            remote.then(|| ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http"));
        let target = server
            .as_ref()
            .map(|s| format!("{}/history", s.base_url))
            .unwrap_or_else(|| "history".into());
        let mut child = cmd()
            .arg("--dir")
            .arg(temp.path())
            .args([
                "duplex",
                &target,
                "--since",
                "2020-09-13T12:26:40.003Z",
                "--timeout",
                "150ms",
                "--json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("duplex");
        // Keep stdin open while the follow half reads history and times out.
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .expect("stdout")
            .read_to_end(&mut output)
            .expect("read");
        assert_eq!(child.wait().expect("wait").code(), Some(124));
        assert_eq!(
            parse_json_lines(&output)
                .iter()
                .map(|m| m["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }
}

#[test]
fn remote_timeout_counts_output_instead_of_unmatched_arrivals() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let temp = tempfile::tempdir().expect("tempdir");
    let mut pool = seed(temp.path(), 1, 0);
    let server = ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http");
    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = stop.clone();
    let writer = thread::spawn(move || {
        let mut i = 2;
        while !writer_stop.load(Ordering::Acquire) {
            append(&mut pool, i, 0, 1_600_000_000_000_000_000 + i * 1_000_000);
            i += 1;
            thread::sleep(Duration::from_millis(1));
        }
    });
    let target = format!("{}/history", server.base_url);
    let start = Instant::now();
    let output = history(
        temp.path(),
        &target,
        &[
            "--since",
            "2020-01-01T00:00:00Z",
            "--where",
            "false",
            "--timeout",
            "150ms",
            "--json",
        ],
    );
    stop.store(true, Ordering::Release);
    writer.join().expect("writer");
    assert_eq!(
        output.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "unmatched arrivals must not reset the timeout"
    );
}

#[test]
fn replay_timeout_includes_playback_delay_without_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut pool = seed(temp.path(), 1, 0);
    append(&mut pool, 2, 0, 1_600_000_000_241_000_000);
    let output = history(
        temp.path(),
        "history",
        &[
            "--tail",
            "2",
            "--replay",
            "1",
            "--timeout",
            "225ms",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(parse_json_lines(&output.stdout).len(), 1);
}

#[test]
fn remote_finite_reads_adapt_to_short_server_tail_limits() {
    for cap in ["20", "1"] {
        let temp = tempfile::tempdir().expect("tempdir");
        seed(temp.path(), 5, 0);
        let server = ServeProcess::start_with_args_and_scheme(
            temp.path(),
            &["--max-tail-timeout-ms", cap],
            "http",
        );
        let target = format!("{}/history", server.base_url);
        let output = history(
            temp.path(),
            &target,
            &["--tail", "5", "--no-follow", "--json"],
        );
        assert_success(&output);
        assert_eq!(parse_json_lines(&output.stdout).len(), 5);
    }
}

// A protocol fixture controls response delivery so timeout checks do not depend
// on how quickly a real server drains retained history.
fn stub_remote_tail(
    delay: Duration,
    error: Option<&'static str>,
    sequence: u64,
) -> (String, thread::JoinHandle<usize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let target = format!("http://{}/history", listener.local_addr().expect("address"));
    listener.set_nonblocking(true).expect("nonblocking");
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut requests = 0;
        while Instant::now() < deadline {
            let (stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(err) => panic!("accept: {err}"),
            };
            // macOS may inherit the listener's nonblocking flag on accept.
            // Read each accepted request in blocking mode with a bounded wait.
            stream.set_nonblocking(false).expect("blocking connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("read timeout");
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            if reader.read_line(&mut request).expect("request") == 0 {
                continue;
            }
            let mut content_length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("header");
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    if name.eq_ignore_ascii_case("Content-Length") {
                        content_length = value.trim().parse::<usize>().expect("content length");
                    }
                }
            }
            assert!(content_length <= 65_536, "bounded fixture request body");
            // Closing a socket with unread request bytes can reset the
            // connection before the client receives our response on macOS.
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).expect("request body");
            requests += 1;
            let tail = request.contains("/tail?");
            let (status, body) = if tail {
                match error {
                    Some(message) => (
                        "400 Bad Request",
                        json!({"error":{"kind":"Usage","message":message}}).to_string(),
                    ),
                    None => (
                        "200 OK",
                        format!(
                            "{}\n",
                            json!({"seq":sequence,"time":"2020-09-13T12:26:40Z","meta":{"tags":[]},"data":{"i":1}})
                        ),
                    ),
                }
            } else {
                ("200 OK", json!({"pool":{"path":"history","file_size":1048576,"ring_offset":256,"ring_size":1048320,"bounds":{"oldest":sequence,"newest":sequence}}}).to_string())
            };
            let stream = reader.get_mut();
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).expect("response headers");
            stream.flush().expect("flush");
            if tail {
                thread::sleep(delay);
            }
            stream.write_all(body.as_bytes()).expect("response body");
            if tail {
                return requests;
            }
        }
        requests
    });
    (target, server)
}

#[test]
fn remote_late_matching_line_does_not_reset_output_timeout() {
    let (target, server) = stub_remote_tail(Duration::from_millis(300), None, 1);
    let output = cmd()
        .args([
            "follow",
            &target,
            "--tail",
            "1",
            "--one",
            "--timeout",
            "225ms",
            "--json",
        ])
        .output()
        .expect("follow");
    let requests = server.join().expect("server");
    assert_eq!(
        output.status.code(),
        Some(124),
        "requests={requests}, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests, 3);
    assert!(output.stdout.is_empty());
}

#[test]
fn remote_unrelated_usage_error_is_not_retried() {
    let message = "different request constraint";
    let (target, server) = stub_remote_tail(Duration::ZERO, Some(message), 1);
    let output = cmd()
        .args(["follow", &target, "--tail", "1", "--no-follow", "--json"])
        .output()
        .expect("follow");
    assert_eq!(server.join().expect("server"), 3);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        parse_error_json(&output.stderr)["error"]["message"],
        message
    );
}

#[test]
fn remote_finite_read_terminates_at_maximum_sequence() {
    let (target, server) = stub_remote_tail(Duration::ZERO, None, u64::MAX);
    let output = cmd()
        .args(["follow", &target, "--tail", "1", "--no-follow", "--json"])
        .output()
        .expect("follow");
    assert_eq!(server.join().expect("server"), 3);
    assert_success(&output);
    assert_eq!(parse_json_lines(&output.stdout)[0]["seq"], u64::MAX);
}

#[test]
fn untailed_live_follow_reports_quiet_end_gap_without_double_counting() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut pool = seed(temp.path(), 0, 0);
    let mut child = cmd()
        .arg("--dir")
        .arg(temp.path())
        .args(["follow", "history", "--json", "--timeout", "1500ms"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("follow");
    let stdout = child.stdout.take().expect("stdout");
    let (attached_tx, attached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (output_tx, output_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        reader.read_line(&mut first).expect("first output");
        if first.is_empty() {
            return;
        }
        attached_tx
            .send(parse_json(first.trim())["seq"].as_u64().expect("sequence"))
            .expect("attached");
        resume_rx.recv().expect("resume");
        reader.read_to_string(&mut first).expect("drain");
        output_tx.send(first).expect("output");
    });
    // A default live follow has no history to acknowledge attachment. Write
    // until one message is observed, instead of guessing its startup timing.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut value = 1;
    let first_seq = loop {
        append(
            &mut pool,
            value,
            32_768,
            1_600_000_000_000_000_000 + value * 1_000_000,
        );
        value += 1;
        match attached_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(sequence) => break sequence,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("follower attachment: {err}");
            }
        }
    };
    // The stopped stdout reader forces backpressure. Overwrite more than twice
    // the ring capacity, then stop writing completely before resuming reads.
    for _ in 0..300 {
        append(
            &mut pool,
            value,
            32_768,
            1_600_000_000_000_000_000 + value * 1_000_000,
        );
        value += 1;
    }
    let newest = pool.header_from_mmap().expect("header").newest_seq;
    resume_tx.send(()).expect("resume");
    let output = output_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("quiet end must finish");
    reader.join().expect("reader");
    assert_eq!(child.wait().expect("wait").code(), Some(124));
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_end(&mut stderr)
        .expect("stderr read");
    let messages = parse_json_lines(output.as_bytes());
    let notices = parse_json_lines(&stderr);
    let dropped: u64 = notices
        .iter()
        .map(|notice| {
            notice["notice"]["details"]["dropped_count"]
                .as_u64()
                .expect("drop count")
        })
        .sum();
    assert!(dropped > 0, "quiet retention gap must be reported");
    assert!(
        messages
            .windows(2)
            .all(|pair| pair[0]["seq"].as_u64().unwrap() < pair[1]["seq"].as_u64().unwrap())
    );
    assert_eq!(messages.len() as u64 + dropped, newest - first_seq + 1);
    assert_eq!(
        messages.last().expect("last retained message")["seq"],
        newest
    );
}
