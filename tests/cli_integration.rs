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
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
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

fn cmd_tty(args: &[&str]) -> std::process::Output {
    let exe = env!("CARGO_BIN_EXE_plasmite");
    #[cfg(target_os = "linux")]
    {
        // util-linux script requires -c for command execution; otherwise leading
        // wrapped-command flags (for example --dir) are parsed as script flags.
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(exe);
        argv.extend_from_slice(args);
        let command = argv
            .into_iter()
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        Command::new("script")
            .args(["-q", "-e", "-c", &command, "/dev/null"])
            .output()
            .expect("script tty")
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new("script")
            .args(["-q", "/dev/null", exe])
            .args(args)
            .output()
            .expect("script tty")
    }
}

#[cfg(target_os = "linux")]
fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn sanitize_tty_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace(['\u{4}', '\u{8}', '\r'], "")
}

static SERVER_LOCK: Mutex<()> = Mutex::new(());

struct ServeProcess {
    child: Child,
    base_url: String,
    _server_guard: MutexGuard<'static, ()>,
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
        let guard = SERVER_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut last_error = String::from("server failed to start");
        for _attempt in 0..5 {
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
            let mut child = command
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn serve");

            match wait_for_server(&mut child, bind.parse().expect("addr")) {
                Ok(()) => {
                    return Self {
                        child,
                        base_url,
                        _server_guard: guard,
                    };
                }
                Err(err) => {
                    last_error = err.to_string();
                    let _ = child.kill();
                    let _ = child.wait();
                    sleep(Duration::from_millis(30));
                }
            }
        }
        panic!("server ready: {last_error}");
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

fn read_line_with_timeout<R: Read + Send + 'static>(reader: R, timeout: Duration) -> String {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(timeout)
        .expect("timed out waiting for line")
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

fn fetch_message(pool_dir: &Path, pool: &str, seq: u64) -> Value {
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().expect("pool_dir"),
            "fetch",
            pool,
            &seq.to_string(),
        ])
        .output()
        .expect("fetch");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_json(std::str::from_utf8(&output.stdout).expect("utf8"))
}

fn assert_actionable_usage_feedback(
    output: &std::process::Output,
    expected_message_fragment: &str,
    expected_hint_fragment: &str,
) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected clap usage exit code, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let err = parse_error_json(&output.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner
        .get("message")
        .and_then(|v| v.as_str())
        .expect("usage message");
    assert!(
        message.contains(expected_message_fragment),
        "expected message to contain '{expected_message_fragment}', got '{message}'"
    );
    // Keep guidance concise so both TTY-oriented and JSON stderr outputs stay actionable.
    assert!(!message.contains('\n'), "message should be single-line");
    let hint = inner
        .get("hint")
        .and_then(|v| v.as_str())
        .expect("usage hint");
    assert!(
        hint.contains(expected_hint_fragment),
        "expected hint to contain '{expected_hint_fragment}', got '{hint}'"
    );
}

fn pick_port() -> std::io::Result<u16> {
    let start = Instant::now();
    loop {
        match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => {
                let port = listener.local_addr()?.port();
                drop(listener);
                return Ok(port);
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AddrNotAvailable
                ) && start.elapsed() <= Duration::from_secs(2) =>
            {
                sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err),
        }
    }
}

fn wait_for_server(child: &mut Child, addr: SocketAddr) -> std::io::Result<()> {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let detail = stderr.trim();
            let message = format!(
                "server exited before ready (status: {status}, stderr: {})",
                if detail.is_empty() { "<empty>" } else { detail }
            );
            return Err(std::io::Error::other(message));
        }
        if start.elapsed() > Duration::from_secs(8) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "server did not start in time",
            ));
        }
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn top_level_help_lists_common_pool_operations() {
    let output = cmd().arg("--help").output().expect("help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(
        stdout.lines().any(|l| {
            let t = l.trim();
            t.starts_with("pool") && t.ends_with("Manage pool files")
        }),
        "expected 'pool ... Manage pool files' in help output"
    );
    assert!(stdout.contains("plasmite pool list"));
}

#[test]
fn help_subcommand_is_enabled() {
    let output = cmd().arg("help").output().expect("help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("USAGE"));
    assert!(stdout.contains("COMMANDS"));
}

#[test]
fn help_pool_lists_pool_subcommands() {
    let output = cmd().args(["help", "pool"]).output().expect("help pool");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("Usage: plasmite pool <COMMAND>"));
    assert!(stdout.contains("list    List pools in the pool directory"));
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
fn follow_with_no_args_prints_help() {
    let output = cmd().args(["follow"]).output().expect("follow");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite follow"));
}

#[test]
fn tap_with_no_args_prints_help() {
    let output = cmd().args(["tap"]).output().expect("tap");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite tap"));
    assert!(stderr.contains("Capture command output into a local pool"));
}

#[test]
fn tap_help_renders_examples() {
    let output = cmd().args(["tap", "--help"]).output().expect("tap help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("plasmite tap build --create -- cargo build"));
    assert!(stdout.contains("plasmite tap api --create --create-size 64M -- ./server"));
    assert!(stdout.contains("`--` is required before wrapped command args"));
}

#[test]
fn tap_requires_wrapped_command_after_separator() {
    let output = cmd().args(["tap", "build"]).output().expect("tap");
    assert_actionable_usage_feedback(
        &output,
        "tap requires a wrapped command after `--`",
        "plasmite tap <pool> -- <command...>",
    );
}

#[test]
fn tap_remote_url_rejected_as_local_only() {
    let output = cmd()
        .args(["tap", "http://127.0.0.1:65535/demo", "--", "echo", "hi"])
        .output()
        .expect("tap");
    assert_actionable_usage_feedback(
        &output,
        "tap accepts local pool refs only",
        "Use a local pool name/path",
    );
}

#[test]
fn tap_non_tty_stderr_suppresses_status_lines() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "demo",
            "--create",
            "--",
            "echo",
            "hello",
        ])
        .output()
        .expect("tap");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::str::from_utf8(&output.stdout).expect("utf8"),
        "hello\n"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(!stderr.contains("tapping"));
    assert!(!stderr.contains("tapped"));
}

#[test]
fn tap_tty_stderr_emits_startup_and_completion_lines() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    let output = cmd_tty(&[
        "--dir",
        pool_dir.to_str().unwrap(),
        "tap",
        "demo",
        "--create",
        "--",
        "echo",
        "hello",
    ]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = sanitize_tty_text(&output.stdout);
    assert!(text.contains("tapping demo <- echo hello"), "output={text}");
    assert!(text.contains("tapped 1 lines ("), "output={text}");
    assert!(text.contains("-> demo exit 0"), "output={text}");
}

#[test]
fn tap_basic_capture_writes_start_line_and_exit_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "demo",
            "--create",
            "--",
            "echo",
            "hello",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );

    let start = fetch_message(&pool_dir, "demo", 1);
    let line = fetch_message(&pool_dir, "demo", 2);
    let exit = fetch_message(&pool_dir, "demo", 3);

    assert_eq!(start["data"]["kind"], "start");
    assert_eq!(start["data"]["cmd"], json!(["echo", "hello"]));
    assert_eq!(start["meta"]["tags"], json!(["lifecycle"]));

    assert_eq!(line["data"]["kind"], "line");
    assert_eq!(line["data"]["stream"], "stdout");
    assert_eq!(line["data"]["line"], "hello");

    assert_eq!(exit["data"]["kind"], "exit");
    assert_eq!(exit["data"]["code"], 0);
    assert!(exit["data"]["elapsed_ms"].as_u64().is_some());
    assert_eq!(exit["meta"]["tags"], json!(["lifecycle"]));
}

#[test]
fn tap_captures_stderr_lines() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "errpool",
            "--create",
            "--",
            "sh",
            "-c",
            "echo err >&2",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );

    let line = fetch_message(&pool_dir, "errpool", 2);
    assert_eq!(line["data"]["stream"], "stderr");
    assert_eq!(line["data"]["line"], "err");
}

#[test]
fn tap_forwards_exit_code_and_records_exit_lifecycle_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "failpool",
            "--create",
            "--",
            "false",
        ])
        .output()
        .expect("tap");
    assert_eq!(tap.status.code(), Some(1));

    let exit = fetch_message(&pool_dir, "failpool", 2);
    assert_eq!(exit["data"]["kind"], "exit");
    assert_eq!(exit["data"]["code"], 1);
    assert!(exit["data"].get("signal").is_none());
}

#[test]
fn tap_applies_user_tags_to_lines_and_lifecycle_tag_to_start_exit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "tagpool",
            "--create",
            "--tag",
            "ci",
            "--tag",
            "build",
            "--",
            "echo",
            "ok",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );

    let start = fetch_message(&pool_dir, "tagpool", 1);
    let line = fetch_message(&pool_dir, "tagpool", 2);
    let exit = fetch_message(&pool_dir, "tagpool", 3);
    assert_eq!(start["meta"]["tags"], json!(["lifecycle"]));
    assert_eq!(line["meta"]["tags"], json!(["ci", "build"]));
    assert_eq!(exit["meta"]["tags"], json!(["lifecycle"]));
}

#[test]
fn tap_quiet_suppresses_passthrough_but_capture_still_works() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "quietpool",
            "--create",
            "-q",
            "--",
            "echo",
            "hello",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );
    assert!(
        tap.stdout.is_empty(),
        "stdout should be empty in quiet mode"
    );

    let line = fetch_message(&pool_dir, "quietpool", 2);
    assert_eq!(line["data"]["line"], "hello");
}

#[test]
fn tap_multiline_capture_preserves_line_order() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "mpool",
            "--create",
            "--",
            "sh",
            "-c",
            "echo a; echo b; echo c",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );

    let l1 = fetch_message(&pool_dir, "mpool", 2);
    let l2 = fetch_message(&pool_dir, "mpool", 3);
    let l3 = fetch_message(&pool_dir, "mpool", 4);
    let exit = fetch_message(&pool_dir, "mpool", 5);
    assert_eq!(l1["data"]["line"], "a");
    assert_eq!(l2["data"]["line"], "b");
    assert_eq!(l3["data"]["line"], "c");
    assert_eq!(exit["data"]["kind"], "exit");
}

#[test]
fn tap_captures_unterminated_final_line() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "nolf",
            "--create",
            "--",
            "python3",
            "-c",
            "import sys; sys.stdout.write('tail-without-newline')",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );

    let line = fetch_message(&pool_dir, "nolf", 2);
    assert_eq!(line["data"]["line"], "tail-without-newline");
}

#[test]
fn tap_preserves_long_line_without_truncation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let tap = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "longline",
            "--create",
            "--",
            "python3",
            "-c",
            "print('x' * 65536)",
        ])
        .output()
        .expect("tap");
    assert!(
        tap.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&tap.stderr)
    );

    let line = fetch_message(&pool_dir, "longline", 2);
    let captured = line["data"]["line"].as_str().expect("line string");
    assert_eq!(captured.len(), 65536);
    assert!(captured.chars().all(|ch| ch == 'x'));
}

#[test]
fn tap_missing_wrapped_executable_is_actionable_nonzero_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "missingcmd",
            "--create",
            "--",
            "nonexistent-command-xyz",
        ])
        .output()
        .expect("tap");
    assert_actionable_usage_feedback(
        &output,
        "wrapped command not found",
        "Check PATH or use an absolute executable path",
    );
}

#[test]
fn tap_missing_pool_without_create_has_create_hint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "tap",
            "missingpool",
            "--",
            "echo",
            "x",
        ])
        .output()
        .expect("tap");
    assert_eq!(output.status.code(), Some(3));
    let err = parse_error_json(&output.stderr);
    let inner = err.get("error").and_then(|v| v.as_object()).expect("error");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("NotFound"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        hint.contains("--create"),
        "expected --create hint in '{hint}'"
    );
}

#[test]
fn tap_empty_command_after_separator_is_usage_error() {
    let output = cmd()
        .args(["tap", "demo", "--create", "--"])
        .output()
        .expect("tap");
    assert_actionable_usage_feedback(
        &output,
        "tap requires a wrapped command after `--`",
        "plasmite tap <pool> -- <command...>",
    );
}

#[test]
fn pool_create_with_no_args_prints_help() {
    let output = cmd()
        .args(["pool", "create"])
        .output()
        .expect("pool create");
    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite pool create"));
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
fn pool_create_defaults_to_table_output() {
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

    let stdout = std::str::from_utf8(&create.stdout).expect("utf8");
    assert!(stdout.contains("NAME"));
    assert!(stdout.contains("SIZE"));
    assert!(stdout.contains("INDEX"));
    assert!(stdout.contains("PATH"));
    assert!(stdout.contains("demo"));
}

#[test]
fn pool_info_json_includes_metrics() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "metrics",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let feed_one = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "metrics",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(feed_one.status.success());

    let feed_two = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "metrics",
            "{\"x\":2}",
        ])
        .output()
        .expect("feed");
    assert!(feed_two.status.success());

    let info = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "metrics",
            "--json",
        ])
        .output()
        .expect("info");
    assert!(info.status.success());
    let info_json = parse_json(std::str::from_utf8(&info.stdout).expect("utf8"));
    assert!(info_json["index_capacity"].as_u64().unwrap() > 0);
    assert!(info_json["index_size_bytes"].as_u64().unwrap() > 0);
    let metrics = info_json.get("metrics").expect("metrics");
    assert_eq!(metrics["message_count"], 2);
    assert_eq!(metrics["seq_span"], 2);
    assert!(metrics["utilization"]["used_bytes"].as_u64().unwrap() > 0);
    assert!(metrics["utilization"]["free_bytes"].as_u64().unwrap() > 0);
    assert!(metrics["utilization"]["used_percent"].is_number());
    assert!(metrics["age"]["oldest_time"].is_string());
    assert!(metrics["age"]["newest_time"].is_string());
    assert!(metrics["age"]["oldest_age_ms"].is_number());
    assert!(metrics["age"]["newest_age_ms"].is_number());
}

#[test]
fn pool_create_supports_explicit_and_zero_index_capacity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create_explicit = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "--size",
            "1M",
            "--index-capacity",
            "1024",
            "indexed",
        ])
        .output()
        .expect("create");
    assert!(create_explicit.status.success());

    let info_explicit = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "indexed",
            "--json",
        ])
        .output()
        .expect("info");
    assert!(info_explicit.status.success());
    let json_explicit = parse_json(std::str::from_utf8(&info_explicit.stdout).expect("utf8"));
    assert_eq!(json_explicit["index_capacity"], json!(1024));
    assert_eq!(json_explicit["index_size_bytes"], json!(1024 * 16));

    let create_scan_only = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "--size",
            "1M",
            "--index-capacity",
            "0",
            "scanonly",
        ])
        .output()
        .expect("create");
    assert!(create_scan_only.status.success());

    let info_scan_only = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "scanonly",
            "--json",
        ])
        .output()
        .expect("info");
    assert!(info_scan_only.status.success());
    let json_scan_only = parse_json(std::str::from_utf8(&info_scan_only.stdout).expect("utf8"));
    assert_eq!(json_scan_only["index_capacity"], json!(0));
    assert_eq!(json_scan_only["index_size_bytes"], json!(0));
    assert_eq!(json_scan_only["ring_offset"], json!(4096));
}

#[test]
fn pool_create_rejects_oversized_index_capacity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "--size",
            "64K",
            "--index-capacity",
            "5000",
            "too-big",
        ])
        .output()
        .expect("create");
    assert!(!create.status.success());

    let err = parse_error_json(&create.stderr);
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
    assert!(message.contains("index capacity"));
}

#[test]
fn pool_info_default_is_human_readable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "pretty",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "pretty",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let info = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "pretty",
        ])
        .output()
        .expect("info");
    assert!(info.status.success());
    let stdout = std::str::from_utf8(&info.stdout).expect("utf8");
    assert!(stdout.contains("Pool: pretty"));
    assert!(stdout.contains("Path: "));
    assert!(stdout.contains("Bounds: "));
    assert!(stdout.contains("Utilization: "));
    assert!(stdout.contains("Oldest: "));
    assert!(stdout.contains("Newest: "));
}

#[test]
fn pool_info_tty_is_compact_and_hides_ring_offset() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "pretty",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let info = cmd_tty(&[
        "--color",
        "never",
        "--dir",
        pool_dir.to_str().unwrap(),
        "pool",
        "info",
        "pretty",
    ]);
    assert!(info.status.success());
    let stdout = sanitize_tty_text(&info.stdout);
    assert!(stdout.contains("pretty"));
    assert!(stdout.contains("path:      pretty.plasmite"));
    assert!(stdout.contains("messages:  empty"));
    assert!(stdout.contains("oldest:    —"));
    assert!(stdout.contains("newest:    —"));
    assert!(stdout.contains("index:     4096 slots (64K)"));
    assert!(stdout.contains("ring:      956K"));
    assert!(!stdout.contains("offset"));
}

#[test]
fn pool_info_missing_does_not_emit_path_or_causes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let output = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "channel",
        ])
        .output()
        .expect("pool info");
    assert_eq!(output.status.code(), Some(3));
    let err = parse_error_json(&output.stderr);
    let error = err
        .get("error")
        .and_then(|value| value.as_object())
        .expect("error");
    assert_eq!(
        error.get("kind").and_then(|value| value.as_str()),
        Some("NotFound")
    );
    assert_eq!(
        error.get("message").and_then(|value| value.as_str()),
        Some("not found")
    );
    assert!(
        error.get("path").is_none(),
        "path should not be emitted for missing pool name"
    );
    assert!(
        error.get("causes").is_none(),
        "causes should not be emitted for missing pool name"
    );
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

    let feed_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
            "--tag",
            "ping",
        ])
        .output()
        .expect("feed");
    assert!(feed_out.status.success());
    let feed_json = parse_json(std::str::from_utf8(&feed_out.stdout).expect("utf8"));
    let seq = feed_json.get("seq").unwrap().as_u64().unwrap();

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "fetch",
            "demo",
            &seq.to_string(),
        ])
        .output()
        .expect("fetch");
    assert!(get.status.success());
    let get_json = parse_json(std::str::from_utf8(&get.stdout).expect("utf8"));
    assert_eq!(get_json.get("seq").unwrap().as_u64().unwrap(), seq);

    let mut follower = cmd()
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
            "fetch",
            "testpool",
            "999",
        ])
        .output()
        .expect("fetch");
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
    assert!(hint.contains("pool info") || hint.contains("follow"));
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

    let emit_out = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "feed", "testpool"])
        .output()
        .expect("feed");
    assert_eq!(emit_out.status.code().unwrap(), 2);
    let err = parse_error_json(&emit_out.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--file") || hint.contains("pipe JSON"));
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
            "--json",
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
            "--json",
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
            "--color",
            "always",
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
        .expect("follow jsonl");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let line = line.trim_end();
    assert!(!line.contains("\u{1b}["));
    let _ = parse_json(line);
    let _ = follower.kill();
    let _ = follower.wait();
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
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "list",
            "--json",
        ])
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
fn pool_list_defaults_to_table_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");

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

    std::fs::write(pool_dir.join("bad.plasmite"), b"NOPE").expect("write bad");

    let list = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "list"])
        .output()
        .expect("list");
    assert!(list.status.success());

    let stdout = std::str::from_utf8(&list.stdout).expect("utf8");
    assert!(stdout.contains("NAME"));
    assert!(stdout.contains("STATUS"));
    assert!(stdout.contains("DETAIL"));
    assert!(stdout.contains("alpha"));
    assert!(stdout.contains("beta"));
    assert!(stdout.contains("bad"));
    assert!(stdout.contains("ERR"));
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
            "--json",
            "deleteme",
        ])
        .output()
        .expect("delete");
    assert!(delete.status.success());
    let output = parse_json(std::str::from_utf8(&delete.stdout).expect("utf8"));
    let deleted = output
        .get("deleted")
        .and_then(|v| v.as_array())
        .expect("deleted array");
    assert_eq!(deleted.len(), 1);
    assert_eq!(
        deleted[0].get("pool").and_then(|v| v.as_str()),
        Some("deleteme")
    );
    let failed = output
        .get("failed")
        .and_then(|v| v.as_array())
        .expect("failed array");
    assert!(failed.is_empty());
    let pool_path = pool_dir.join("deleteme.plasmite");
    assert!(!pool_path.exists());
}

#[test]
fn pool_delete_multiple_best_effort_mixed_results() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "a",
            "b",
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
            "--json",
            "a",
            "missing",
            "b",
        ])
        .output()
        .expect("delete");
    assert_eq!(delete.status.code(), Some(3));

    let output = parse_json(std::str::from_utf8(&delete.stdout).expect("utf8"));
    let deleted = output
        .get("deleted")
        .and_then(|v| v.as_array())
        .expect("deleted array");
    assert_eq!(deleted.len(), 2);
    let deleted_names = deleted
        .iter()
        .filter_map(|entry| entry.get("pool").and_then(|v| v.as_str()))
        .collect::<Vec<_>>();
    assert!(deleted_names.contains(&"a"));
    assert!(deleted_names.contains(&"b"));

    let failed = output
        .get("failed")
        .and_then(|v| v.as_array())
        .expect("failed array");
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].get("pool").and_then(|v| v.as_str()),
        Some("missing")
    );
    let error = failed[0]
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(error.get("kind").and_then(|v| v.as_str()), Some("NotFound"));

    assert!(!pool_dir.join("a.plasmite").exists());
    assert!(!pool_dir.join("b.plasmite").exists());
}

#[test]
fn pool_delete_multiple_with_invalid_ref_continues() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "ok"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let delete = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "delete",
            "--json",
            "ok",
            "http://127.0.0.1:9700/demo",
        ])
        .output()
        .expect("delete");
    assert_eq!(delete.status.code(), Some(2));

    let output = parse_json(std::str::from_utf8(&delete.stdout).expect("utf8"));
    let deleted = output
        .get("deleted")
        .and_then(|v| v.as_array())
        .expect("deleted array");
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].get("pool").and_then(|v| v.as_str()), Some("ok"));

    let failed = output
        .get("failed")
        .and_then(|v| v.as_array())
        .expect("failed array");
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].get("pool").and_then(|v| v.as_str()),
        Some("http://127.0.0.1:9700/demo")
    );
    let error = failed[0]
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(error.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    assert!(!pool_dir.join("ok.plasmite").exists());
}

#[test]
fn pool_delete_defaults_to_table_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "ok"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let delete = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "delete",
            "ok",
            "missing",
        ])
        .output()
        .expect("delete");
    assert_eq!(delete.status.code(), Some(3));

    let stdout = std::str::from_utf8(&delete.stdout).expect("utf8");
    assert!(stdout.contains("NAME"));
    assert!(stdout.contains("STATUS"));
    assert!(stdout.contains("PATH"));
    assert!(stdout.contains("DETAIL"));
    assert!(stdout.contains("ok"));
    assert!(stdout.contains("missing"));
    assert!(stdout.contains("OK"));
    assert!(stdout.contains("ERR"));
}

#[test]
fn errors_are_json_on_non_tty_stderr() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let follower = cmd()
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "missing",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code().unwrap(), 3);

    let err = parse_error_json(&follower.stderr);
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
    assert!(hint.contains("--create") || hint.contains("exact command"));
}

#[test]
fn clap_errors_are_concise_in_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let bad = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--definitely-not-a-flag",
        ])
        .output()
        .expect("follow");
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
fn misuse_feedback_matrix_is_actionable_across_command_families() {
    let cases: [(&[&str], &str, &str); 5] = [
        (
            &["feed", "demo", "{\"x\":1}", "--retry-delay", "1s"],
            "--retry-delay requires --retry",
            "Add --retry",
        ),
        (
            &["doctor", "demo", "--all"],
            "--all cannot be combined",
            "Use --all by itself",
        ),
        (
            &["serve", "--bind", "nope", "check"],
            "invalid bind address",
            "host:port",
        ),
        (&["completion", "nope"], "invalid value", "plasmite --help"),
        (
            &["version", "extra"],
            "unexpected argument",
            "version --help",
        ),
    ];

    for (args, expected_message_fragment, expected_hint_fragment) in cases {
        let output = cmd().args(args).output().expect("command");
        assert_actionable_usage_feedback(
            &output,
            expected_message_fragment,
            expected_hint_fragment,
        );
    }
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
    assert_eq!(create.status.code().unwrap(), 6);
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
fn permission_denied_matrix_for_write_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("readonly-pools");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "base",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let mut perms = std::fs::metadata(&pool_dir)
        .expect("metadata")
        .permissions();
    let original_mode = perms.mode();
    perms.set_readonly(true);
    std::fs::set_permissions(&pool_dir, perms).expect("set perms");

    let output_create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "other",
        ])
        .output()
        .expect("create denied");
    assert!(
        matches!(output_create.status.code(), Some(6) | Some(8)),
        "unexpected create status: {:?}",
        output_create.status.code()
    );
    let output_json = parse_error_json(&output_create.stderr);
    let inner_create = output_json
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert!(
        matches!(
            inner_create.get("kind").and_then(|v| v.as_str()),
            Some("Permission" | "Io")
        ),
        "unexpected create kind: {:?}",
        inner_create.get("kind")
    );

    let output_delete = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "delete",
            "base",
        ])
        .output()
        .expect("delete denied");
    assert_eq!(output_delete.status.code(), Some(8));

    let restore = std::fs::Permissions::from_mode(original_mode);
    std::fs::set_permissions(&pool_dir, restore).expect("unset perms");
}

#[test]
fn truncated_pool_file_variants_are_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    std::fs::create_dir_all(&pool_dir).expect("mkdir");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "probe",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let pool_path = pool_dir.join("probe.plasmite");
    let valid = std::fs::read(&pool_path).expect("read valid pool");

    let mut cases = Vec::new();
    cases.push(("empty", Vec::new()));
    cases.push(("one-byte", valid[..1].to_vec()));
    cases.push(("half", valid[..valid.len() / 2].to_vec()));
    let mut truncated = valid.clone();
    truncated.truncate(valid.len().saturating_sub(128.min(valid.len())));
    cases.push(("truncated-end", truncated));
    cases.push(("zero-header", vec![0; 4096]));

    for (case_name, bytes) in cases {
        std::fs::write(&pool_path, &bytes).expect("write mutated pool");
        let output = cmd()
            .args(["--dir", pool_dir.to_str().unwrap(), "pool", "info", "probe"])
            .output()
            .expect("info");
        assert_ne!(
            output.status.code(),
            Some(0),
            "{case_name} unexpectedly succeeded"
        );
        assert!(
            !output.stderr.is_empty(),
            "{case_name} should return structured error stderr"
        );
        let error_json = parse_error_json(&output.stderr);
        let inner = error_json
            .get("error")
            .and_then(|v| v.as_object())
            .expect("error object");
        assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Corrupt"));
    }
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
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "doctor",
            "doctorpool",
            "--json",
        ])
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
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "doctor",
            "bad",
            "--json",
        ])
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
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "doctor",
            "--all",
            "--json",
        ])
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
fn doctor_requires_pool_or_all() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let doctor = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "doctor"])
        .output()
        .expect("doctor");
    assert_eq!(doctor.status.code(), Some(2));
    let stderr = std::str::from_utf8(&doctor.stderr).expect("utf8");
    assert!(stderr.contains("Usage: plasmite doctor"));
    assert!(stderr.contains("Diagnose pool health"));
}

#[test]
fn doctor_defaults_to_human_output() {
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
    let stdout = std::str::from_utf8(&doctor.stdout).expect("utf8");
    assert!(stdout.contains("OK: doctorpool"));
}

#[test]
fn doctor_tty_reports_count_and_seq_range_for_healthy_pool() {
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

    for idx in 1..=4 {
        let feed = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "doctorpool",
                &format!("{{\"x\":{idx}}}"),
            ])
            .output()
            .expect("feed");
        assert!(feed.status.success());
    }

    let doctor = cmd_tty(&[
        "--color",
        "never",
        "--dir",
        pool_dir.to_str().unwrap(),
        "doctor",
        "doctorpool",
    ]);
    assert!(doctor.status.success());
    let stdout = sanitize_tty_text(&doctor.stdout);
    assert!(stdout.contains("doctorpool: healthy"));
    assert!(stdout.contains("messages:  4 (seq 1..4)"));
    assert!(stdout.contains("checked:   header, index, ring — 0 issues"));
}

#[test]
fn doctor_all_tty_uses_pool_names_and_message_counts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "tour-main",
            "tour-aux",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    for idx in 1..=4 {
        let feed = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "feed",
                "tour-main",
                &format!("{{\"x\":{idx}}}"),
            ])
            .output()
            .expect("feed");
        assert!(feed.status.success());
    }

    let doctor = cmd_tty(&[
        "--color",
        "never",
        "--dir",
        pool_dir.to_str().unwrap(),
        "doctor",
        "--all",
    ]);
    assert!(doctor.status.success());
    let stdout = sanitize_tty_text(&doctor.stdout);
    assert!(stdout.contains("All 2 pools healthy."));
    assert!(stdout.contains("tour-main"));
    assert!(stdout.contains("4 messages"));
    assert!(stdout.contains("tour-aux"));
    assert!(stdout.contains("0 messages"));
    assert!(stdout.contains("0 issues"));
    assert!(!stdout.contains(".plasmite"));
}

#[test]
fn doctor_rejects_pool_with_all() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let doctor = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "doctor",
            "foo",
            "--all",
        ])
        .output()
        .expect("doctor");
    assert!(!doctor.status.success());
    let err = parse_error_json(&doctor.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()), Some("Usage"));
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap_or("");
    assert!(message.contains("--all cannot be combined"));
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
fn serve_init_help_is_available() {
    let output = cmd()
        .args(["serve", "init", "--help"])
        .output()
        .expect("serve init help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Generate token + TLS artifacts"));
}

#[test]
fn serve_check_outputs_resolved_config() {
    let output = cmd()
        .args(["serve", "check", "--json"])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    let payload = parse_json(stdout);
    let check = payload.get("check").expect("check");
    let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(status, "valid");
    let base_url = check.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
    assert!(base_url.contains("127.0.0.1:9700"));
    let mcp = check.get("mcp").and_then(|v| v.as_str()).unwrap_or("");
    assert!(mcp.ends_with("/mcp"));
}

#[test]
fn serve_check_human_uses_readable_limits_and_fingerprint() {
    let temp = tempfile::tempdir().expect("tempdir");
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

    let output = cmd()
        .args([
            "serve",
            "--tls-cert",
            cert_path.to_str().unwrap(),
            "--tls-key",
            key_path.to_str().unwrap(),
            "check",
        ])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("Configuration valid."));
    assert!(stdout.contains("MCP:    https://127.0.0.1:9700/mcp"));
    assert!(stdout.contains("Limits: body 1M, timeout 30s, concurrency 64"));
    assert!(stdout.contains("Fingerprint: SHA256:"));
}

#[test]
fn serve_check_json_includes_tls_fingerprint() {
    let temp = tempfile::tempdir().expect("tempdir");
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

    let output = cmd()
        .args([
            "serve",
            "--tls-cert",
            cert_path.to_str().unwrap(),
            "--tls-key",
            key_path.to_str().unwrap(),
            "check",
            "--json",
        ])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let payload = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    let fingerprint = payload
        .get("check")
        .and_then(|v| v.get("tls_fingerprint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(fingerprint.starts_with("SHA256:"));
}

#[test]
fn serve_check_defaults_to_human_output() {
    let output = cmd()
        .args(["serve", "check"])
        .output()
        .expect("serve check");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("Configuration valid."));
}

#[test]
fn serve_check_rejects_invalid_config() {
    let output = cmd()
        .args(["serve", "--bind", "0.0.0.0:0", "check"])
        .output()
        .expect("serve check");
    assert!(!output.status.success());
    let err = parse_error_json(&output.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "Usage");
}

#[test]
fn serve_init_writes_artifacts_and_next_commands() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out_dir = temp.path().join("serve-init");
    let output = cmd()
        .args([
            "serve",
            "init",
            "--output-dir",
            out_dir.to_str().unwrap(),
            "--bind",
            "0.0.0.0:9700",
        ])
        .output()
        .expect("serve init");
    assert!(output.status.success());

    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    let payload = parse_json(stdout);
    let artifacts = payload
        .get("init")
        .and_then(|v| v.get("artifact_paths"))
        .expect("artifact_paths");
    let token_file = artifacts
        .get("token_file")
        .and_then(|v| v.as_str())
        .expect("token_file");
    let tls_cert = artifacts
        .get("tls_cert")
        .and_then(|v| v.as_str())
        .expect("tls_cert");
    let tls_key = artifacts
        .get("tls_key")
        .and_then(|v| v.as_str())
        .expect("tls_key");
    assert!(Path::new(token_file).exists());
    assert!(Path::new(tls_cert).exists());
    assert!(Path::new(tls_key).exists());

    let token = std::fs::read_to_string(token_file).expect("read token");
    assert!(!token.trim().is_empty());
    assert!(
        !stdout.contains(token.trim()),
        "token value should not be echoed to stdout"
    );

    let server_commands = payload
        .get("init")
        .and_then(|v| v.get("server_commands"))
        .and_then(|v| v.as_array())
        .expect("server_commands");
    assert!(server_commands.iter().any(|v| {
        v.as_str()
            .unwrap_or("")
            .contains("plasmite serve --bind 0.0.0.0:9700 --allow-non-loopback")
    }));
    let client_commands = payload
        .get("init")
        .and_then(|v| v.get("client_commands"))
        .and_then(|v| v.as_array())
        .expect("client_commands");
    assert!(
        client_commands
            .iter()
            .any(|v| v.as_str().unwrap_or("").contains("plasmite feed")),
        "expected plasmite feed client command"
    );
    let tls_fingerprint = payload
        .get("init")
        .and_then(|v| v.get("tls_fingerprint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(tls_fingerprint.starts_with("SHA256:"));
}

#[test]
fn serve_init_requires_force_for_existing_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out_dir = temp.path().join("serve-init");

    let first = cmd()
        .args(["serve", "init", "--output-dir", out_dir.to_str().unwrap()])
        .output()
        .expect("first serve init");
    assert!(first.status.success());

    let second = cmd()
        .args(["serve", "init", "--output-dir", out_dir.to_str().unwrap()])
        .output()
        .expect("second serve init");
    assert!(!second.status.success());
    let err = parse_error_json(&second.stderr);
    let kind = err
        .get("error")
        .and_then(|v| v.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(kind, "AlreadyExists");
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("--force"));
}

#[test]
fn serve_init_tty_reports_created_and_overwritten() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out_dir = temp.path().join("serve-init");

    let first = cmd_tty(&[
        "--color",
        "never",
        "serve",
        "init",
        "--output-dir",
        out_dir.to_str().unwrap(),
        "--bind",
        "0.0.0.0:9700",
    ]);
    assert!(first.status.success());
    let first_text = sanitize_tty_text(&first.stdout);
    assert!(first_text.contains("Secure serving initialized."));
    assert!(first_text.contains("Output directory:"));
    assert!(first_text.contains("Files created:"));
    assert!(first_text.contains("    pls serve \\"));
    assert!(first_text.contains("      --bind 0.0.0.0:9700 \\"));
    assert!(first_text.contains("      --allow-non-loopback \\"));
    assert!(first_text.contains("      --token-file "));
    assert!(first_text.contains("      --tls-cert "));
    assert!(first_text.contains("      --tls-key "));
    assert!(!first_text.contains("THIS-HOST"));
    let feed_line = first_text
        .lines()
        .find(|line| line.contains("pls feed https://"))
        .expect("feed line");
    assert!(feed_line.contains(":9700/demo \\"));
    let follow_line = first_text
        .lines()
        .find(|line| line.contains("pls follow https://"))
        .expect("follow line");
    assert!(follow_line.contains(":9700/demo \\"));

    let second = cmd_tty(&[
        "--color",
        "never",
        "serve",
        "init",
        "--output-dir",
        out_dir.to_str().unwrap(),
        "--bind",
        "0.0.0.0:9700",
        "--force",
    ]);
    assert!(second.status.success());
    let second_text = sanitize_tty_text(&second.stdout);
    assert!(second_text.contains("Secure serving re-initialized."));
    assert!(second_text.contains("Files overwritten:"));
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
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
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
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_rejects_token_and_token_file_combination_with_init_hint() {
    let temp = tempfile::tempdir().expect("tempdir");
    let token_path = temp.path().join("token.txt");
    std::fs::write(&token_path, "secret").expect("write token");

    let serve = cmd()
        .args([
            "serve",
            "--token",
            "dev-token",
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
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_rejects_conflicting_tls_flags_with_init_hint() {
    let serve = cmd()
        .args([
            "serve",
            "--tls-self-signed",
            "--tls-cert",
            "cert.pem",
            "--tls-key",
            "key.pem",
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
    let hint = err
        .get("error")
        .and_then(|v| v.get("hint"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(hint.contains("serve init"));
}

#[test]
fn serve_rejects_wildcard_cors_origin() {
    let serve = cmd()
        .args(["serve", "--bind", "127.0.0.1:0", "--cors-origin", "*"])
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
    assert!(message.contains("wildcard"));
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
        "tags": ["oversized"],
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
    let start = Instant::now();
    loop {
        match ureq::get(&tail_url).call() {
            Ok(_) => panic!("expected tail timeout rejection"),
            Err(ureq::Error::Status(code, resp)) => {
                assert_eq!(code, 400);
                let body = resp.into_string().expect("body");
                let value: Value = serde_json::from_str(&body).expect("json");
                assert_eq!(value["error"]["kind"], "Usage");
                break;
            }
            Err(ureq::Error::Transport(err)) => {
                if start.elapsed() >= Duration::from_millis(250) {
                    panic!("request failed after startup retry window: {err:?}");
                }
                sleep(Duration::from_millis(25));
            }
        }
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

// --- Shell completion tests ---

#[test]
fn completion_bash_generates_valid_output() {
    let output = cmd()
        .args(["completion", "bash"])
        .output()
        .expect("completion bash");
    assert!(output.status.success(), "completion bash should succeed");
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(!stdout.is_empty(), "bash completion should produce output");
    assert!(
        stdout.contains("_plasmite"),
        "bash should define _plasmite function"
    );
    assert!(
        stdout.contains("pool"),
        "bash should include pool subcommand"
    );
    assert!(
        stdout.contains("feed"),
        "bash should include feed subcommand"
    );
    assert!(
        stdout.contains("follow"),
        "bash should include follow subcommand"
    );
}

#[test]
fn completion_zsh_generates_valid_output() {
    let output = cmd()
        .args(["completion", "zsh"])
        .output()
        .expect("completion zsh");
    assert!(output.status.success(), "completion zsh should succeed");
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(!stdout.is_empty(), "zsh completion should produce output");
    assert!(
        stdout.contains("#compdef") || stdout.contains("_plasmite"),
        "zsh should contain #compdef or _plasmite"
    );
    assert!(
        stdout.contains("pool"),
        "zsh should include pool subcommand"
    );
}

#[test]
fn completion_fish_generates_valid_output() {
    let output = cmd()
        .args(["completion", "fish"])
        .output()
        .expect("completion fish");
    assert!(output.status.success(), "completion fish should succeed");
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(!stdout.is_empty(), "fish completion should produce output");
    assert!(
        stdout.contains("complete"),
        "fish should use 'complete' command"
    );
    assert!(
        stdout.contains("plasmite"),
        "fish should reference plasmite"
    );
}

#[test]
fn completion_invalid_shell_fails() {
    let output = cmd()
        .args(["completion", "fake-shell"])
        .output()
        .expect("completion fake-shell");
    assert!(
        !output.status.success(),
        "completion with unsupported shell should fail"
    );
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

fn mcp_send_request(stdin: &mut impl Write, request: &Value) {
    serde_json::to_writer(&mut *stdin, request).expect("write request");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush request");
}

fn mcp_read_response(stdout: &mut BufReader<impl Read>) -> Value {
    let mut line = String::new();
    let read = stdout.read_line(&mut line).expect("read response");
    assert!(read > 0, "expected MCP response line");
    parse_json(line.trim())
}

#[test]
fn mcp_stdio_initialize_and_tool_resource_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let mut child = cmd()
        .args(["mcp", "--dir", pool_dir.to_str().expect("pool dir")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    );
    let initialize = mcp_read_response(&mut stdout);
    assert_eq!(initialize["id"], json!(1));
    assert_eq!(initialize["result"]["protocolVersion"], json!("2025-11-25"));
    assert_eq!(
        initialize["result"]["capabilities"]["tools"]["listChanged"],
        json!(false)
    );
    assert_eq!(
        initialize["result"]["capabilities"]["resources"]["listChanged"],
        json!(false)
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );
    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "ping",
            "params": {}
        }),
    );
    let ping = mcp_read_response(&mut stdout);
    assert_eq!(ping["id"], json!(2));
    assert_eq!(ping["result"], json!({}));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 22,
            "method": "tools/list",
            "params": {}
        }),
    );
    let tools_list = mcp_read_response(&mut stdout);
    assert_eq!(tools_list["id"], json!(22));
    let tool_names = tools_list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"plasmite_pool_create"));
    assert!(tool_names.contains(&"plasmite_read"));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "plasmite_pool_create",
                "arguments": { "name": "demo" }
            }
        }),
    );
    let create = mcp_read_response(&mut stdout);
    assert_eq!(create["id"], json!(3));
    assert_eq!(
        create["result"]["structuredContent"]["pool"]["name"],
        json!("demo")
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "plasmite_feed",
                "arguments": {
                    "pool": "demo",
                    "data": {"msg": "hello"},
                    "tags": ["chat"]
                }
            }
        }),
    );
    let feed = mcp_read_response(&mut stdout);
    let seq = feed["result"]["structuredContent"]["message"]["seq"]
        .as_u64()
        .expect("seq");
    assert_eq!(feed["id"], json!(4));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 41,
            "method": "tools/call",
            "params": {
                "name": "plasmite_read",
                "arguments": {
                    "pool": "demo",
                    "after_seq": seq.saturating_sub(1),
                    "since": "1970-01-01T00:00:00Z",
                    "count": 5
                }
            }
        }),
    );
    let read = mcp_read_response(&mut stdout);
    assert_eq!(read["id"], json!(41));
    assert_eq!(
        read["result"]["structuredContent"]["messages"][0]["seq"],
        json!(seq)
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "plasmite_fetch",
                "arguments": {
                    "pool": "demo",
                    "seq": seq
                }
            }
        }),
    );
    let fetch = mcp_read_response(&mut stdout);
    assert_eq!(fetch["id"], json!(42));
    assert_eq!(
        fetch["result"]["structuredContent"]["message"]["data"]["msg"],
        json!("hello")
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "resources/list",
            "params": {}
        }),
    );
    let resources = mcp_read_response(&mut stdout);
    assert_eq!(resources["id"], json!(5));
    assert_eq!(
        resources["result"]["resources"][0]["uri"],
        json!("plasmite:///pools/demo")
    );

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "resources/read",
            "params": {
                "uri": "plasmite:///pools/demo"
            }
        }),
    );
    let resource_read = mcp_read_response(&mut stdout);
    assert_eq!(resource_read["id"], json!(6));
    let text_payload = resource_read["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    let payload = parse_json(text_payload);
    assert_eq!(payload["next_after_seq"], json!(seq));
    assert_eq!(payload["messages"][0]["data"]["msg"], json!("hello"));

    mcp_send_request(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "plasmite_pool_delete",
                "arguments": {
                    "pool": "demo"
                }
            }
        }),
    );
    let delete = mcp_read_response(&mut stdout);
    assert_eq!(delete["id"], json!(7));
    assert_ne!(delete["result"]["isError"], json!(true));

    drop(stdin);
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            assert!(status.success(), "mcp exited non-zero: {status}");
            break;
        }
        if start.elapsed() > Duration::from_secs(3) {
            panic!("mcp process did not exit after stdin close");
        }
        sleep(Duration::from_millis(20));
    }
}
