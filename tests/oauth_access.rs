//! OAuth discovery and rejection of legacy client registration on the live HTTPS router.

use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde_json::{Value, json};
use std::sync::Arc;

#[allow(dead_code)] // This test uses only the trusted HTTPS test server.
#[path = "support/server.rs"]
mod server;
use server::TestServer;

#[test]
fn direct_https_oauth_advertises_metadata_clients_only() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let server = TestServer::try_start_oauth(temp.path())?;
    let issuer = &server.remote_url;
    let resource = format!("{issuer}/mcp");
    let agent = trusted_agent(temp.path())?;
    let challenge = agent
        .post(&resource)
        .set("Accept", "application/json, text/event-stream")
        .set("MCP-Protocol-Version", "2026-07-28")
        .set("Mcp-Method", "server/discover")
        .send_json(json!({"jsonrpc":"2.0", "id":1, "method":"server/discover", "params":{
            "_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}
        }}));
    let Err(ureq::Error::Status(401, challenge)) = challenge else {
        return Err("expected authentication challenge".into());
    };
    assert_eq!(
        challenge.header("www-authenticate"),
        Some(
            format!(
                "Bearer resource_metadata=\"{issuer}/.well-known/oauth-protected-resource/mcp\""
            )
            .as_str()
        )
    );
    let protected: Value = agent
        .get(&format!(
            "{issuer}/.well-known/oauth-protected-resource/mcp"
        ))
        .call()?
        .into_json()?;
    assert_eq!(protected["resource"], resource);
    assert_eq!(protected["authorization_servers"], json!([issuer]));
    let metadata: Value = agent
        .get(&format!("{issuer}/.well-known/oauth-authorization-server"))
        .call()?
        .into_json()?;
    assert_eq!(metadata["issuer"], *issuer);
    assert_eq!(metadata["client_id_metadata_document_supported"], true);
    assert_eq!(
        metadata["authorization_response_iss_parameter_supported"],
        true
    );
    assert_eq!(
        metadata["code_challenge_methods_supported"],
        json!(["S256"])
    );
    assert!(metadata.get("registration_endpoint").is_none());
    assert!(matches!(
        agent
            .post(&format!("{issuer}/oauth/register"))
            .send_json(json!({
                "client_name":"Legacy fixture", "redirect_uris":["http://127.0.0.1:41543/callback"]
            })),
        Err(ureq::Error::Status(404, _))
    ));
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", "old-public-client-id")
        .append_pair("resource", &resource)
        .append_pair("refresh_token", "synthetic")
        .finish();
    let Err(ureq::Error::Status(400, token_error)) = agent
        .post(&format!("{issuer}/oauth/token"))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&form)
    else {
        return Err("expected legacy OAuth client rejection".into());
    };
    assert_eq!(token_error.into_json::<Value>()?["error"], "invalid_client");
    Ok(())
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
