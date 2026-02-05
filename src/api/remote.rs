//! Purpose: Provide an HTTP/JSON remote client for the Plasmite v0 protocol.
//! Exports: `RemoteClient`, `RemotePool`, `RemoteTail`.
//! Role: Transport-agnostic client that mirrors local pool operations remotely.
//! Invariants: Requests/response envelopes align with spec/remote/v0/SPEC.md.
//! Invariants: Pool refs resolve to a base URL + pool identifier (name only).
//! Invariants: Tail streams are JSONL and cancelable via drop or `cancel()`.
#![allow(clippy::result_large_err)]

use super::{Message, Meta, PoolRef, TailOptions};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::{AppendOptions, Bounds, Durability, PoolInfo, PoolOptions};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

type ApiResult<T> = Result<T, Error>;

#[derive(Clone)]
pub struct RemoteClient {
    inner: Arc<RemoteClientInner>,
}

struct RemoteClientInner {
    base_url: Url,
    token: Option<String>,
    agent: ureq::Agent,
}

#[derive(Clone)]
pub struct RemotePool {
    client: RemoteClient,
    base_url: Url,
    pool: String,
    pool_ref: PoolRef,
}

pub struct RemoteTail {
    reader: Option<BufReader<Box<dyn std::io::Read + Send + Sync>>>,
}

#[derive(Clone, Debug)]
struct ResolvedPool {
    base_url: Url,
    pool: String,
}

#[derive(Deserialize)]
struct PoolEnvelope {
    pool: RemotePoolInfo,
}

#[derive(Deserialize)]
struct PoolsEnvelope {
    pools: Vec<RemotePoolInfo>,
}

#[derive(Deserialize)]
struct MessageEnvelope {
    message: RemoteMessage,
}

#[derive(Deserialize)]
struct RemoteMessage {
    seq: u64,
    time: String,
    meta: RemoteMeta,
    data: Value,
}

#[derive(Deserialize)]
struct RemoteMeta {
    descrips: Vec<String>,
}

#[derive(Deserialize)]
struct RemotePoolInfo {
    name: Option<String>,
    path: String,
    file_size: u64,
    ring_offset: u64,
    ring_size: u64,
    #[serde(default)]
    bounds: RemoteBounds,
}

#[derive(Deserialize, Default)]
struct RemoteBounds {
    oldest: Option<u64>,
    newest: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: RemoteError,
}

#[derive(Deserialize)]
struct RemoteError {
    kind: String,
    message: Option<String>,
    hint: Option<String>,
    path: Option<String>,
    seq: Option<u64>,
    offset: Option<u64>,
}

#[derive(Serialize)]
struct CreatePoolRequest<'a> {
    pool: &'a str,
    size_bytes: u64,
}

#[derive(Serialize)]
struct OpenPoolRequest<'a> {
    pool: &'a str,
}

#[derive(Serialize)]
struct AppendRequest<'a> {
    data: &'a Value,
    descrips: &'a [String],
    durability: &'a str,
}

impl RemoteClient {
    pub fn new(base_url: impl Into<String>) -> ApiResult<Self> {
        let base_url = normalize_base_url(base_url.into())?;
        let agent = ureq::AgentBuilder::new().build();
        Ok(Self {
            inner: Arc::new(RemoteClientInner {
                base_url,
                token: None,
                agent,
            }),
        })
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.token = Some(token.into());
        } else {
            self.inner = Arc::new(RemoteClientInner {
                base_url: self.inner.base_url.clone(),
                token: Some(token.into()),
                agent: self.inner.agent.clone(),
            });
        }
        self
    }

    pub fn base_url(&self) -> &Url {
        &self.inner.base_url
    }

    pub fn create_pool(&self, pool_ref: &PoolRef, options: PoolOptions) -> ApiResult<PoolInfo> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let payload = CreatePoolRequest {
            pool: &resolved.pool,
            size_bytes: options.file_size,
        };
        let url = build_url(&resolved.base_url, &["v0", "pools"])?;
        let envelope: PoolEnvelope = self
            .request_json("POST", &url, &payload)
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(pool_info_from_remote(&resolved.pool, envelope.pool))
    }

    pub fn open_pool(&self, pool_ref: &PoolRef) -> ApiResult<RemotePool> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let payload = OpenPoolRequest {
            pool: &resolved.pool,
        };
        let url = build_url(&resolved.base_url, &["v0", "pools", "open"])?;
        let _envelope: PoolEnvelope = self
            .request_json("POST", &url, &payload)
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(RemotePool {
            client: self.clone(),
            base_url: resolved.base_url,
            pool: resolved.pool,
            pool_ref: pool_ref.clone(),
        })
    }

    pub fn pool_info(&self, pool_ref: &PoolRef) -> ApiResult<PoolInfo> {
        let resolved = self.resolve_pool_ref(pool_ref)?;
        let url = build_url(&resolved.base_url, &["v0", "pools", &resolved.pool, "info"])?;
        let envelope: PoolEnvelope = self
            .request_json::<(), _>("GET", &url, &())
            .map_err(|err| err.with_path(resolved.pool.clone()))?;
        Ok(pool_info_from_remote(&resolved.pool, envelope.pool))
    }

    pub fn list_pools(&self) -> ApiResult<Vec<PoolInfo>> {
        let url = build_url(&self.inner.base_url, &["v0", "pools"])?;
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
        let url = build_url(&resolved.base_url, &["v0", "pools", &resolved.pool])?;
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
        let request = self.request(method, url).set("Accept", "application/json");
        let response = if method == "GET" {
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

        match response {
            Ok(resp) => read_json_response(resp),
            Err(ureq::Error::Status(code, resp)) => Err(parse_error_response(code, resp)),
            Err(ureq::Error::Transport(err)) => Err(Error::new(ErrorKind::Io)
                .with_message("request failed")
                .with_source(err)),
        }
    }

    fn request(&self, method: &str, url: &Url) -> ureq::Request {
        let mut request = self.inner.agent.request(method, url.as_str());
        if let Some(token) = &self.inner.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        request
    }

    fn request_stream(&self, url: &Url) -> ApiResult<ureq::Response> {
        let response = self
            .request("GET", url)
            .set("Accept", "application/json")
            .call();
        match response {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(code, resp)) => Err(parse_error_response(code, resp)),
            Err(ureq::Error::Transport(err)) => Err(Error::new(ErrorKind::Io)
                .with_message("request failed")
                .with_source(err)),
        }
    }
}

impl RemotePool {
    pub fn pool_ref(&self) -> PoolRef {
        self.pool_ref.clone()
    }

    pub fn info(&self) -> ApiResult<PoolInfo> {
        let url = build_url(&self.base_url, &["v0", "pools", &self.pool, "info"])?;
        let envelope: PoolEnvelope = self
            .client
            .request_json::<(), _>("GET", &url, &())
            .map_err(|err| err.with_path(self.pool.clone()))?;
        Ok(pool_info_from_remote(&self.pool, envelope.pool))
    }

    pub fn append_json(
        &self,
        data: &Value,
        descrips: &[String],
        options: AppendOptions,
    ) -> ApiResult<Message> {
        if options.timestamp_ns != 0 {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("remote append does not support explicit timestamps"));
        }
        let url = build_url(&self.base_url, &["v0", "pools", &self.pool, "append"])?;
        let payload = AppendRequest {
            data,
            descrips,
            durability: durability_to_str(options.durability),
        };
        let envelope: MessageEnvelope = self
            .client
            .request_json("POST", &url, &payload)
            .map_err(|err| err.with_path(self.pool.clone()))?;
        Ok(message_from_remote(envelope.message))
    }

    pub fn append_json_now(
        &self,
        data: &Value,
        descrips: &[String],
        durability: Durability,
    ) -> ApiResult<Message> {
        self.append_json(
            data,
            descrips,
            AppendOptions {
                timestamp_ns: 0,
                durability,
            },
        )
    }

    pub fn get_message(&self, seq: u64) -> ApiResult<Message> {
        let url = build_url(
            &self.base_url,
            &["v0", "pools", &self.pool, "messages", &seq.to_string()],
        )?;
        let envelope: MessageEnvelope = self
            .client
            .request_json::<(), _>("GET", &url, &())
            .map_err(|err| err.with_path(self.pool.clone()).with_seq(seq))?;
        Ok(message_from_remote(envelope.message))
    }

    pub fn tail(&self, options: TailOptions) -> ApiResult<RemoteTail> {
        let mut url = build_url(&self.base_url, &["v0", "pools", &self.pool, "tail"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(since) = options.since_seq {
                pairs.append_pair("since_seq", &since.to_string());
            }
            if let Some(max) = options.max_messages {
                pairs.append_pair("max", &max.to_string());
            }
            if let Some(timeout) = options.timeout {
                pairs.append_pair("timeout_ms", &timeout.as_millis().to_string());
            }
        }

        let response = self
            .client
            .request_stream(&url)
            .map_err(|err| err.with_path(self.pool.clone()))?;
        Ok(RemoteTail {
            reader: Some(BufReader::new(response.into_reader())),
        })
    }
}

impl RemoteTail {
    pub fn next_message(&mut self) -> ApiResult<Option<Message>> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read tail stream")
                    .with_source(err)
            })?;
            if bytes == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            let message: RemoteMessage = serde_json::from_str(&line).map_err(|err| {
                Error::new(ErrorKind::Internal)
                    .with_message("invalid tail message json")
                    .with_source(err)
            })?;
            return Ok(Some(message_from_remote(message)));
        }
    }

    pub fn cancel(&mut self) {
        self.reader = None;
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
    let pool = extract_pool_from_url(&url)?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(ResolvedPool {
        base_url: url,
        pool,
    })
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

fn parse_error_response(status: u16, response: ureq::Response) -> Error {
    let body = response.into_string().unwrap_or_default();
    if let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(&body) {
        return error_from_remote(envelope.error);
    }
    Error::new(ErrorKind::Io).with_message(format!("remote error status {status}"))
}

fn error_from_remote(remote: RemoteError) -> Error {
    let kind = parse_error_kind(&remote.kind);
    let mut err = Error::new(kind);
    if let Some(message) = remote.message {
        err = err.with_message(message);
    }
    if let Some(hint) = remote.hint {
        err = err.with_hint(hint);
    }
    if let Some(path) = remote.path {
        err = err.with_path(path);
    }
    if let Some(seq) = remote.seq {
        err = err.with_seq(seq);
    }
    if let Some(offset) = remote.offset {
        err = err.with_offset(offset);
    }
    err
}

fn parse_error_kind(kind: &str) -> ErrorKind {
    match kind {
        "Internal" => ErrorKind::Internal,
        "Usage" => ErrorKind::Usage,
        "NotFound" => ErrorKind::NotFound,
        "AlreadyExists" => ErrorKind::AlreadyExists,
        "Busy" => ErrorKind::Busy,
        "Permission" => ErrorKind::Permission,
        "Corrupt" => ErrorKind::Corrupt,
        "Io" => ErrorKind::Io,
        _ => ErrorKind::Internal,
    }
}

fn pool_info_from_remote(pool_ref: &str, pool: RemotePoolInfo) -> PoolInfo {
    let path = if pool.path.is_empty() {
        PathBuf::from(pool_ref)
    } else {
        PathBuf::from(pool.path)
    };
    PoolInfo {
        path,
        file_size: pool.file_size,
        ring_offset: pool.ring_offset,
        ring_size: pool.ring_size,
        bounds: Bounds {
            oldest_seq: pool.bounds.oldest,
            newest_seq: pool.bounds.newest,
        },
    }
}

fn message_from_remote(remote: RemoteMessage) -> Message {
    Message {
        seq: remote.seq,
        time: remote.time,
        meta: Meta {
            descrips: remote.meta.descrips,
        },
        data: remote.data,
    }
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
        RemoteClient, extract_pool_from_url, normalize_base_url, parse_error_kind, parse_pool_uri,
    };
    use crate::api::PoolRef;
    use crate::core::error::ErrorKind;
    use crate::core::pool::PoolOptions;

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
    fn parse_error_kind_maps_known_values() {
        assert_eq!(parse_error_kind("Usage"), ErrorKind::Usage);
        assert_eq!(parse_error_kind("AlreadyExists"), ErrorKind::AlreadyExists);
        assert_eq!(parse_error_kind("Permission"), ErrorKind::Permission);
        assert_eq!(parse_error_kind("Corrupt"), ErrorKind::Corrupt);
        assert_eq!(parse_error_kind("Busy"), ErrorKind::Busy);
    }

    #[test]
    fn remote_client_rejects_path_pool_ref() {
        let client = RemoteClient::new("http://localhost:8080").expect("client");
        let pool_ref = PoolRef::path("/tmp/evil.plasmite");
        let err = client
            .create_pool(&pool_ref, PoolOptions::new(1024))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
}
