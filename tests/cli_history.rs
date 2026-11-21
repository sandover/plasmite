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
    let tags = if value.is_multiple_of(2) {
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
        .args(["fetch", target])
        .args(args);
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
        &["--last", "2", "--format", "json", "--tag", "even"],
    );
    assert_success(&output);
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["seq"], 4);
}

#[test]
fn finite_empty_history_exits_without_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 0, 0);
    let output = history(
        temp.path(),
        "history",
        &["--last", "10", "--format", "json"],
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
            "--format",
            "json",
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
        &["--since", "1s", "--format", "json"],
    );
    assert_success(&output);
    assert!(output.stdout.is_empty());
}

#[test]
fn finite_missing_pool_still_reports_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let output = history(temp.path(), "missing", &["--format", "json"]);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        parse_error_json(&output.stderr)["error"]["kind"],
        "NotFound"
    );
}

#[test]
fn json_format_is_uncolored_and_pretty_is_readable() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 1, 0);
    let output = history(
        temp.path(),
        "history",
        &["--last", "1", "--color", "always", "--format", "json"],
    );
    assert_success(&output);
    assert_eq!(parse_json_lines(&output.stdout).len(), 1);
    assert!(!output.stdout.contains(&0x1b));
    let output = history(
        temp.path(),
        "history",
        &["--last", "1", "--color", "never", "--format", "pretty"],
    );
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("\n  \""));
}

#[test]
fn compact_output_is_explicit_on_pipes() {
    let temp = tempfile::tempdir().unwrap();
    seed(temp.path(), 1, 0);
    let output = history(temp.path(), "history", &[]);
    assert_success(&output);
    let messages = parse_json_lines(&output.stdout);
    assert_eq!(messages[0]["data"], json!({"i": 1,"padding": ""}));
    assert_eq!(messages[0]["meta"]["tags"], json!(["odd"]));

    let output = history(temp.path(), "history", &["-o", "compact"]);
    assert_success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("  #1  odd  {\"i\":1,\"padding\":\"\"}"));
}

#[cfg(unix)]
#[test]
fn terminal_reads_default_to_one_line_and_honor_width_and_no_color() {
    use std::os::fd::FromRawFd;
    use unicode_width::UnicodeWidthStr;

    let temp = tempfile::tempdir().unwrap();
    seed(temp.path(), 1, 80);
    let width = 42;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: width,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    for args in [
        &["fetch", "history"][..],
        &["follow", "history", "--last", "1", "--one"],
        &["duplex", "history", "--last", "1"],
    ] {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty writes two owned file descriptors on success.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size,
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut child = KillOnDrop(
            cmd()
                .arg("--dir")
                .arg(temp.path())
                .args(args)
                .env("NO_COLOR", "1")
                .env("TZ", "EST5")
                .stdin(Stdio::null())
                .stdout(Stdio::from(slave))
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let output = read_message_lines(master, 1, Duration::from_secs(5));
        if args[0] != "duplex" {
            assert!(wait_within(&mut child.0, Duration::from_secs(5), "terminal read").success());
        }
        let output = sanitize_tty_text(&output);
        let line = output.trim_end();
        assert!(
            line.starts_with("2020-09-13 07:26:40  #1  odd  {\"i\":1"),
            "{line:?}"
        );
        assert!(line.ends_with('…'), "{line:?}");
        assert_eq!(line.width(), usize::from(width));
        assert!(!line.contains('\x1b'));
    }
}

#[test]
fn local_and_remote_finite_reads_share_selection_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    seed(temp.path(), 5, 0);
    let server = ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http");
    let target = format!("{}/history", server.base_url);
    for selection in [
        vec![],
        vec!["--tag", "even"],
        vec!["--last", "2", "--tag", "even"],
        vec!["--since", "2020-09-13T12:26:40.003Z", "--tag", "odd"],
        vec!["--since", "2999-01-01T00:00:00Z"],
    ] {
        let mut args = selection;
        args.extend(["--format", "json"]);
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
            .args(["fetch", &target, "--format", "json"])
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
        .args(["fetch", "history", "--format", "json"])
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
        !status.success(),
        "finite history loss must fail; status={status}, stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    let messages = parse_json_lines(format!("{first}{remaining}").as_bytes());
    assert!(
        messages.len() < 128,
        "pipe capacity must hold less than the selected history"
    );
    let error = parse_error_json(&stderr);
    assert_eq!(error["error"]["kind"], "RetentionGap");
    assert_eq!(error["error"]["missing"], messages.len() as u64 + 1);
    assert!(error["error"]["next_seq"].as_u64().unwrap() > 128);
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
        let mut child = KillOnDrop(
            cmd()
                .arg("--dir")
                .arg(temp.path())
                .args([
                    "duplex",
                    &target,
                    "--since",
                    "2020-09-13T12:26:40.003Z",
                    "--format",
                    "json",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("duplex"),
        );
        let output = read_message_lines(child.0.stdout.take().unwrap(), 3, Duration::from_secs(5));
        #[cfg(unix)]
        assert!(wait_within(&mut child.0, Duration::from_secs(5), "duplex").success());
        assert_eq!(
            parse_json_lines(&output)
                .iter()
                .map(|m| m["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn remote_finite_reads_continue_after_server_shortens_streams() {
    for cap in ["20", "1"] {
        let temp = tempfile::tempdir().expect("tempdir");
        seed(temp.path(), 5, 0);
        let server = ServeProcess::start_with_read_limits(
            temp.path(),
            "http",
            &[("PLASMITE_TEST_READ_TIMEOUT_MS", cap)],
        );
        let target = format!("{}/history", server.base_url);
        let output = history(temp.path(), &target, &["--format", "json"]);
        assert_success(&output);
        assert_eq!(parse_json_lines(&output.stdout).len(), 5);
    }
}

// A protocol fixture controls response delivery so timeout checks do not depend
// on how quickly a real server drains retained history.
#[test]
fn fetch_reads_all_retained_history_including_empty_and_wrapped_pools() {
    for count in [0, 5, 300] {
        let temp = tempfile::tempdir().unwrap();
        let pool = seed(temp.path(), count, 32768);
        let bounds = pool.info().unwrap().bounds;
        let server = ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http");
        for target in ["history".to_owned(), format!("{}/history", server.base_url)] {
            let output = history(temp.path(), &target, &["--format", "json"]);
            assert_success(&output);
            let messages = parse_json_lines(&output.stdout);
            let expected = match (bounds.oldest_seq, bounds.newest_seq) {
                (Some(oldest), Some(newest)) => (oldest..=newest).collect::<Vec<_>>(),
                _ => vec![],
            };
            assert_eq!(
                messages
                    .iter()
                    .map(|m| m["seq"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
}
