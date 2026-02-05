//! Purpose: Provide the HTTP/JSON remote server for Plasmite.
//! Exports: `ServeConfig`, `serve`.
//! Role: Axum-based loopback server implementing the remote v0 spec.
//! Invariants: JSON envelopes match spec/remote/v0/SPEC.md; error kinds remain stable.
//! Invariants: Loopback-only unless explicitly allowed (v0 policy).
//! Notes: Streaming uses JSONL or framed Lite3; tail is at-least-once and resumable.

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use hyper_util::service::TowerToHyperService;
use rcgen::{Certificate, CertificateParams, SanType};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::future::IntoFuture;
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio::time::Duration;
use tokio_rustls::TlsAcceptor;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::trace::TraceLayer;
use tower_service::Service;
use tracing_subscriber::EnvFilter;

use plasmite::api::{
    Bounds, Durability, Error, ErrorKind, LocalClient, PoolApiExt, PoolInfo, PoolOptions, PoolRef,
    TailOptions, lite3,
};

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub bind: SocketAddr,
    pub pool_dir: PathBuf,
    pub token: Option<String>,
    pub access_mode: AccessMode,
    pub allow_non_loopback: bool,
    pub insecure_no_tls: bool,
    pub token_file_used: bool,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_self_signed: bool,
    pub max_body_bytes: u64,
    pub max_tail_timeout_ms: u64,
    pub max_concurrent_tails: usize,
}

#[derive(Clone)]
struct AppState {
    client: LocalClient,
    token: Option<String>,
    access_mode: AccessMode,
    max_tail_timeout_ms: u64,
    tail_semaphore: Arc<Semaphore>,
}

#[derive(Clone, Copy, Debug)]
pub enum AccessMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

impl AccessMode {
    fn allows_read(self) -> bool {
        matches!(self, AccessMode::ReadOnly | AccessMode::ReadWrite)
    }

    fn allows_write(self) -> bool {
        matches!(self, AccessMode::WriteOnly | AccessMode::ReadWrite)
    }
}

pub async fn serve(config: ServeConfig) -> Result<(), Error> {
    validate_config(&config)?;

    init_tracing();

    let max_body_bytes: usize = config
        .max_body_bytes
        .try_into()
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("--max-body-bytes is too large"))?;

    let tls_config = build_tls_config(&config).await?;

    let state = Arc::new(AppState {
        client: LocalClient::new().with_pool_dir(config.pool_dir),
        token: config.token,
        access_mode: config.access_mode,
        max_tail_timeout_ms: config.max_tail_timeout_ms,
        tail_semaphore: Arc::new(Semaphore::new(config.max_concurrent_tails)),
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v0/pools", post(create_pool).get(list_pools))
        .route("/v0/pools/open", post(open_pool))
        .route("/v0/pools/:pool/info", get(pool_info))
        .route("/v0/pools/:pool", delete(delete_pool))
        .route("/v0/pools/:pool/append", post(append_message))
        .route("/v0/pools/:pool/append_lite3", post(append_lite3))
        .route("/v0/pools/:pool/messages/:seq", get(get_message))
        .route("/v0/pools/:pool/messages/:seq/lite3", get(get_lite3))
        .route("/v0/pools/:pool/tail", get(tail_messages))
        .route("/v0/pools/:pool/tail_lite3", get(tail_lite3))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    if let Some(tls_config) = tls_config {
        return serve_tls(config.bind, app, tls_config).await;
    }
    serve_plain(config.bind, app).await
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback(),
    }
}

fn validate_config(config: &ServeConfig) -> Result<(), Error> {
    let is_loopback_bind = is_loopback(config.bind.ip());
    if !is_loopback_bind && !config.allow_non_loopback {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("non-loopback bind requires explicit opt-in")
            .with_hint("Re-run with --allow-non-loopback or use a loopback address."));
    }

    if config.tls_cert.is_some() != config.tls_key.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("TLS requires both --tls-cert and --tls-key")
            .with_hint("Provide both paths or use --tls-self-signed."));
    }

    if config.tls_self_signed && (config.tls_cert.is_some() || config.tls_key.is_some()) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--tls-self-signed cannot be combined with --tls-cert/--tls-key")
            .with_hint("Use either --tls-self-signed or provide certificate paths."));
    }

    if config.max_body_bytes == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-body-bytes must be greater than zero")
            .with_hint("Use a positive value like 1048576."));
    }

    if config.max_tail_timeout_ms == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-tail-timeout-ms must be greater than zero")
            .with_hint("Use a positive value like 30000."));
    }

    if config.max_concurrent_tails == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-tail-concurrency must be greater than zero")
            .with_hint("Use a positive value like 64."));
    }

    if config.max_body_bytes > usize::MAX as u64 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-body-bytes exceeds platform limits")
            .with_hint("Use a smaller value that fits in memory."));
    }

    if !is_loopback_bind && config.access_mode.allows_write() {
        if !config.token_file_used {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("non-loopback write requires --token-file")
                .with_hint("Use --token-file for safer deployments."));
        }
        if !config.insecure_no_tls && !tls_is_configured(config) {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("non-loopback write requires TLS")
                .with_hint("Use --tls-cert/--tls-key, --tls-self-signed, or --insecure-no-tls."));
        }
    }

    Ok(())
}

fn tls_is_configured(config: &ServeConfig) -> bool {
    config.tls_self_signed || (config.tls_cert.is_some() && config.tls_key.is_some())
}

async fn build_tls_config(config: &ServeConfig) -> Result<Option<Arc<ServerConfig>>, Error> {
    if config.tls_self_signed {
        let mut params = CertificateParams::new(vec!["localhost".to_string()]);
        params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        if !config.bind.ip().is_unspecified() {
            params
                .subject_alt_names
                .push(SanType::IpAddress(config.bind.ip()));
        }
        let cert = Certificate::from_params(params).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("failed to generate self-signed certificate")
                .with_source(err)
        })?;
        let cert_der = cert.serialize_der().map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("failed to serialize self-signed certificate")
                .with_source(err)
        })?;
        let key_der = cert.serialize_private_key_der();
        let certs = vec![CertificateDer::from(cert_der)];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
        let tls = build_server_config(certs, key)?;
        return Ok(Some(Arc::new(tls)));
    }

    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        let tls = load_tls_config_from_pem(cert, key)?;
        return Ok(Some(Arc::new(tls)));
    }

    Ok(None)
}

fn load_tls_config_from_pem(
    cert_path: &PathBuf,
    key_path: &PathBuf,
) -> Result<ServerConfig, Error> {
    let cert_bytes = std::fs::read(cert_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read TLS certificate")
            .with_path(cert_path)
            .with_source(err)
    })?;
    let key_bytes = std::fs::read(key_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read TLS key")
            .with_path(key_path)
            .with_source(err)
    })?;

    let mut cert_reader = Cursor::new(cert_bytes);
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to parse TLS certificate")
                .with_path(cert_path)
                .with_source(err)
        })?;
    if certs.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("TLS certificate file contains no certificates")
            .with_path(cert_path));
    }

    let mut key_reader = Cursor::new(key_bytes);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to parse TLS key")
                .with_path(key_path)
                .with_source(err)
        })?
        .ok_or_else(|| {
            Error::new(ErrorKind::Usage)
                .with_message("TLS key file contains no private key")
                .with_path(key_path)
        })?;

    build_server_config(certs, key)
}

fn build_server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, Error> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid TLS certificate or key")
                .with_source(err)
        })?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

async fn serve_plain(bind: SocketAddr, app: Router) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to bind server")
            .with_source(err)
    })?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result.map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("server failed")
                    .with_source(err)
            })?;
        }
        _ = shutdown_signal() => {
            let _ = shutdown_tx.send(());
            match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                Ok(result) => result.map_err(|err| {
                    Error::new(ErrorKind::Io)
                        .with_message("server failed")
                        .with_source(err)
                })?,
                Err(_) => {
                    return Err(Error::new(ErrorKind::Io).with_message("server shutdown timed out"));
                }
            }
        }
    };
    Ok(())
}

async fn serve_tls(
    bind: SocketAddr,
    app: Router,
    tls_config: Arc<ServerConfig>,
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to bind TLS server")
            .with_source(err)
    })?;
    let acceptor = TlsAcceptor::from(tls_config);
    let builder = AutoBuilder::new(TokioExecutor::new());
    let mut make_service = app.into_make_service();
    let mut tasks = JoinSet::new();

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accept = listener.accept() => {
                let (stream, peer_addr) = match accept {
                    Ok(result) => result,
                    Err(err) => {
                        return Err(Error::new(ErrorKind::Io)
                            .with_message("failed to accept TLS connection")
                            .with_source(err));
                    }
                };

                let service = match make_service.call(peer_addr).await {
                    Ok(service) => service,
                    Err(_) => continue,
                };

                let acceptor = acceptor.clone();
                let builder = builder.clone();
                tasks.spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(stream) => stream,
                        Err(_) => return,
                    };
                    let io = TokioIo::new(tls_stream);
                    let service = TowerToHyperService::new(service);
                    let _ = builder.serve_connection_with_upgrades(io, service).await;
                });
            }
        }
    }

    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            Some(_) = tasks.join_next() => {}
            else => break,
        }
    }

    Ok(())
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        signal.recv().await;
    };
    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

fn authorize(headers: &HeaderMap, state: &AppState) -> Result<(), Error> {
    let Some(token) = state.token.as_ref() else {
        return Ok(());
    };
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(Error::new(ErrorKind::Permission).with_message("missing bearer token"));
    };
    let value = value.to_str().unwrap_or_default();
    let expected = format!("Bearer {token}");
    if value != expected {
        return Err(Error::new(ErrorKind::Permission).with_message("invalid bearer token"));
    }
    Ok(())
}

fn ensure_read_access(state: &AppState) -> Result<(), Error> {
    if state.access_mode.allows_read() {
        Ok(())
    } else {
        Err(access_error("read operations"))
    }
}

fn ensure_write_access(state: &AppState) -> Result<(), Error> {
    if state.access_mode.allows_write() {
        Ok(())
    } else {
        Err(access_error("write operations"))
    }
}

fn access_error(action: &str) -> Error {
    Error::new(ErrorKind::Permission)
        .with_message(format!("forbidden: access mode disallows {action}"))
        .with_hint("Adjust --access to permit this operation.")
}

#[derive(Debug, Deserialize)]
struct CreatePoolRequest {
    pool: String,
    size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PoolRequest {
    pool: String,
}

#[derive(Debug, Deserialize)]
struct AppendRequest {
    data: serde_json::Value,
    descrips: Option<Vec<String>>,
    durability: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TailQuery {
    since_seq: Option<u64>,
    max: Option<u64>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AppendLite3Query {
    durability: Option<String>,
}

async fn healthz() -> Response {
    json_response(json!({ "ok": true }))
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
}

async fn create_pool(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CreatePoolRequest>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_write_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&payload.pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let size_bytes = payload.size_bytes.unwrap_or(1024 * 1024);
    let result = state
        .client
        .create_pool(&pool_ref, PoolOptions::new(size_bytes));
    match result {
        Ok(info) => json_response(json!({ "pool": pool_info_json(&payload.pool, &info) })),
        Err(err) => error_response(err),
    }
}

async fn open_pool(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<PoolRequest>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&payload.pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    match state.client.pool_info(&pool_ref) {
        Ok(info) => json_response(json!({ "pool": pool_info_json(&payload.pool, &info) })),
        Err(err) => error_response(err),
    }
}

async fn pool_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    match state.client.pool_info(&pool_ref) {
        Ok(info) => json_response(json!({ "pool": pool_info_json(&pool, &info) })),
        Err(err) => error_response(err),
    }
}

async fn list_pools(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    match state.client.list_pools() {
        Ok(pools) => {
            let mut out = Vec::new();
            for info in pools {
                let name = info
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .trim_end_matches(".plasmite")
                    .to_string();
                out.push(pool_info_json(&name, &info));
            }
            json_response(json!({ "pools": out }))
        }
        Err(err) => error_response(err),
    }
}

async fn delete_pool(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_write_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    match state.client.delete_pool(&pool_ref) {
        Ok(()) => json_response(json!({ "ok": true })),
        Err(err) => error_response(err),
    }
}

async fn append_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
    Json(payload): Json<AppendRequest>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_write_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let durability = durability_from_str(payload.durability.as_deref());
    let descrips = payload.descrips.unwrap_or_default();

    let result = state
        .client
        .open_pool(&pool_ref)
        .and_then(|mut pool| pool.append_json_now(&payload.data, &descrips, durability));
    match result {
        Ok(message) => json_response(json!({ "message": message_json(&message) })),
        Err(err) => error_response(err),
    }
}

async fn append_lite3(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
    Query(query): Query<AppendLite3Query>,
    payload: Bytes,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_write_access(&state) {
        return error_response(err);
    }
    if let Some(content_type) = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
    {
        if !content_type.starts_with("application/x-plasmite-lite3") {
            return error_response(
                Error::new(ErrorKind::Usage).with_message("invalid content-type for lite3 append"),
            );
        }
    }
    if payload.is_empty() {
        return error_response(
            Error::new(ErrorKind::Usage).with_message("lite3 payload is required"),
        );
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let durability = durability_from_str(query.durability.as_deref());
    let payload = payload.to_vec();
    let result = state.client.open_pool(&pool_ref).and_then(|mut pool| {
        let seq = pool.append_lite3_now(&payload, durability)?;
        pool.get_message(seq)
    });
    match result {
        Ok(message) => json_response(json!({ "message": message_json(&message) })),
        Err(err) => error_response(err),
    }
}

async fn get_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((pool, seq)): AxumPath<(String, u64)>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let result = state
        .client
        .open_pool(&pool_ref)
        .and_then(|pool| pool.get_message(seq));

    match result {
        Ok(message) => json_response(json!({ "message": message_json(&message) })),
        Err(err) => error_response(err),
    }
}

async fn get_lite3(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((pool, seq)): AxumPath<(String, u64)>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let result = state.client.open_pool(&pool_ref).and_then(|pool| {
        let frame = pool.get_lite3(seq)?;
        let payload = frame.payload.to_vec();
        lite3::validate_bytes(&payload)?;
        Ok(payload)
    });
    match result {
        Ok(payload) => {
            let mut response = Response::new(Body::from(Bytes::copy_from_slice(&payload)));
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("application/x-plasmite-lite3"),
            );
            response.headers_mut().insert(
                "plasmite-seq",
                HeaderValue::from_str(&seq.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );
            response
                .headers_mut()
                .insert("plasmite-version", HeaderValue::from_static("0"));
            response
        }
        Err(err) => error_response(err),
    }
}

async fn tail_messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
    Query(query): Query<TailQuery>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    let permit = match state.tail_semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                Error::new(ErrorKind::Busy)
                    .with_message("too many concurrent tail requests")
                    .with_hint("Try again later or reduce tail concurrency."),
            );
        }
    };
    if let Some(timeout_ms) = query.timeout_ms {
        if timeout_ms > state.max_tail_timeout_ms {
            return error_response(
                Error::new(ErrorKind::Usage)
                    .with_message("tail timeout exceeds server limit")
                    .with_hint(format!("Use timeout_ms <= {}.", state.max_tail_timeout_ms)),
            );
        }
    }
    let timeout_ms = query.timeout_ms.unwrap_or(state.max_tail_timeout_ms);

    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(16);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = client.open_pool(&pool_ref).and_then(|pool| {
            let options = TailOptions {
                since_seq: query.since_seq,
                max_messages: query.max.map(|value| value as usize),
                timeout: Some(std::time::Duration::from_millis(timeout_ms)),
                ..TailOptions::default()
            };
            let mut tail = pool.tail(options);
            while let Some(message) = tail.next_message()? {
                let line = match serde_json::to_vec(&message_json(&message)) {
                    Ok(mut bytes) => {
                        bytes.push(b'\n');
                        bytes
                    }
                    Err(err) => {
                        return Err(Error::new(ErrorKind::Internal)
                            .with_message("failed to encode message")
                            .with_source(err));
                    }
                };
                if tx.blocking_send(Ok(Bytes::from(line))).is_err() {
                    break;
                }
            }
            Ok(())
        });
        if let Err(err) = result {
            let _ = tx.blocking_send(Err(err));
        }
    });

    let stream = ReceiverStream::new(rx)
        .map(|result| result.map_err(|err| std::io::Error::other(err.to_string())));

    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/jsonl"),
    );
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

async fn tail_lite3(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
    Query(query): Query<TailQuery>,
) -> Response {
    if let Err(err) = authorize(&headers, &state) {
        return error_response(err);
    }
    if let Err(err) = ensure_read_access(&state) {
        return error_response(err);
    }
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    if let Some(since_seq) = query.since_seq {
        let precheck = state.client.open_pool(&pool_ref).and_then(|pool| {
            let frame = pool.get_lite3(since_seq)?;
            lite3::validate_bytes(frame.payload)?;
            Ok(())
        });
        match precheck {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return error_response(err),
        }
    }
    let client = state.client.clone();
    let permit = match state.tail_semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                Error::new(ErrorKind::Busy)
                    .with_message("too many concurrent tail requests")
                    .with_hint("Try again later or reduce tail concurrency."),
            );
        }
    };
    if let Some(timeout_ms) = query.timeout_ms {
        if timeout_ms > state.max_tail_timeout_ms {
            return error_response(
                Error::new(ErrorKind::Usage)
                    .with_message("tail timeout exceeds server limit")
                    .with_hint(format!("Use timeout_ms <= {}.", state.max_tail_timeout_ms)),
            );
        }
    }
    let timeout_ms = query.timeout_ms.unwrap_or(state.max_tail_timeout_ms);

    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(16);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = client.open_pool(&pool_ref).and_then(|pool| {
            let options = TailOptions {
                since_seq: query.since_seq,
                max_messages: query.max.map(|value| value as usize),
                timeout: Some(std::time::Duration::from_millis(timeout_ms)),
                ..TailOptions::default()
            };
            let mut tail = pool.tail_lite3(options);
            while let Some(frame) = tail.next_frame()? {
                lite3::validate_bytes(frame.payload)?;
                let encoded = encode_lite3_stream_frame(&frame)?;
                if tx.blocking_send(Ok(encoded)).is_err() {
                    break;
                }
            }
            Ok(())
        });
        if let Err(err) = result {
            let _ = tx.blocking_send(Err(err));
        }
    });

    let stream = ReceiverStream::new(rx)
        .map(|result| result.map_err(|err| std::io::Error::other(err.to_string())));

    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/x-plasmite-lite3-stream"),
    );
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

fn pool_ref_from_request(pool: &str) -> Result<PoolRef, Error> {
    if pool.contains('/') {
        return Err(
            Error::new(ErrorKind::Usage).with_message("pool name must not contain path separators")
        );
    }
    Ok(PoolRef::name(pool))
}

fn message_json(message: &plasmite::api::Message) -> serde_json::Value {
    json!({
        "seq": message.seq,
        "time": message.time.clone(),
        "meta": { "descrips": message.meta.descrips.clone() },
        "data": message.data.clone(),
    })
}

fn pool_info_json(pool_ref: &str, info: &PoolInfo) -> serde_json::Value {
    json!({
        "name": pool_ref,
        "path": info.path.display().to_string(),
        "file_size": info.file_size,
        "ring_offset": info.ring_offset,
        "ring_size": info.ring_size,
        "bounds": bounds_json(info.bounds),
    })
}

fn bounds_json(bounds: Bounds) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(oldest) = bounds.oldest_seq {
        map.insert("oldest".to_string(), json!(oldest));
    }
    if let Some(newest) = bounds.newest_seq {
        map.insert("newest".to_string(), json!(newest));
    }
    serde_json::Value::Object(map)
}

fn json_response(payload: serde_json::Value) -> Response {
    let mut response = Json(payload).into_response();
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

fn encode_lite3_stream_frame(frame: &plasmite::api::FrameRef<'_>) -> Result<Bytes, Error> {
    let payload_len: u32 = frame.payload.len().try_into().map_err(|_| {
        Error::new(ErrorKind::Usage).with_message("lite3 payload exceeds max frame length")
    })?;
    let mut buf = Vec::with_capacity(8 + 8 + 4 + payload_len as usize);
    buf.extend_from_slice(&frame.seq.to_be_bytes());
    buf.extend_from_slice(&frame.timestamp_ns.to_be_bytes());
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.extend_from_slice(frame.payload);
    Ok(Bytes::from(buf))
}

fn durability_from_str(value: Option<&str>) -> Durability {
    match value {
        Some("flush") => Durability::Flush,
        _ => Durability::Fast,
    }
}

fn error_response(err: Error) -> Response {
    let status = match err.kind() {
        ErrorKind::Usage => StatusCode::BAD_REQUEST,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        ErrorKind::Busy => StatusCode::LOCKED,
        ErrorKind::Permission => {
            if is_access_forbidden(&err) {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            }
        }
        ErrorKind::Corrupt | ErrorKind::Io | ErrorKind::Internal => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    error_response_with_status(err, status)
}

fn error_response_with_status(err: Error, status: StatusCode) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            kind: format!("{:?}", err.kind()),
            message: err.message().unwrap_or("error").to_string(),
            path: err.path().map(|path| path.to_string_lossy().to_string()),
            seq: err.seq(),
            offset: err.offset(),
        },
    };
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

fn is_access_forbidden(err: &Error) -> bool {
    err.message()
        .is_some_and(|message| message.starts_with("forbidden:"))
}

#[cfg(test)]
mod tests {
    use super::{AccessMode, ErrorKind, ServeConfig, serve, validate_config};

    #[tokio::test]
    async fn serve_rejects_non_loopback_bind() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ServeConfig {
            bind: "0.0.0.0:0".parse().expect("bind"),
            pool_dir: temp.path().to_path_buf(),
            token: None,
            access_mode: AccessMode::ReadWrite,
            allow_non_loopback: false,
            insecure_no_tls: false,
            token_file_used: false,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        };
        let err = serve(config).await.expect_err("expected usage error");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn non_loopback_requires_allow_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ServeConfig {
            bind: "0.0.0.0:0".parse().expect("bind"),
            pool_dir: temp.path().to_path_buf(),
            token: None,
            access_mode: AccessMode::ReadOnly,
            allow_non_loopback: false,
            insecure_no_tls: false,
            token_file_used: false,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        };
        let err = validate_config(&config).expect_err("expected usage error");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn non_loopback_read_only_allows_unauthenticated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ServeConfig {
            bind: "0.0.0.0:0".parse().expect("bind"),
            pool_dir: temp.path().to_path_buf(),
            token: None,
            access_mode: AccessMode::ReadOnly,
            allow_non_loopback: true,
            insecure_no_tls: false,
            token_file_used: false,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        };
        validate_config(&config).expect("config ok");
    }

    #[test]
    fn non_loopback_write_requires_token_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ServeConfig {
            bind: "0.0.0.0:0".parse().expect("bind"),
            pool_dir: temp.path().to_path_buf(),
            token: Some("dev".to_string()),
            access_mode: AccessMode::WriteOnly,
            allow_non_loopback: true,
            insecure_no_tls: true,
            token_file_used: false,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        };
        let err = validate_config(&config).expect_err("expected usage error");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn non_loopback_write_requires_tls_or_insecure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ServeConfig {
            bind: "0.0.0.0:0".parse().expect("bind"),
            pool_dir: temp.path().to_path_buf(),
            token: Some("dev".to_string()),
            access_mode: AccessMode::WriteOnly,
            allow_non_loopback: true,
            insecure_no_tls: false,
            token_file_used: true,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        };
        let err = validate_config(&config).expect_err("expected usage error");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn safety_limits_require_positive_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = ServeConfig {
            bind: "127.0.0.1:0".parse().expect("bind"),
            pool_dir: temp.path().to_path_buf(),
            token: None,
            access_mode: AccessMode::ReadOnly,
            allow_non_loopback: false,
            insecure_no_tls: false,
            token_file_used: false,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
            max_body_bytes: 0,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 64,
        };
        let err = validate_config(&config).expect_err("expected usage error");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }
}
