//! The native access key works across a server restart and a fresh client process.

use plasmite::api::{Durability, PoolOptions, PoolRef, RemoteClient, access};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

#[allow(dead_code)] // This test uses only part of the shared server fixture.
#[path = "support/server.rs"]
mod server;
use server::TestServer;

#[test]
fn native_key_connects_and_reuses_server_identity()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let temp = tempfile::tempdir()?;
    let pool_dir = temp.path().join("pools");
    let client_home = temp.path().join("client");
    // This target has one test, so its process-wide override has no competing user.
    unsafe { std::env::set_var("PLASMITE_ACCESS_HOME", &client_home) };

    let server = TestServer::start_with_args_and_scheme(&pool_dir, &[], "https");
    let url = server.base_url.clone();
    assert!(
        ureq::get(&format!("{}/v0/pools", server.local_url))
            .call()
            .is_ok()
    );
    let forged_origin = ureq::get(&format!("{}/v0/pools", server.local_url))
        .set("Origin", "https://evil.test")
        .call();
    assert!(matches!(forged_origin, Err(ureq::Error::Status(403, _))));
    let forged_host = ureq::get(&format!("{}/v0/pools", server.local_url))
        .set("Host", "evil.test")
        .call();
    assert!(matches!(forged_host, Err(ureq::Error::Status(403, _))));
    let before = access::status(&url)?;
    assert!(!before.credentials_saved);
    assert_eq!(before.reachable, Some(true));
    assert_eq!(before.accepted, None);
    let invite = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "access",
            "invite",
            "--name",
            "laptop",
        ])
        .arg("--json")
        .output()?;
    assert!(
        invite.status.success(),
        "{}",
        String::from_utf8_lossy(&invite.stderr)
    );
    let reply: Value = serde_json::from_slice(&invite.stdout)?;
    let key = reply["access_key"].as_str().ok_or("missing access key")?;
    assert!(key.starts_with("pk1."));

    let status = access::connect(&url, key)?;
    assert!(status.credentials_saved);
    assert_eq!(status.accepted, Some(true));
    let saved_client = RemoteClient::new(&url)?;
    let original_key_snapshot = RemoteClient::with_access_key(&url, key)?;
    access::connect(&url, key)?;
    let saved_connections: Value =
        serde_json::from_slice(&std::fs::read(client_home.join("connections.json"))?)?;
    let saved_before_reconnect = std::fs::read(client_home.join("connections.json"))?;
    assert_eq!(
        saved_connections["connections"].as_object().unwrap().len(),
        1
    );
    let secret = key.rsplit('.').next().ok_or("missing secret")?;
    let wrong_pin = format!("pk1.{}.{}", "0".repeat(64), secret);
    let wrong_pin_error = access::connect(&url, &wrong_pin).expect_err("wrong pin rejected");
    let hint = wrong_pin_error.hint().expect("actionable TLS guidance");
    assert!(hint.contains("MagicDNS"));
    assert!(hint.contains("--front-cert"));
    assert!(hint.contains("withheld"));
    assert_eq!(
        std::fs::read(client_home.join("connections.json"))?,
        saved_before_reconnect
    );
    let bad_secret = format!("pk1.{}.{}", &key[4..68], "0".repeat(64));
    assert!(access::connect(&url, &bad_secret).is_err());
    let unauthorised = RemoteClient::with_access_key(&url, &bad_secret)?;
    assert!(unauthorised.list_pools().is_err());
    assert_eq!(access::status(&url)?.accepted, Some(true));

    let replacement_invite = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "access",
            "invite",
            "--name",
            "replacement",
        ])
        .arg("--json")
        .output()?;
    assert!(
        replacement_invite.status.success(),
        "{}",
        String::from_utf8_lossy(&replacement_invite.stderr)
    );
    let replacement_reply: Value = serde_json::from_slice(&replacement_invite.stdout)?;
    let replacement_key = replacement_reply["access_key"]
        .as_str()
        .ok_or("missing replacement access key")?;
    let lock_path = client_home.join("connections.lock");
    std::fs::remove_file(&lock_path)?;
    std::fs::create_dir(&lock_path)?;
    let failed_save = access::connect(&url, replacement_key);
    std::fs::remove_dir(&lock_path)?;
    assert!(failed_save.is_err());
    assert_eq!(
        std::fs::read(client_home.join("connections.json"))?,
        saved_before_reconnect,
        "failed credential storage must preserve the previous connection"
    );
    assert_eq!(access::status(&url)?.accepted, Some(true));
    access::connect(&url, replacement_key)?;
    assert_ne!(
        std::fs::read(client_home.join("connections.json"))?,
        saved_before_reconnect,
        "successful connection should replace the saved credential"
    );

    let replacement_snapshot = RemoteClient::with_access_key(&url, replacement_key)?;
    let access_keys = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args(["--dir", pool_dir.to_str().unwrap(), "access", "keys"])
        .arg("--json")
        .output()?;
    assert!(
        access_keys.status.success(),
        "{}",
        String::from_utf8_lossy(&access_keys.stderr)
    );
    let listed: Value = serde_json::from_slice(&access_keys.stdout)?;
    let original_id = listed["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter()
                .find(|entry| entry["name"] == "laptop")
                .and_then(|entry| entry["id"].as_str())
        })
        .ok_or("access keys omitted original key")?;
    let revoke = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "access",
            "revoke",
            original_id,
        ])
        .arg("--json")
        .output()?;
    assert!(
        revoke.status.success(),
        "{}",
        String::from_utf8_lossy(&revoke.stderr)
    );
    let revoke_reply: Value = serde_json::from_slice(&revoke.stdout)?;
    assert_eq!(revoke_reply["id"], original_id);
    assert_eq!(revoke_reply["revoked"], true);
    assert!(original_key_snapshot.list_pools().is_err());
    assert!(
        saved_client.list_pools().is_ok(),
        "an existing saved client must use the replacement key"
    );
    assert!(replacement_snapshot.list_pools().is_ok());

    let pool_ref = PoolRef::name("shared");
    saved_client
        .create_pool(&pool_ref, PoolOptions::new(1024 * 1024))
        .map_err(|error| format!("create shared pool: {error:?}"))?;
    let pool = saved_client
        .open_pool(&pool_ref)
        .map_err(|error| format!("open shared pool: {error:?}"))?;
    let message = pool
        .append_json_now(&json!({"message":"hello"}), &[], Durability::Fast)
        .map_err(|error| format!("append shared message: {error:?}"))?;
    assert_eq!(
        pool.get_message(message.seq)
            .map_err(|error| format!("read shared message: {error:?}"))?
            .data,
        json!({"message":"hello"})
    );

    let other_dir = temp.path().join("other-pools");
    let other = TestServer::start_with_args_and_scheme(&other_dir, &[], "https");
    let other_key = other.access_key();
    access::connect(&other.base_url, other_key)?;
    assert_eq!(access::status(&other.base_url)?.accepted, Some(true));
    assert_eq!(access::status(&url)?.accepted, Some(true));

    let independent_client = RemoteClient::with_access_key(&url, replacement_key)?;
    access::disconnect(&url)?;
    assert!(independent_client.list_pools().is_ok());
    assert!(
        saved_client.list_pools().is_err(),
        "an existing saved client must observe disconnect"
    );
    assert!(!access::status(&url)?.credentials_saved);
    assert_eq!(access::status(&other.base_url)?.accepted, Some(true));
    access::connect(&url, replacement_key)?;
    assert!(saved_client.list_pools().is_ok());

    drop(server);
    access::disconnect(&url)?;
    let cli_disconnect = std::process::Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args(["access", "disconnect", &url])
        .env("PLASMITE_ACCESS_HOME", &client_home)
        .arg("--json")
        .output()?;
    assert!(
        cli_disconnect.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_disconnect.stderr)
    );
    let disconnect_reply: Value = serde_json::from_slice(&cli_disconnect.stdout)?;
    assert_eq!(disconnect_reply["credentials_saved"], false);
    assert!(!access::status(&url)?.credentials_saved);
    assert_eq!(access::status(&other.base_url)?.accepted, Some(true));

    let restarted = TestServer::start_with_args_and_scheme(&pool_dir, &[], "https");
    assert!(!access::status(&restarted.base_url)?.credentials_saved);
    assert!(
        RemoteClient::new(&restarted.base_url)?
            .list_pools()
            .is_err()
    );
    access::connect(&restarted.base_url, replacement_key)?;
    let saved = RemoteClient::new(&restarted.base_url)?;
    let restored_pool = saved
        .open_pool(&pool_ref)
        .map_err(|error| format!("reopen pool after server restart: {error:?}"))?;
    let restored_message = restored_pool
        .get_message(message.seq)
        .map_err(|error| format!("read message after server restart: {error:?}"))?;
    assert_eq!(restored_message.data, json!({"message":"hello"}));
    saved
        .delete_pool(&pool_ref)
        .map_err(|error| format!("delete pool after server restart: {error:?}"))?;

    let wrong_host_dir = temp.path().join("wrong-host");
    let wrong_host_cert = temp.path().join("wrong-host-cert.pem");
    let wrong_host_key = temp.path().join("wrong-host-key.pem");
    let cert = rcgen::Certificate::from_params(rcgen::CertificateParams::new(vec![
        "elsewhere.example".into(),
    ]))?;
    std::fs::write(&wrong_host_cert, cert.serialize_pem()?)?;
    std::fs::write(&wrong_host_key, cert.serialize_private_key_pem())?;
    let wrong_host = TestServer::start_with_args_and_scheme(
        &wrong_host_dir,
        &[
            "--tls-cert",
            wrong_host_cert.to_str().unwrap(),
            "--tls-key",
            wrong_host_key.to_str().unwrap(),
        ],
        "https",
    );
    assert!(access::connect(&wrong_host.base_url, wrong_host.access_key()).is_err());

    let expired_dir = temp.path().join("expired");
    let expired_cert = temp.path().join("expired-cert.pem");
    let expired_key = temp.path().join("expired-key.pem");
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]);
    params.not_before = time::OffsetDateTime::from_unix_timestamp(1_577_836_800)?;
    params.not_after = time::OffsetDateTime::from_unix_timestamp(1_609_459_200)?;
    let cert = rcgen::Certificate::from_params(params)?;
    std::fs::write(&expired_cert, cert.serialize_pem()?)?;
    std::fs::write(&expired_key, cert.serialize_private_key_pem())?;
    let expired = TestServer::start_with_args_and_scheme(
        &expired_dir,
        &[
            "--tls-cert",
            expired_cert.to_str().unwrap(),
            "--tls-key",
            expired_key.to_str().unwrap(),
        ],
        "https",
    );
    assert!(access::connect(&expired.base_url, expired.access_key()).is_err());

    let target = TcpListener::bind("127.0.0.1:0")?;
    target.set_nonblocking(true)?;
    let redirect = TcpListener::bind("127.0.0.1:0")?;
    let redirect_url = format!("https://localhost:{}", redirect.local_addr()?.port());
    let cert =
        rcgen::Certificate::from_params(rcgen::CertificateParams::new(vec!["localhost".into()]))?;
    let fingerprint = access::spki_fingerprint(&cert.serialize_der()?)?;
    let key = format!("pk1.{fingerprint}.{}", "1".repeat(64));
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                cert.serialize_der()?,
            )],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                cert.serialize_private_key_der(),
            )),
        )?;
    let location = format!(
        "http://127.0.0.1:{}/v0/access/check",
        target.local_addr()?.port()
    );
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let responder = std::thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let (socket, _) = redirect.accept()?;
            socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
            let mut stream =
                rustls::StreamOwned::new(rustls::ServerConnection::new(Arc::new(tls))?, socket);
            let mut request_start = [0u8; 1];
            stream.read_exact(&mut request_start)?;
            stream.write_all(response.as_bytes())?;
            stream.flush()?;
            Ok(())
        },
    );
    assert!(access::connect(&redirect_url, &key).is_err());
    responder.join().expect("redirect responder")?;
    assert!(
        target.accept().is_err(),
        "redirect target received the access secret"
    );
    Ok(())
}
