//! Direct MCP OAuth uses the same access key and survives a server restart.

use oauth_as::pkce::code_challenge_s256;
use plasmite::api::{RemoteClient, access};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use url::Url;

#[allow(dead_code)] // This test uses only the HTTPS and restart helpers.
#[path = "support/server.rs"]
mod server;
use server::TestServer;

#[test]
fn direct_mcp_approval_refresh_restart_and_revocation() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    unsafe { std::env::set_var("PLASMITE_ACCESS_HOME", temp.path().join("client")) };
    let server = TestServer::try_start_oauth(temp.path())?;
    let port = Url::parse(&server.remote_url)?
        .port()
        .ok_or("missing port")?;
    let agent = trusted_agent(temp.path())?;
    let issuer = server.remote_url.clone();
    let shared_key = server.access_key().to_owned();
    access::connect(&issuer, &shared_key)?;
    let native = RemoteClient::new(&issuer)?;
    native.list_pools()?;
    let resource = format!("{issuer}/mcp");
    let callback = "http://127.0.0.1:13579/callback";

    let unauthorized = agent
        .post(&resource)
        .send_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    let challenge = match unauthorized {
        Err(ureq::Error::Status(401, response)) => response,
        other => return Err(format!("expected OAuth challenge, got {other:?}").into()),
    };
    assert!(
        challenge
            .header("www-authenticate")
            .unwrap_or_default()
            .contains("resource_metadata=")
    );

    let metadata: Value = agent
        .get(&format!("{issuer}/.well-known/oauth-authorization-server"))
        .call()?
        .into_json()?;
    assert_eq!(metadata["issuer"], issuer);
    assert_eq!(
        metadata["code_challenge_methods_supported"],
        json!(["S256"])
    );
    let protected: Value = agent
        .get(&format!(
            "{issuer}/.well-known/oauth-protected-resource/mcp"
        ))
        .call()?
        .into_json()?;
    assert_eq!(protected["resource"], resource);

    let registered: Value = agent
        .post(&format!("{issuer}/oauth/register"))
        .send_json(json!({
            "client_name": "Test harness",
            "redirect_uris": [callback],
            "token_endpoint_auth_method": "none"
        }))?
        .into_json()?;
    let client_id = registered["client_id"]
        .as_str()
        .ok_or("missing client ID")?;
    let verifier = "A".repeat(43);
    let mut authorization = Url::parse(&format!("{issuer}/oauth/authorize"))?;
    authorization
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", callback)
        .append_pair("code_challenge", &code_challenge_s256(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", &resource)
        .append_pair("resource", &resource)
        .append_pair("state", "sentinel");
    let mut wrong_resource = authorization.clone();
    wrong_resource
        .query_pairs_mut()
        .append_pair("resource", &format!("{issuer}/other"));
    assert!(matches!(
        agent.get(wrong_resource.as_str()).call(),
        Err(ureq::Error::Status(400, _))
    ));
    let canceled_page = agent.get(authorization.as_str()).call()?;
    assert!(
        !canceled_page
            .header("content-security-policy")
            .unwrap_or_default()
            .contains("form-action")
    );
    let canceled_cookie = canceled_page
        .header("set-cookie")
        .ok_or("missing cancellation cookie")?
        .split(';')
        .next()
        .ok_or("missing cancellation cookie value")?
        .to_owned();
    let canceled_body = canceled_page.into_string()?;
    let canceled_request = canceled_body
        .split("name=\"request\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .ok_or("missing cancellation request")?;
    let denied = agent
        .post(&format!("{issuer}/oauth/approve"))
        .set("Origin", &issuer)
        .set("Cookie", &canceled_cookie)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form(&[
            ("request", canceled_request),
            ("decision", "deny"),
        ]))
        .map_err(|error| format!("cancel approval: {error:?}"))?;
    let denial = Url::parse(denied.header("location").ok_or("missing denial redirect")?)?;
    assert_eq!(
        denial
            .query_pairs()
            .find(|(key, _)| key == "error")
            .unwrap()
            .1,
        "access_denied"
    );
    assert!(denial.query_pairs().all(|(key, _)| key != "code"));
    let approval_page = agent.get(authorization.as_str()).call()?;
    let csrf_cookie = approval_page
        .header("set-cookie")
        .ok_or("missing CSRF cookie")?
        .split(';')
        .next()
        .ok_or("missing CSRF value")?
        .to_owned();
    let page = approval_page.into_string()?;
    let request_id = page
        .split("name=\"request\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .ok_or("missing approval request")?;
    let approval_form = form(&[("request", request_id), ("access_key", server.access_key())]);
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/approve"))
            .set("Origin", "https://evil.example")
            .set("Cookie", &csrf_cookie)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&approval_form),
        Err(ureq::Error::Status(403, _))
    ));
    let approved = agent
        .post(&format!("{issuer}/oauth/approve"))
        .set("Origin", &issuer)
        .set("Cookie", &csrf_cookie)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&approval_form)
        .map_err(|error| format!("approve access: {error:?}"))?;
    assert_eq!(approved.status(), 303);
    let callback_url = Url::parse(approved.header("location").ok_or("missing callback")?)?;
    assert_eq!(
        callback_url.origin().ascii_serialization(),
        "http://127.0.0.1:13579"
    );
    assert_eq!(
        callback_url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1,
        "sentinel"
    );
    let code = callback_url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .ok_or("missing code")?
        .1
        .into_owned();

    let wrong_callback = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", &code),
        ("redirect_uri", "http://127.0.0.1:13579/wrong"),
        ("code_verifier", &verifier),
        ("resource", &resource),
    ]);
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/token"))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&wrong_callback),
        Err(ureq::Error::Status(400, _))
    ));
    let code = approve_code(&agent, authorization.as_str(), &issuer, &shared_key)?;

    let exchange = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", &code),
        ("redirect_uri", callback),
        ("code_verifier", &verifier),
        ("resource", &resource),
    ]);
    let tokens: Value = agent
        .post(&format!("{issuer}/oauth/token"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&exchange)?
        .into_json()?;
    let access_token = tokens["access_token"]
        .as_str()
        .ok_or("missing access token")?;
    let refresh_token = tokens["refresh_token"]
        .as_str()
        .ok_or("missing refresh token")?
        .to_owned();
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/token"))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&exchange),
        Err(ureq::Error::Status(400, _))
    ));
    mcp_initialize_http(&agent, &resource, access_token)?;
    let accepted = mcp_list(&agent, &resource, access_token, 2)?;
    assert_eq!(accepted.status(), 200);
    let fed = mcp_call(
        &agent,
        &resource,
        access_token,
        "plasmite_feed",
        json!({"pool":"oauth-flow","data":{"text":"through direct MCP"},"create":true}),
        20,
    )?;
    assert_eq!(fed["result"]["isError"], Value::Null);
    assert_eq!(
        fed["result"]["structuredContent"]["message"]["data"]["text"],
        "through direct MCP"
    );
    let read = mcp_call(
        &agent,
        &resource,
        access_token,
        "plasmite_read",
        json!({"pool":"oauth-flow"}),
        21,
    )?;
    assert_eq!(
        read["result"]["structuredContent"]["messages"][0]["data"]["text"],
        "through direct MCP"
    );

    drop(server);
    let server = TestServer::try_start_oauth_at(temp.path(), port)?;
    assert_eq!(server.remote_url, issuer);
    let refresh_form = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", &refresh_token),
        ("resource", &resource),
    ]);
    let renewed: Value = agent
        .post(&format!("{issuer}/oauth/token"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&refresh_form)?
        .into_json()?;
    let second_refresh = renewed["refresh_token"]
        .as_str()
        .ok_or("missing second refresh token")?;
    let second_refresh_form = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", second_refresh),
        ("resource", &resource),
    ]);
    let twice_renewed: Value = agent
        .post(&format!("{issuer}/oauth/token"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&second_refresh_form)?
        .into_json()?;
    let twice_renewed_access = twice_renewed["access_token"]
        .as_str()
        .ok_or("missing twice-renewed access token")?;
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/token"))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&refresh_form),
        Err(ureq::Error::Status(400, _))
    ));
    assert!(
        matches!(
            mcp_list(&agent, &resource, twice_renewed_access, 3),
            Err(error) if matches!(*error, ureq::Error::Status(401, _))
        ),
        "refresh token from two rotations ago must revoke its token family"
    );

    let fresh_code = approve_code(&agent, authorization.as_str(), &issuer, &shared_key)?;
    let fresh_exchange = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", &fresh_code),
        ("redirect_uri", callback),
        ("code_verifier", &verifier),
        ("resource", &resource),
    ]);
    let fresh: Value = agent
        .post(&format!("{issuer}/oauth/token"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&fresh_exchange)?
        .into_json()?;
    let fresh_access = fresh["access_token"]
        .as_str()
        .ok_or("missing fresh access token")?;
    let fresh_refresh = fresh["refresh_token"]
        .as_str()
        .ok_or("missing fresh refresh token")?;
    assert_eq!(mcp_list(&agent, &resource, fresh_access, 4)?.status(), 200);

    agent
        .post(&format!("{issuer}/oauth/revoke"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form(&[("token", fresh_access), ("client_id", client_id)]))?;
    assert!(matches!(
        mcp_list(&agent, &resource, fresh_access, 5),
        Err(error) if matches!(*error, ureq::Error::Status(401, _))
    ));
    let ended_refresh = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", fresh_refresh),
        ("resource", &resource),
    ]);
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/token"))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&ended_refresh),
        Err(ureq::Error::Status(400, _))
    ));

    let active_code = approve_code(&agent, authorization.as_str(), &issuer, &shared_key)?;
    let active_exchange = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", &active_code),
        ("redirect_uri", callback),
        ("code_verifier", &verifier),
        ("resource", &resource),
    ]);
    let active: Value = agent
        .post(&format!("{issuer}/oauth/token"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&active_exchange)?
        .into_json()?;
    let active_access = active["access_token"]
        .as_str()
        .ok_or("missing active access token")?;
    let active_refresh = active["refresh_token"]
        .as_str()
        .ok_or("missing active refresh token")?;
    assert_eq!(mcp_list(&agent, &resource, active_access, 6)?.status(), 200);
    native.list_pools()?;
    let browser_login = agent
        .post(&format!("{issuer}/v0/browser/session"))
        .set("Origin", &issuer)
        .send_json(json!({"access_key":shared_key}))?;
    let browser_cookie = browser_login
        .header("set-cookie")
        .ok_or("missing browser cookie")?
        .split(';')
        .next()
        .ok_or("missing browser cookie value")?
        .to_owned();
    agent
        .get(&format!("{issuer}/v0/pools"))
        .set("Cookie", &browser_cookie)
        .call()?;
    let mut local_mcp = Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args(["mcp", "--remote", &issuer])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut mcp_input = local_mcp.stdin.take().ok_or("missing MCP stdin")?;
    let mut mcp_output = BufReader::new(local_mcp.stdout.take().ok_or("missing MCP stdout")?);
    mcp_initialize_local(&mut mcp_input, &mut mcp_output)?;
    let local_read = local_mcp_read(&mut mcp_input, &mut mcp_output, 30)?;
    assert_eq!(
        local_read["result"]["structuredContent"]["messages"][0]["data"]["text"],
        "through direct MCP"
    );
    let local_write = local_mcp_call(
        &mut mcp_input,
        &mut mcp_output,
        31,
        "plasmite_feed",
        json!({"pool":"oauth-flow","data":{"text":"through local stdio MCP"}}),
    )?;
    assert_eq!(
        local_write["result"]["structuredContent"]["message"]["data"]["text"],
        "through local stdio MCP"
    );
    access::disconnect(&issuer)?;
    let disconnected = local_mcp_read(&mut mcp_input, &mut mcp_output, 32)?;
    assert!(disconnected["result"]["isError"] == true);
    assert!(
        disconnected["result"]["structuredContent"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("plasmite access connect")),
        "disconnected MCP should tell the user how to reconnect: {disconnected}"
    );
    let replacement = Command::new(env!("CARGO_BIN_EXE_plasmite"))
        .args([
            "--dir",
            temp.path().to_str().ok_or("non-UTF-8 test directory")?,
            "access",
            "invite",
            "--name",
            "replacement",
        ])
        .output()?;
    assert!(replacement.status.success());
    let invitation: Value = serde_json::from_slice(&replacement.stdout)?;
    access::connect(
        &issuer,
        invitation["access_key"]
            .as_str()
            .ok_or("missing replacement key")?,
    )?;
    let replaced = local_mcp_read(&mut mcp_input, &mut mcp_output, 33)?;
    assert_eq!(
        replaced["result"]["structuredContent"]["messages"][0]["data"]["text"],
        "through direct MCP"
    );
    access::connect(&issuer, &shared_key)?;

    let keys: Value = ureq::get(&format!("{}/v0/access/keys", server.local_url))
        .set(
            "x-plasmite-server-fingerprint",
            server.access_key().split('.').nth(1).unwrap(),
        )
        .call()?
        .into_json()?;
    let key_id = keys["keys"]
        .as_array()
        .and_then(|keys| keys.iter().find(|key| key["name"] == "integration-test"))
        .and_then(|key| key["id"].as_str())
        .ok_or("missing shared key ID")?;
    ureq::post(&format!("{}/v0/access/revoke", server.local_url)).send_json(json!({
        "id": key_id,
        "server_fingerprint": server.access_key().split('.').nth(1).unwrap()
    }))?;
    assert!(matches!(
        mcp_list(&agent, &resource, active_access, 7),
        Err(error) if matches!(*error, ureq::Error::Status(401, _))
    ));
    assert!(native.list_pools().is_err());
    let local_revoked = local_mcp_read(&mut mcp_input, &mut mcp_output, 34)?;
    assert!(
        local_revoked["result"]["isError"] == true || local_revoked.get("error").is_some(),
        "local MCP must reject the revoked saved connection: {local_revoked}"
    );
    drop(mcp_input);
    local_mcp.wait()?;
    assert!(matches!(
        agent
            .get(&format!("{issuer}/v0/pools"))
            .set("Cookie", &browser_cookie)
            .call(),
        Err(ureq::Error::Status(401, _))
    ));
    RemoteClient::with_access_key(
        &issuer,
        invitation["access_key"]
            .as_str()
            .ok_or("missing replacement key")?,
    )?
    .list_pools()?;
    server.client()?.list_pools()?;
    let revoked_refresh = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", active_refresh),
        ("resource", &resource),
    ]);
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/token"))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&revoked_refresh),
        Err(ureq::Error::Status(400, _))
    ));
    Ok(())
}

fn approve_code(
    agent: &ureq::Agent,
    authorization_url: &str,
    issuer: &str,
    access_key: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let page = agent.get(authorization_url).call()?;
    let cookie = page
        .header("set-cookie")
        .ok_or("missing approval cookie")?
        .split(';')
        .next()
        .ok_or("missing approval cookie value")?
        .to_owned();
    let body = page.into_string()?;
    let request = body
        .split("name=\"request\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .ok_or("missing approval request")?;
    let approved = agent
        .post(&format!("{issuer}/oauth/approve"))
        .set("Origin", issuer)
        .set("Cookie", &cookie)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form(&[("request", request), ("access_key", access_key)]))
        .map_err(|error| format!("approve_code: {error:?}"))?;
    let callback = Url::parse(approved.header("location").ok_or("missing callback")?)?;
    assert_eq!(
        callback
            .query_pairs()
            .find(|(key, _)| key == "iss")
            .unwrap()
            .1,
        issuer
    );
    Ok(callback
        .query_pairs()
        .find(|(key, _)| key == "code")
        .ok_or("missing code")?
        .1
        .into_owned())
}

fn mcp_initialize_http(
    agent: &ureq::Agent,
    resource: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let reply: Value = agent
        .post(resource)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json, text/event-stream")
        .send_json(json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params": {
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"integration-test","version":"1"}
            }
        }))?
        .into_json()?;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    let initialized = agent
        .post(resource)
        .set("Authorization", &format!("Bearer {token}"))
        .set("MCP-Protocol-Version", "2025-11-25")
        .set("Accept", "application/json, text/event-stream")
        .send_json(json!({
            "jsonrpc":"2.0", "method":"notifications/initialized"
        }))?;
    assert_eq!(initialized.status(), 202);
    Ok(())
}

fn mcp_initialize_local(
    input: &mut impl Write,
    output: &mut impl BufRead,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(
        input,
        "{}",
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params": {
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"integration-test","version":"1"}
            }
        })
    )?;
    input.flush()?;
    let mut line = String::new();
    output.read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line)?;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )?;
    input.flush()?;
    Ok(())
}

fn mcp_list(
    agent: &ureq::Agent,
    resource: &str,
    token: &str,
    id: i64,
) -> Result<ureq::Response, Box<ureq::Error>> {
    agent
        .post(resource)
        .set("Authorization", &format!("Bearer {token}"))
        .set("MCP-Protocol-Version", "2025-11-25")
        .set("Accept", "application/json, text/event-stream")
        .send_json(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/list"
        }))
        .map_err(Box::new)
}

fn mcp_call(
    agent: &ureq::Agent,
    resource: &str,
    token: &str,
    name: &str,
    arguments: Value,
    id: i64,
) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(agent
        .post(resource)
        .set("Authorization", &format!("Bearer {token}"))
        .set("MCP-Protocol-Version", "2025-11-25")
        .set("Accept", "application/json, text/event-stream")
        .send_json(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }))?
        .into_json()?)
}

fn local_mcp_read(
    input: &mut impl Write,
    output: &mut impl BufRead,
    id: i64,
) -> Result<Value, Box<dyn std::error::Error>> {
    local_mcp_call(
        input,
        output,
        id,
        "plasmite_read",
        json!({"pool":"oauth-flow"}),
    )
}

fn local_mcp_call(
    input: &mut impl Write,
    output: &mut impl BufRead,
    id: i64,
    name: &str,
    arguments: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let request = json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    });
    writeln!(input, "{request}")?;
    input.flush()?;
    let mut line = String::new();
    if output.read_line(&mut line)? == 0 {
        return Err("local MCP process ended before replying".into());
    }
    Ok(serde_json::from_str(&line)?)
}

fn trusted_agent(pool_dir: &std::path::Path) -> Result<ureq::Agent, Box<dyn std::error::Error>> {
    let identity: Value = serde_json::from_slice(&std::fs::read(
        pool_dir.join(".plasmite-serve/identity.json"),
    )?)?;
    let cert_file = identity["cert_file"]
        .as_str()
        .ok_or("missing certificate")?;
    let cert = CertificateDer::pem_file_iter(pool_dir.join(".plasmite-serve").join(cert_file))?
        .next()
        .ok_or("missing certificate PEM")??;
    let mut roots = RootCertStore::empty();
    roots.add(cert)?;
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(ureq::AgentBuilder::new()
        .tls_config(Arc::new(tls))
        .redirects(0)
        .build())
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
}
