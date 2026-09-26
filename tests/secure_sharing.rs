//! The native access key works across a server restart and a fresh client process.

use plasmite::api::{Durability, PoolOptions, PoolRef, RemoteClient, access};
use serde_json::{Value, json};

#[allow(dead_code)] // This test uses only part of the shared server fixture.
#[path = "support/server.rs"]
mod server;
use server::TestServer;

#[test]
fn native_key_connects_and_reuses_server_identity() -> Result<(), Box<dyn std::error::Error>> {
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
    let secret = key.rsplit('.').next().ok_or("missing secret")?;
    let wrong_pin = format!("pk1.{}.{}", "0".repeat(64), secret);
    assert!(access::connect(&url, &wrong_pin).is_err());
    let bad_secret = format!("pk1.{}.{}", &key[4..68], "0".repeat(64));
    assert!(access::connect(&url, &bad_secret).is_err());
    let unauthorised = RemoteClient::with_access_key(&url, &bad_secret)?;
    assert!(unauthorised.list_pools().is_err());
    assert_eq!(access::status(&url)?.accepted, Some(true));
    let client = RemoteClient::new(&url)?;
    let pool_ref = PoolRef::name("shared");
    client.create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = client.open_pool(&pool_ref)?;
    let message = pool.append_json_now(&json!({"message":"hello"}), &[], Durability::Fast)?;
    assert_eq!(
        pool.get_message(message.seq)?.data,
        json!({"message":"hello"})
    );

    let other_dir = temp.path().join("other-pools");
    let other = TestServer::start_with_args_and_scheme(&other_dir, &[], "https");
    let other_key = other.access_key();
    access::connect(&other.base_url, other_key)?;
    assert_eq!(access::status(&other.base_url)?.accepted, Some(true));
    assert_eq!(access::status(&url)?.accepted, Some(true));

    drop(server);
    let restarted = TestServer::start_with_args_and_scheme(&pool_dir, &[], "https");
    access::connect(&restarted.base_url, key)?;
    let saved = RemoteClient::new(&restarted.base_url)?;
    assert_eq!(
        saved.open_pool(&pool_ref)?.get_message(message.seq)?.data,
        json!({"message":"hello"})
    );
    saved.delete_pool(&pool_ref)?;

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
    Ok(())
}
