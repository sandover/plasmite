//! A YAML message reaches a live consumer before the stream closes.

pub mod support;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use support::cli::cmd;

struct LiveProcess(Child);

impl Drop for LiveProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn yaml_follow_delivers_complete_documents_while_still_running() {
    let temp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["pool", "create", "stream"]);
    run(&["feed", "stream", "{\"number\":1}"]);
    let child = cmd()
        .arg("--dir")
        .arg(temp.path())
        .args([
            "follow",
            "stream",
            "--format",
            "yaml",
            "--tail",
            "1",
            "--timeout",
            "15s",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = LiveProcess(child);
    let stdout = process.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut document = String::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            document.push_str(&line);
            document.push('\n');
            if line == "..." && send.send(std::mem::take(&mut document)).is_err() {
                break;
            }
        }
    });
    for number in [1, 2] {
        if number == 2 {
            run(&["feed", "stream", "{\"number\":2}"]);
        }
        let document = receive
            .recv_timeout(Duration::from_secs(5))
            .expect("a complete YAML document must arrive without EOF or another message");
        let value: serde_json::Value = yaml_serde::from_str(&document).unwrap();
        assert_eq!(value["data"]["number"], number);
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "follow should remain live"
        );
    }
    drop(process);
    reader.join().unwrap();
}

#[test]
fn yaml_feed_appends_each_complete_document_before_stdin_closes() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(["pool", "create", "stream"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let child = cmd()
        .arg("--dir")
        .arg(temp.path())
        .args(["feed", "stream", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = LiveProcess(child);
    let mut stdin = process.0.stdin.take().unwrap();
    let stdout = process.0.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    for number in [1, 2] {
        write!(stdin, "---\nnumber: {number}\n...\n").unwrap();
        let receipt = receive
            .recv_timeout(Duration::from_secs(5))
            .expect("feed must acknowledge the append before reading the next document")
            .unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(receipt["seq"], number);
        assert_eq!(receipt["data"]["number"], number);
    }
    drop(stdin);
    assert!(process.0.wait().unwrap().success());
    reader.join().unwrap();
}
