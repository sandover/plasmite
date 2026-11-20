//! Purpose: End-to-end tests for authenticated remote HTTPS server/client behavior.
//! Exports: None (integration test module).
//! Role: Validate remote append/get/tail and error propagation across TCP.
//! Invariants: Uses loopback-only server with temp pool directory.
//! Invariants: Bounded waits avoid test flakiness.
//! Invariants: Server processes are cleaned up on drop.

use plasmite::api::{
    AppendOptions, Durability, ErrorKind, Feed, Pool, PoolOptions, PoolRef, Read as ReadOptions,
    RemoteClient, RemotePool, Since, access,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub mod support;
use support::server::TestServer;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn fetch_message(
    pool: &RemotePool,
    seq: u64,
) -> Result<plasmite::api::Message, Box<plasmite::api::Error>> {
    pool.fetch(ReadOptions::since(Since::Seq(seq)))?
        .next_message()?
        .ok_or_else(|| Box::new(plasmite::api::Error::new(ErrorKind::NotFound).with_seq(seq)))
}

fn fetch_frame(
    pool: &RemotePool,
    seq: u64,
) -> Result<plasmite::api::RemoteLite3Frame, Box<plasmite::api::Error>> {
    pool.fetch_lite3(ReadOptions::since(Since::Seq(seq)))?
        .next_frame()?
        .ok_or_else(|| Box::new(plasmite::api::Error::new(ErrorKind::NotFound).with_seq(seq)))
}

#[test]
fn remote_append_and_get() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("chat");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let payload = json!({"kind": "note", "body": "hello"});
    let message = pool.feed(&payload, Feed::default())?;

    let fetched = fetch_message(&pool, message.seq)?;
    assert_eq!(fetched.seq, message.seq);
    assert_eq!(fetched.data, payload);
    Ok(())
}

#[test]
fn remote_delete_sends_no_body_and_reuses_connection() -> TestResult<()> {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let client = RemoteClient::new(format!("http://{}", listener.local_addr()?))?;
    let (done_tx, done_rx) = mpsc::channel();
    let server = std::thread::spawn(move || -> std::io::Result<Vec<usize>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline || done_rx.try_recv().is_ok() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "client did not connect to the wire server",
                        ));
                    }
                    sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        };
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut body_lengths = Vec::new();
        for (request_line, response) in [
            ("DELETE /v2/pools/shared HTTP/1.1", "{\"ok\":true}"),
            ("GET /v2/pools HTTP/1.1", "{\"pools\":[]}"),
        ] {
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                assert!(headers.len() < 8192, "request headers exceed test limit");
                let mut byte = [0];
                socket.read_exact(&mut byte)?;
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).expect("UTF-8 request headers");
            assert_eq!(headers.lines().next(), Some(request_line));
            let headers = headers.to_ascii_lowercase();
            assert!(!headers.contains("transfer-encoding:"));
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|value| value.trim().parse::<usize>().expect("content length"))
                .unwrap_or(0);
            assert!(length <= 1024, "request body exceeds test limit");
            // Drain any unexpected body so the final framing assertion fails
            // deterministically instead of racing a socket close.
            socket.read_exact(&mut vec![0; length])?;
            body_lengths.push(length);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response}",
                response.len()
            )?;
            socket.flush()?;
        }
        // Keep the connection open until the client consumes the last response.
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(std::io::Error::other)?;
        Ok(body_lengths)
    });

    let result: TestResult<bool> = (|| {
        client.delete_pool(&PoolRef::name("shared"))?;
        Ok(client.list_pools()?.is_empty())
    })();
    let _ = done_tx.send(());
    let body_lengths = server.join().expect("wire server")?;
    assert!(result?);
    assert_eq!(body_lengths, [0, 0], "DELETE and GET must send no body");
    Ok(())
}

#[test]
fn remote_append_get_follow_lite3() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("lite3");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let message = pool.feed(&json!({"x": 1}), Feed::default())?;
    let payload = match fetch_frame(&pool, message.seq).map(|frame| frame.payload) {
        Ok(payload) => payload,
        Err(err) => return Err(format!("get_lite3 failed: {err}").into()),
    };

    let seq = match pool
        .feed_lite3_receipt(&payload, Feed::default())
        .map(|receipt| receipt.seq)
    {
        Ok(seq) => seq,
        Err(err) => return Err(format!("append_lite3_now failed: {err}").into()),
    };

    let fetched = match fetch_frame(&pool, seq).map(|frame| frame.payload) {
        Ok(payload) => payload,
        Err(err) => return Err(format!("get_lite3 failed: {err}").into()),
    };
    assert_eq!(fetched, payload);
    let frame = fetch_frame(&pool, seq)?;
    assert_eq!(frame.seq, seq);
    assert_eq!(frame.payload, payload);
    assert_eq!(frame.time, fetch_message(&pool, seq)?.time);

    let options = ReadOptions::since(Since::Seq(seq));
    let mut tail = match pool.follow_lite3(options) {
        Ok(tail) => tail,
        Err(err) => return Err(format!("follow_lite3 failed: {err}").into()),
    };
    let frame = match tail.next_frame() {
        Ok(Some(frame)) => frame,
        Ok(None) => return Err("follow_lite3 returned no frame".into()),
        Err(err) => return Err(format!("follow_lite3 next_frame failed: {err}").into()),
    };
    assert_eq!(frame.seq, seq);
    assert_eq!(frame.payload, payload);
    Ok(())
}

#[test]
fn remote_lite3_receipt_preserves_payload_and_rejects_unreadable_data() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let client = server.client()?;
    let reference = PoolRef::name("binary-receipt");
    client.create_pool(&reference, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&reference, false)?;
    let tags = vec!["heartbeat".to_string()];
    let seed = pool.feed(&json!({"x": "y"}), Feed::new().tags(tags.clone()))?;
    let payload = fetch_frame(&pool, seed.seq).map(|frame| frame.payload)?;
    let copied = pool.feed_lite3(&payload, Feed::new())?;
    assert_eq!(copied.seq, seed.seq + 1);
    assert_eq!(copied.meta.tags, tags);
    assert_eq!(copied.data, seed.data);
    assert_eq!(
        fetch_frame(&pool, copied.seq).map(|frame| frame.payload)?,
        payload
    );
    let receipt = pool.feed_lite3_receipt(&payload, Feed::new())?;
    assert_eq!(receipt.seq, copied.seq + 1);
    let frame = pool
        .fetch_lite3(ReadOptions::since(Since::Seq(receipt.seq)))?
        .next_frame()?
        .expect("committed receipt must name a frame");
    assert_eq!(frame.time, receipt.time);
    assert_eq!(frame.payload, payload);

    let doc = plasmite::api::Lite3DocRef::new(&payload);
    let data = doc.key_offset("data")?;
    let value = doc.key_offset_at(data, "x")?;
    let mut invalid = payload.clone();
    assert_eq!(invalid[value + 5], b'y');
    invalid[value + 5] = 0xff;
    let before = pool.info()?.bounds;
    let err = pool
        .feed_lite3_receipt(&invalid, Feed::default())
        .map(|receipt| receipt.seq)
        .expect_err("nested invalid UTF-8 must not append");
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    assert_eq!(pool.info()?.bounds, before);
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

    let server = TestServer::try_start(pool_dir)?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("bad-lite3");
    let pool = client.open_pool(&pool_ref, false)?;

    let err = fetch_frame(&pool, 1)
        .map(|frame| frame.payload)
        .expect_err("invalid lite3 get");
    assert_eq!(err.kind(), ErrorKind::Corrupt);

    let err = pool
        .feed_lite3_receipt(&[0x01], Feed::default())
        .map(|receipt| receipt.seq)
        .expect_err("invalid lite3 append");
    assert_eq!(err.kind(), ErrorKind::Corrupt);

    let options = ReadOptions::since(Since::Seq(1));
    let err = pool
        .follow_lite3(options)?
        .next_frame()
        .expect_err("invalid lite3 tail");
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    Ok(())
}

#[test]
fn remote_tail_streams_in_order() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let first = pool.feed(&json!({"n": 1}), Feed::default())?;
    let second = pool.feed(&json!({"n": 2}), Feed::default())?;

    let options = ReadOptions::since(Since::Seq(first.seq));
    let mut tail = pool.follow(options)?;

    let msg1 = tail.next_message()?.expect("first message");
    let msg2 = tail.next_message()?.expect("second message");
    assert_eq!(msg1.seq, first.seq);
    assert_eq!(msg2.seq, second.seq);
    Ok(())
}

#[test]
fn remote_tail_reconnects_with_stable_since_seq_without_duplicates() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-reconnect");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;

    let first = pool.feed(&json!({"seq": 1}), Feed::default())?;
    let second = pool.feed(&json!({"seq": 2}), Feed::default())?;

    let mut first_tail = pool.fetch(ReadOptions::since(Since::Oldest))?;
    let message_one = first_tail.next_message()?.expect("first replay message");
    let message_two = first_tail.next_message()?.expect("second replay message");
    assert_eq!(message_one.seq, first.seq);
    assert_eq!(message_two.seq, second.seq);
    assert!(
        first_tail.next_message()?.is_none(),
        "replay tail should stop after max_messages"
    );

    let third = pool.feed(&json!({"seq": 3}), Feed::default())?;
    let fourth = pool.feed(&json!({"seq": 4}), Feed::default())?;

    let mut reconnect_tail = pool.fetch(ReadOptions::since(Since::Seq(message_two.seq + 1)))?;
    let resume_three = reconnect_tail
        .next_message()?
        .expect("first resumed message");
    let resume_four = reconnect_tail
        .next_message()?
        .expect("second resumed message");
    assert_eq!(resume_three.seq, third.seq);
    assert_eq!(resume_four.seq, fourth.seq);
    assert_ne!(resume_three.seq, message_one.seq);
    assert!(
        reconnect_tail.next_message()?.is_none(),
        "resumed tail should stop after max_messages"
    );
    Ok(())
}

#[test]
fn remote_tail_cancel_under_active_writes_is_prompt() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let base_client = server.client()?;
    let pool_ref = PoolRef::name("tail-cancel-active");

    base_client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;

    let append_client = base_client.clone();
    let writer_pool = append_client.open_pool(&pool_ref, false)?;
    let tail_pool = base_client.open_pool(&pool_ref, false)?;

    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));

    let (tail_started_tx, tail_started_rx) = mpsc::channel::<()>();
    let (first_message_tx, first_message_rx) = mpsc::channel::<()>();

    let done_writer = Arc::clone(&done);
    let writer = std::thread::spawn(move || -> Result<(), String> {
        let mut seq = 0u64;
        while !done_writer.load(Ordering::Acquire) {
            let payload = json!({"seq": seq});
            writer_pool
                .feed(&payload, Feed::default())
                .map(|_| ())
                .map_err(|err| err.to_string())?;
            seq += 1;
            if seq > 1_000 {
                break;
            }
            sleep(Duration::from_millis(10));
        }
        Ok(())
    });

    let cancel_tail = Arc::clone(&cancel);
    let done_tail = Arc::clone(&done);
    let reader = std::thread::spawn(move || -> Result<usize, String> {
        // No timeout: a tail's timeout is a deadline from its start, so a slow first
        // append would end it empty. The writer appends every 10 ms, so each read
        // returns soon and the loop checks for cancellation.
        let mut tail = tail_pool
            .follow(ReadOptions::since(Since::Oldest))
            .map_err(|err| err.to_string())?;
        tail_started_tx
            .send(())
            .map_err(|err| format!("failed to signal tail start: {err}"))?;
        let mut observed = 0usize;
        loop {
            if cancel_tail.load(Ordering::Acquire) {
                tail.close();
                return Ok(observed);
            }
            if tail
                .next_message()
                .map_err(|err| err.to_string())?
                .is_some()
            {
                if observed == 0 {
                    first_message_tx
                        .send(())
                        .map_err(|err| format!("failed to signal first message: {err}"))?;
                }
                observed += 1;
            }
            if observed > 2_000 || done_tail.load(Ordering::Acquire) {
                return Ok(observed);
            }
        }
    });

    tail_started_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|err| format!("tail failed to start: {err}"))?;
    first_message_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|err| format!("tail did not observe a message before cancel: {err}"))?;

    let start = Instant::now();
    cancel.store(true, Ordering::Release);
    let observed = reader
        .join()
        .map_err(|_| std::io::Error::other("reader thread panicked"))?
        .map_err(std::io::Error::other)?;
    done.store(true, Ordering::Release);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "cancellation path was unexpectedly slow"
    );

    assert!(
        observed > 0,
        "expected at least one message before cancellation"
    );
    writer
        .join()
        .map_err(|_| std::io::Error::other("writer thread panicked"))?
        .map_err(std::io::Error::other)?;
    Ok(())
}

#[test]
fn remote_errors_propagate_kind() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let err = match client.open_pool(&PoolRef::name("missing"), false) {
        Ok(_) => return Err("expected missing pool error".into()),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::NotFound);
    Ok(())
}

#[test]
fn remote_auth_requires_a_valid_named_access_key() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;

    let missing = access::status(&server.remote_url)?;
    assert!(!missing.credentials_saved);
    assert_eq!(missing.reachable, Some(true));
    assert_eq!(missing.accepted, None);

    let key_fields: Vec<_> = server.access_key().split('.').collect();
    let wrong_secret = if key_fields[2] == "0".repeat(64) {
        "1".repeat(64)
    } else {
        "0".repeat(64)
    };
    let invalid_key = format!("{}.{}.{}", key_fields[0], key_fields[1], wrong_secret);
    let invalid = RemoteClient::with_access_key(server.remote_url.clone(), &invalid_key)?;
    let err = invalid.list_pools().expect_err("invalid access key");
    assert_eq!(err.kind(), ErrorKind::Permission);

    let client = server.client()?;
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
    let server = TestServer::try_start(temp_dir.path())?;

    let create_url = format!("{}/v2/pools", server.local_url);
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

    let open_url = format!("{}/v2/pools/%2Ftmp%2Fevil/open", server.local_url);
    let open_body = r#"{"create":true}"#;
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
    let server = TestServer::try_start(temp_dir.path())?;
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
    let server = TestServer::try_start(pool_dir)?;
    let client = server.client()?;

    let corrupt_path = pool_dir.join("bad.plasmite");
    std::fs::write(&corrupt_path, b"NOPE")?;
    let err = match client.open_pool(&PoolRef::name("bad"), false) {
        Ok(_) => return Err("expected corrupt pool error".into()),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::Corrupt);
    Ok(())
}

#[test]
fn remote_json_tail_can_fail_closed_on_retention_gaps() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-gap");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let first = pool.feed(&json!({"kind": "first"}), Feed::default())?;
    for i in 0..10_000 {
        pool.feed(
            &json!({"i": i, "padding": "x".repeat(64 * 1024)}),
            Feed::default(),
        )?;
        match fetch_message(&pool, first.seq) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::RetentionGap => break,
            Err(err) => return Err(err.into()),
        }
        if i == 9_999 {
            return Err("first sequence remained retained after filling pool".into());
        }
    }

    let mut failing = pool.follow(ReadOptions::since(Since::Seq(first.seq)).tags(["never"]))?;
    let err = failing
        .next_message()
        .expect_err("expected remote retention gap");
    assert_eq!(err.kind(), ErrorKind::RetentionGap);
    assert_eq!(err.seq(), Some(first.seq));
    let repeated = failing
        .next_message()
        .expect_err("the failure remains visible");
    assert_eq!(repeated.kind(), ErrorKind::RetentionGap);
    assert_eq!(repeated.missing(), err.missing());
    assert_eq!(repeated.next_seq(), err.next_seq());
    Ok(())
}

#[test]
fn remote_tail_filters_by_tags() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-tags");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let first = pool.feed(
        &json!({"kind": "drop"}),
        Feed::default().tags(["drop".to_string()]),
    )?;
    let second = pool.feed(
        &json!({"kind": "keep"}),
        Feed::default().tags(["keep".to_string()]),
    )?;

    let options = ReadOptions::since(Since::Seq(first.seq)).tags(["keep"]);
    let mut tail = pool.follow(options)?;
    let msg = tail.next_message()?.expect("filtered message");
    assert_eq!(msg.seq, second.seq);
    assert_eq!(msg.data, json!({"kind": "keep"}));
    Ok(())
}

#[test]
fn remote_tail_filters_by_tags_with_commas() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-tags-commas");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let first = pool.feed(
        &json!({"kind": "drop"}),
        Feed::default().tags(["keep".to_string()]),
    )?;
    let second = pool.feed(
        &json!({"kind": "keep"}),
        Feed::default().tags(["keep,prod".to_string()]),
    )?;

    let options = ReadOptions::since(Since::Seq(first.seq)).tags(["keep,prod"]);
    let mut tail = pool.follow(options)?;
    let msg = tail.next_message()?.expect("filtered message");
    assert_eq!(msg.seq, second.seq);
    assert_eq!(msg.data, json!({"kind": "keep"}));
    Ok(())
}

#[test]
fn remote_ui_routes_serve_single_page_html() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;

    let ui = ureq::get(&format!("{}/ui", server.local_url))
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
    assert!(body.contains("href=\"/ui/map\""));

    let map = ureq::get(&format!("{}/ui/map", server.local_url))
        .call()
        .expect("map route");
    assert_eq!(map.status(), 200);
    assert!(
        map.header("content-security-policy")
            .unwrap_or_default()
            .contains("font-src 'self'")
    );
    assert!(map.into_string()?.contains("Plasmite Map"));

    let font = ureq::get(&format!("{}/ui/assets/inconsolata.woff2", server.local_url))
        .call()
        .expect("font asset");
    assert_eq!(font.status(), 200);
    assert_eq!(font.header("content-type"), Some("font/woff2"));
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut font.into_reader(), &mut bytes)?;
    assert!(bytes.starts_with(b"wOF2"));

    let common = ureq::get(&format!("{}/ui/assets/common.js", server.local_url))
        .call()
        .expect("shared UI script");
    assert_eq!(common.status(), 200);
    assert_eq!(
        common.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert!(common.into_string()?.contains("showDirectory"));

    let pool_view = ureq::get(&format!("{}/ui/pools/demo", server.local_url))
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
fn local_ui_pool_list_reports_ring_layout() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("ring");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    client.create_pool(&PoolRef::name("empty"), PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    pool.feed(&json!({"body": "short"}), Feed::default())?;
    pool.feed(&json!({"body": "x".repeat(2000)}), Feed::default())?;

    let body: Value = ureq::get(&format!("{}/v0/ui/pools", server.local_url))
        .call()
        .expect("ui pool list")
        .into_json()?;
    let pools = body["pools"].as_array().expect("pools array");
    let find = |name: &str| pools.iter().find(|pool| pool["name"] == name).expect(name);

    let ring = &find("ring")["ring"];
    let frames: Vec<(u64, u64)> = ring["frames"]
        .as_array()
        .expect("frames array")
        .iter()
        .map(|frame| {
            let at = |i: usize| frame[i].as_u64().expect("frame number");
            (at(0), at(1))
        })
        .collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].0, ring["tail"].as_u64().expect("tail"));
    assert_eq!(ring["first"], find("ring")["bounds"]["oldest"]);
    assert_eq!(frames[1].0, frames[0].0 + frames[0].1);
    assert_eq!(
        frames[1].0 + frames[1].1,
        ring["head"].as_u64().expect("head")
    );
    assert!(frames[1].1 > frames[0].1 + 1900);
    assert_eq!(
        frames[0].1 + frames[1].1,
        find("ring")["metrics"]["utilization"]["used_bytes"]
            .as_u64()
            .expect("used bytes")
    );
    assert_eq!(find("empty")["ring"]["frames"], json!([]));
    Ok(())
}

fn read_pool_event(reader: &mut impl BufRead) -> TestResult<Value> {
    let mut event = String::new();
    let mut data = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err("pool stream closed before an event".into());
        }
        if line == "\n" || line == "\r\n" {
            if event.is_empty() {
                continue;
            }
            assert_eq!(event, "pools");
            return Ok(serde_json::from_str(&data)?);
        }
        if let Some(value) = line.strip_prefix("event: ") {
            event = value.trim_end().to_string();
        } else if let Some(value) = line.strip_prefix("data: ") {
            data.push_str(value.trim_end());
        }
    }
}

/// How soon a pool change must reach the stream: the server checks every 200 ms
/// and then takes a snapshot, so a busy debug build needs room. Much longer than
/// this would mean the stream had stopped pushing.
const PUSH_WITHIN: Duration = Duration::from_secs(3);

#[test]
fn local_ui_pool_stream_sends_changes_and_stays_quiet() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let response = ureq::get(&format!("{}/v0/ui/pools/stream", server.local_url))
        .timeout(Duration::from_secs(5))
        .call()?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    let mut reader = BufReader::new(response.into_reader());
    let first = read_pool_event(&mut reader)?;
    let get: Value = ureq::get(&format!("{}/v0/ui/pools", server.local_url))
        .call()?
        .into_json()?;
    assert_eq!(first, get);

    let pool_ref = PoolRef::name("changing");
    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let created_at = Instant::now();
    let created = read_pool_event(&mut reader)?;
    assert!(created_at.elapsed() < PUSH_WITHIN);
    assert_eq!(created["pools"][0]["name"], "changing");

    let pool = client.open_pool(&pool_ref, false)?;
    let message = pool.feed(&json!({"n": 1}), Feed::default())?;
    let appended_at = Instant::now();
    let appended = read_pool_event(&mut reader)?;
    assert!(appended_at.elapsed() < PUSH_WITHIN);
    assert_eq!(appended["pools"][0]["bounds"]["newest"], message.seq);

    client.delete_pool(&pool_ref)?;
    let deleted_at = Instant::now();
    let deleted = read_pool_event(&mut reader)?;
    assert!(deleted_at.elapsed() < PUSH_WITHIN);
    assert_eq!(deleted["pools"], json!([]));

    // A pool that holds messages must also go quiet. Its message ages grow on
    // every read, and the stream must not count that as a change.
    let resting_ref = PoolRef::name("resting");
    client.create_pool(&resting_ref, PoolOptions::new(1024 * 1024))?;
    let resting = client.open_pool(&resting_ref, false)?;
    let last = resting.feed(&json!({"n": 1}), Feed::default())?;
    while read_pool_event(&mut reader)?["pools"][0]["bounds"]["newest"] != last.seq {}

    // A second reader thread lets this assertion time the idle interval from
    // the last event, instead of from when the HTTP request began.
    let (tx, rx) = mpsc::channel();
    let idle_reader = std::thread::spawn(move || {
        let _ = tx.send(read_pool_event(&mut reader).is_ok());
    });
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    idle_reader.join().expect("idle reader thread");
    Ok(())
}

#[test]
fn browser_read_uses_v2_and_receives_pool_incarnation() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("ui-events");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    let created = pool.feed(&json!({"kind": "ui", "ok": true}), Feed::default())?;

    let response = ureq::get(&format!(
        "{}/v2/pools/ui-events/fetch?since={}&until=1",
        server.local_url, created.seq
    ))
    .call()
    .expect("local sse request");
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("application/jsonl"));
    assert!(response.header("plasmite-incarnation").is_some());
    let body = response.into_string()?;
    let mut lines = body.lines();
    assert_eq!(
        serde_json::from_str::<Value>(lines.next().unwrap())?["seq"],
        1
    );
    assert_eq!(
        serde_json::from_str::<Value>(lines.next().unwrap())?["type"],
        "complete"
    );
    Ok(())
}

#[test]
fn continuation_checks_incarnation_before_replacement_bounds() -> TestResult<()> {
    let dir = tempfile::tempdir()?;
    let server = TestServer::try_start(dir.path())?;
    let client = server.client()?;
    let reference = PoolRef::name("replaced");
    let pool = client.create_pool(&reference, PoolOptions::new(65536))?;
    for n in 1..=3 {
        pool.feed(&json!({"n": n}), Feed::default())?;
    }
    let url = format!("{}/v2/pools/replaced/fetch", server.local_url);
    let response = ureq::get(&url).call()?;
    let original = response.header("plasmite-incarnation").unwrap().to_owned();
    let _ = response.into_string()?;

    drop(pool);
    client.delete_pool(&reference)?;
    client.create_pool(&reference, PoolOptions::new(65536))?;

    let result = ureq::get(&url)
        .query("since", "2")
        .query("until", "3")
        .query("incarnation", &original)
        .call();
    let Err(ureq::Error::Status(status, response)) = result else {
        panic!("expected replacement to fail");
    };
    assert_eq!(status, 404);
    assert_eq!(response.into_json::<Value>()?["error"]["kind"], "NotFound");
    Ok(())
}

#[test]
fn local_ui_tail_reads_reported_oldest_after_small_pool_wrap() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("wrapped");
    client.create_pool(&pool_ref, PoolOptions::new(64 * 1024))?;
    let pool = client.open_pool(&pool_ref, false)?;
    for i in 1..=450 {
        pool.feed(
            &json!({"message": format!("tick {i}"), "level": "info"}),
            Feed::default(),
        )?;
    }
    let oldest = pool.info()?.bounds.oldest_seq.expect("oldest");
    assert!(oldest > 1, "the pool must wrap and overwrite messages");

    let response = ureq::get(&format!(
        "{}/v2/pools/wrapped/fetch?since={oldest}&until={oldest}",
        server.local_url
    ))
    .call()?;
    let body = response.into_string()?;
    let message: Value = serde_json::from_str(body.lines().next().unwrap())?;
    assert_eq!(message["seq"], oldest);
    Ok(())
}

fn mcp_payload(payload: &Value) -> Value {
    let mut payload = payload.clone();
    if payload.get("params").is_none() {
        payload["params"] = json!({});
    }
    payload["params"]["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}
    });
    payload
}

fn mcp_request(base_url: &str, payload: &Value) -> ureq::Request {
    let method = payload["method"].as_str().expect("MCP request method");
    let mut request = ureq::post(&format!("{base_url}/mcp"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .set("MCP-Protocol-Version", "2026-07-28")
        .set("Mcp-Method", method);
    let name = match method {
        "tools/call" => payload["params"]["name"].as_str(),
        "resources/read" => payload["params"]["uri"].as_str(),
        _ => None,
    };
    if let Some(name) = name {
        request = request.set("Mcp-Name", name);
    }
    request
}

fn mcp_post(base_url: &str, payload: &Value) -> Result<ureq::Response, Box<ureq::Error>> {
    mcp_request(base_url, payload)
        .send_json(mcp_payload(payload))
        .map_err(Box::new)
}

#[test]
fn remote_mcp_http_profile_request_obsolete_lifecycle_and_get() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;

    let response = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc":"2.0", "id":1, "method":"server/discover"
        }),
    )
    .expect("discovery");
    assert_eq!(response.status(), 200);
    assert!(
        response
            .header("content-type")
            .unwrap_or_default()
            .starts_with("application/json")
    );
    let discovery: Value = serde_json::from_str(&response.into_string()?)?;
    assert_eq!(discovery["id"], 1);
    assert_eq!(
        discovery["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );

    for payload in [
        json!({"jsonrpc":"2.0", "id":2, "method":"initialize", "params":{"protocolVersion":"2025-11-25"}}),
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
    ] {
        let response = ureq::post(&format!("{}/mcp", server.local_url))
            .set("Content-Type", "application/json")
            .send_json(payload);
        let Err(ureq::Error::Status(400, response)) = response else {
            panic!("obsolete lifecycle was not rejected");
        };
        let rejected: Value = serde_json::from_str(&response.into_string()?)?;
        assert_eq!(rejected["error"]["code"], -32601);
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("2026-07-28")
        );
    }

    let response_payload = ureq::post(&format!("{}/mcp", server.local_url))
        .set("Content-Type", "application/json")
        .set("MCP-Protocol-Version", "2025-11-25")
        .send_json(json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": {}
        }));
    assert!(matches!(response_payload, Err(ureq::Error::Status(400, _))));

    let unknown = mcp_post(
        &server.local_url,
        &json!({"jsonrpc":"2.0","id":43,"method":"missing/action","params":{}}),
    );
    assert!(matches!(unknown, Err(error) if matches!(*error, ureq::Error::Status(404, _))));

    match ureq::get(&format!("{}/mcp", server.local_url)).call() {
        Ok(_) => return Err("expected GET /mcp to be rejected".into()),
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 405),
        Err(err) => return Err(err.into()),
    }

    Ok(())
}

#[test]
fn remote_mcp_tool_flow_via_http_post() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;

    let tools_list = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    )
    .expect("tools/list");
    let tools_json: Value = serde_json::from_str(&tools_list.into_string()?)?;
    let names = tools_json["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(names.contains(&"plasmite_pool_create"));
    assert!(names.contains(&"plasmite_pool_delete"));

    let create = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "plasmite_pool_create",
                "arguments": { "name": "flow" }
            }
        }),
    )
    .expect("create");
    let create_json: Value = serde_json::from_str(&create.into_string()?)?;
    assert_eq!(
        create_json["result"]["structuredContent"]["pool"]["name"],
        json!("flow")
    );

    let feed_one = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "plasmite_feed",
                "arguments": { "pool": "flow", "data": {"n": 1} }
            }
        }),
    )
    .expect("feed one");
    let feed_one_json: Value = serde_json::from_str(&feed_one.into_string()?)?;
    let first_seq = feed_one_json["result"]["structuredContent"]["message"]["seq"]
        .as_u64()
        .expect("first seq");

    let feed_two = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "plasmite_feed",
                "arguments": { "pool": "flow", "data": {"n": 2} }
            }
        }),
    )
    .expect("feed two");
    let feed_two_json: Value = serde_json::from_str(&feed_two.into_string()?)?;
    let second_seq = feed_two_json["result"]["structuredContent"]["message"]["seq"]
        .as_u64()
        .expect("second seq");

    let read = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "plasmite_fetch",
                "arguments": {
                    "pool": "flow",
                    "since": first_seq + 1,
                    "count": 10
                }
            }
        }),
    )
    .expect("read");
    let read_json: Value = serde_json::from_str(&read.into_string()?)?;
    assert_eq!(
        read_json["result"]["structuredContent"]["messages"][0]["seq"],
        json!(second_seq)
    );

    let fetch = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "plasmite_fetch",
                "arguments": { "pool": "flow", "since": second_seq, "count": 1 }
            }
        }),
    )
    .expect("fetch");
    let fetch_json: Value = serde_json::from_str(&fetch.into_string()?)?;
    assert_eq!(
        fetch_json["result"]["structuredContent"]["messages"][0]["data"]["n"],
        json!(2)
    );

    let delete = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "plasmite_pool_delete",
                "arguments": { "pool": "flow" }
            }
        }),
    )
    .expect("delete");
    let delete_json: Value = serde_json::from_str(&delete.into_string()?)?;
    assert_ne!(delete_json["result"]["isError"], json!(true));

    Ok(())
}

#[test]
fn remote_mcp_protocol_version_header_validation() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });

    let mut obsolete = mcp_payload(&payload);
    obsolete["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2025-11-25");
    let response = mcp_request(&server.local_url, &payload)
        .set("MCP-Protocol-Version", "2025-11-25")
        .send_json(obsolete);
    let Err(ureq::Error::Status(400, response)) = response else {
        panic!("obsolete transport version was not rejected");
    };
    let rejected: Value = serde_json::from_str(&response.into_string()?)?;
    assert_eq!(rejected["error"]["code"], -32022);
    assert_eq!(
        rejected["error"]["data"],
        json!({
            "requested":"2025-11-25", "supported":["2026-07-28"]
        })
    );

    let supported = mcp_post(&server.local_url, &payload).expect("supported protocol");
    assert_eq!(supported.status(), 200);

    let absent = ureq::post(&format!("{}/mcp", server.local_url))
        .set("Content-Type", "application/json")
        .send_json(mcp_payload(&payload));
    assert!(matches!(absent, Err(ureq::Error::Status(400, _))));
    Ok(())
}

#[test]
fn remote_mcp_origin_header_validation() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });

    match mcp_request(&server.local_url, &payload)
        .set("Origin", "not a valid origin")
        .send_json(mcp_payload(&payload))
    {
        Ok(_) => return Err("expected invalid Origin to fail".into()),
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 403),
        Err(err) => return Err(err.into()),
    }

    let valid = mcp_request(&server.local_url, &payload)
        .set("Origin", &server.local_url)
        .send_json(mcp_payload(&payload))
        .expect("valid Origin");
    assert_eq!(valid.status(), 200);
    Ok(())
}

#[test]
fn http_follow_keeps_the_same_response_open_on_a_quiet_pool() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::start_with_args_and_scheme(
        temp.path(),
        &["--max-tail-timeout-ms", "500"],
        "http",
    );
    let client = server.client()?;
    let pool = client.create_pool(&PoolRef::name("events"), PoolOptions::new(65536))?;
    let url = format!("{}/v2/pools/events/follow", server.local_url);
    let response = ureq::get(&url).timeout(Duration::from_secs(3)).call()?;
    let mut lines = BufReader::new(response.into_reader()).lines();
    for n in 1..=2 {
        sleep(Duration::from_millis(650));
        let saved = pool.feed(&json!({"n": n}), Feed::new())?;
        let line = lines.next().expect("follow remains open")?;
        let record: Value = serde_json::from_str(&line)?;
        assert_eq!(record["seq"], saved.seq, "{record}");
        assert_eq!(record["data"], saved.data);
    }
    Ok(())
}

#[test]
fn real_fetch_continues_after_cutoff_with_its_original_end() -> TestResult<()> {
    use plasmite::api::LocalClient;

    let temp = tempfile::tempdir()?;
    let server = TestServer::start_with_args_and_scheme(
        temp.path(),
        &["--max-tail-timeout-ms", "300"],
        "http",
    );
    let mut writer = LocalClient::new().with_pool_dir(temp.path()).create_pool(
        &PoolRef::name("history"),
        PoolOptions::new(24 * 1024 * 1024),
    )?;
    let payload = json!({"padding": "x".repeat(64 * 1024)});
    let count = 192;
    for _ in 0..count {
        writer.feed(&payload, Feed::new())?;
    }
    let url = format!("{}/v2/pools/history/fetch", server.local_url);
    let request = |url: &str| -> TestResult<ureq::Response> {
        Ok(ureq::get(url).timeout(Duration::from_secs(5)).call()?)
    };
    let read = |response: ureq::Response, next: &mut u64| -> TestResult<Value> {
        let mut terminal = None;
        for line in BufReader::new(response.into_reader()).lines() {
            let record: Value = serde_json::from_str(&line?)?;
            assert!(terminal.is_none(), "record after stream ended");
            if let Some(seq) = record["seq"].as_u64() {
                assert_eq!(seq, *next);
                *next += 1;
            } else {
                terminal = Some(record);
            }
        }
        Ok(terminal.expect("one terminal record"))
    };

    let response = request(&url)?;
    // Let the initial response hit the socket's backpressure before draining it.
    sleep(Duration::from_millis(450));
    let mut next = 1;
    let mut terminal = read(response, &mut next)?;
    assert_eq!(
        terminal,
        json!({"type":"cutoff", "next_seq":next, "until":count})
    );
    assert!(next > 1 && next <= count);
    writer.feed(&json!({"after":"opening boundary"}), Feed::new())?;
    while next <= count {
        let start = next;
        terminal = read(
            request(&format!("{url}?since={start}&until={count}"))?,
            &mut next,
        )?;
        assert!(next > start);
    }
    assert_eq!(next, count + 1);
    assert_eq!(terminal, json!({"type":"complete"}));

    Ok(())
}

#[test]
fn ordinary_http_rejects_untrusted_origin_before_body_extraction() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let response = ureq::post(&format!("{}/v2/pools", server.local_url))
        .set("Origin", "https://untrusted.example")
        .set("Content-Type", "application/json")
        .send_string("{");
    assert!(matches!(response, Err(ureq::Error::Status(403, _))));
    Ok(())
}

#[test]
fn v2_live_reads_open_before_returning_and_fetch_is_finite() -> TestResult<()> {
    use plasmite::api::{Feed, Read, Since};

    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let client = server.client()?;
    let name = PoolRef::name("v2");
    client.create_pool(&name, PoolOptions::new(65536))?;
    let pool = client.open_pool(&name, false)?;
    let first = pool.feed(&json!({"point":[1,2]}), Feed::new().tags(["gfx"]))?;
    let mut follow = pool.follow(Read::new().tags(["gfx"]))?;
    pool.feed(&json!({"ready":true}), Feed::new())?;
    let third = pool.feed(&first, Feed::new())?;
    assert_eq!(
        follow.next_message()?.expect("next matching message"),
        third
    );
    follow.close();

    let mut fetch = pool.fetch_lite3(Read::since(Since::Seq(2)))?;
    pool.feed(&json!({"later":true}), Feed::new())?;
    let frames = fetch.by_ref().collect::<Result<Vec<_>, _>>()?;
    let seqs: Vec<_> = frames.iter().map(|frame| frame.seq).collect();
    assert_eq!(seqs, [2, 3]);
    Ok(())
}

#[test]
fn v2_binary_read_ends_with_a_structured_retention_gap() -> TestResult<()> {
    use plasmite::api::{Feed, Read, Since};
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let client = server.client()?;
    let name = PoolRef::name("gaps");
    client.create_pool(&name, PoolOptions::new(16384))?;
    let pool = client.open_pool(&name, false)?;
    let value = json!({"padding": "x".repeat(1024)});
    for _ in 0..24 {
        pool.feed(&value, Feed::new())?;
    }
    let oldest = pool.info()?.bounds.oldest_seq.expect("retained history");
    assert!(oldest > 1);
    let mut reader = pool.fetch_lite3(Read::since(Since::Seq(1)).tags(["no match"]))?;
    let error = reader.next_frame().expect_err("filter cannot conceal loss");
    assert_eq!(error.kind(), ErrorKind::RetentionGap);
    assert_eq!(error.missing(), Some(1));
    assert_eq!(error.next_seq(), Some(oldest));
    let repeated = reader
        .next_frame()
        .expect_err("the failure remains visible");
    assert_eq!(repeated.kind(), ErrorKind::RetentionGap);
    assert_eq!(repeated.missing(), Some(1));
    assert_eq!(repeated.next_seq(), Some(oldest));
    Ok(())
}

#[test]
fn version_one_pool_routes_name_required_upgrade() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    for path in ["pools", "pools/events/append", "pools/events/follow"] {
        let url = format!("{}/v0/{path}", server.local_url);
        let Err(ureq::Error::Status(426, response)) = ureq::get(&url).call() else {
            panic!("expected upgrade error for {path}");
        };
        let value: Value = response.into_json()?;
        assert_eq!(value["error"]["kind"], "Usage");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("HTTP API v2")
        );
    }
    Ok(())
}

#[test]
fn v2_cancels_a_blocked_https_reader() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::start_with_args_and_scheme(
        temp.path(),
        &[
            "--max-tail-timeout-ms",
            "5000",
            "--max-tail-concurrency",
            "1",
        ],
        "https",
    );
    let client = server.client()?;
    let name = PoolRef::name("quiet");
    let pool = client.create_pool(&name, PoolOptions::new(65536))?;
    for binary in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let options = ReadOptions::new().cancel(Arc::clone(&cancel));
        let (done, completed) = mpsc::channel();
        if binary {
            let mut reader = pool.follow_lite3(options)?;
            std::thread::spawn(move || {
                let _ = done.send(reader.next_frame().map(|record| record.is_none()));
            });
        } else {
            let mut reader = pool.follow(options)?;
            std::thread::spawn(move || {
                let _ = done.send(reader.next_message().map(|record| record.is_none()));
            });
        }
        assert!(matches!(
            completed.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        cancel.store(true, Ordering::Release);
        assert!(completed.recv_timeout(Duration::from_secs(2))??);
        // The next iteration uses the same server slot.
        sleep(Duration::from_millis(120));
    }
    Ok(())
}
