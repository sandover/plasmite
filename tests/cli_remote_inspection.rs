//! Black-box coverage for local and remote pool inspection and destination access.

pub mod support;
use plasmite::api::{Durability, LocalClient, PoolApiExt, PoolOptions, PoolRef};
use support::cli::*;

fn create_pool(directory: &Path, name: &str) {
    std::fs::create_dir_all(directory).expect("pool directory");
    let client = LocalClient::new().with_pool_dir(directory);
    client
        .create_pool(&PoolRef::name(name), PoolOptions::new(1024 * 1024))
        .expect("create pool");
    let mut pool = client.open_pool(&PoolRef::name(name)).expect("open pool");
    pool.append_json_now(&json!({"hello": "world"}), &[], Durability::Fast)
        .expect("append");
}

fn inspect(home: &Path, args: &[&str]) -> std::process::Output {
    cmd()
        .env("PLASMITE_ACCESS_HOME", home)
        .args(args)
        .output()
        .expect("inspection command")
}

fn report(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.contains(&0x1b),
        "structured output has color"
    );
    serde_json::from_slice(&output.stdout).expect("one JSON document")
}

fn connect(home: &Path, server: &ServeProcess) {
    let mut child = cmd()
        .env("PLASMITE_ACCESS_HOME", home)
        .args(["access", "connect", &server.remote_url, "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("connect");
    writeln!(
        child.stdin.take().expect("stdin"),
        "{}",
        server.access_key()
    )
    .expect("access key input");
    let output = child.wait_with_output().expect("connection result");
    report(&output);
}

#[test]
fn loopback_inspection_matches_local_reports() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    let home = temp.path().join("access");
    create_pool(&directory, "events.part");
    let server = ServeProcess::start(&directory);
    let local_info = report(&inspect(
        &home,
        &[
            "--dir",
            directory.to_str().unwrap(),
            "pool",
            "info",
            "events.part",
            "--json",
        ],
    ));
    let pool_url = format!("{}/events.part", server.local_url);
    let remote_info = report(&inspect(
        &home,
        &["pool", "info", &pool_url, "--json", "--color", "always"],
    ));
    for key in [
        "path",
        "file_size",
        "index_capacity",
        "index_size_bytes",
        "ring_offset",
        "ring_size",
        "bounds",
    ] {
        assert_eq!(local_info[key], remote_info[key], "info field {key}");
    }
    assert_eq!(remote_info["metrics"]["message_count"], 1);
    let local_list = report(&inspect(
        &home,
        &[
            "--dir",
            directory.to_str().unwrap(),
            "pool",
            "list",
            "--json",
        ],
    ));
    let remote_list = report(&inspect(
        &home,
        &[
            "pool",
            "list",
            &server.local_url,
            "--json",
            "--color",
            "always",
        ],
    ));
    for key in ["name", "path", "file_size", "bounds"] {
        assert_eq!(
            local_list["pools"][0][key], remote_list["pools"][0][key],
            "list field {key}"
        );
    }
    assert_eq!(remote_list["pools"][0]["name"], "events.part");
    assert!(remote_list["pools"][0]["mtime"].is_null());
    let localhost = server.local_url.replace("127.0.0.1", "localhost");
    let localhost_list = report(&inspect(&home, &["pool", "list", &localhost, "--json"]));
    assert_eq!(localhost_list["pools"], remote_list["pools"]);
    let human_info = inspect(&home, &["pool", "info", &pool_url]);
    assert!(human_info.status.success());
    assert!(
        String::from_utf8_lossy(&human_info.stdout).contains("Bounds: oldest=1 newest=1 count=1")
    );
    let human_list = inspect(&home, &["pool", "list", &server.local_url]);
    assert!(human_list.status.success());
    let text = String::from_utf8_lossy(&human_list.stdout);
    assert!(text.contains("NAME") && text.contains("events.part"));
}

#[test]
fn saved_connections_follow_each_server_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("access");
    let first_dir = temp.path().join("first");
    let second_dir = temp.path().join("second");
    create_pool(&first_dir, "alpha");
    create_pool(&second_dir, "beta");
    let first = ServeProcess::start(&first_dir);
    let second = ServeProcess::start(&second_dir);
    connect(&home, &first);
    let unsaved = inspect(&home, &["pool", "list", &second.remote_url, "--json"]);
    assert_eq!(unsaved.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&unsaved.stderr).expect("structured error");
    assert_eq!(error["error"]["kind"], "Permission");
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("access connect")
    );
    connect(&home, &second);
    for (server, name) in [(&first, "alpha"), (&second, "beta")] {
        let list = report(&inspect(
            &home,
            &["pool", "list", &server.remote_url, "--json"],
        ));
        assert_eq!(list["pools"][0]["name"], name);
        let pool_url = format!("{}/{name}", server.remote_url);
        let info = report(&inspect(&home, &["pool", "info", &pool_url, "--json"]));
        assert_eq!(info["bounds"]["newest"], 1);
        let fetched = report(&inspect(
            &home,
            &["fetch", &pool_url, "1", "--json", "--color", "always"],
        ));
        assert_eq!(fetched["seq"], 1);
        assert_eq!(fetched["data"], json!({"hello": "world"}));
        assert_eq!(fetched["meta"]["tags"], json!([]));
        assert!(fetched["time"].is_string());
        let human = inspect(&home, &["fetch", &pool_url, "1"]);
        assert!(human.status.success());
        let text = String::from_utf8_lossy(&human.stdout);
        assert!(text.contains("world"));
        assert!(
            text.lines().count() > 1,
            "human read should use readable presentation"
        );
    }
}

#[test]
fn inspection_rejects_unsafe_and_misshapen_targets() {
    let home = tempfile::tempdir().expect("tempdir");
    for target in [
        "http://example.com:9700",
        "ftp://localhost:9700",
        "https://localhost:9743/demo",
        "https://localhost:9743/v0/pools",
        "https://localhost:9743/?query=1",
        "https://localhost:9743/#fragment",
        "https://user:password@localhost:9743",
    ] {
        let output = inspect(home.path(), &["pool", "list", target, "--json"]);
        assert_eq!(output.status.code(), Some(2), "target={target}");
        let error: Value = serde_json::from_slice(&output.stderr).expect("structured error");
        assert_eq!(error["error"]["kind"], "Usage", "target={target}");
    }
    for target in [
        "http://example.com:9700/demo",
        "https://localhost:9743/demo/",
        "https://localhost:9743/v0/pools/demo",
        "https://localhost:9743/pools/demo",
        "https://localhost:9743/demo?query=1",
        "https://localhost:9743/demo#fragment",
        "https://localhost:9743/demo%2Fother",
        "https://user:password@localhost:9743/demo",
    ] {
        let output = inspect(home.path(), &["pool", "info", target, "--json"]);
        assert_eq!(output.status.code(), Some(2), "target={target}");
        let error: Value = serde_json::from_slice(&output.stderr).expect("structured error");
        assert_eq!(error["error"]["kind"], "Usage", "target={target}");
    }
}

#[test]
fn remote_missing_pool_preserves_server_error_and_local_mutations_reject_urls() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    let home = temp.path().join("access");
    create_pool(&directory, "existing");
    let server = ServeProcess::start(&directory);
    let pool_url = format!("{}/missing", server.local_url);
    let output = inspect(&home, &["pool", "info", &pool_url, "--json"]);
    assert_eq!(output.status.code(), Some(3));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured error");
    assert_eq!(error["error"]["kind"], "NotFound");
    assert!(
        !error["error"]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("--create")
    );
    for command in ["create", "delete"] {
        let output = inspect(
            &home,
            &[
                "--dir",
                directory.to_str().unwrap(),
                "pool",
                command,
                &pool_url,
                "--json",
            ],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(!directory.join("missing.plasmite").exists());
    }
}

#[test]
fn empty_remote_list_has_one_report_and_correct_human_guidance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("pools");
    std::fs::create_dir_all(&directory).expect("pool directory");
    let server = ServeProcess::start(&directory);
    let output = report(&inspect(
        temp.path(),
        &["pool", "list", &server.local_url, "--json"],
    ));
    assert_eq!(output, json!({"pools": []}));
    let human = cmd_tty(&["pool", "list", &server.local_url]);
    assert!(human.status.success());
    let text = sanitize_tty_text(&human.stdout);
    assert!(text.contains(&format!("No pools found at {}", server.local_url)));
    assert!(!text.contains("Create one:"));
}
