//! Black-box checks for YAML report and message output.

pub mod support;

use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use std::process::Output;
use support::cli::{ServeProcess, cmd};

fn cli_at(pool_dir: &Path, args: &[&str]) -> Output {
    cmd()
        .arg("--dir")
        .arg(pool_dir)
        .args(args)
        .output()
        .expect("run plasmite")
}

fn create_pool(pool_dir: &Path, name: &str) {
    let output = cli_at(pool_dir, &["pool", "create", name, "--json"]);
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
        .args(["feed", "edge-values"])
        .arg(input)
        .args(["--yaml", "--color", "always"])
        .output()
        .expect("feed JSON input with YAML receipt");
    let receipt = yaml_report(&feed);
    assert_eq!(receipt["seq"], 1);
    assert!(receipt["time"].is_string());

    let local_json = cli_at(
        &pool_dir,
        &["fetch", "edge-values", "1", "--json", "--color", "always"],
    );
    let local_json = json_report(&local_json);
    let local_yaml = cli_at(
        &pool_dir,
        &["fetch", "edge-values", "1", "--yaml", "--color", "always"],
    );
    let local_yaml = yaml_report(&local_yaml);
    assert_eq!(local_yaml, local_json);
    // Compare exported values: storage already converts unsigned integers above i64::MAX.
    assert_eq!(local_yaml["data"]["strings"], payload["strings"]);
    assert_eq!(local_yaml["data"]["i64_min"], i64::MIN);

    let server = ServeProcess::start(&pool_dir);
    let remote_ref = format!("{}/edge-values", server.local_url);
    let remote_json = cmd()
        .args(["fetch", &remote_ref, "1", "--json", "--color", "always"])
        .output()
        .expect("remote JSON fetch");
    let remote_json = json_report(&remote_json);
    let remote_yaml = cmd()
        .args(["fetch", &remote_ref, "1", "--yaml", "--color", "always"])
        .output()
        .expect("remote YAML fetch");
    let remote_yaml = yaml_report(&remote_yaml);
    assert_eq!(remote_yaml, remote_json);
    assert_eq!(remote_yaml, local_json);
}

#[test]
fn yaml_pool_reports_and_version_match_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "first");
    create_pool(&pool_dir, "second");

    for (yaml_args, json_args) in [
        (
            &["version", "--yaml", "--color", "always"][..],
            &["version", "--json"][..],
        ),
        (
            &["pool", "list", "--yaml", "--color", "always"][..],
            &["pool", "list", "--json"][..],
        ),
        (
            &["pool", "info", "first", "--yaml", "--color", "always"][..],
            &["pool", "info", "first", "--json"][..],
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
fn finite_follow_emits_marked_yaml_documents_and_data_only_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "history");
    for value in 1..=3 {
        let output = cmd()
            .arg("--dir")
            .arg(&pool_dir)
            .args(["feed", "history"])
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
            "--yaml",
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

    let values = cli_at(
        &pool_dir,
        &[
            "follow",
            "history",
            "--tail",
            "2",
            "--no-follow",
            "--data-only",
            "--yaml",
            "--color",
            "always",
        ],
    );
    assert_success(&values);
    assert_no_ansi(&values);
    assert_document_markers(&values.stdout, 2);
    assert_eq!(
        yaml_documents(&values.stdout),
        vec![
            serde_json::json!({"value": 2}),
            serde_json::json!({"value": 3})
        ]
    );
}

#[test]
fn yaml_changes_feed_output_without_accepting_yaml_input() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");
    create_pool(&pool_dir, "json-input");

    let valid = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args([
            "feed",
            "json-input",
            "{\"value\":1}",
            "--yaml",
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
            "value: 2",
            "--yaml",
            "--color",
            "always",
        ])
        .output()
        .expect("try YAML text as feed input");
    let error = yaml_error(&yaml_input);
    assert!(error["error"]["message"].is_string());

    let missing_second_message = cli_at(
        &pool_dir,
        &["fetch", "json-input", "2", "--yaml", "--color", "always"],
    );
    let error = yaml_error(&missing_second_message);
    assert_eq!(error["error"]["kind"], "NotFound");
}

#[test]
fn yaml_parse_errors_and_format_conflicts_use_yaml_errors_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let parse_error = cli_at(&pool_dir, &["version", "--yaml", "--unknown-option"]);
    assert_eq!(parse_error.status.code(), Some(2));
    assert_eq!(yaml_error(&parse_error)["error"]["kind"], "Usage");

    let conflicting_feed = cmd()
        .arg("--dir")
        .arg(&pool_dir)
        .args(["feed", "not-created", "{}", "--create", "--yaml", "--json"])
        .output()
        .expect("conflicting feed output flags");
    assert_eq!(conflicting_feed.status.code(), Some(2));
    assert_eq!(yaml_error(&conflicting_feed)["error"]["kind"], "Usage");
    assert!(
        !pool_dir.exists(),
        "conflicting flags must not create a pool"
    );

    create_pool(&pool_dir, "conflicts");
    for format in ["json", "lite3"] {
        let conflict = cli_at(
            &pool_dir,
            &["fetch", "conflicts", "1", "--yaml", "--format", format],
        );
        assert_eq!(conflict.status.code(), Some(2));
        assert_eq!(yaml_error(&conflict)["error"]["kind"], "Usage");
    }

    let jsonl_conflict = cli_at(
        &pool_dir,
        &[
            "follow",
            "conflicts",
            "--tail",
            "1",
            "--no-follow",
            "--yaml",
            "--jsonl",
        ],
    );
    assert_eq!(jsonl_conflict.status.code(), Some(2));
    assert_eq!(yaml_error(&jsonl_conflict)["error"]["kind"], "Usage");
}
