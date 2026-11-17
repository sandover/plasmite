//! Purpose: Provide an HTTP client for the Plasmite 2.0 protocol.
//! Exports: remote client and pool handles, and the shared read interface.
//! Role: Transport-agnostic client that mirrors local pool operations remotely.
//! Invariants: Request and response envelopes align with spec/v2/SPEC.md.
//! Invariants: Pool refs resolve to a base URL + pool identifier (name only).
//! Invariants: Follow streams are JSONL (messages) or framed Lite3 bytes (fast path).
#![allow(clippy::result_large_err)]

use super::{Message, Meta, PoolRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::{
    Bounds, Durability, PoolAgeMetrics, PoolInfo, PoolMetrics, PoolOptions, PoolUtilization,
};
use crate::interface_wire::{
    HttpErrorEnvelope, HttpErrorWire, MessageWire, PoolInfoWire, PoolMetricsWire,
    error_kind_from_status,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod cancellable_read;
mod read_v2;
pub use read_v2::{RemoteLite3Read, RemoteRead};

/// How long a one-shot request may take, start to finish. Streams have no deadline: a
/// follower on a quiet pool waits as long as the pool stays quiet.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REMOTE_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
use url::{Host, Url};

type ApiResult<T> = Result<T, Error>;
type TailReader = BufReader<Box<dyn Read + Send + Sync>>;

#[derive(Clone)]
pub struct RemoteClient {
    inner: Arc<RemoteClientInner>,
}

struct RemoteClientInner {
    base_url: Url,
    credentials: CredentialSource,
    agent: ureq::Agent,
    saved_agent: Mutex<Option<([u8; 32], ureq::Agent)>>,
}

enum CredentialSource {
    Saved,
    Explicit(super::access::AccessKey),
    None,
}

#[derive(Clone)]
pub struct RemotePool {
    client: RemoteClient,
    base_url: Url,
    pool: String,
    pool_ref: PoolRef,
}

#[derive(Clone, Debug)]
pub struct RemoteLite3Frame {
    pub seq: u64,
    pub time: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
struct ResolvedPool {
    base_url: Url,
    pool: String,
}

#[derive(Deserialize)]
struct PoolEnvelope {
    pool: PoolInfoWire,
}

#[derive(Deserialize)]
struct PoolsEnvelope {
    pools: Vec<PoolInfoWire>,
}

#[derive(Deserialize)]
struct MessageEnvelope {
    message: MessageWire,
}

#[derive(Serialize)]
struct CreatePoolRequest<'a> {
    pool: &'a str,
    size_bytes: u64,
}

#[derive(Serialize)]
struct OpenPoolRequest {
    create: bool,
}

#[derive(Serialize)]
struct AppendRequest<'a> {
    data: &'a Value,
    tags: &'a [String],
    durability: &'a str,
}

impl RemoteClient {
    pub fn new(base_url: impl Into<String>) -> ApiResult<Self> {
        let base_url = normalize_base_url(base_url.into())?;
        if super::access::load_saved_key(base_url.as_str())?.is_some()
            && base_url.scheme() != "https"
        {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("saved native connections must use HTTPS"));
        }
        Ok(Self::using_saved_connections(base_url))
    }

    /// Create an in-memory native client from an access key without saving it.
    pub fn with_access_key(base_url: impl Into<String>, access_key: &str) -> ApiResult<Self> {
        let base_url = super::access::secure_destination(&base_url.into())?;
        let key = super::access::AccessKey::parse(access_key)?;
        Ok(Self::from_access_key(base_url, key))
    }

    pub fn base_url(&self) -> &Url {
        &self.inner.base_url
    }

    pub(super) fn from_access_key(base_url: Url, key: super::access::AccessKey) -> Self {
        let fingerprint = *key.spki_fingerprint();
        Self {
            inner: Arc::new(RemoteClientInner {
                base_url,
                credentials: CredentialSource::Explicit(key),
                agent: super::access::access_agent(Some(fingerprint)),
                saved_agent: Mutex::new(None),
            }),
        }
    }

    pub(super) fn for_access_probe(base_url: Url) -> Self {
        Self {
            inner: Arc::new(RemoteClientInner {
                base_url,
                credentials: CredentialSource::None,
                agent: super::access::access_agent(None),
                saved_agent: Mutex::new(None),
            }),
        }
    }

    fn using_saved_connections(base_url: Url) -> Self {
        Self {
            inner: Arc::new(RemoteClientInner {
                base_url,
                credentials: CredentialSource::Saved,
                agent: ureq::AgentBuilder::new().redirects(0).build(),
                saved_agent: Mutex::new(None),
            }),
        }
    }

    pub(super) fn check_access(&self) -> ApiResult<bool> {
        #[derive(Deserialize)]
        struct AccessCheckResponse {
            accepted: bool,
        }

        let url = build_url(&self.inner.base_url, &["v0", "access", "check"])?;
        let result = self
            .request("GET", &url)?
            .set("Accept", "application/json")
            .call();
        match result {
            Ok(response) => Ok(read_json_response::<AccessCheckResponse>(response)?.accepted),
            Err(ureq::Error::Status(401, _response)) => Ok(false),
            Err(ureq::Error::Status(300..=399, _response)) => Err(Error::new(ErrorKind::Io)
                .with_message(format!("server redirected the access check at {url}"))
                .with_hint("Use the final HTTPS server URL; Plasmite does not forward credentials across redirects.")),
            Err(ureq::Error::Status(code, response)) => Err(parse_error_response(code, response)),
            Err(ureq::Error::Transport(error)) => Err(Error::new(ErrorKind::Io)
                .with_message(format!("failed to reach or verify {url}"))
                .with_hint("Check the exact HTTPS hostname and port, network route, certificate name and validity, and the key's SPKI fingerprint. For Tailscale, check MagicDNS and port permissions. A TLS-terminating proxy must present the certificate identified by --front-cert; raw TCP forwarding preserves Plasmite's certificate.")
                .with_source(error)),
        }
    }

    pub fn create_pool(&self, pool_ref: &PoolRef, options: PoolOptions) -> ApiResult<RemotePool> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let payload = CreatePoolRequest {
            pool: &resolved.pool,
            size_bytes: options.file_size,
        };
        let url = build_url(&resolved.base_url, &["v2", "pools"])?;
        let _: PoolEnvelope = self
            .request_json("POST", &url, &payload)
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(self.pool_handle(pool_ref, resolved))
    }

    pub fn open_pool(&self, pool_ref: &PoolRef, create: bool) -> ApiResult<RemotePool> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let payload = OpenPoolRequest { create };
        let url = build_url(&resolved.base_url, &["v2", "pools", &resolved.pool, "open"])?;
        let _envelope: PoolEnvelope = self
            .request_json("POST", &url, &payload)
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(self.pool_handle(pool_ref, resolved))
    }

    fn pool_handle(&self, pool_ref: &PoolRef, resolved: ResolvedPool) -> RemotePool {
        RemotePool {
            client: self.clone(),
            base_url: resolved.base_url,
            pool: resolved.pool,
            pool_ref: pool_ref.clone(),
        }
    }

    pub fn pool_info(&self, pool_ref: &PoolRef) -> ApiResult<PoolInfo> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let url = build_url(&resolved.base_url, &["v2", "pools", &resolved.pool])?;
        let envelope: PoolEnvelope = self
            .request_json::<(), _>("GET", &url, &())
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(pool_info_from_remote(&resolved.pool, envelope.pool))
    }

    pub fn list_pools(&self) -> ApiResult<Vec<PoolInfo>> {
        let url = build_url(&self.inner.base_url, &["v2", "pools"])?;
        let envelope: PoolsEnvelope = self.request_json("GET", &url, &())?;
        Ok(envelope
            .pools
            .into_iter()
            .map(|pool| {
                let name = pool.name.clone().unwrap_or_default();
                pool_info_from_remote(&name, pool)
            })
            .collect())
    }

    pub fn delete_pool(&self, pool_ref: &PoolRef) -> ApiResult<()> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let url = build_url(&resolved.base_url, &["v2", "pools", &resolved.pool])?;
        let _value: serde_json::Value = self
            .request_json::<(), _>("DELETE", &url, &())
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(())
    }

    fn resolve_pool_ref(&self, pool_ref: &PoolRef) -> ApiResult<ResolvedPool> {
        match pool_ref {
            PoolRef::Name(name) => {
                ensure_pool_name(name)?;
                Ok(ResolvedPool {
                    base_url: self.inner.base_url.clone(),
                    pool: name.clone(),
                })
            }
            PoolRef::Path(_) => Err(Error::new(ErrorKind::Usage)
                .with_message("remote pool refs must use pool names, not filesystem paths")),
            PoolRef::Uri(uri) => {
                let resolved = parse_pool_uri(uri)?;
                if resolved.base_url.origin() != self.inner.base_url.origin() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("pool URL names a different server")
                        .with_hint("Create a RemoteClient for that server's URL."));
                }
                ensure_pool_name(&resolved.pool)?;
                Ok(resolved)
            }
        }
    }

    fn request_json<T, R>(&self, method: &str, url: &Url, body: &T) -> ApiResult<R>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let request = self.request(method, url)?.set("Accept", "application/json");
        // These endpoints have no request body. An unread DELETE body can make
        // the server close the connection while the client reads its response.
        let response = if method == "GET" || method == "DELETE" {
            request.call()
        } else {
            let payload = serde_json::to_string(body).map_err(|err| {
                Error::new(ErrorKind::Internal)
                    .with_message("failed to encode request json")
                    .with_source(err)
            })?;
            request
                .set("Content-Type", "application/json")
                .send_string(&payload)
        };

        let response = if url.path().starts_with("/v2/pools") {
            read_v2::request_result_v2(response)?
        } else {
            request_result(response)?
        };
        read_json_response(response)
    }

    fn request(&self, method: &str, url: &Url) -> ApiResult<ureq::Request> {
        Ok(self.open(method, url)?.timeout(REQUEST_TIMEOUT))
    }

    fn access_key(&self) -> ApiResult<Option<std::borrow::Cow<'_, super::access::AccessKey>>> {
        match &self.inner.credentials {
            CredentialSource::Explicit(key) => Ok(Some(std::borrow::Cow::Borrowed(key))),
            CredentialSource::None => Ok(None),
            CredentialSource::Saved if self.inner.base_url.scheme() != "https" => Ok(None),
            CredentialSource::Saved => super::access::load_saved_key(self.inner.base_url.as_str())?
                .map(|key| Some(std::borrow::Cow::Owned(key)))
                .ok_or_else(|| {
                    Error::new(ErrorKind::Permission)
                        .with_message(format!("no saved connection to {}", self.inner.base_url))
                        .with_hint(format!(
                            "Run `plasmite access connect {}` with a current access key.",
                            self.inner.base_url
                        ))
                }),
        }
    }

    fn open(&self, method: &str, url: &Url) -> ApiResult<ureq::Request> {
        let key = self.access_key()?;
        if key.is_some() && self.inner.base_url.scheme() != "https" {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("saved native connections must use HTTPS"));
        }
        let agent = if matches!(&self.inner.credentials, CredentialSource::Saved) {
            if let Some(key) = key.as_ref() {
                let fingerprint = *key.spki_fingerprint();
                let mut saved = self.inner.saved_agent.lock().map_err(|_| {
                    Error::new(ErrorKind::Internal)
                        .with_message("saved connection transport is unavailable")
                })?;
                if saved.as_ref().is_none_or(|(pin, _)| *pin != fingerprint) {
                    *saved = Some((fingerprint, super::access::access_agent(Some(fingerprint))));
                }
                saved
                    .as_ref()
                    .expect("saved agent was just created")
                    .1
                    .clone()
            } else {
                self.inner.agent.clone()
            }
        } else {
            self.inner.agent.clone()
        };
        let mut request = agent.request(method, url.as_str());
        if url.scheme() == "https"
            && url.origin() == self.inner.base_url.origin()
            && let Some(key) = key
        {
            request = request.set("Authorization", &format!("Bearer {}", key.secret_hex()));
        }
        Ok(request)
    }
}

impl RemotePool {
    pub fn pool_ref(&self) -> PoolRef {
        self.pool_ref.clone()
    }

    pub fn info(&self) -> ApiResult<PoolInfo> {
        let url = build_url(&self.base_url, &["v2", "pools", &self.pool])?;
        let envelope: PoolEnvelope = self
            .client
            .request_json::<(), _>("GET", &url, &())
            .map_err(|err| err.with_path(self.pool.clone()))?;
        Ok(pool_info_from_remote(&self.pool, envelope.pool))
    }

    fn request_lite3_feed(
        &self,
        payload: &[u8],
        durability: Durability,
        accept: &str,
    ) -> ApiResult<ureq::Response> {
        let mut url = build_url(&self.base_url, &["v2", "pools", &self.pool, "feed"])?;
        if durability == Durability::Flush {
            url.query_pairs_mut()
                .append_pair("durability", durability_to_str(durability));
        }
        let response = self
            .client
            .request("POST", &url)?
            .set("Accept", accept)
            .set("Content-Type", "application/x-plasmite-lite3")
            .send_bytes(payload);

        read_v2::request_result_v2(response)
    }
}

fn normalize_base_url(raw: String) -> ApiResult<Url> {
    let mut url = Url::parse(&raw).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid remote base url")
            .with_source(err)
    })?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote base url must use http or https scheme"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote base url must not contain credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote base url must not contain a query or fragment"));
    }
    if scheme == "http" && !is_loopback_url(&url) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote HTTP is allowed only for the local machine")
            .with_hint("Use HTTPS for a server on another machine."));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("remote base url must not include a path")
        );
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn build_url(base_url: &Url, segments: &[&str]) -> ApiResult<Url> {
    let mut url = base_url.clone();
    {
        let mut path = url.path_segments_mut().map_err(|_| {
            Error::new(ErrorKind::Usage).with_message("remote base url cannot be a base")
        })?;
        path.clear();
        for segment in segments {
            path.push(segment);
        }
    }
    Ok(url)
}

fn parse_pool_uri(uri: &str) -> ApiResult<ResolvedPool> {
    let mut url = Url::parse(uri).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid pool uri")
            .with_source(err)
    })?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(
            Error::new(ErrorKind::Usage).with_message("pool uri must use http or https scheme")
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("pool uri must not contain credentials")
        );
    }
    if url.fragment().is_some() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("pool uri must not contain a fragment")
        );
    }
    if url.query().is_some() {
        return Err(Error::new(ErrorKind::Usage).with_message("pool uri must not contain a query"));
    }
    if scheme == "http" && !is_loopback_url(&url) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote HTTP is allowed only for the local machine")
            .with_hint("Use HTTPS for a server on another machine."));
    }
    let pool = extract_pool_from_url(&url)?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(ResolvedPool {
        base_url: url,
        pool,
    })
}

fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn extract_pool_from_url(url: &Url) -> ApiResult<String> {
    let segments: Vec<_> = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.is_empty() {
        return Err(Error::new(ErrorKind::Usage).with_message("pool uri missing path"));
    }
    if segments.len() == 3 && segments[0] == "v0" && segments[1] == "pools" {
        return Ok(segments[2].to_string());
    }
    if segments.len() == 2 && (segments[0] == "pools" || segments[0] == "pool") {
        return Ok(segments[1].to_string());
    }
    if segments.len() == 1 {
        return Ok(segments[0].to_string());
    }
    Err(Error::new(ErrorKind::Usage).with_message("pool uri path must include pool name"))
}

fn ensure_pool_name(pool: &str) -> ApiResult<()> {
    if pool.contains('/') {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool names must not contain path separators"));
    }
    Ok(())
}

fn read_json_response<R>(response: ureq::Response) -> ApiResult<R>
where
    R: DeserializeOwned,
{
    let body = response.into_string().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read response body")
            .with_source(err)
    })?;
    serde_json::from_str(&body).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("invalid response json")
            .with_source(err)
    })
}

fn read_jsonl_line(reader: &mut Option<TailReader>, max_bytes: usize) -> ApiResult<Option<String>> {
    let Some(stream) = reader.as_mut() else {
        return Ok(None);
    };
    let mut line = String::new();
    let result = stream
        .take(max_bytes.saturating_add(1) as u64)
        .read_line(&mut line)
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read tail stream")
                .with_source(err)
        })
        .and_then(|bytes| {
            if bytes == 0 {
                return Ok(None);
            }
            if bytes > max_bytes {
                return Err(Error::new(ErrorKind::Io)
                    .with_message("tail line exceeded maximum response size"));
            }
            Ok(Some(line))
        });
    if result.is_err() {
        *reader = None;
    }
    result
}

fn request_result(response: Result<ureq::Response, ureq::Error>) -> ApiResult<ureq::Response> {
    response.map_err(|err| match err {
        ureq::Error::Status(code, response) => parse_error_response(code, response),
        ureq::Error::Transport(err) => Error::new(ErrorKind::Io)
            .with_message("request failed")
            .with_source(err),
    })
}

fn parse_error_response(status: u16, response: ureq::Response) -> Error {
    let body = response.into_string().unwrap_or_default();
    error_response_body(status, body.as_bytes())
}

fn error_response_body(status: u16, body: &[u8]) -> Error {
    if let Ok(envelope) = serde_json::from_slice::<HttpErrorEnvelope>(body) {
        return error_from_remote(envelope.error);
    }
    let kind = error_kind_from_status(status);
    Error::new(kind).with_message(format!("remote error status {status}"))
}

fn read_exact_or_eof(reader: &mut dyn Read, buf: &mut [u8]) -> ApiResult<bool> {
    let mut offset = 0;
    while offset < buf.len() {
        match reader.read(&mut buf[offset..]) {
            Ok(0) => {
                if offset == 0 {
                    return Ok(false);
                }
                return Err(
                    Error::new(ErrorKind::Io).with_message("unexpected eof in lite3 stream")
                );
            }
            Ok(read) => {
                offset += read;
            }
            Err(err) => {
                return Err(Error::new(ErrorKind::Io)
                    .with_message("failed to read lite3 stream")
                    .with_source(err));
            }
        }
    }
    Ok(true)
}

fn error_from_remote(remote: HttpErrorWire) -> Error {
    let kind = remote.kind().unwrap_or(ErrorKind::Internal);
    remote.into_error(kind)
}

fn pool_info_from_remote(pool_ref: &str, pool: PoolInfoWire) -> PoolInfo {
    let path = if pool.path.is_empty() {
        PathBuf::from(pool_ref)
    } else {
        PathBuf::from(pool.path)
    };
    PoolInfo {
        path,
        file_size: pool.file_size,
        index_offset: pool.index_offset,
        index_capacity: pool.index_capacity,
        index_size_bytes: pool.index_size_bytes,
        ring_offset: pool.ring_offset,
        ring_size: pool.ring_size,
        bounds: Bounds {
            oldest_seq: pool.bounds.oldest,
            newest_seq: pool.bounds.newest,
        },
        metrics: pool.metrics.map(pool_metrics_from_remote),
    }
}

fn pool_metrics_from_remote(metrics: PoolMetricsWire) -> PoolMetrics {
    let used_percent_hundredths = if metrics.utilization.used_percent.is_finite() {
        let rounded = (metrics.utilization.used_percent * 100.0).round();
        rounded.clamp(0.0, 10_000.0) as u64
    } else {
        0
    };
    PoolMetrics {
        message_count: metrics.message_count,
        seq_span: metrics.seq_span,
        utilization: PoolUtilization {
            used_bytes: metrics.utilization.used_bytes,
            free_bytes: metrics.utilization.free_bytes,
            used_percent_hundredths,
        },
        age: PoolAgeMetrics {
            oldest_time: metrics.age.oldest_time,
            newest_time: metrics.age.newest_time,
            oldest_age_ms: metrics.age.oldest_age_ms,
            newest_age_ms: metrics.age.newest_age_ms,
        },
    }
}

fn message_from_remote(remote: MessageWire) -> ApiResult<Message> {
    Ok(Message {
        seq: remote.seq,
        time: super::message::parse_timestamp_ns(&remote.time)?,
        meta: Meta {
            tags: remote.meta.tags,
        },
        data: remote.data,
    })
}

fn durability_to_str(durability: Durability) -> &'static str {
    match durability {
        Durability::Fast => "fast",
        Durability::Flush => "flush",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RemoteClient, TailReader, extract_pool_from_url, normalize_base_url, parse_error_response,
        parse_pool_uri, read_jsonl_line,
    };
    use crate::api::PoolRef;
    use crate::core::error::ErrorKind;
    use crate::core::pool::PoolOptions;
    use std::io::{BufReader, Cursor};

    #[test]
    fn normalize_base_url_strips_path() {
        let url = normalize_base_url("http://localhost:8080".to_string()).expect("url");
        assert_eq!(url.as_str(), "http://localhost:8080/");
    }

    #[test]
    fn parse_pool_uri_accepts_pool_prefix() {
        let resolved = parse_pool_uri("http://localhost:8080/pool/chat").expect("pool");
        assert_eq!(resolved.base_url.as_str(), "http://localhost:8080/");
        assert_eq!(resolved.pool, "chat");
    }

    #[test]
    fn parse_pool_uri_accepts_v0_prefix() {
        let resolved = parse_pool_uri("http://localhost:8080/v0/pools/chat").expect("pool");
        assert_eq!(resolved.pool, "chat");
    }

    #[test]
    fn extract_pool_from_url_rejects_multi_segment() {
        let url = url::Url::parse("http://localhost:8080/alpha/beta").expect("url");
        let err = extract_pool_from_url(&url).expect_err("err");
        assert_eq!(err.kind(), super::ErrorKind::Usage);
    }

    #[test]
    fn remote_error_details_survive_new_kinds_and_plain_http_failures() {
        let response = ureq::Response::new(
            409,
            "Conflict",
            r#"{"error":{"kind":"FutureKind","message":"retry the append","hint":"refresh the cursor","path":"pools/test","seq":72,"offset":4096}}"#,
        )
        .unwrap();
        let error = parse_error_response(409, response);
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert_eq!(error.message(), Some("retry the append"));
        assert_eq!(error.hint(), Some("refresh the cursor"));
        assert_eq!(error.path(), Some(std::path::Path::new("pools/test")));
        assert_eq!(error.seq(), Some(72));
        assert_eq!(error.offset(), Some(4096));

        let response = ureq::Response::new(423, "Locked", "upstream proxy error").unwrap();
        let error = parse_error_response(423, response);
        assert_eq!(error.kind(), ErrorKind::Busy);
        assert_eq!(error.message(), Some("remote error status 423"));
    }

    #[test]
    fn remote_client_rejects_path_pool_ref() {
        let client =
            RemoteClient::for_access_probe(url::Url::parse("http://localhost:8080").expect("url"));
        let pool_ref = PoolRef::path("/tmp/evil.plasmite");
        let err = match client.create_pool(&pool_ref, PoolOptions::new(1024)) {
            Err(err) => err,
            Ok(_) => panic!("remote clients reject file paths"),
        };
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn jsonl_line_reader_enforces_limit() {
        let mut reader: Option<TailReader> = Some(BufReader::new(Box::new(Cursor::new(
            b"123\n1234\n".to_vec(),
        ))));
        assert_eq!(
            read_jsonl_line(&mut reader, 4).expect("line"),
            Some("123\n".into())
        );
        let err = read_jsonl_line(&mut reader, 4).expect_err("oversized line");
        assert_eq!(err.kind(), ErrorKind::Io);
        assert!(reader.is_none());
    }
}
