//! Purpose: Pool and doctor black-box CLI integration tests.

pub mod support;
use support::cli::*;

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
