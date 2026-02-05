//! Purpose: End-to-end tests for the remote HTTP/JSON server/client.
//! Exports: None (integration test module).
//! Role: Validate remote append/get/tail and error propagation across TCP.
//! Invariants: Uses loopback-only server with temp pool directory.
//! Invariants: Bounded waits avoid test flakiness.
//! Invariants: Server processes are cleaned up on drop.

use plasmite::api::{Durability, ErrorKind, PoolOptions, PoolRef, RemoteClient, TailOptions};
use serde_json::{Value, json};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct TestServer {
    child: Child,
    base_url: String,
    token: Option<String>,
}

impl TestServer {
    fn start(pool_dir: &std::path::Path) -> TestResult<Self> {
        Self::start_with_token(pool_dir, None)
    }

    fn start_with_token(pool_dir: &std::path::Path, token: Option<&str>) -> TestResult<Self> {
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
            .stderr(Stdio::null());
        if let Some(token) = token {
            command.arg("--token").arg(token);
        }
        let child = command.spawn()?;

        wait_for_server(bind.parse()?)?;

        Ok(Self {
            child,
            base_url,
            token: token.map(str::to_string),
        })
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

fn pick_port() -> TestResult<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn wait_for_server(addr: SocketAddr) -> TestResult<()> {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err("server did not start in time".into());
        }
        sleep(Duration::from_millis(20));
    }
}
