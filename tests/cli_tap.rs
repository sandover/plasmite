//! Purpose: Tap black-box CLI integration tests.

pub mod support;
use support::cli::*;

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
    let help = cmd().args(["tap", "--help"]).output().expect("tap help");
    assert!(help.status.success());
    let stdout = std::str::from_utf8(&help.stdout).expect("utf8");
    assert!(stdout.contains("Usage: plasmite tap [OPTIONS] <POOL> -- <COMMAND>..."));

    let output = cmd().args(["tap", "build"]).output().expect("tap");
    assert_eq!(output.status.code(), Some(2));
    let error = parse_error_json(&output.stderr);
    assert_eq!(error["error"]["kind"], "Usage");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("requires a wrapped command after `--`"))
    );
    assert!(
        error["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("plasmite tap <pool> -- <command...>"))
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
