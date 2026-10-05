//! Access revocation stops existing streams and survives a server restart.

use plasmite::api::{Durability, PoolOptions, PoolRef, RemoteClient, TailOptions};
use serde_json::{Value, json};
use std::fs;
use std::sync::mpsc;
use std::time::Duration;

#[allow(dead_code)] // This test uses only the access and HTTPS helpers.
#[path = "support/server.rs"]
mod server;
use server::TestServer;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn invite(pool_dir: &std::path::Path, name: &str) -> TestResult<String> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .arg("--dir")
        .arg(pool_dir)
        .args(["access", "invite", "--name", name, "--json"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "access invite failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    Ok(reply["access_key"]
        .as_str()
        .ok_or("invite response omitted access_key")?
        .to_owned())
}

fn access_cli(pool_dir: &std::path::Path, args: &[&str]) -> TestResult<Value> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .arg("--dir")
        .arg(pool_dir)
        .arg("access")
        .args(args)
        .arg("--json")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "access {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn list_keys(local_url: &str, fingerprint: &str) -> TestResult<Value> {
    let response = ureq::get(&format!("{local_url}/v0/access/keys"))
        .set("x-plasmite-server-fingerprint", fingerprint)
        .call()?;
    Ok(serde_json::from_str(&response.into_string()?)?)
}

fn key_by_id<'a>(keys: &'a Value, id: &str) -> &'a Value {
    keys["keys"]
        .as_array()
        .expect("keys response contains a keys array")
        .iter()
        .find(|key| key["id"] == id)
        .expect("access key appears in listing")
}

fn wait_for_stream_end(
    mut tail: plasmite::api::RemoteTail,
    last_admitted_seq: u64,
) -> mpsc::Receiver<bool> {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let ended = loop {
            match tail.next_message() {
                Ok(Some(message)) if message.seq <= last_admitted_seq => continue,
                Ok(Some(_)) => break false,
                Ok(None) => break true,
                Err(_) => break false,
            }
        };
        let _ = done_tx.send(ended);
    });
    done_rx
}

fn revoke(pool_dir: &std::path::Path, id: &str) -> TestResult<()> {
    let result = access_cli(pool_dir, &["revoke", id])?;
    assert_eq!(result["id"], id);
    assert_eq!(result["revoked"], true);
    Ok(())
}

fn contains_pool(client: &RemoteClient, pool: &str) -> TestResult<bool> {
    Ok(client
        .list_pools()?
        .iter()
        .any(|info| info.path.file_stem().and_then(|name| name.to_str()) == Some(pool)))
}

#[test]
fn copied_local_address_cannot_administer_another_directory() -> TestResult<()> {
    use std::io::Write;

    let temp = tempfile::tempdir()?;
    let original = temp.path().join("original");
    let copied = temp.path().join("copied");
    let _server = TestServer::start(&original);
    let before = access_cli(&original, &["keys"])?;
    let id = before["keys"][0]["id"]
        .as_str()
        .ok_or("first key omitted id")?;

    let original_state = original.join(".plasmite-serve");
    let copied_state = copied.join(".plasmite-serve");
    // Initialize valid private state so the copied address reaches the ownership check.
    drop(TestServer::start(&copied));
    let identity: Value = serde_json::from_slice(&fs::read(original_state.join("identity.json"))?)?;
    let copied_identity: Value =
        serde_json::from_slice(&fs::read(copied_state.join("identity.json"))?)?;
    let original_cert = identity["front_cert_file"]
        .as_str()
        .or(identity["cert_file"].as_str())
        .ok_or("identity omitted certificate")?;
    let copied_cert = copied_identity["front_cert_file"]
        .as_str()
        .or(copied_identity["cert_file"].as_str())
        .ok_or("copied identity omitted certificate")?;
    // Overwrite existing private files; new copies receive default Windows ACLs.
    for (source, destination) in [(original_cert, copied_cert), ("local.json", "local.json")] {
        let bytes = fs::read(original_state.join(source))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(copied_state.join(destination))?;
        file.write_all(&bytes)?;
    }
    assert_eq!(
        fs::read(copied_state.join(copied_cert))?,
        fs::read(original_state.join(original_cert))?,
        "copied state must identify the original server"
    );

    for args in [
        vec!["invite", "unexpected"],
        vec!["keys"],
        vec!["revoke", id],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
            .arg("--dir")
            .arg(&copied)
            .arg("access")
            .args(&args)
            .arg("--json")
            .output()?;
        assert!(
            !output.status.success(),
            "{} unexpectedly succeeded",
            args.join(" ")
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("local server does not own the selected pool directory"),
            "{} failed for the wrong reason: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(access_cli(&original, &["keys"])?, before);
    let alias = original.join("..").join("original");
    assert_eq!(access_cli(&alias, &["keys"])?, before);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn access_commands_accept_non_utf8_directory_names() -> TestResult<()> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let temp = tempfile::tempdir()?;
    let pool_dir = temp.path().join(OsStr::from_bytes(b"pool-\xff"));
    let _server = TestServer::start(&pool_dir);
    let before = access_cli(&pool_dir, &["keys"])?;
    assert_eq!(before["keys"].as_array().map(Vec::len), Some(1));

    let key = invite(&pool_dir, "non-utf8 owner")?;
    assert!(key.starts_with("pk1."));
    let after_invite = access_cli(&pool_dir, &["keys"])?;
    let id = after_invite["keys"]
        .as_array()
        .ok_or("missing keys")?
        .iter()
        .find(|row| row["name"] == "non-utf8 owner")
        .and_then(|row| row["id"].as_str())
        .ok_or("invited key omitted id")?;
    revoke(&pool_dir, id)?;
    Ok(())
}

#[test]
fn revoke_stops_streams_denies_new_requests_and_survives_restart() -> TestResult<()> {
    let temp = tempfile::tempdir()?;
    let pool_dir = temp.path().join("pools");
    let server = TestServer::start(&pool_dir);
    let first_key = server.access_key().to_owned();
    let second_key = invite(&pool_dir, "reader-two")?;
    let fingerprint = first_key
        .split('.')
        .nth(1)
        .ok_or("access key omitted server fingerprint")?
        .to_owned();

    let listed = access_cli(&pool_dir, &["keys"])?;
    let listed_keys = listed["keys"].as_array().ok_or("missing keys array")?;
    assert_eq!(listed_keys.len(), 2);
    let listing_json = listed.to_string();
    for key in [&first_key, &second_key] {
        let secret = key.rsplit('.').next().ok_or("access key omitted secret")?;
        assert!(
            !listing_json.contains(key),
            "listing disclosed an access key"
        );
        assert!(
            !listing_json.contains(secret),
            "listing disclosed a key secret"
        );
    }
    let first_record = listed_keys
        .iter()
        .find(|key| key["name"] == "integration-test")
        .ok_or("first invitation missing")?;
    let first_id = first_record["id"].as_str().ok_or("first key omitted id")?;
    assert_eq!(first_record["revoked"], false);
    assert!(first_record["created_at"].is_number());
    assert!(first_record.get("last_used_at").is_some());
    let first_id = first_id.to_owned();
    let second_record = listed_keys
        .iter()
        .find(|key| key["name"] == "reader-two")
        .ok_or("second invitation missing")?;
    let second_id = second_record["id"]
        .as_str()
        .ok_or("second key omitted id")?;
    assert_eq!(second_record["revoked"], false);
    assert!(second_record["created_at"].is_number());
    let second_id = second_id.to_owned();

    let first_client = RemoteClient::with_access_key(&server.remote_url, &first_key)?;
    let second_client = RemoteClient::with_access_key(&server.remote_url, &second_key)?;
    let pool_ref = PoolRef::name("lifecycle");
    first_client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let first_pool = first_client.open_pool(&pool_ref)?;
    let second_pool = second_client.open_pool(&pool_ref)?;
    let initial = first_pool.append_json_now(&json!({"n": 1}), &[], Durability::Fast)?;

    // One stream has already delivered data; the other waits at the current end.
    let mut active = first_pool.tail(TailOptions {
        since_seq: Some(initial.seq),
        timeout: Some(Duration::from_secs(20)),
        ..TailOptions::default()
    })?;
    assert_eq!(
        active.next_message()?.expect("initial stream message").seq,
        initial.seq
    );
    let idle = first_pool.tail(TailOptions {
        since_seq: Some(initial.seq + 2),
        timeout: Some(Duration::from_secs(20)),
        ..TailOptions::default()
    })?;

    // This write is admitted while the invitation remains active.
    let before_cutoff =
        first_pool.append_json_now(&json!({"write": "before cutoff"}), &[], Durability::Fast)?;
    assert_eq!(before_cutoff.seq, initial.seq + 1);

    revoke(&pool_dir, &first_id)?;

    // This later sequence must not reach streams opened with the revoked key.
    let after_cutoff =
        second_pool.append_json_now(&json!({"write": "other key"}), &[], Durability::Fast)?;
    assert!(after_cutoff.seq > before_cutoff.seq);
    assert!(
        first_pool
            .append_json_now(&json!({"write": "after cutoff"}), &[], Durability::Fast)
            .is_err(),
        "revoked key admitted a new write"
    );

    let active_done_rx = wait_for_stream_end(active, before_cutoff.seq);
    let idle_done_rx = wait_for_stream_end(idle, before_cutoff.seq);

    assert!(
        active_done_rx.recv_timeout(Duration::from_secs(2))?,
        "stream that had delivered data remained open after revocation"
    );
    assert!(
        idle_done_rx.recv_timeout(Duration::from_secs(2))?,
        "idle stream remained open after revocation"
    );
    assert!(
        first_client.list_pools().is_err(),
        "revoked key authorized a new request"
    );
    assert!(contains_pool(&second_client, "lifecycle")?);

    let after_revoke = list_keys(&server.local_url, &fingerprint)?;
    assert_eq!(key_by_id(&after_revoke, &first_id)["revoked"], true);
    assert_eq!(key_by_id(&after_revoke, &second_id)["revoked"], false);

    drop(server);
    let restarted = TestServer::start(&pool_dir);
    let after_restart = list_keys(&restarted.local_url, &fingerprint)?;
    assert_eq!(key_by_id(&after_restart, &first_id)["revoked"], true);
    assert_eq!(key_by_id(&after_restart, &second_id)["revoked"], false);
    assert!(
        RemoteClient::with_access_key(&restarted.remote_url, &first_key)?
            .list_pools()
            .is_err(),
        "revocation did not survive restart"
    );
    assert!(
        contains_pool(
            &RemoteClient::with_access_key(&restarted.remote_url, &second_key)?,
            "lifecycle"
        )?,
        "unrevoked key stopped working after restart"
    );
    Ok(())
}
