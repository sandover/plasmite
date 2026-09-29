//! Purpose: End-to-end tests for authenticated remote HTTPS server/client behavior.
//! Exports: None (integration test module).
//! Role: Validate remote append/get/tail and error propagation across TCP.
//! Invariants: Uses loopback-only server with temp pool directory.
//! Invariants: Bounded waits avoid test flakiness.
//! Invariants: Server processes are cleaned up on drop.

use plasmite::api::{
    AppendOptions, Durability, ErrorKind, GapPolicy, Pool, PoolOptions, PoolRef, RemoteClient,
    TailOptions, access,
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

#[test]
fn remote_append_and_get() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
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
    let server = TestServer::try_start(temp_dir.path())?;
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

    let server = TestServer::try_start(pool_dir)?;
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
    let server = TestServer::try_start(temp_dir.path())?;
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
fn remote_tail_reconnects_with_stable_since_seq_without_duplicates() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-reconnect");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;

    let first = pool.append_json_now(&json!({"seq": 1}), &[], Durability::Fast)?;
    let second = pool.append_json_now(&json!({"seq": 2}), &[], Durability::Fast)?;

    let mut first_tail = pool.tail(TailOptions {
        max_messages: Some(2),
        timeout: Some(Duration::from_millis(150)),
        ..TailOptions::default()
    })?;
    let message_one = first_tail.next_message()?.expect("first replay message");
    let message_two = first_tail.next_message()?.expect("second replay message");
    assert_eq!(message_one.seq, first.seq);
    assert_eq!(message_two.seq, second.seq);
    assert!(
        first_tail.next_message()?.is_none(),
        "replay tail should stop after max_messages"
    );

    let third = pool.append_json_now(&json!({"seq": 3}), &[], Durability::Fast)?;
    let fourth = pool.append_json_now(&json!({"seq": 4}), &[], Durability::Fast)?;

    let mut reconnect_tail = pool.tail(TailOptions {
        since_seq: Some(message_two.seq + 1),
        max_messages: Some(2),
        timeout: Some(Duration::from_millis(300)),
        ..TailOptions::default()
    })?;
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
    let writer_pool = append_client.open_pool(&pool_ref)?;
    let tail_pool = base_client.open_pool(&pool_ref)?;

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
                .append_json_now(&payload, &[], Durability::Fast)
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
        let mut tail = tail_pool
            .tail(TailOptions {
                timeout: Some(Duration::from_millis(120)),
                max_messages: Some(10_000),
                ..TailOptions::default()
            })
            .map_err(|err| err.to_string())?;
        tail_started_tx
            .send(())
            .map_err(|err| format!("failed to signal tail start: {err}"))?;
        let mut observed = 0usize;
        loop {
            if cancel_tail.load(Ordering::Acquire) {
                tail.cancel();
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
    let err = match client.open_pool(&PoolRef::name("missing")) {
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

    let create_url = format!("{}/v0/pools", server.local_url);
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

    let open_url = format!("{}/v0/pools/open", server.local_url);
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
    let server = TestServer::try_start(temp_dir.path())?;
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
fn remote_json_tail_can_fail_closed_on_retention_gaps() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-gap");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let first = pool.append_json_now(&json!({"kind": "first"}), &[], Durability::Fast)?;
    for i in 0..10_000 {
        pool.append_json_now(
            &json!({"i": i, "padding": "x".repeat(64 * 1024)}),
            &[],
            Durability::Fast,
        )?;
        match pool.get_message(first.seq) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => break,
            Err(err) => return Err(err.into()),
        }
        if i == 9_999 {
            return Err("first sequence remained retained after filling pool".into());
        }
    }

    let mut continuing = pool.tail(TailOptions {
        since_seq: Some(first.seq),
        max_messages: Some(1),
        timeout: Some(Duration::from_millis(500)),
        ..TailOptions::default()
    })?;
    let continued = continuing
        .next_message()?
        .ok_or("default remote tail did not continue")?;
    assert!(continued.seq > first.seq);

    let mut failing = pool.tail(TailOptions {
        since_seq: Some(first.seq),
        tags: vec!["never".to_string()],
        timeout: Some(Duration::from_millis(500)),
        gap_policy: GapPolicy::Error,
        ..TailOptions::default()
    })?;
    let err = failing
        .next_message()
        .expect_err("expected remote retention gap");
    assert_eq!(err.kind(), ErrorKind::RetentionGap);
    assert_eq!(err.seq(), Some(first.seq));
    assert!(failing.next_message()?.is_none());
    Ok(())
}

#[test]
fn remote_tail_rejects_invalid_or_unsupported_gap_policies() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("tail-gap-policy");
    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;

    let err = match pool.tail_lite3(TailOptions {
        gap_policy: GapPolicy::Error,
        ..TailOptions::default()
    }) {
        Ok(_) => return Err("expected remote Lite3 gap-policy rejection".into()),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::Usage);

    for path in ["tail?gap_policy=unknown", "tail_lite3?gap_policy=error"] {
        let url = format!(
            "{}/v0/pools/tail-gap-policy/{path}",
            server.local_url.trim_end_matches('/')
        );
        match ureq::get(&url).call() {
            Err(ureq::Error::Status(400, _)) => {}
            other => return Err(format!("expected HTTP 400 for {path}, got {other:?}").into()),
        }
    }
    Ok(())
}

#[test]
fn remote_tail_filters_by_tags() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
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
    let server = TestServer::try_start(temp_dir.path())?;
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
    let pool = client.open_pool(&pool_ref)?;
    pool.append_json_now(&json!({"body": "short"}), &[], Durability::Fast)?;
    pool.append_json_now(&json!({"body": "x".repeat(2000)}), &[], Durability::Fast)?;

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
    assert!(created_at.elapsed() < Duration::from_millis(500));
    assert_eq!(created["pools"][0]["name"], "changing");

    let pool = client.open_pool(&pool_ref)?;
    let message = pool.append_json_now(&json!({"n": 1}), &[], Durability::Fast)?;
    let appended_at = Instant::now();
    let appended = read_pool_event(&mut reader)?;
    assert!(appended_at.elapsed() < Duration::from_millis(500));
    assert_eq!(appended["pools"][0]["bounds"]["newest"], message.seq);

    client.delete_pool(&pool_ref)?;
    let deleted_at = Instant::now();
    let deleted = read_pool_event(&mut reader)?;
    assert!(deleted_at.elapsed() < Duration::from_millis(500));
    assert_eq!(deleted["pools"], json!([]));

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
fn local_ui_events_stream_sends_sse() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("ui-events");

    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let created =
        pool.append_json_now(&json!({"kind": "ui", "ok": true}), &[], Durability::Fast)?;

    let response = ureq::get(&format!(
        "{}/v0/ui/pools/ui-events/events?since_seq={}&max=1",
        server.local_url, created.seq
    ))
    .call()
    .expect("local sse request");
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    let body = response.into_string()?;
    assert!(body.contains("event: message"));
    assert!(body.contains("\"seq\":1"));
    Ok(())
}

#[test]
fn local_ui_tail_reads_reported_oldest_after_small_pool_wrap() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;
    let client = server.client()?;
    let pool_ref = PoolRef::name("wrapped");
    client.create_pool(&pool_ref, PoolOptions::new(64 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    for i in 1..=450 {
        pool.append_json_now(
            &json!({"message": format!("tick {i}"), "level": "info"}),
            &[],
            Durability::Fast,
        )?;
    }
    let oldest = pool.info()?.bounds.oldest_seq.expect("oldest");
    assert!(oldest > 1, "the pool must wrap and overwrite messages");

    let response = ureq::get(&format!(
        "{}/v0/ui/pools/wrapped/events?gap_policy=error&since_seq={oldest}&max=1",
        server.local_url
    ))
    .call()?;
    let body = response.into_string()?;
    assert!(body.contains("event: message"), "{body}");
    let data = body
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("message data");
    let message: Value = serde_json::from_str(data)?;
    assert_eq!(message["seq"], oldest);
    Ok(())
}

fn mcp_post(base_url: &str, payload: &Value) -> Result<ureq::Response, Box<ureq::Error>> {
    let method = payload["method"]
        .as_str()
        .expect("MCP request method")
        .to_owned();
    let request = ureq::post(&format!("{base_url}/mcp"))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream");
    let request = if method == "initialize" {
        request
    } else {
        request.set("MCP-Protocol-Version", "2025-11-25")
    };
    request.send_string(&payload.to_string()).map_err(Box::new)
}

#[test]
fn remote_mcp_http_profile_request_notification_and_get() -> TestResult<()> {
    let temp_dir = tempfile::tempdir()?;
    let server = TestServer::try_start(temp_dir.path())?;

    let initialize = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "integration-test", "version": "1" }
            }
        }),
    )
    .expect("initialize");
    assert_eq!(initialize.status(), 200);
    assert!(
        initialize
            .header("content-type")
            .unwrap_or_default()
            .starts_with("application/json")
    );
    let initialize_json: Value = serde_json::from_str(&initialize.into_string()?)?;
    assert_eq!(initialize_json["id"], json!(1));
    assert_eq!(
        initialize_json["result"]["protocolVersion"],
        json!("2025-11-25")
    );

    let notification = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .expect("notification");
    assert_eq!(notification.status(), 202);
    assert_eq!(notification.into_string()?, "");

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

    let initialize = mcp_post(
        &server.local_url,
        &json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "integration-test", "version": "1" }
            }
        }),
    )
    .expect("initialize");
    assert_eq!(initialize.status(), 200);
    let initialized: Value = serde_json::from_str(&initialize.into_string()?)?;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-11-25")
    );

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
                "name": "plasmite_read",
                "arguments": {
                    "pool": "flow",
                    "since": "1970-01-01T00:00:00Z",
                    "after_seq": first_seq,
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
                "arguments": { "pool": "flow", "seq": second_seq }
            }
        }),
    )
    .expect("fetch");
    let fetch_json: Value = serde_json::from_str(&fetch.into_string()?)?;
    assert_eq!(
        fetch_json["result"]["structuredContent"]["message"]["data"]["n"],
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

    match ureq::post(&format!("{}/mcp", server.local_url))
        .set("Content-Type", "application/json")
        .set("MCP-Protocol-Version", "not-supported")
        .send_string(&payload.to_string())
    {
        Ok(_) => return Err("expected unsupported protocol version to fail".into()),
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 400),
        Err(err) => return Err(err.into()),
    }

    let supported = mcp_post(&server.local_url, &payload).expect("supported protocol");
    assert_eq!(supported.status(), 200);

    let absent = ureq::post(&format!("{}/mcp", server.local_url))
        .set("Content-Type", "application/json")
        .send_string(&payload.to_string());
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

    match ureq::post(&format!("{}/mcp", server.local_url))
        .set("Content-Type", "application/json")
        .set("MCP-Protocol-Version", "2025-11-25")
        .set("Origin", "not a valid origin")
        .send_string(&payload.to_string())
    {
        Ok(_) => return Err("expected invalid Origin to fail".into()),
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 403),
        Err(err) => return Err(err.into()),
    }

    let valid = ureq::post(&format!("{}/mcp", server.local_url))
        .set("Content-Type", "application/json")
        .set("MCP-Protocol-Version", "2025-11-25")
        .set("Origin", &server.local_url)
        .send_string(&payload.to_string())
        .expect("valid Origin");
    assert_eq!(valid.status(), 200);
    Ok(())
}
