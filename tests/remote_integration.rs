//! Purpose: End-to-end tests for the remote HTTP/JSON server/client.
//! Exports: None (integration test module).
//! Role: Validate remote append/get/tail and error propagation across TCP.
//! Invariants: Uses loopback-only server with temp pool directory.
//! Invariants: Bounded waits avoid test flakiness.
//! Invariants: Server processes are cleaned up on drop.

use plasmite::api::{Durability, ErrorKind, PoolOptions, PoolRef, RemoteClient, TailOptions};
use serde_json::json;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct TestServer {
    child: Child,
    base_url: String,
}

impl TestServer {
    fn start(pool_dir: &std::path::Path) -> TestResult<Self> {
        let port = pick_port()?;
        let bind = format!("127.0.0.1:{port}");
        let base_url = format!("http://{bind}");

        let child = Command::new(env!("CARGO_BIN_EXE_plasmite"))
            .arg("--dir")
            .arg(pool_dir)
            .arg("serve")
            .arg("--bind")
            .arg(&bind)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        wait_for_server(bind.parse()?)?;

        Ok(Self { child, base_url })
    }

    fn client(&self) -> TestResult<RemoteClient> {
        Ok(RemoteClient::new(self.base_url.clone())?)
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
        if start.elapsed() > Duration::from_secs(2) {
            return Err("server did not start in time".into());
        }
        sleep(Duration::from_millis(20));
    }
}
