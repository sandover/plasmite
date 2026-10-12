//! Black-box checks for YAML report and message output.

pub mod support;

use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::Output;
use support::cli::{ServeProcess, Stdio, cmd};

fn cli_at(pool_dir: &Path, args: &[&str]) -> Output {
    cmd()
        .arg("--dir")
        .arg(pool_dir)
        .args(args)
        .output()
        .expect("run plasmite")
}

fn create_pool(pool_dir: &Path, name: &str) {
    let output = cli_at(pool_dir, &["pool", "create", name, "--format", "json"]);
    assert_success(&output);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_no_ansi(output: &Output) {
    assert!(!output.stdout.contains(&0x1b), "stdout contains ANSI codes");
    assert!(!output.stderr.contains(&0x1b), "stderr contains ANSI codes");
}

fn yaml_documents(bytes: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(bytes).expect("YAML output is UTF-8");
    serde_yaml_ng::Deserializer::from_str(text)
        .map(|document| Value::deserialize(document).expect("valid YAML document"))
        .collect()
}

fn yaml_report(output: &Output) -> Value {
    assert_success(output);
    assert_no_ansi(output);
    let mut documents = yaml_documents(&output.stdout);
    assert_eq!(documents.len(), 1, "expected one YAML document");
    documents.remove(0)
}

fn json_report(output: &Output) -> Value {
    assert_success(output);
    assert_no_ansi(output);
    serde_json::from_slice(&output.stdout).expect("one JSON report")
}

fn yaml_error(output: &Output) -> Value {
    assert!(!output.status.success(), "expected a failing command");
    assert_no_ansi(output);
    let mut documents = yaml_documents(&output.stderr);
    assert_eq!(documents.len(), 1, "expected one YAML error document");
    let error = documents.remove(0);
    assert!(
        error["error"].is_object(),
        "expected an error envelope: {error}"
    );
    assert!(error["error"]["kind"].is_string());
    error
}

fn assert_document_markers(bytes: &[u8], count: usize) {
    let text = std::str::from_utf8(bytes).expect("YAML output is UTF-8");
    let starts = text.lines().filter(|line| *line == "---").count();
    let ends = text.lines().filter(|line| *line == "...").count();
    assert_eq!(
        starts, count,
        "each document needs an explicit start marker"
    );
    assert_eq!(ends, count, "each document needs an explicit end marker");
}

#[test]
fn fetch_yaml_matches_json_for_local_and_remote_edge_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "edge-values");

    let payload = serde_json::json!({
        "u64_max": u64::MAX,
        "i64_min": i64::MIN,
        "strings": [
            "true",
            "null",
            "yes",
            "2026-10-12",
            "000123",
            "first line\nsecond line\n",
            "Unicode: snowman ☃ and rocket 🚀",
            "controls: nul\u{0000} tab\t escape\u{001b}",
        ],
        "null_value": null,
        "empty_array": [],
        "empty_map": {},
    });
    let input = serde_json::to_string(&payload).expect("JSON payload");
    let feed = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args(["feed", "edge-values", "--data"])
        .arg(input)
        .args(["--format", "yaml", "--color", "always"])
        .output()
        .expect("feed JSON input with YAML receipt");
    let receipt = yaml_report(&feed);
    assert_eq!(receipt["seq"], 1);
    assert!(receipt["time"].is_string());

    let local_json = cli_at(
        &pool_dir,
        &[
            "fetch",
            "edge-values",
            "1",
            "--format",
            "json",
            "--color",
            "always",
        ],
    );
    let local_json = json_report(&local_json);
    let local_yaml = cli_at(
        &pool_dir,
        &[
            "fetch",
            "edge-values",
            "1",
            "--format",
            "yaml",
            "--color",
            "always",
        ],
    );
    let local_yaml = yaml_report(&local_yaml);
    assert_eq!(local_yaml, local_json);
    // Compare exported values: storage already converts unsigned integers above i64::MAX.
    assert_eq!(local_yaml["data"]["strings"], payload["strings"]);
    assert_eq!(local_yaml["data"]["i64_min"], i64::MIN);

    let server = ServeProcess::start(&pool_dir);
    let remote_ref = format!("{}/edge-values", server.local_url);
    let remote_json = cmd()
        .args([
            "fetch",
            &remote_ref,
            "1",
            "--format",
            "json",
            "--color",
            "always",
        ])
        .output()
        .expect("remote JSON fetch");
    let remote_json = json_report(&remote_json);
    let remote_yaml = cmd()
        .args([
            "fetch",
            &remote_ref,
            "1",
            "--format",
            "yaml",
            "--color",
            "always",
        ])
        .output()
        .expect("remote YAML fetch");
    let remote_yaml = yaml_report(&remote_yaml);
    assert_eq!(remote_yaml, remote_json);
    assert_eq!(remote_yaml, local_json);
}

#[test]
fn fetched_yaml_message_feeds_without_an_input_format_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "source");
    create_pool(&pool_dir, "destination");

    let payload = serde_json::json!({"event": "yaml-copy", "value": 17});
    let input = serde_json::to_string(&payload).expect("JSON payload");
    let appended = cli_at(
        &pool_dir,
        &[
            "feed", "source", "--data", &input, "--tag", "copy", "--format", "json",
        ],
    );
    let source_message = json_report(&appended);

    let yaml = cli_at(&pool_dir, &["fetch", "source", "1", "--format", "yaml"]);
    assert_success(&yaml);
    assert_document_markers(&yaml.stdout, 1);

    let mut feed = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args(["feed", "destination", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start YAML feed");
    feed.stdin
        .take()
        .expect("feed stdin")
        .write_all(&yaml.stdout)
        .expect("write fetched YAML message");
    let copied = feed.wait_with_output().expect("wait for YAML feed");
    let destination_message = json_report(&copied);

    assert_eq!(source_message["data"], payload);
    assert_eq!(source_message["meta"]["tags"], serde_json::json!(["copy"]));
    assert_eq!(destination_message["data"], payload);
    assert_eq!(
        destination_message["meta"]["tags"],
        source_message["meta"]["tags"]
    );
}

#[test]
fn yaml_pool_reports_and_version_match_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "first");
    create_pool(&pool_dir, "second");

    for (yaml_args, json_args) in [
        (
            &["version", "--format", "yaml", "--color", "always"][..],
            &["version", "--format", "json"][..],
        ),
        (
            &["pool", "list", "--format", "yaml", "--color", "always"][..],
            &["pool", "list", "--format", "json"][..],
        ),
        (
            &[
                "pool", "info", "first", "--format", "yaml", "--color", "always",
            ][..],
            &["pool", "info", "first", "--format", "json"][..],
        ),
    ] {
        let yaml = yaml_report(&cli_at(&pool_dir, yaml_args));
        let json = json_report(&cli_at(&pool_dir, json_args));
        assert_eq!(
            yaml, json,
            "YAML and JSON reports should carry the same values"
        );
    }
}

#[test]
fn finite_follow_emits_marked_yaml_documents() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "history");
    for value in 1..=3 {
        let output = cmd()
            .arg("--dir")
            .arg(&pool_dir)
            .args(["feed", "history", "--data"])
            .arg(format!("{{\"value\":{value}}}"))
            .output()
            .expect("feed history value");
        assert_success(&output);
    }

    let envelopes = cli_at(
        &pool_dir,
        &[
            "follow",
            "history",
            "--tail",
            "2",
            "--no-follow",
            "--format",
            "yaml",
            "--color",
            "always",
        ],
    );
    assert_success(&envelopes);
    assert_no_ansi(&envelopes);
    assert_document_markers(&envelopes.stdout, 2);
    let envelopes = yaml_documents(&envelopes.stdout);
    assert_eq!(envelopes.len(), 2);
    assert_eq!(envelopes[0]["seq"], 2);
    assert_eq!(envelopes[1]["seq"], 3);
    assert_eq!(envelopes[0]["data"]["value"], 2);
    assert_eq!(envelopes[1]["data"]["value"], 3);
}

#[test]
fn feed_accepts_explicit_yaml_data_and_writes_complete_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "json-input");

    let valid = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args([
            "feed",
            "json-input",
            "--data",
            "{\"value\":1}",
            "--format",
            "yaml",
            "--color",
            "always",
        ])
        .output()
        .expect("feed JSON input");
    assert_eq!(yaml_report(&valid)["seq"], 1);

    let yaml_input = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args([
            "feed",
            "json-input",
            "--data",
            "--input-format",
            "yaml",
            "value: 2",
            "--format",
            "yaml",
            "--color",
            "always",
        ])
        .output()
        .expect("feed YAML data");
    assert_eq!(yaml_report(&yaml_input)["data"]["value"], 2);

    let second_message = cli_at(
        &pool_dir,
        &[
            "fetch",
            "json-input",
            "2",
            "--format",
            "yaml",
            "--color",
            "always",
        ],
    );
    assert_eq!(yaml_report(&second_message)["data"]["value"], 2);
}

#[test]
fn yaml_parse_errors_and_removed_formats_fail_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let parse_error = cli_at(
        &pool_dir,
        &["version", "--format", "yaml", "--unknown-option"],
    );
    assert_eq!(parse_error.status.code(), Some(2));
    assert_eq!(yaml_error(&parse_error)["error"]["kind"], "Usage");

    let invalid_feed_format = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args([
            "feed",
            "not-created",
            "--create",
            "--data",
            "{}",
            "--format",
            "jsonl",
        ])
        .output()
        .expect("unsupported feed format");
    assert_eq!(invalid_feed_format.status.code(), Some(2));
    assert!(
        !pool_dir.exists(),
        "an unsupported format must not create a pool"
    );

    let removed_alias = cli_at(&pool_dir, &["version", "--yaml"]);
    assert_eq!(removed_alias.status.code(), Some(2));
}
