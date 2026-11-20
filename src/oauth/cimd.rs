//! Public OAuth client metadata discovery. Do not send credentials, follow redirects,
//! or use a proxy that could resolve untrusted hostnames independently.
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use url::{Host, Url};

const MAX_DOCUMENT_BYTES: u64 = 32 * 1024;
const MAX_CACHE_ENTRIES: usize = 128;
const MAX_CONCURRENT_FETCHES: usize = 8;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CACHE_AGE: u64 = 300;

#[derive(Clone, Copy)]
pub(super) enum MetadataError {
    Invalid,
    IdentityMismatch,
    Unavailable,
}

#[cfg(test)]
type Fixture = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

#[derive(Deserialize)]
pub(super) struct Document {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(flatten)]
    other: HashMap<String, Value>,
}

impl Document {
    pub(super) fn id_host(id: &str) -> Option<String> {
        document_url(id).ok()?.host_str().map(str::to_owned)
    }

    pub(super) fn name(&self) -> &str {
        &self.client_name
    }

    pub(super) fn allows_redirect(&self, redirect: &str) -> bool {
        safe_redirect(redirect)
            && self.redirect_uris.iter().any(|registered| {
                registered == redirect
                    || native_redirect_without_port(registered)
                        .zip(native_redirect_without_port(redirect))
                        .is_some_and(|(registered, redirect)| registered == redirect)
            })
    }

    pub(super) fn permits_refresh(&self) -> bool {
        self.grant_types
            .as_ref()
            .is_some_and(|grants| grants.iter().any(|grant| grant == "refresh_token"))
    }
}

pub(super) struct Resolver {
    cache: Mutex<HashMap<String, (Instant, Arc<Document>)>>,
    slots: Arc<Semaphore>,
    #[cfg(test)]
    fixture: Option<Fixture>,
}

impl Default for Resolver {
    fn default() -> Self {
        Self {
            cache: Mutex::default(),
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES)),
            #[cfg(test)]
            fixture: None,
        }
    }
}

impl Resolver {
    pub(super) async fn lookup(&self, id: &str) -> Result<Arc<Document>, MetadataError> {
        let url = document_url(id)?;
        {
            let mut cache = self.cache.lock().map_err(|_| MetadataError::Unavailable)?;
            cache.retain(|_, (expiry, _)| *expiry > Instant::now());
            if let Some((_, doc)) = cache.get(id) {
                return Ok(doc.clone());
            }
        }
        #[cfg(test)]
        if let Some(fixture) = &self.fixture {
            return parse_document(id, &fixture(id)).map(Arc::new);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| MetadataError::Unavailable)?;
        let requested = id.to_owned();
        let (doc, ttl) = tokio::time::timeout(
            FETCH_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                // Keep the slot until this operation actually exits, even if DNS
                // takes longer than the request's deadline.
                let _permit = permit;
                fetch_document(&requested, &url)
            }),
        )
        .await
        .map_err(|_| MetadataError::Unavailable)?
        .map_err(|_| MetadataError::Unavailable)??;
        let doc = Arc::new(doc);
        if ttl > 0 {
            let mut cache = self.cache.lock().map_err(|_| MetadataError::Unavailable)?;
            if cache.len() >= MAX_CACHE_ENTRIES
                && let Some(key) = cache
                    .iter()
                    .min_by_key(|(_, (expiry, _))| expiry)
                    .map(|(key, _)| key.clone())
            {
                cache.remove(&key);
            }
            cache.insert(
                id.to_owned(),
                (Instant::now() + Duration::from_secs(ttl), doc.clone()),
            );
        }
        Ok(doc)
    }

    #[cfg(test)]
    pub(super) fn with_fixture(fixture: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        Self {
            fixture: Some(Arc::new(fixture)),
            ..Self::default()
        }
    }
}

fn document_url(id: &str) -> Result<Url, MetadataError> {
    if id.len() > 2048
        || !id.starts_with("https://")
        || id
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(MetadataError::Invalid);
    }
    let url = Url::parse(id).map_err(|_| MetadataError::Invalid)?;
    let Some(Host::Domain(host)) = url.host() else {
        return Err(MetadataError::Invalid);
    };
    // Only named public HTTPS endpoints on the standard port are fetched.
    if !host.trim_end_matches('.').contains('.')
        || host.trim_end_matches('.').ends_with(".localhost")
        || host.trim_end_matches('.').ends_with(".local")
        || !url.username().is_empty()
        || url.password().is_some()
        || id
            .split('/')
            .nth(2)
            .is_some_and(|authority| authority.contains('@') || authority.contains('\\'))
        || url.port_or_known_default() != Some(443)
        || url.path() == "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MetadataError::Invalid);
    }
    Ok(url)
}

fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => !matches!(
            ip.octets(),
            [0 | 10 | 127 | 224..=255, ..]
                | [100, 64..=127, ..]
                | [169, 254, ..]
                | [172, 16..=31, ..]
                | [192, 0, 0 | 2, _]
                | [192, 31, 196, _]
                | [192, 52, 193, _]
                | [192, 88, 99, _]
                | [192, 168, ..]
                | [192, 175, 48, _]
                | [198, 18 | 19, ..]
                | [198, 51, 100, _]
                | [203, 0, 113, _]
        ),
        // Only global unicast; excluded ranges in the IANA special-purpose registry.
        IpAddr::V6(ip) => {
            matches!(ip.segments()[0], 0x2000..=0x3fff)
                && !matches!(
                    ip.segments(),
                    [0x2001, 0..=0x1ff | 0x0db8, ..]
                        | [0x2002 | 0x3fff, ..]
                        | [0x2620, 0x004f, 0x8000, ..]
                )
        }
    }
}

fn resolve_public(authority: &str) -> io::Result<Vec<SocketAddr>> {
    let addresses: Vec<_> = authority.to_socket_addrs()?.take(17).collect();
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses.iter().any(|ip| !public_address(ip.ip()))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "client metadata must resolve to public addresses",
        ));
    }
    Ok(addresses)
}

fn fetch_document(id: &str, url: &Url) -> Result<(Document, u64), MetadataError> {
    // The resolver returns the actual addresses used by this connection. There
    // is no second DNS lookup between validation and connect.
    let agent = ureq::AgentBuilder::new()
        .resolver(resolve_public)
        .redirects(0)
        .try_proxy_from_env(false)
        .timeout(FETCH_TIMEOUT)
        .build();
    let response = match agent
        .get(url.as_str())
        .set("Accept", "application/json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::Status(500..=599, _)) => return Err(MetadataError::Unavailable),
        Err(ureq::Error::Transport(error))
            if error.kind() == ureq::ErrorKind::Dns
                || error.kind() == ureq::ErrorKind::ConnectionFailed =>
        {
            return Err(MetadataError::Unavailable);
        }
        Err(_) => return Err(MetadataError::Invalid),
    };
    if response.status() != 200
        || !response.header("Content-Type").is_some_and(|content_type| {
            content_type
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(MetadataError::Invalid);
    }
    let ttl = cache_lifetime(response.header("Cache-Control"), response.header("Age"));
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| MetadataError::Unavailable)?;
    parse_document(id, &body).map(|document| (document, ttl))
}

fn cache_lifetime(control: Option<&str>, age: Option<&str>) -> u64 {
    let mut ttl = 60;
    for directive in control.unwrap_or("").split(',').map(str::trim) {
        let (name, value) = directive.split_once('=').unwrap_or((directive, ""));
        if name.eq_ignore_ascii_case("no-store") || name.eq_ignore_ascii_case("no-cache") {
            return 0;
        }
        if name.eq_ignore_ascii_case("max-age") {
            ttl = value
                .trim_matches('"')
                .parse::<u64>()
                .unwrap_or(0)
                .min(MAX_CACHE_AGE);
        }
    }
    ttl.saturating_sub(age.and_then(|age| age.parse().ok()).unwrap_or(0))
}

fn parse_document(id: &str, bytes: &[u8]) -> Result<Document, MetadataError> {
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(MetadataError::Invalid);
    }
    let doc: Document = serde_json::from_slice(bytes).map_err(|_| MetadataError::Invalid)?;
    if doc.client_id != id {
        return Err(MetadataError::IdentityMismatch);
    }
    // Public clients only. Additional advertised methods can be ignored when
    // 'none' is explicitly supported; never accept a shared secret.
    let methods = doc.token_endpoint_auth_methods_supported.as_deref();
    let method = doc.token_endpoint_auth_method.as_deref();
    let public = methods.map_or(method.is_none_or(|method| method == "none"), |methods| {
        methods.iter().any(|method| method == "none")
    });
    let allowed_auth_method = |method: &str| matches!(method, "none" | "private_key_jwt");
    if !public
        || !method.is_none_or(allowed_auth_method)
        || methods.is_some_and(|methods| !methods.iter().all(|method| allowed_auth_method(method)))
        || [
            "client_secret",
            "client_secret_expires_at",
            "software_statement",
            "jwks",
        ]
        .iter()
        .any(|name| doc.other.contains_key(*name))
        || doc.client_name.trim().is_empty()
        || doc.client_name.len() > 128
        || doc.client_name.chars().any(char::is_control)
        || doc.redirect_uris.is_empty()
        || doc.redirect_uris.len() > 8
        || doc.redirect_uris.iter().any(|uri| !safe_redirect(uri))
        || doc
            .grant_types
            .as_ref()
            .is_some_and(|grants| !grants.iter().any(|grant| grant == "authorization_code"))
        || doc
            .response_types
            .as_ref()
            .is_some_and(|types| !types.iter().any(|kind| kind == "code"))
    {
        return Err(MetadataError::Invalid);
    }
    Ok(doc)
}

fn safe_redirect(uri: &str) -> bool {
    uri.len() <= 2048
        && !uri
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == b'\\')
        && uri
            .split('/')
            .nth(2)
            .is_some_and(|authority| !authority.contains('@'))
        && super::valid_redirect(uri)
        && Url::parse(uri).is_ok_and(|url| {
            !url.query_pairs()
                .any(|(key, _)| matches!(key.as_ref(), "code" | "error" | "state" | "iss"))
        })
}

fn native_redirect_without_port(uri: &str) -> Option<(&str, &str)> {
    let uri = uri.strip_prefix("http://")?;
    let (authority, tail) = uri.split_at(uri.find('/')?);
    // Accept the same loopback host and literal path/query. Only the port varies.
    // localhost is needed by native clients including Claude Code and Codex.
    for host in ["127.0.0.1", "[::1]", "localhost"] {
        if let Some(port) = authority.strip_prefix(host)
            && (port.is_empty()
                || port
                    .strip_prefix(':')
                    .and_then(|port| port.parse::<u16>().ok())
                    .is_some_and(|port| port != 0))
        {
            return Some((host, tail));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "https://client.example/oauth/metadata.json";

    #[test]
    fn accepts_a_public_client_with_exact_or_native_loopback_callbacks() {
        let doc = json!({
            "client_id": ID, "client_name": "Test document",
            "redirect_uris": ["https://client.example/callback", "http://localhost/callback", "http://127.0.0.1:1234/callback"],
            "token_endpoint_auth_method": "private_key_jwt",
            "token_endpoint_auth_methods_supported": ["private_key_jwt", "none"],
            "grant_types": ["authorization_code", "refresh_token"],
            "jwks_uri": "https://client.example/keys.json"
        });
        let doc = parse_document(ID, doc.to_string().as_bytes())
            .unwrap_or_else(|_| panic!("valid metadata"));
        assert!(doc.permits_refresh());
        for redirect in [
            "https://client.example/callback",
            "http://localhost:43123/callback",
            "http://127.0.0.1:43124/callback",
        ] {
            assert!(doc.allows_redirect(redirect), "{redirect}");
        }
        for redirect in [
            "https://client.example:444/callback",
            "https://client.example/callback?iss=x",
            "http://localhost:42/elsewhere",
            "http://localhost:42/callback?q=changed",
        ] {
            assert!(!doc.allows_redirect(redirect), "{redirect}");
        }
    }

    #[test]
    fn rejects_unsafe_urls_and_unusable_metadata() {
        for id in [
            "http://client.example/doc",
            "https://127.0.0.1/doc",
            "https://[::1]/doc",
            "https://metadata.local/doc",
            "https://client.example",
            "https://client.example/doc?x=1",
            "https://client.example/doc#",
            "https://user@client.example/doc",
            "https://client.example:8443/doc",
        ] {
            assert!(document_url(id).is_err(), "{id}");
        }
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.0.2.1",
            "192.168.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
        ] {
            assert!(!public_address(address.parse().unwrap()), "{address}");
        }
        assert!(public_address("1.0.0.1".parse().unwrap()));
        assert!(public_address("2606:4700::1".parse().unwrap()));
        let valid = json!({ "client_id": ID, "client_name": "Release check", "redirect_uris": ["https://client.example/callback"] });
        let mut bad_id = valid.clone();
        bad_id["client_id"] = json!("https://other.example/doc");
        assert!(matches!(
            parse_document(ID, bad_id.to_string().as_bytes()),
            Err(MetadataError::IdentityMismatch)
        ));
        for (field, misconfigured) in [
            ("redirect_uris", json!(["http://client.example/callback"])),
            ("client_name", json!("")),
            ("token_endpoint_auth_method", json!("private_key_jwt")),
            ("grant_types", json!(["client_credentials"])),
            ("client_secret", Value::Null),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = misconfigured;
            assert!(
                matches!(
                    parse_document(ID, invalid.to_string().as_bytes()),
                    Err(MetadataError::Invalid)
                ),
                "{field}"
            );
        }
        assert!(parse_document(ID, b"not JSON").is_err());
        assert!(parse_document(ID, &vec![b' '; MAX_DOCUMENT_BYTES as usize + 1]).is_err());
        assert!(
            parse_document(ID, valid.to_string().as_bytes())
                .is_ok_and(|doc| !doc.permits_refresh())
        );
    }

    #[test]
    fn bounded_cache_respects_freshness_and_disallows_storing_no_store() {
        assert_eq!(cache_lifetime(Some("max-age=1200"), Some("100")), 200);
        assert_eq!(cache_lifetime(Some("max-age=20, no-store"), None), 0);
        assert_eq!(cache_lifetime(Some("no-cache, max-age=20"), None), 0);
        assert_eq!(cache_lifetime(Some("max-age=20"), Some("100")), 0);
    }
}
