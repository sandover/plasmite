//! Purpose: Share black-box CLI integration-test process and parsing helpers.
//! Exports: Command, TTY, JSON, pool, server, and MCP test utilities.
//! Role: Keep command-family suites focused on behavior rather than setup.

pub use super::server::TestServer as ServeProcess;
pub use fs2::FileExt;
pub use rcgen::{Certificate, CertificateParams, SanType};
pub use serde_json::{Value, json};
pub use std::fs::File;
pub use std::io::{BufRead, BufReader, Read, Write};
pub use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
pub use std::os::unix::fs::PermissionsExt;
pub use std::path::Path;
pub use std::process::{Command, Stdio};
pub use std::sync::Arc;
pub use std::sync::mpsc;
pub use std::thread;
pub use std::thread::sleep;
pub use std::time::{Duration, Instant};

pub fn cmd() -> Command {
    let exe = env!("CARGO_BIN_EXE_plasmite");
    Command::new(exe)
}

pub fn cmd_tty(args: &[&str]) -> std::process::Output {
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
pub fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

pub fn sanitize_tty_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace(['\u{4}', '\u{8}', '\r'], "")
}

pub fn parse_json(value: &str) -> Value {
    serde_json::from_str(value).expect("valid json")
}

pub fn parse_json_lines(output: &[u8]) -> Vec<Value> {
    let text = String::from_utf8_lossy(output);
    text.lines().map(parse_json).collect()
}

pub fn read_line_with_timeout<R: Read + Send + 'static>(reader: R, timeout: Duration) -> String {
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

pub fn read_json_value<R: Read>(reader: R) -> Value {
    let mut stream = serde_json::Deserializer::from_reader(reader).into_iter::<Value>();
    stream.next().expect("json value").expect("valid json")
}

pub fn parse_error_json(output: &[u8]) -> Value {
    let text = std::str::from_utf8(output).expect("utf8");
    parse_json(text)
}

pub fn parse_notice_json(line: &str) -> Value {
    parse_json(line.trim())
}

pub fn fetch_message(pool_dir: &Path, pool: &str, seq: u64) -> Value {
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

pub fn assert_actionable_usage_feedback(
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

pub fn mcp_send_request(stdin: &mut impl Write, request: &Value) {
    serde_json::to_writer(&mut *stdin, request).expect("write request");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush request");
}

pub fn mcp_read_response(stdout: &mut BufReader<impl Read>) -> Value {
    let mut line = String::new();
    let read = stdout.read_line(&mut line).expect("read response");
    assert!(read > 0, "expected MCP response line");
    parse_json(line.trim())
}
