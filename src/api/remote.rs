//! Purpose: Provide an HTTP client for the Plasmite v0 protocol (JSON + Lite3 bytes).
//! Exports: `RemoteClient`, `RemotePool`, `RemoteTail`, `RemoteLite3Tail`, `RemoteLite3Frame`.
//! Role: Transport-agnostic client that mirrors local pool operations remotely.
//! Invariants: Requests/response envelopes align with spec/remote/v0/SPEC.md.
//! Invariants: Pool refs resolve to a base URL + pool identifier (name only).
//! Invariants: Tail streams are JSONL (messages) or framed Lite3 bytes (fast path).
#![allow(clippy::result_large_err)]

use super::{Message, Meta, PoolRef, TailOptions};
use crate::core::error::{Error, ErrorKind};
use crate::core::pool::{
    AppendOptions, Bounds, Durability, PoolAgeMetrics, PoolInfo, PoolMetrics, PoolOptions,
    PoolUtilization,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ureq::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use ureq::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use ureq::rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
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

#[derive(Debug)]
struct AcceptAllServerCertVerifier;

impl ServerCertVerifier for AcceptAllServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ureq::rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
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

pub struct RemoteLite3Tail {
    reader: Option<BufReader<Box<dyn std::io::Read + Send + Sync>>>,
}

#[derive(Clone, Debug)]
pub struct RemoteLite3Frame {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub payload: Vec<u8>,
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
struct Lite3AppendEnvelope {
    message: Lite3AppendMessage,
}

#[derive(Deserialize)]
struct Lite3AppendMessage {
    seq: u64,
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
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct RemotePoolInfo {
    name: Option<String>,
    path: String,
    file_size: u64,
    #[serde(default)]
    index_offset: u64,
    #[serde(default)]
    index_capacity: u32,
    #[serde(default)]
    index_size_bytes: u64,
    ring_offset: u64,
    ring_size: u64,
    #[serde(default)]
    bounds: RemoteBounds,
    #[serde(default)]
    metrics: Option<RemotePoolMetrics>,
}

#[derive(Deserialize, Default)]
struct RemoteBounds {
    oldest: Option<u64>,
    newest: Option<u64>,
}

#[derive(Deserialize)]
struct RemotePoolMetrics {
    message_count: u64,
    seq_span: u64,
    utilization: RemotePoolUtilization,
    age: RemotePoolAgeMetrics,
}

#[derive(Deserialize)]
struct RemotePoolUtilization {
    used_bytes: u64,
    free_bytes: u64,
    used_percent: f64,
}

#[derive(Deserialize)]
struct RemotePoolAgeMetrics {
    oldest_time: Option<String>,
    newest_time: Option<String>,
    oldest_age_ms: Option<u64>,
    newest_age_ms: Option<u64>,
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
    tags: &'a [String],
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

    pub fn with_tls_ca_file(mut self, path: impl AsRef<Path>) -> ApiResult<Self> {
        let path = path.as_ref();
        let cert_bytes = std::fs::read(path).map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("failed to read TLS CA/certificate file")
                .with_path(path)
                .with_source(err)
        })?;
        let mut cert_reader = Cursor::new(cert_bytes);
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                Error::new(ErrorKind::Usage)
                    .with_message("failed to parse TLS CA/certificate file")
                    .with_path(path)
                    .with_source(err)
            })?;
        if certs.is_empty() {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("TLS CA/certificate file contains no certificates")
                .with_path(path));
        }

        let _ = ureq::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut root_store = ureq::rustls::RootCertStore::empty();
        let (added, _) = root_store.add_parsable_certificates(certs);
        if added == 0 {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("TLS CA/certificate file contains no parsable certificates")
                .with_path(path));
        }

        let tls_config = ureq::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let agent = ureq::builder().tls_config(Arc::new(tls_config)).build();
        self = self.with_agent(agent);
        Ok(self)
    }

    pub fn with_tls_skip_verify(mut self) -> Self {
        let _ = ureq::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let tls_config = ureq::rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAllServerCertVerifier))
            .with_no_client_auth();
        let agent = ureq::builder().tls_config(Arc::new(tls_config)).build();
        self = self.with_agent(agent);
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

    fn request_stream_lite3(&self, url: &Url) -> ApiResult<ureq::Response> {
        let response = self
            .request("GET", url)
            .set("Accept", "application/x-plasmite-lite3-stream")
            .call();
        match response {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(code, resp)) => Err(parse_error_response(code, resp)),
            Err(ureq::Error::Transport(err)) => Err(Error::new(ErrorKind::Io)
                .with_message("request failed")
                .with_source(err)),
        }
    }

    fn with_agent(mut self, agent: ureq::Agent) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.agent = agent;
        } else {
            self.inner = Arc::new(RemoteClientInner {
                base_url: self.inner.base_url.clone(),
                token: self.inner.token.clone(),
                agent,
            });
        }
        self
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
        tags: &[String],
        options: AppendOptions,
    ) -> ApiResult<Message> {
        if options.timestamp_ns != 0 {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("remote append does not support explicit timestamps"));
        }
        let url = build_url(&self.base_url, &["v0", "pools", &self.pool, "append"])?;
        let payload = AppendRequest {
            data,
            tags,
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
        tags: &[String],
        durability: Durability,
    ) -> ApiResult<Message> {
        self.append_json(
            data,
            tags,
            AppendOptions {
                timestamp_ns: 0,
                durability,
            },
        )
    }

    pub fn append_lite3(&self, payload: &[u8], options: AppendOptions) -> ApiResult<u64> {
        if options.timestamp_ns != 0 {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("remote append does not support explicit timestamps"));
        }
        let mut url = build_url(&self.base_url, &["v0", "pools", &self.pool, "append_lite3"])?;
        if options.durability == Durability::Flush {
            url.query_pairs_mut()
                .append_pair("durability", durability_to_str(options.durability));
        }
        let response = self
            .client
            .request("POST", &url)
            .set("Accept", "application/json")
            .set("Content-Type", "application/x-plasmite-lite3")
            .send_bytes(payload);

        match response {
            Ok(resp) => {
                let envelope: Lite3AppendEnvelope = read_json_response(resp)?;
                Ok(envelope.message.seq)
            }
            Err(ureq::Error::Status(code, resp)) => Err(parse_error_response(code, resp)),
            Err(ureq::Error::Transport(err)) => Err(Error::new(ErrorKind::Io)
                .with_message("request failed")
                .with_source(err)),
        }
    }

    pub fn append_lite3_now(&self, payload: &[u8], durability: Durability) -> ApiResult<u64> {
        self.append_lite3(
            payload,
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

    pub fn get_lite3(&self, seq: u64) -> ApiResult<Vec<u8>> {
        let url = build_url(
            &self.base_url,
            &[
                "v0",
                "pools",
                &self.pool,
                "messages",
                &seq.to_string(),
                "lite3",
            ],
        )?;
        let response = self
            .client
            .request("GET", &url)
            .set("Accept", "application/x-plasmite-lite3")
            .call();
        match response {
            Ok(resp) => {
                let mut reader = resp.into_reader();
                let mut out = Vec::new();
                reader.read_to_end(&mut out).map_err(|err| {
                    Error::new(ErrorKind::Io)
                        .with_message("failed to read lite3 response")
                        .with_source(err)
                })?;
                Ok(out)
            }
            Err(ureq::Error::Status(code, resp)) => Err(parse_error_response(code, resp)),
            Err(ureq::Error::Transport(err)) => Err(Error::new(ErrorKind::Io)
                .with_message("request failed")
                .with_source(err)),
        }
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
            for tag in &options.tags {
                pairs.append_pair("tag", tag);
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

    pub fn tail_lite3(&self, options: TailOptions) -> ApiResult<RemoteLite3Tail> {
        let mut url = build_url(&self.base_url, &["v0", "pools", &self.pool, "tail_lite3"])?;
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
            for tag in &options.tags {
                pairs.append_pair("tag", tag);
            }
        }

        let response = self
            .client
            .request_stream_lite3(&url)
            .map_err(|err| err.with_path(self.pool.clone()))?;
        Ok(RemoteLite3Tail {
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

impl RemoteLite3Tail {
    pub fn next_frame(&mut self) -> ApiResult<Option<RemoteLite3Frame>> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };
        let mut header = [0u8; 20];
        if !read_exact_or_eof(reader, &mut header)? {
            return Ok(None);
        }
        let seq = u64::from_be_bytes(header[0..8].try_into().expect("seq header"));
        let timestamp_ns = u64::from_be_bytes(header[8..16].try_into().expect("timestamp header"));
        let len = u32::from_be_bytes(header[16..20].try_into().expect("len header")) as usize;
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read lite3 payload")
                .with_source(err)
        })?;
        Ok(Some(RemoteLite3Frame {
            seq,
            timestamp_ns,
            payload,
        }))
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

fn error_kind_from_status(status: u16) -> ErrorKind {
    match status {
        400 | 413 => ErrorKind::Usage,
        401 | 403 => ErrorKind::Permission,
        404 => ErrorKind::NotFound,
        409 => ErrorKind::AlreadyExists,
        423 => ErrorKind::Busy,
        500..=599 => ErrorKind::Internal,
        _ => ErrorKind::Io,
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

fn pool_metrics_from_remote(metrics: RemotePoolMetrics) -> PoolMetrics {
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

fn message_from_remote(remote: RemoteMessage) -> Message {
    Message {
        seq: remote.seq,
        time: remote.time,
        meta: Meta {
            tags: remote.meta.tags,
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
