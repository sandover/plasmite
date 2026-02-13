//! Purpose: End-to-end tests for the remote HTTP/JSON server/client.
//! Exports: None (integration test module).
//! Role: Validate remote append/get/tail and error propagation across TCP.
//! Invariants: Uses loopback-only server with temp pool directory.
//! Invariants: Bounded waits avoid test flakiness.
//! Invariants: Server processes are cleaned up on drop.

use plasmite::api::{
    AppendOptions, Durability, ErrorKind, LocalClient, Pool, PoolApiExt, PoolOptions, PoolRef,
    RemoteClient, TailOptions,
};
use serde_json::{Value, json};
use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread::sleep;
use std::time::{Duration, Instant};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

static SERVER_LOCK: Mutex<()> = Mutex::new(());

struct TestServer {
    child: Child,
    base_url: String,
    token: Option<String>,
    _server_guard: MutexGuard<'static, ()>,
}

impl TestServer {
    fn start(pool_dir: &std::path::Path) -> TestResult<Self> {
        Self::start_with_options(pool_dir, None, None, &[])
    }

    fn start_with_token(pool_dir: &std::path::Path, token: Option<&str>) -> TestResult<Self> {
        Self::start_with_options(pool_dir, token, None, &[])
    }

    fn start_with_access(pool_dir: &std::path::Path, access: &str) -> TestResult<Self> {
        Self::start_with_options(pool_dir, None, Some(access), &[])
    }

    fn start_with_cors(pool_dir: &std::path::Path, cors_origins: &[&str]) -> TestResult<Self> {
        Self::start_with_options(pool_dir, None, None, cors_origins)
    }

    fn start_with_options(
        pool_dir: &std::path::Path,
        token: Option<&str>,
        access: Option<&str>,
        cors_origins: &[&str],
    ) -> TestResult<Self> {
        let guard = SERVER_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut last_err: Option<Box<dyn std::error::Error>> = None;
        for _attempt in 0..3 {
            let port = pick_port()?;
            let bind = format!("127.0.0.1:{port}");
            let base_url = format!("http://{bind}");

            let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
            command
                .arg("--dir")
                .arg(pool_dir)
                .arg("serve")
                .arg("--bind")
                .arg(&bind)
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            if let Some(access) = access {
                command.arg("--access").arg(access);
            }
            if let Some(token) = token {
                command.arg("--token").arg(token);
            }
            for origin in cors_origins {
                command.arg("--cors-origin").arg(origin);
            }
            let mut child = command.spawn()?;

            match wait_for_server(&mut child, bind.parse()?) {
                Ok(()) => {
                    return Ok(Self {
                        child,
                        base_url,
                        token: token.map(str::to_string),
                        _server_guard: guard,
                    });
                }
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    last_err = Some(err);
                    sleep(Duration::from_millis(30));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| "server failed to start".into()))
    }

    fn client(&self) -> TestResult<RemoteClient> {
        Ok(RemoteClient::new(self.base_url.clone())?)
    }

    fn client_with_token(&self) -> TestResult<RemoteClient> {
        let mut client = RemoteClient::new(self.base_url.clone())?;
        if let Some(token) = &self.token {
            client = client.with_token(token.clone());
        }
        Ok(client)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn remote_append_and_get() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("chat");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let payload = json!({"kind": "note", "body": "hello"});
    let message = pool.append_json_now(&payload, &[], Durability::Fast)?;

    let fetched = pool.get_message(message.seq)?;
    assert_eq!(fetched.seq, message.seq);
    assert_eq!(fetched.data, payload);
    Ok(())
}

#[test]
fn remote_append_get_tail_lite3() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("lite3");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let message = pool.append_json_now(&json!({"x": 1}), &[], Durability::Fast)?;
    let payload = match pool.get_lite3(message.seq) {
        Ok(payload) => payload,
        Err(err) => return Err(format!("get_lite3 failed: {err}").into()),
    };

    let seq = match pool.append_lite3_now(&payload, Durability::Fast) {
        Ok(seq) => seq,
        Err(err) => return Err(format!("append_lite3_now failed: {err}").into()),
    };

    let fetched = match pool.get_lite3(seq) {
        Ok(payload) => payload,
        Err(err) => return Err(format!("get_lite3 failed: {err}").into()),
    };
    assert_eq!(fetched, payload);

    let options = TailOptions {
        since_seq: Some(seq),
        max_messages: Some(1),
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    };
    let mut tail = match pool.tail_lite3(options) {
        Ok(tail) => tail,
        Err(err) => return Err(format!("tail_lite3 failed: {err}").into()),
    };
    let frame = match tail.next_frame() {
        Ok(Some(frame)) => frame,
        Ok(None) => return Err("tail_lite3 returned no frame".into()),
        Err(err) => return Err(format!("tail_lite3 next_frame failed: {err}").into()),
    };
    assert_eq!(frame.seq, seq);
    assert_eq!(frame.payload, payload);
    Ok(())
}

#[test]
fn remote_lite3_invalid_payloads_error() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let pool_dir = temp_dir.path();
    let pool_path = pool_dir.join("bad-lite3.plasmite");
    let mut raw_pool = Pool::create(&pool_path, PoolOptions::new(1024 * 1024))?;
    raw_pool.append_with_options(&[0x01], AppendOptions::new(123, Durability::Fast))?;
    drop(raw_pool);

    let server = TestServer::start(pool_dir)?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("bad-lite3");
    let pool = client.open_pool(&pool_ref)?;

    let err = pool.get_lite3(1).expect_err("invalid lite3 get");
    assert_eq!(err.kind(), ErrorKind::Corrupt);

    let err = pool
        .append_lite3_now(&[0x01], Durability::Fast)
        .expect_err("invalid lite3 append");
    assert_eq!(err.kind(), ErrorKind::Corrupt);

    let options = TailOptions {
        since_seq: Some(1),
        max_messages: Some(1),
        timeout: Some(Duration::from_millis(200)),
        ..TailOptions::default()
    };
    let err = match pool.tail_lite3(options) {
        Ok(_) => return Err("expected invalid lite3 tail".into()),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    Ok(())
}

#[test]
fn remote_tail_streams_in_order() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let first = pool.append_json_now(&json!({"n": 1}), &[], Durability::Fast)?;
    let second = pool.append_json_now(&json!({"n": 2}), &[], Durability::Fast)?;

    let options = TailOptions {
        since_seq: Some(first.seq),
        max_messages: Some(2),
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    };
    let mut tail = pool.tail(options)?;

    let msg1 = tail.next_message()?.expect("first message");
    let msg2 = tail.next_message()?.expect("second message");
    assert_eq!(msg1.seq, first.seq);
    assert_eq!(msg2.seq, second.seq);
    Ok(())
}

#[test]
fn remote_errors_propagate_kind() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let err = match client.open_pool(&PoolRef::name("missing")) {
        Ok(_) => return Err("expected missing pool error".into()),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::NotFound);
    Ok(())
}

#[test]
fn remote_auth_requires_valid_token() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start_with_token(temp_dir.path(), Some("secret"))?;

    let missing = server.client()?;
    let err = missing.list_pools().expect_err("missing token");
    assert_eq!(err.kind(), ErrorKind::Permission);

    let invalid = RemoteClient::new(server.base_url.clone())?.with_token("bad");
    let err = invalid.list_pools().expect_err("invalid token");
    assert_eq!(err.kind(), ErrorKind::Permission);

    let client = server.client_with_token()?;
    client.create_pool(&PoolRef::name("alpha"), PoolOptions::new(1024 * 1024))?;
    let pools = client.list_pools()?;
    assert!(pools.iter().any(|pool| {
        pool.path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "alpha.plasmite")
    }));
    Ok(())
}

#[test]
fn remote_rejects_path_pool_names() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;

    let create_url = format!("{}/v0/pools", server.base_url);
    let create_body = r#"{"pool":"/tmp/evil","size_bytes":1024}"#;
    match ureq::post(&create_url)
        .set("Content-Type", "application/json")
        .send_string(create_body)
    {
        Ok(_) => return Err("expected create to fail with Usage error".into()),
        Err(ureq::Error::Status(code, resp)) => {
            assert_eq!(code, 400);
            let body = resp.into_string()?;
            let value: Value = serde_json::from_str(&body)?;
            assert_eq!(value["error"]["kind"], "Usage");
        }
        Err(err) => return Err(err.into()),
    }

    let open_url = format!("{}/v0/pools/open", server.base_url);
    let open_body = r#"{"pool":"/tmp/evil"}"#;
    match ureq::post(&open_url)
        .set("Content-Type", "application/json")
        .send_string(open_body)
    {
        Ok(_) => return Err("expected open to fail with Usage error".into()),
        Err(ureq::Error::Status(code, resp)) => {
            assert_eq!(code, 400);
            let body = resp.into_string()?;
            let value: Value = serde_json::from_str(&body)?;
            assert_eq!(value["error"]["kind"], "Usage");
        }
        Err(err) => return Err(err.into()),
    }

    Ok(())
}

#[test]
fn remote_list_delete_and_info() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("info");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let info = client.pool_info(&pool_ref)?;
    assert!(info.file_size >= 1024 * 1024);

    let pools = client.list_pools()?;
    assert!(
        pools
            .iter()
            .any(|pool| pool.path.ends_with("info.plasmite"))
    );

    client.delete_pool(&pool_ref)?;
    let pools = client.list_pools()?;
    assert!(
        !pools
            .iter()
            .any(|pool| pool.path.ends_with("info.plasmite"))
    );
    Ok(())
}

#[test]
fn remote_corrupt_errors() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let pool_dir = temp_dir.path();
    let server = TestServer::start(pool_dir)?;
    let client = server.client()?;

    let corrupt_path = pool_dir.join("bad.plasmite");
    std::fs::write(&corrupt_path, b"NOPE")?;
    let err = match client.open_pool(&PoolRef::name("bad")) {
        Ok(_) => return Err("expected corrupt pool error".into()),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    Ok(())
}

#[test]
fn remote_tail_respects_limits_and_timeouts() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-limits");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let first = pool.append_json_now(&json!({"n": 1}), &[], Durability::Fast)?;
    let _second = pool.append_json_now(&json!({"n": 2}), &[], Durability::Fast)?;

    let options = TailOptions {
        since_seq: Some(first.seq),
        max_messages: Some(1),
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    };
    let mut tail = pool.tail(options)?;
    let msg = tail.next_message()?.expect("first message");
    assert_eq!(msg.seq, first.seq);
    assert!(tail.next_message()?.is_none());

    let mut tail = pool.tail(TailOptions {
        since_seq: Some(9999),
        timeout: Some(Duration::from_millis(100)),
        ..TailOptions::default()
    })?;
    assert!(tail.next_message()?.is_none());

    let _third = pool.append_json_now(&json!({"n": 3}), &[], Durability::Fast)?;
    let mut tail = pool.tail(TailOptions {
        since_seq: Some(3),
        max_messages: Some(1),
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    })?;
    let msg = tail.next_message()?.expect("resumed message");
    assert_eq!(msg.data, json!({"n": 3}));
    Ok(())
}

#[test]
fn remote_tail_filters_by_tags() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-tags");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let first = pool.append_json_now(
        &json!({"kind": "drop"}),
        &["drop".to_string()],
        Durability::Fast,
    )?;
    let second = pool.append_json_now(
        &json!({"kind": "keep"}),
        &["keep".to_string()],
        Durability::Fast,
    )?;

    let options = TailOptions {
        since_seq: Some(first.seq),
        max_messages: Some(1),
        tags: vec!["keep".to_string()],
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    };
    let mut tail = pool.tail(options)?;
    let msg = tail.next_message()?.expect("filtered message");
    assert_eq!(msg.seq, second.seq);
    assert_eq!(msg.data, json!({"kind": "keep"}));
    Ok(())
}

#[test]
fn remote_tail_filters_by_tags_with_commas() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-tags-commas");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let first = pool.append_json_now(
        &json!({"kind": "drop"}),
        &["keep".to_string()],
        Durability::Fast,
    )?;
    let second = pool.append_json_now(
        &json!({"kind": "keep"}),
        &["keep,prod".to_string()],
        Durability::Fast,
    )?;

    let options = TailOptions {
        since_seq: Some(first.seq),
        max_messages: Some(1),
        tags: vec!["keep,prod".to_string()],
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    };
    let mut tail = pool.tail(options)?;
    let msg = tail.next_message()?.expect("filtered message");
    assert_eq!(msg.seq, second.seq);
    assert_eq!(msg.data, json!({"kind": "keep"}));
    Ok(())
}

#[test]
fn remote_ui_routes_serve_single_page_html() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start(temp_dir.path())?;

    let ui = ureq::get(&format!("{}/ui", server.base_url))
        .call()
        .expect("ui route");
    assert_eq!(ui.status(), 200);
    assert!(
        ui.header("content-type")
            .unwrap_or_default()
            .starts_with("text/html")
    );
    let body = ui.into_string()?;
    assert!(body.contains("Plasmite UI"));

    let pool_view = ureq::get(&format!("{}/ui/pools/demo", server.base_url))
        .call()
        .expect("pool ui route");
    assert_eq!(pool_view.status(), 200);
    assert!(
        pool_view
            .header("content-type")
            .unwrap_or_default()
            .starts_with("text/html")
    );
    Ok(())
}

#[test]
fn remote_ui_events_stream_sends_sse_and_requires_auth() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::start_with_token(temp_dir.path(), Some("secret"))?;
    let client = server.client_with_token()?;
    let pool_ref = PoolRef::name("ui-events");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let created =
        pool.append_json_now(&json!({"kind": "ui", "ok": true}), &[], Durability::Fast)?;

    match ureq::get(&format!(
        "{}/v0/ui/pools/ui-events/events?since_seq={}&max=1",
        server.base_url, created.seq
    ))
    .call()
    {
        Ok(_) => return Err("expected unauthorized SSE request to fail".into()),
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 401),
        Err(err) => return Err(err.into()),
    }

    let response = ureq::get(&format!(
        "{}/v0/ui/pools/ui-events/events?since_seq={}&max=1",
        server.base_url, created.seq
    ))
    .set("Authorization", "Bearer secret")
    .call()
    .expect("authorized sse request");
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    let body = response.into_string()?;
    assert!(body.contains("event: message"));
    assert!(body.contains("\"seq\":1"));
    Ok(())
}

#[test]
fn remote_ui_routes_emit_cors_headers_for_allowed_origin() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let origin = "https://demo.wratify.ai";
    let server = TestServer::start_with_cors(temp_dir.path(), &[origin])?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("cors-allowed");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let created =
        pool.append_json_now(&json!({"kind": "cors", "ok": true}), &[], Durability::Fast)?;

    let pools_resp = ureq::get(&format!("{}/v0/ui/pools", server.base_url))
        .set("Origin", origin)
        .call()
        .expect("pools request");
    assert_eq!(pools_resp.status(), 200);
    assert_eq!(
        pools_resp.header("access-control-allow-origin"),
        Some(origin)
    );

    let preflight_resp = ureq::request(
        "OPTIONS",
        &format!("{}/v0/ui/pools/cors-allowed/events", server.base_url),
    )
    .set("Origin", origin)
    .set("Access-Control-Request-Method", "GET")
    .call()
    .expect("preflight request");
    assert!(matches!(preflight_resp.status(), 200 | 204));
    assert_eq!(
        preflight_resp.header("access-control-allow-origin"),
        Some(origin)
    );

    let events_resp = ureq::get(&format!(
        "{}/v0/ui/pools/cors-allowed/events?since_seq={}&max=1",
        server.base_url, created.seq
    ))
    .set("Origin", origin)
    .call()
    .expect("events request");
    assert_eq!(events_resp.status(), 200);
    assert_eq!(
        events_resp.header("access-control-allow-origin"),
        Some(origin)
    );
    let body = events_resp.into_string()?;
    assert!(body.contains("event: message"));
    Ok(())
}

#[test]
fn remote_ui_routes_reject_disallowed_preflight_origin() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let allowed_origin = "https://demo.wratify.ai";
    let disallowed_origin = "https://evil.example";
    let server = TestServer::start_with_cors(temp_dir.path(), &[allowed_origin])?;

    let pools_resp = ureq::get(&format!("{}/v0/ui/pools", server.base_url))
        .set("Origin", disallowed_origin)
        .call()
        .expect("pools request");
    assert_eq!(pools_resp.status(), 200);
    assert_ne!(
        pools_resp.header("access-control-allow-origin"),
        Some(disallowed_origin)
    );

    let preflight = ureq::request(
        "OPTIONS",
        &format!("{}/v0/ui/pools/demo/events", server.base_url),
    )
    .set("Origin", disallowed_origin)
    .set("Access-Control-Request-Method", "GET")
    .call();

    match preflight {
        Ok(resp) => {
            assert!(matches!(resp.status(), 200 | 204));
            assert_ne!(
                resp.header("access-control-allow-origin"),
                Some(disallowed_origin)
            );
        }
        Err(ureq::Error::Status(code, resp)) => {
            assert_eq!(code, 403);
            assert_ne!(
                resp.header("access-control-allow-origin"),
                Some(disallowed_origin)
            );
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

#[test]
fn remote_read_only_allows_reads_but_rejects_writes() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let pool_dir = temp_dir.path();
    let local = LocalClient::new().with_pool_dir(pool_dir);
    let pool_ref = PoolRef::name("ro-demo");
    local.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let mut pool = local.open_pool(&pool_ref)?;
    let created = pool.append_json_now(&json!({"n": 1}), &[], Durability::Fast)?;

    let server = TestServer::start_with_access(pool_dir, "read-only")?;
    let client = server.client()?;
    let remote_pool = client.open_pool(&pool_ref)?;
    let fetched = remote_pool.get_message(created.seq)?;
    assert_eq!(fetched.seq, created.seq);

    let err = remote_pool
        .append_json_now(&json!({"n": 2}), &[], Durability::Fast)
        .expect_err("append should be forbidden");
    assert_eq!(err.kind(), ErrorKind::Permission);
    Ok(())
}

#[test]
fn remote_write_only_allows_append_but_rejects_reads() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let pool_dir = temp_dir.path();
    let local = LocalClient::new().with_pool_dir(pool_dir);
    let pool_ref = PoolRef::name("wo-demo");
    local.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;

    let server = TestServer::start_with_access(pool_dir, "write-only")?;
    let append_url = format!("{}/v0/pools/wo-demo/append", server.base_url);
    let append_body = r#"{"data":{"n":1},"tags":[],"durability":"fast"}"#;
    let append = ureq::post(&append_url)
        .set("Content-Type", "application/json")
        .send_string(append_body)
        .expect("append");
    assert_eq!(append.status(), 200);

    let get_url = format!("{}/v0/pools/wo-demo/messages/1", server.base_url);
    match ureq::get(&get_url).call() {
        Ok(_) => return Err("expected get to be forbidden".into()),
        Err(ureq::Error::Status(code, resp)) => {
            assert_eq!(code, 403);
            let body: Value = serde_json::from_str(&resp.into_string()?)?;
            assert_eq!(body["error"]["kind"], "Permission");
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn pick_port() -> TestResult<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn wait_for_server(child: &mut Child, addr: SocketAddr) -> TestResult<()> {
    // Use healthz endpoint - it's not subject to access control and works for all modes
    let url = format!("http://{addr}/healthz");
    let start = Instant::now();
    loop {
        if let Ok(resp) = ureq::get(&url).call() {
            if resp.status() == 200 {
                return Ok(());
            }
        }
        if let Some(status) = child.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let detail = stderr.trim();
            return Err(format!(
                "server exited before ready (status: {status}, stderr: {})",
                if detail.is_empty() { "<empty>" } else { detail }
            )
            .into());
        }
        if start.elapsed() > Duration::from_secs(8) {
            return Err("server did not start in time".into());
        }
        sleep(Duration::from_millis(20));
    }
}
