//! Browser sessions use the same access record and pool operations as native clients.

use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::{Value, json};
use std::sync::Arc;

#[allow(dead_code)] // The shared fixture includes helpers for other integration tests.
#[path = "support/server.rs"]
mod server;
use server::TestServer;

#[tokio::test]
async fn browser_writes_accept_http2_authority() -> Result<(), Box<dyn std::error::Error>> {
    use axum::body::Body;
    use hyper::Request;
    use hyper::client::conn::http2;
    use hyper::header::{CONTENT_TYPE, COOKIE, HOST, ORIGIN, SET_COOKIE};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tokio_rustls::TlsConnector;

    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let identity: Value = serde_json::from_slice(&std::fs::read(
        temp.path().join(".plasmite-serve/identity.json"),
    )?)?;
    let cert_file = identity["cert_file"]
        .as_str()
        .ok_or("missing certificate")?;
    let cert = CertificateDer::pem_file_iter(temp.path().join(".plasmite-serve").join(cert_file))?
        .next()
        .ok_or("missing certificate PEM")??;
    let mut roots = RootCertStore::empty();
    roots.add(cert)?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h2".to_vec()];

    let origin = &server.remote_url;
    let port = url::Url::parse(origin)?.port().ok_or("missing port")?;
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let stream = TlsConnector::from(Arc::new(tls))
        .connect(ServerName::try_from("localhost")?, stream)
        .await?;
    let (mut sender, connection) =
        http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    tokio::spawn(connection);

    let login = Request::builder()
        .method("POST")
        .uri(format!("{origin}/v0/browser/session"))
        .header(ORIGIN, origin.as_str())
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"access_key": server.access_key()}).to_string(),
        ))?;
    let response = sender.send_request(login).await?;
    assert_eq!(response.status(), 200);
    let cookie = response
        .headers()
        .get(SET_COOKIE)
        .ok_or("missing session cookie")?
        .to_str()?
        .split(';')
        .next()
        .ok_or("missing session cookie value")?
        .to_owned();

    let create = Request::builder()
        .method("POST")
        .uri(format!("{origin}/v0/pools"))
        .header(ORIGIN, origin.as_str())
        .header(COOKIE, &cookie)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"pool":"http2-browser"}"#))?;
    assert!(sender.send_request(create).await?.status().is_success());

    let conflicting_host = Request::builder()
        .method("POST")
        .uri(format!("{origin}/v0/pools/http2-browser/append"))
        .header(ORIGIN, origin.as_str())
        .header(HOST, "evil.example")
        .header(COOKIE, &cookie)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"data":{"text":"forged"}}"#))?;
    assert!(
        !sender
            .send_request(conflicting_host)
            .await?
            .status()
            .is_success()
    );
    Ok(())
}

#[test]
fn browser_session_obeys_origin_logout_and_key_revocation() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start(temp.path())?;
    let access_key = server.access_key().to_owned();
    let identity: Value = serde_json::from_slice(&std::fs::read(
        temp.path().join(".plasmite-serve/identity.json"),
    )?)?;
    let cert_file = identity["cert_file"]
        .as_str()
        .ok_or("missing certificate")?;
    let cert = CertificateDer::pem_file_iter(temp.path().join(".plasmite-serve").join(cert_file))?
        .next()
        .ok_or("missing certificate PEM")??;
    let mut roots = RootCertStore::empty();
    roots.add(cert)?;
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let agent = ureq::AgentBuilder::new().tls_config(Arc::new(tls)).build();
    let origin = &server.remote_url;

    let login_url = format!("{origin}/v0/browser/session");
    let forged_login = agent
        .post(&login_url)
        .set("Origin", "https://evil.example")
        .send_json(json!({"access_key": server.access_key()}));
    assert!(matches!(forged_login, Err(ureq::Error::Status(403, _))));

    let login = agent
        .post(&login_url)
        .set("Origin", origin)
        .send_json(json!({"access_key": server.access_key()}))?;
    let cookie_header = login.header("set-cookie").ok_or("missing session cookie")?;
    for flag in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/v0"] {
        assert!(cookie_header.contains(flag), "missing {flag}");
    }
    let cookie = cookie_header
        .split(';')
        .next()
        .ok_or("missing cookie value")?;
    let pools_url = format!("{origin}/v0/pools");
    assert!(agent.get(&pools_url).set("Cookie", cookie).call().is_ok());
    let forged_write = agent
        .post(&pools_url)
        .set("Cookie", cookie)
        .set("Origin", "https://evil.example")
        .send_json(json!({"pool":"browser"}));
    assert!(matches!(forged_write, Err(ureq::Error::Status(403, _))));
    agent
        .post(&pools_url)
        .set("Cookie", cookie)
        .set("Origin", origin)
        .send_json(json!({"pool":"browser"}))?;
    let append_url = format!("{origin}/v0/pools/browser/append");
    let appended: Value = agent
        .post(&append_url)
        .set("Cookie", cookie)
        .set("Origin", origin)
        .send_json(json!({"data":{"text":"<img src=x onerror=alert(1)>"}}))?
        .into_json()?;
    let seq = appended["message"]["seq"]
        .as_u64()
        .ok_or("missing sequence")?;
    let read: Value = agent
        .get(&format!("{origin}/v0/pools/browser/messages/{seq}"))
        .set("Cookie", cookie)
        .call()?
        .into_json()?;
    assert_eq!(
        read["message"]["data"]["text"],
        "<img src=x onerror=alert(1)>"
    );

    drop(server);
    let server = TestServer::try_start(temp.path())?;
    let origin = &server.remote_url;
    let pools_url = format!("{origin}/v0/pools");
    let login_url = format!("{origin}/v0/browser/session");
    assert!(
        agent.get(&pools_url).set("Cookie", cookie).call().is_ok(),
        "browser session should survive a server restart"
    );

    let logout = agent
        .delete(&login_url)
        .set("Cookie", cookie)
        .set("Origin", origin)
        .call()?;
    assert!(
        logout
            .header("set-cookie")
            .unwrap_or_default()
            .contains("Max-Age=0")
    );
    assert!(matches!(
        agent.get(&pools_url).set("Cookie", cookie).call(),
        Err(ureq::Error::Status(401, _))
    ));
    let secret = access_key.rsplit('.').next().ok_or("missing key secret")?;
    assert!(
        agent
            .get(&pools_url)
            .set("Authorization", &format!("Bearer {secret}"))
            .call()
            .is_ok(),
        "logout must leave the access key valid"
    );
    let login = agent
        .post(&login_url)
        .set("Origin", origin)
        .send_json(json!({"access_key":access_key}))?;
    let cookie = login
        .header("set-cookie")
        .ok_or("missing renewed session cookie")?
        .split(';')
        .next()
        .ok_or("missing renewed cookie value")?;

    let status: Value = ureq::get(&format!("{}/v0/access/status", server.local_url))
        .call()?
        .into_json()?;
    let fingerprint = status["server_fingerprint"]
        .as_str()
        .ok_or("missing fingerprint")?;
    let forged_admin = ureq::post(&format!("{}/v0/access/invite", server.local_url))
        .set("Origin", "https://evil.example")
        .send_json(json!({"name":"forged","server_fingerprint":fingerprint}));
    assert!(matches!(forged_admin, Err(ureq::Error::Status(403, _))));
    let remote_admin = agent
        .post(&format!("{origin}/v0/access/invite"))
        .set("Origin", origin)
        .send_json(json!({"name":"remote","server_fingerprint":fingerprint}));
    assert!(matches!(remote_admin, Err(ureq::Error::Status(401, _))));
    let keys: Value = ureq::get(&format!("{}/v0/access/keys", server.local_url))
        .set("x-plasmite-server-fingerprint", fingerprint)
        .call()?
        .into_json()?;
    let id = keys["keys"][0]["id"].as_str().ok_or("missing key id")?;
    ureq::post(&format!("{}/v0/access/revoke", server.local_url))
        .send_json(json!({"id":id,"server_fingerprint":fingerprint}))?;
    let revoked = agent.get(&pools_url).set("Cookie", cookie).call();
    assert!(matches!(revoked, Err(ureq::Error::Status(401, _))));

    let access_page = ureq::get(&format!("{}/access", server.local_url)).call()?;
    assert!(access_page.into_string()?.contains("Create key"));
    let invited: Value = ureq::post(&format!("{}/v0/access/invite", server.local_url))
        .send_json(json!({"name":"page invitation","server_fingerprint":fingerprint}))?
        .into_json()?;
    assert!(
        invited["access_key"]
            .as_str()
            .is_some_and(|key| key.starts_with("pk1."))
    );

    assert!(matches!(
        agent.get(&format!("{origin}/access")).call(),
        Err(ureq::Error::Status(404, _))
    ));
    Ok(())
}
