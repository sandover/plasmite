//! Purpose: Provide the HTTP/JSON remote server for Plasmite.
//! Exports: `ServeConfig`, `serve_secure_pair`.
//! Role: Axum-based local and TLS remote servers implementing the remote v0 spec.
//! Invariants: JSON envelopes match spec/remote/v0/SPEC.md; error kinds remain stable.
//! Invariants: Local administration stays on loopback; remote access requires TLS and an access key.
//! Notes: Streaming uses JSONL or framed Lite3; tail is at-least-once and resumable.

use axum::body::Body;
use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequestParts, MatchedPath, Path as AxumPath, Query,
    RawQuery, State,
};
use axum::http::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use bytes::Bytes;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use hyper_util::service::TowerToHyperService;
use rustls::ServerConfig;
use rustls::pki_types::pem::{Error as PemError, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Duration;
use tokio_rustls::TlsAcceptor;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::timeout::RequestBodyTimeoutLayer;
use tower_http::trace::TraceLayer;
use tower_service::Service;
use tracing_subscriber::EnvFilter;
use url::{Host, Url};

use crate::access_store::{AccessGrant, AccessStore};
#[path = "activity.rs"]
mod activity;
#[path = "share_addresses.rs"]
mod share_addresses;
use activity::{ActivityRegistry, RequestActivity};
#[path = "browser_session.rs"]
mod browser_session;
use crate::interface_error_kind;
use crate::interface_wire::{MessageWire, error_policy};
use crate::pool_info_json::pool_info_json;
use browser_session::{BrowserSessions, COOKIE_NAME, LIFETIME};
#[path = "oauth.rs"]
mod oauth;
use oauth::OauthService;
use plasmite::api::{
    Durability, Error, ErrorKind, GapPolicy, LocalClient, Pool, PoolApiExt, PoolInfo, PoolOptions,
    PoolRef, TailOptions, lite3,
};
use plasmite::mcp::{
    DispatchOutcome, JsonRpcError as McpJsonRpcError, MCP_PROTOCOL_VERSION, McpDispatcher,
    McpHandler, McpResource, McpTool, McpToolAccess, PlasmiteMcpHandler, ResourceReadRequest,
    ResourceReadResult, ToolCallRequest, ToolCallResult,
};

const UI_INDEX_HTML: &str = include_str!("../ui/index.html");
const UI_ACCESS_HTML: &str = include_str!("../ui/access.html");
const UI_MAP_HTML: &str = include_str!("../ui/map.html");
const UI_POOL_HTML: &str = include_str!("../ui/pool.html");
const UI_COMMON_JS: &str = include_str!("../ui/common.js");
const UI_INCONSOLATA_WOFF2: &[u8] =
    include_bytes!("../ui/fonts/inconsolata-latin-wght-normal.woff2");
const READY_FILE_ENV: &str = "PLASMITE_SERVE_READY_FILE";
// Bound sockets before authentication, including incomplete TLS handshakes.
const MAX_TLS_CONNECTIONS: usize = 128;
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_BODY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub bind: SocketAddr,
    pub pool_dir: PathBuf,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub max_body_bytes: u64,
    pub max_tail_timeout_ms: u64,
    pub max_concurrent_tails: usize,
}

#[derive(Clone)]
struct StorageExecutor {
    permits: Arc<Semaphore>,
}

impl StorageExecutor {
    fn new(capacity: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(capacity.max(1))),
        }
    }

    async fn run<T, F>(&self, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, Error> + Send + 'static,
    {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            Error::new(ErrorKind::Busy)
                .with_message("server storage executor is saturated")
                .with_hint("Retry after an in-flight storage request completes.")
        })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("server storage task failed")
                .with_source(err)
        })?
    }

    async fn run_authorized<T, F>(
        &self,
        grant: Option<AccessGrant>,
        operation: F,
    ) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, Error> + Send + 'static,
    {
        self.run(move || {
            check_grant(grant.as_ref())?;
            operation()
        })
        .await
    }
}

fn check_grant(grant: Option<&AccessGrant>) -> Result<(), Error> {
    if grant.is_some_and(AccessGrant::is_revoked) {
        Err(Error::new(ErrorKind::Permission).with_message("access key was revoked"))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
struct AppState {
    client: LocalClient,
    secure_access: Option<Arc<AccessStore>>,
    oauth: Option<Arc<OauthService>>,
    browser_sessions: Arc<BrowserSessions>,
    local_admin: bool,
    max_tail_timeout_ms: u64,
    tail_semaphore: Arc<Semaphore>,
    mcp_semaphore: Arc<Semaphore>,
    storage_executor: StorageExecutor,
    ui_pool_watch: Arc<Mutex<Option<watch::Sender<Value>>>>,
    activity: ActivityRegistry,
}

pub(crate) async fn serve_secure_pair(
    local: ServeConfig,
    remote: ServeConfig,
    access: Arc<AccessStore>,
) -> Result<(), Error> {
    validate_config(&local)?;
    validate_config(&remote)?;
    if !local.bind.ip().is_loopback() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("local administration must bind to loopback")
        );
    }
    if local.bind == remote.bind && local.bind.port() != 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("local and remote listeners need different addresses"));
    }
    if remote.tls_cert.is_none() || remote.tls_key.is_none() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote sharing requires a TLS certificate and key"));
    }
    let activity = ActivityRegistry::default();
    let local_server =
        prepare_server_with_access(&local, Some(access.clone()), true, activity.clone()).await?;
    let remote_server =
        prepare_server_with_access(&remote, Some(access.clone()), false, activity).await?;
    let local_listener = tokio::net::TcpListener::bind(local.bind)
        .await
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to bind local administration listener")
                .with_source(err)
        })?;
    let remote_listener = tokio::net::TcpListener::bind(remote.bind)
        .await
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to bind remote HTTPS listener")
                .with_source(err)
        })?;
    access.write_local_bind(local_listener.local_addr().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to inspect local listener")
            .with_source(err)
    })?)?;
    let remote_address = remote_listener.local_addr().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to inspect HTTPS listener")
            .with_source(err)
    })?;
    let remote_url = access.shared_address().map(str::to_owned).or_else(|| {
        remote_address
            .ip()
            .is_loopback()
            .then(|| format!("https://{remote_address}"))
    });
    let registered = crate::serve_registry::register(
        &local.pool_dir,
        local_listener.local_addr().map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to inspect local listener")
                .with_source(err)
        })?,
        remote_url,
    )?;
    let registration = registered.registration.clone();
    let status_route = Router::new()
        .route(
            crate::serve_registry::STATUS_PATH,
            get(move || {
                let registration = registration.clone();
                async move { Json(registration) }
            }),
        )
        .layer(middleware::from_fn(local_request_guard));
    let local_app = local_server.app.merge(status_route);
    notify_ready_file(&remote_listener)?;
    let remote_tls = remote_server.tls_config.ok_or_else(|| {
        Error::new(ErrorKind::Internal).with_message("remote TLS was not configured")
    })?;
    tokio::try_join!(
        serve_plain(local_listener, local_app, shutdown_signal()),
        serve_tls(
            remote_listener,
            remote_server.app,
            remote_tls,
            shutdown_signal()
        ),
    )?;
    Ok(())
}

fn notify_ready_file(listener: &tokio::net::TcpListener) -> Result<(), Error> {
    let Some(path) = std::env::var_os(READY_FILE_ENV) else {
        return Ok(());
    };
    let addr = listener.local_addr().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to inspect bound server address")
            .with_source(err)
    })?;
    let path = PathBuf::from(path);
    let temporary_path = path.with_extension("tmp");
    std::fs::write(&temporary_path, addr.to_string())
        .and_then(|()| std::fs::rename(&temporary_path, &path))
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to publish bound server address")
                .with_path(path)
                .with_source(err)
        })
}

struct PreparedServer {
    app: Router,
    tls_config: Option<Arc<ServerConfig>>,
}

async fn prepare_server_with_access(
    config: &ServeConfig,
    secure_access: Option<Arc<AccessStore>>,
    local_admin: bool,
    activity: ActivityRegistry,
) -> Result<PreparedServer, Error> {
    validate_config(config)?;

    init_tracing();

    let max_body_bytes: usize = config
        .max_body_bytes
        .try_into()
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("--max-body-bytes is too large"))?;

    let tls_config = build_tls_config(config).await?;

    let oauth = if local_admin {
        None
    } else {
        secure_access
            .as_ref()
            .map(|access| OauthService::open(access.clone()))
            .transpose()?
            .flatten()
    };
    let browser_sessions = Arc::new(BrowserSessions::new(if local_admin {
        None
    } else {
        secure_access.clone()
    })?);
    let state = Arc::new(AppState {
        client: LocalClient::new().with_pool_dir(config.pool_dir.clone()),
        secure_access,
        oauth: oauth.clone(),
        browser_sessions,
        local_admin,
        max_tail_timeout_ms: config.max_tail_timeout_ms,
        tail_semaphore: Arc::new(Semaphore::new(config.max_concurrent_tails)),
        mcp_semaphore: Arc::new(Semaphore::new(config.max_concurrent_tails)),
        // Reuse the existing server concurrency budget instead of adding another
        // public tuning flag. Storage operations are shorter lived than tails.
        storage_executor: StorageExecutor::new(config.max_concurrent_tails),
        ui_pool_watch: Arc::new(Mutex::new(None)),
        activity,
    });

    let mut app = Router::new()
        .route("/", get(ui_index))
        .route("/healthz", get(healthz))
        .route("/v0/access/invite", post(access_invite))
        .route("/v0/access/keys", get(access_keys))
        .route("/v0/access/revoke", post(access_revoke))
        .route("/v0/access/check", get(access_check))
        .route("/v0/access/status", get(access_status))
        .route(
            "/v0/browser/session",
            get(browser_session_status)
                .post(browser_login)
                .delete(browser_logout),
        )
        .route("/mcp", post(mcp_post).get(mcp_get))
        .route("/ui", get(ui_index))
        .route("/ui/map", get(ui_map))
        .route("/ui/assets/inconsolata.woff2", get(ui_inconsolata))
        .route("/ui/assets/common.js", get(ui_common_js))
        .route("/ui/pools/:pool", get(ui_pool))
        .route("/access", get(ui_access))
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
        .route("/v0/ui/pools", get(ui_list_pools))
        .route("/v0/ui/pools/stream", get(ui_pool_stream))
        .route("/v0/ui/pools/:pool/info", get(pool_info))
        .route("/v0/ui/pools/:pool/events", get(ui_events))
        .route("/v0/ui/activity", get(ui_activity))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            observe_activity,
        ))
        .with_state(state.clone())
        .layer(TraceLayer::new_for_http());

    if let Some(oauth) = oauth {
        app = app.merge(oauth.router());
    }

    if local_admin {
        app = app.layer(middleware::from_fn(local_request_guard));
    } else {
        app = app.layer(middleware::from_fn_with_state(state, remote_browser_guard));
    }

    app = app
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .layer(RequestBodyTimeoutLayer::new(REQUEST_BODY_TIMEOUT));

    Ok(PreparedServer { app, tls_config })
}

async fn observe_activity(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_default();
    if route != "/mcp" && !observes_pool_route(&route) {
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let kind = if route == "/mcp" {
        "mcp"
    } else if route.starts_with("/v0/ui/")
        || request.headers().contains_key(header::ORIGIN)
        || request
            .headers()
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(';').any(|part| {
                    part.trim()
                        .split_once('=')
                        .is_some_and(|(name, _)| name == COOKIE_NAME)
                })
            })
    {
        "browser"
    } else {
        "native"
    };
    let activity = state.activity.request(peer, request.headers(), kind);
    let method = request.method().to_string();
    let (mut parts, body) = request.into_parts();
    if observes_pool_route(&route)
        && let Ok(AxumPath(params)) =
            AxumPath::<BTreeMap<String, String>>::from_request_parts(&mut parts, &()).await
        && let Some(pool) = params.get("pool")
    {
        activity.pool(pool.clone(), format!("{method} {route}"));
    }
    parts.extensions.insert(activity.clone());
    let response = next.run(Request::from_parts(parts, body)).await;
    if !response.status().is_success() {
        return response;
    }
    activity.retain_for_response(response)
}

fn observes_pool_route(route: &str) -> bool {
    (route.starts_with("/v0/pools/:pool") && !route.ends_with("/info"))
        || route == "/v0/ui/pools/:pool/events"
}

async fn local_request_guard(request: Request<Body>, next: Next) -> Response {
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let Some(host) = host else {
        return reject_after_body(
            request,
            Error::new(ErrorKind::Permission).with_message("local request requires a Host header"),
        )
        .await;
    };
    let parsed_host = Url::parse(&format!("http://{host}/"));
    let host_is_local = parsed_host.as_ref().is_ok_and(|url| {
        let loopback = match url.host() {
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
        loopback && url.username().is_empty() && url.password().is_none()
    });
    let origin_matches = match headers.get(header::ORIGIN) {
        None => true,
        Some(origin) => parsed_host.as_ref().is_ok_and(|host_url| {
            origin.to_str().is_ok_and(|origin| {
                Url::parse(origin).is_ok_and(|origin_url| origin_url.origin() == host_url.origin())
            })
        }),
    };
    if !host_is_local || !origin_matches {
        return reject_after_body(
            request,
            Error::new(ErrorKind::Permission).with_message("untrusted local request origin"),
        )
        .await;
    }
    next.run(request).await
}

async fn remote_browser_guard(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let browser_login =
        request.uri().path() == "/v0/browser/session" && request.method() != Method::GET;
    let cookie_write = !matches!(*request.method(), Method::GET | Method::HEAD)
        && cookie_token(request.headers()).is_some()
        && !request.headers().contains_key(header::AUTHORIZATION);
    if (browser_login || cookie_write) && !same_origin(request.headers(), request.uri(), "https") {
        return reject_after_body(
            request,
            Error::new(ErrorKind::Permission).with_message("untrusted browser request origin"),
        )
        .await;
    }
    // Check permission before the extractors buffer or parse the body.
    let path = request.uri().path();
    let protected_pool = path == "/v0/pools"
        || path.starts_with("/v0/pools/")
        || path == "/v0/ui/pools"
        || path.starts_with("/v0/ui/pools/");
    if protected_pool && let Err(err) = authorize(request.headers(), &state) {
        drain_rejected_body(request).await;
        return error_response(err);
    }
    if path == "/mcp" && request.method() == Method::POST {
        if let Some(oauth) = &state.oauth {
            let bearer = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "));
            if bearer.and_then(|value| oauth.grant(value)).is_none() {
                drain_rejected_body(request).await;
                return oauth.challenge();
            }
        }
    }
    next.run(request).await
}

/// Largest request body a guard reads before it rejects the request.
const REJECTED_BODY_DRAIN_BYTES: usize = 64 * 1024;

/// Rejects a request with 403 after reading its body.
///
/// When a response goes out before the request body arrives, hyper closes the
/// connection, and body bytes that arrive after the close make the kernel reset
/// it. The reset can destroy the response before the client reads it. Reading
/// the body first keeps the connection open. Bodies over the cap still close it.
async fn reject_after_body(request: Request<Body>, err: Error) -> Response {
    drain_rejected_body(request).await;
    error_response_with_status(err, StatusCode::FORBIDDEN)
}

async fn drain_rejected_body(request: Request<Body>) {
    // A client must not keep a rejected request alive by withholding its body.
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        axum::body::to_bytes(request.into_body(), REJECTED_BODY_DRAIN_BYTES),
    )
    .await;
}

fn same_origin(headers: &HeaderMap, uri: &Uri, scheme: &str) -> bool {
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts.next().and_then(|value| value.to_str().ok());
    if hosts.next().is_some() {
        return false;
    }
    let authority = uri.authority().map(|value| value.as_str());
    let host = match (host, authority) {
        (Some(host), Some(authority)) if host.eq_ignore_ascii_case(authority) => host,
        (Some(host), None) => host,
        (None, Some(authority)) => authority,
        _ => return false,
    };
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if origins.next().is_some() {
        return false;
    }
    let expected = Url::parse(&format!("{scheme}://{host}/"));
    let actual = Url::parse(origin);
    expected.as_ref().is_ok_and(|expected| {
        actual.as_ref().is_ok_and(|actual| {
            expected.username().is_empty()
                && expected.password().is_none()
                && actual.username().is_empty()
                && actual.password().is_none()
                && expected.origin() == actual.origin()
        })
    })
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers.get_all(header::COOKIE).iter().find_map(|header| {
        header.to_str().ok()?.split(';').find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == COOKIE_NAME
                && value.len() == 64
                && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(value)
        })
    })
}

fn validate_config(config: &ServeConfig) -> Result<(), Error> {
    if config.tls_cert.is_some() != config.tls_key.is_some() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("TLS requires both --tls-cert and --tls-key")
        );
    }
    if config.max_body_bytes == 0 || config.max_body_bytes > usize::MAX as u64 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-body-bytes must fit in memory and be greater than zero"));
    }
    if config.max_tail_timeout_ms == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-tail-timeout-ms must be greater than zero"));
    }
    if config.max_concurrent_tails == 0 {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--max-tail-concurrency must be greater than zero"));
    }
    Ok(())
}

async fn build_tls_config(config: &ServeConfig) -> Result<Option<Arc<ServerConfig>>, Error> {
    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        let tls = load_tls_config_from_pem(cert, key)?;
        return Ok(Some(Arc::new(tls)));
    }

    Ok(None)
}

fn load_tls_config_from_pem(cert_path: &Path, key_path: &Path) -> Result<ServerConfig, Error> {
    let certs = load_certificates_from_pem(cert_path)?;
    let key_bytes = std::fs::read(key_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read TLS key")
            .with_path(key_path)
            .with_source(err)
    })?;

    let key = PrivateKeyDer::from_pem_reader(key_bytes.as_slice()).map_err(|err| match err {
        PemError::NoItemsFound => Error::new(ErrorKind::Usage)
            .with_message("TLS key file contains no private key")
            .with_path(key_path),
        _ => Error::new(ErrorKind::Io)
            .with_message("failed to parse TLS key")
            .with_path(key_path)
            .with_source(err),
    })?;

    build_server_config(certs, key)
}

pub(crate) fn validate_tls_files(cert_path: &Path, key_path: &Path) -> Result<(), Error> {
    load_tls_config_from_pem(cert_path, key_path).map(|_| ())
}

fn load_certificates_from_pem(cert_path: &Path) -> Result<Vec<CertificateDer<'static>>, Error> {
    let cert_bytes = std::fs::read(cert_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read TLS certificate")
            .with_path(cert_path)
            .with_source(err)
    })?;

    let certs = CertificateDer::pem_slice_iter(&cert_bytes)
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
    Ok(certs)
}

fn build_server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
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

async fn serve_plain(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
        _ = shutdown => {
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
    listener: tokio::net::TcpListener,
    app: Router,
    tls_config: Arc<ServerConfig>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), Error> {
    let acceptor = TlsAcceptor::from(tls_config);
    let mut builder = AutoBuilder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(REQUEST_HEADER_TIMEOUT);
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(Duration::from_secs(15))
        .keep_alive_timeout(Duration::from_secs(10));
    let mut make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    let mut tasks = JoinSet::new();

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accept = listener.accept() => {
                let (stream, peer_addr) = match accept {
                    Ok(result) => result,
                    Err(err) => {
                        tracing::warn!("failed to accept TLS connection: {err}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };

                if tasks.len() >= MAX_TLS_CONNECTIONS {
                    drop(stream);
                    continue;
                }

                let service = match make_service.call(peer_addr).await {
                    Ok(service) => service,
                    Err(_) => continue,
                };

                let acceptor = acceptor.clone();
                let builder = builder.clone();
                tasks.spawn(async move {
                    let tls_stream = match tokio::time::timeout(
                        TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream),
                    ).await {
                        Ok(Ok(stream)) => stream,
                        _ => return,
                    };
                    let io = TokioIo::new(tls_stream);
                    let service = TowerToHyperService::new(service);
                    let _ = builder.serve_connection_with_upgrades(io, service).await;
                });
            }
        }
    }

    let drain = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(Duration::from_secs(10), drain)
        .await
        .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
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

fn authorize(headers: &HeaderMap, state: &AppState) -> Result<Option<AccessGrant>, Error> {
    if state.local_admin {
        return Ok(None);
    }
    if let Some(token) = cookie_token(headers)
        && !headers.contains_key(header::AUTHORIZATION)
    {
        return state
            .browser_sessions
            .grant(token)
            .map(Some)
            .ok_or_else(|| {
                Error::new(ErrorKind::Permission).with_message("browser session ended")
            });
    }
    authorize_bearer(headers, state)
}

fn authorize_bearer(headers: &HeaderMap, state: &AppState) -> Result<Option<AccessGrant>, Error> {
    if state.local_admin {
        return Ok(None);
    }
    if let Some(access) = &state.secure_access {
        let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
            return Err(Error::new(ErrorKind::Permission).with_message("missing access key"));
        };
        let secret = value
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "));
        return secret
            .and_then(|secret| access.authorize_secret(secret))
            .map(Some)
            .ok_or_else(|| Error::new(ErrorKind::Permission).with_message("invalid access key"));
    }
    Err(Error::new(ErrorKind::Permission).with_message("secure access is unavailable"))
}

#[derive(Deserialize)]
struct InviteRequest {
    name: String,
    server_fingerprint: String,
}

async fn access_invite(
    State(state): State<Arc<AppState>>,
    Json(request): Json<InviteRequest>,
) -> Response {
    let access = match local_access(&state, &request.server_fingerprint) {
        Ok(access) => access,
        Err(err) => return error_response(err),
    };
    match access.issue(&request.name) {
        Ok(access_key) => json_response(json!({"access_key": access_key})),
        Err(err) => error_response(err),
    }
}

fn local_access<'a>(state: &'a AppState, fingerprint: &str) -> Result<&'a AccessStore, Error> {
    if !state.local_admin {
        return Err(
            Error::new(ErrorKind::Permission).with_message("access administration is local only")
        );
    }
    let access = state
        .secure_access
        .as_deref()
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("secure sharing is not active"))?;
    if fingerprint != access.fingerprint() {
        return Err(Error::new(ErrorKind::Permission)
            .with_message("local server does not own the selected pool directory"));
    }
    Ok(access)
}

async fn access_keys(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let fingerprint = headers
        .get("x-plasmite-server-fingerprint")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let access = match local_access(&state, fingerprint) {
        Ok(access) => access,
        Err(err) => return error_response(err),
    };
    match access.list() {
        Ok(keys) => json_response(json!({"keys": keys})),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct RevokeRequest {
    id: String,
    server_fingerprint: String,
}

async fn access_revoke(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RevokeRequest>,
) -> Response {
    let access = match local_access(&state, &request.server_fingerprint) {
        Ok(access) => access,
        Err(err) => return error_response(err),
    };
    match access.revoke(&request.id) {
        Ok(()) => json_response(json!({"revoked": true, "id": request.id})),
        Err(err) => error_response(err),
    }
}

async fn access_check(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match authorize_bearer(&headers, &state) {
        Ok(_) => json_response(json!({"accepted": true})),
        Err(err) => error_response(err),
    }
}

async fn access_status(State(state): State<Arc<AppState>>) -> Response {
    if !state.local_admin {
        return error_response(
            Error::new(ErrorKind::Permission).with_message("access administration is local only"),
        );
    }
    // The pool directory and this machine's addresses let the page show the
    // exact command to start sharing.
    let pool_dir = state.client.pool_dir().display().to_string();
    let addresses = share_addresses::candidates();
    let Some(access) = &state.secure_access else {
        return json_response(json!({
            "ready": false,
            "pool_dir": pool_dir,
            "addresses": addresses,
            "next_action": "Start secure serving with --shared-address to invite others."
        }));
    };
    json_response(json!({
        "ready": access.shared_address().is_some(),
        "pool_dir": pool_dir,
        "addresses": addresses,
        "server_fingerprint": access.fingerprint(),
        "shared_address": access.shared_address(),
        "next_action": if access.shared_address().is_none() {
            Some("Restart with --shared-address to tell recipients where to connect.")
        } else {
            None
        }
    }))
}

#[derive(Deserialize)]
struct BrowserLoginRequest {
    access_key: String,
}

async fn browser_login(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BrowserLoginRequest>,
) -> Response {
    if state.local_admin {
        return error_response(
            Error::new(ErrorKind::Permission)
                .with_message("browser key login is available on the HTTPS listener"),
        );
    }
    let Some(access) = &state.secure_access else {
        return error_response(
            Error::new(ErrorKind::Permission).with_message("secure access is unavailable"),
        );
    };
    let Some((key_id, _)) = access.authorize_key(&request.access_key) else {
        return error_response(
            Error::new(ErrorKind::Permission).with_message("invalid access key"),
        );
    };
    let token = match state.browser_sessions.create(&key_id) {
        Ok(token) => token,
        Err(err) => return error_response(err),
    };
    let mut response = json_response(json!({"authenticated": true}));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{COOKIE_NAME}={token}; Path=/v0; Max-Age={}; Secure; HttpOnly; SameSite=Strict",
            LIFETIME.as_secs()
        ))
        .expect("session cookie is ASCII"),
    );
    response
}

async fn browser_session_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if state.local_admin {
        return json_response(json!({"authenticated": true, "local": true}));
    }
    match cookie_token(&headers).and_then(|token| state.browser_sessions.grant(token)) {
        Some(_) => {
            let mut response = json_response(json!({"authenticated": true, "local": false}));
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        None => {
            error_response(Error::new(ErrorKind::Permission).with_message("browser session ended"))
        }
    }
}

async fn browser_logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_token(&headers) {
        if let Err(err) = state.browser_sessions.remove(token) {
            return error_response(err);
        }
    }
    let mut response = json_response(json!({"authenticated": false}));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "plasmite_session=; Path=/v0; Max-Age=0; Secure; HttpOnly; SameSite=Strict",
        ),
    );
    response
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
    tags: Option<Vec<String>>,
    durability: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TailQuery {
    since_seq: Option<u64>,
    max: Option<u64>,
    timeout_ms: Option<u64>,
    gap_policy: Option<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TailStreamEncoding {
    Jsonl,
    Lite3,
    Sse,
}

struct TailRuntime {
    permit: OwnedSemaphorePermit,
    options: TailOptions,
    deadline: Instant,
}

#[derive(Debug, Deserialize)]
struct AppendLite3Query {
    durability: Option<String>,
}

async fn healthz() -> Response {
    json_response(json!({ "ok": true }))
}

async fn mcp_get() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

async fn mcp_post(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
    activity: Option<Extension<RequestActivity>>,
    Json(payload): Json<Value>,
) -> Response {
    let grant = if state.local_admin {
        None
    } else if let Some(oauth) = &state.oauth {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        match bearer.and_then(|token| oauth.grant(token)) {
            Some(grant) => Some(grant),
            None => return oauth.challenge(),
        }
    } else {
        return error_response_with_status(
            Error::new(ErrorKind::Permission)
                .with_message("direct MCP requires a configured shared HTTPS address"),
            StatusCode::SERVICE_UNAVAILABLE,
        );
    };
    if let Err(response) = validate_mcp_protocol_version(&headers, &payload) {
        return response;
    }
    if let Err(err) = validate_mcp_origin_header(&headers) {
        return error_response_with_status(err, StatusCode::FORBIDDEN);
    }
    if headers.contains_key(header::ORIGIN)
        && !same_origin(
            &headers,
            &uri,
            if state.local_admin { "http" } else { "https" },
        )
    {
        return error_response_with_status(
            Error::new(ErrorKind::Permission)
                .with_message("forbidden: MCP Origin does not match Host"),
            StatusCode::FORBIDDEN,
        );
    }
    if is_jsonrpc_response_payload(&payload) {
        return mcp_request_error(
            &payload,
            StatusCode::BAD_REQUEST,
            -32600,
            "JSON-RPC responses are not accepted",
        );
    }

    let permit = match state.mcp_semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                Error::new(ErrorKind::Busy)
                    .with_message("too many concurrent MCP requests")
                    .with_hint("Retry after an in-flight MCP request completes."),
            );
        }
    };
    let dispatch_state = state.clone();
    if let Some(Extension(activity)) = &activity
        && let Some((pool, operation)) = mcp_activity_pool(&payload)
    {
        activity.pool(pool, operation);
    }
    let dispatch = tokio::task::spawn_blocking(move || {
        let _activity = activity;
        let _permit = permit;
        let handler = ServeMcpHandler::new(
            dispatch_state.client.clone(),
            dispatch_state.tail_semaphore.clone(),
        )
        .with_grant(grant);
        let mut dispatcher = McpDispatcher::new(handler);
        dispatcher.dispatch_value(payload)
    })
    .await;
    let outcome = match dispatch {
        Ok(outcome) => outcome,
        Err(err) => {
            return error_response(
                Error::new(ErrorKind::Internal)
                    .with_message("MCP request task failed")
                    .with_source(err),
            );
        }
    };

    match outcome {
        DispatchOutcome::NoResponse => accepted_response(),
        DispatchOutcome::Response(response) => {
            let status = match response.error.as_ref().map(|error| error.code) {
                Some(-32601) => StatusCode::NOT_FOUND,
                Some(-32022..=-32020) => StatusCode::BAD_REQUEST,
                _ => StatusCode::OK,
            };
            let payload = match serde_json::to_value(response) {
                Ok(value) => value,
                Err(err) => {
                    return error_response(
                        Error::new(ErrorKind::Internal)
                            .with_message("failed to encode MCP response")
                            .with_source(err),
                    );
                }
            };
            let mut reply = json_response(payload);
            *reply.status_mut() = status;
            reply
        }
    }
}

fn mcp_activity_pool(payload: &Value) -> Option<(String, String)> {
    match payload.get("method")?.as_str()? {
        "tools/call" => {
            let name = payload.pointer("/params/name")?.as_str()?;
            if !matches!(
                name,
                "plasmite_pool_delete"
                    | "plasmite_feed"
                    | "plasmite_fetch"
                    | "plasmite_read"
                    | "plasmite_wait"
            ) {
                return None;
            }
            let pool = payload.pointer("/params/arguments/pool")?.as_str()?;
            pool_ref_from_request(pool).ok()?;
            Some((pool.to_owned(), name.to_owned()))
        }
        "resources/read" => {
            let uri = payload.pointer("/params/uri")?.as_str()?;
            let pool = uri.strip_prefix("plasmite:///pools/")?;
            pool_ref_from_request(pool).ok()?;
            Some((pool.to_owned(), "resources/read".to_owned()))
        }
        _ => None,
    }
}

struct ServeMcpHandler {
    inner: PlasmiteMcpHandler,
    wait_semaphore: Arc<Semaphore>,
    grant: Option<AccessGrant>,
}

impl ServeMcpHandler {
    fn new(client: LocalClient, wait_semaphore: Arc<Semaphore>) -> Self {
        Self {
            inner: PlasmiteMcpHandler::with_client(client),
            wait_semaphore,
            grant: None,
        }
    }

    fn with_grant(mut self, grant: Option<AccessGrant>) -> Self {
        self.inner = self
            .inner
            .with_cancel(grant.as_ref().map(AccessGrant::cancellation_flag));
        self.grant = grant;
        self
    }

    fn revoked(&self) -> bool {
        self.grant.as_ref().is_some_and(AccessGrant::is_revoked)
    }
}

impl McpHandler for ServeMcpHandler {
    fn list_tools(&mut self) -> Result<Vec<McpTool>, McpJsonRpcError> {
        self.inner.list_tools()
    }

    fn call_tool(&mut self, request: ToolCallRequest) -> Result<ToolCallResult, McpJsonRpcError> {
        if self.revoked() {
            return Ok(mcp_revoked_tool_result());
        }
        let _wait_permit = if request.name == "plasmite_wait" {
            match self.wait_semaphore.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return Ok(mcp_wait_busy_tool_result()),
            }
        } else {
            None
        };
        let is_write = self
            .inner
            .list_tools()?
            .iter()
            .any(|tool| tool.name == request.name && tool.access == McpToolAccess::Write);
        let result = self.inner.call_tool(request);
        if !is_write && self.revoked() {
            Ok(mcp_revoked_tool_result())
        } else {
            result
        }
    }

    fn list_resources(&mut self) -> Result<Vec<McpResource>, McpJsonRpcError> {
        if self.revoked() {
            return Err(McpJsonRpcError::invalid_request("access key was revoked"));
        }
        let result = self.inner.list_resources();
        if self.revoked() {
            Err(McpJsonRpcError::invalid_request("access key was revoked"))
        } else {
            result
        }
    }

    fn read_resource(
        &mut self,
        request: ResourceReadRequest,
    ) -> Result<ResourceReadResult, McpJsonRpcError> {
        if self.revoked() {
            return Err(McpJsonRpcError::invalid_request("access key was revoked"));
        }
        let result = self.inner.read_resource(request);
        if self.revoked() {
            Err(McpJsonRpcError::invalid_request("access key was revoked"))
        } else {
            result
        }
    }
}

fn mcp_revoked_tool_result() -> ToolCallResult {
    ToolCallResult::execution_error_with_structured(
        "access key was revoked",
        Some(json!({"error_kind": "Permission"})),
    )
}

fn mcp_wait_busy_tool_result() -> ToolCallResult {
    ToolCallResult::execution_error_with_structured(
        "busy: too many concurrent tail or MCP wait requests",
        Some(json!({
            "error_kind": "Busy",
            "tool": "plasmite_wait",
            "hint": "Try again later or reduce long-lived read concurrency.",
        })),
    )
}

fn validate_mcp_protocol_version(headers: &HeaderMap, payload: &Value) -> Result<(), Response> {
    let method = payload.get("method").and_then(Value::as_str);
    let header_version = single_mcp_header(headers, "MCP-Protocol-Version")
        .map_err(|_| mcp_header_mismatch(payload, "invalid MCP-Protocol-Version header"))?;
    if header_version == Some(MCP_PROTOCOL_VERSION)
        || (header_version.is_none()
            && matches!(method, Some("initialize" | "notifications/initialized")))
    {
        return Ok(());
    }
    Err(mcp_request_error(
        payload,
        StatusCode::BAD_REQUEST,
        -32600,
        "unsupported or missing MCP-Protocol-Version",
    ))
}

fn single_mcp_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ()> {
    let mut values = headers.get_all(name).iter();
    let first = values
        .next()
        .map(|value| value.to_str().map_err(|_| ()))
        .transpose()?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(first)
}

fn mcp_header_mismatch(payload: &Value, message: &str) -> Response {
    mcp_request_error(payload, StatusCode::BAD_REQUEST, -32020, message)
}

fn mcp_request_error(payload: &Value, status: StatusCode, code: i64, message: &str) -> Response {
    let id = payload.get("id").cloned().unwrap_or(Value::Null);
    (
        status,
        Json(json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})),
    )
        .into_response()
}

fn validate_mcp_origin_header(headers: &HeaderMap) -> Result<(), Error> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let value = origin.to_str().map_err(|_| {
        Error::new(ErrorKind::Permission)
            .with_message("forbidden: invalid Origin header")
            .with_hint("Send a valid Origin URI or omit Origin.")
    })?;
    let parsed = Url::parse(value).map_err(|_| {
        Error::new(ErrorKind::Permission)
            .with_message("forbidden: invalid Origin header")
            .with_hint("Send a valid Origin URI or omit Origin.")
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            Error::new(ErrorKind::Permission).with_message("forbidden: invalid Origin header")
        );
    }
    Ok(())
}

fn is_jsonrpc_response_payload(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && object.contains_key("id")
        && !object.contains_key("method")
        && (object.contains_key("result") || object.contains_key("error"))
}

fn accepted_response() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::ACCEPTED;
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

async fn ui_index() -> Response {
    html_response(UI_INDEX_HTML)
}

async fn ui_map() -> Response {
    html_response(UI_MAP_HTML)
}

async fn ui_inconsolata() -> Response {
    let mut response = Response::new(Body::from(UI_INCONSOLATA_WOFF2));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("font/woff2"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
}

/// The top bar every UI page shares: the served directory and the jump to a pool.
async fn ui_common_js() -> Response {
    let mut response = Response::new(Body::from(UI_COMMON_JS));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn ui_pool(AxumPath(_pool): AxumPath<String>) -> Response {
    html_response(UI_POOL_HTML)
}

async fn ui_access(State(state): State<Arc<AppState>>) -> Response {
    if !state.local_admin {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return response;
    }
    html_response(UI_ACCESS_HTML)
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
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&payload.pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let size_bytes = payload.size_bytes.unwrap_or(1024 * 1024);
    let client = state.client.clone();
    let result = state
        .storage_executor
        .run_authorized(grant, move || {
            client.create_pool(&pool_ref, PoolOptions::new(size_bytes))
        })
        .await;
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
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&payload.pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    match state
        .storage_executor
        .run_authorized(grant, move || client.pool_info(&pool_ref))
        .await
    {
        Ok(info) => json_response(json!({ "pool": pool_info_json(&payload.pool, &info) })),
        Err(err) => error_response(err),
    }
}

async fn pool_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
) -> Response {
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    match state
        .storage_executor
        .run_authorized(grant, move || client.pool_info(&pool_ref))
        .await
    {
        Ok(info) => json_response(json!({ "pool": pool_info_json(&pool, &info) })),
        Err(err) => error_response(err),
    }
}

async fn list_pools(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    match state
        .storage_executor
        .run_authorized(grant, move || client.list_pools())
        .await
    {
        Ok(pools) => {
            let out: Vec<Value> = pools
                .iter()
                .map(|info| pool_info_json(&pool_file_name(&info.path), info))
                .collect();
            json_response(json!({ "pools": out }))
        }
        Err(err) => error_response(err),
    }
}

/// The pool name is the file name without its one `.plasmite` extension.
fn pool_file_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Most frames the map lists per pool. A ring on screen cannot draw more apart.
const UI_MAX_RING_FRAMES: u64 = 4096;

/// Where the ring's stored messages sit: tail and head offsets, and each frame
/// as `[offset, bytes]`, oldest first. `first` is the first frame's sequence
/// number, so the page can match messages to frames from this one read.
/// `frames` is null when the pool holds too many to draw apart, or a writer
/// kept changing the ring during the walk.
fn ring_json(pool: &Pool) -> Value {
    match pool.ring_layout(UI_MAX_RING_FRAMES) {
        Ok(layout) => {
            let first = layout
                .frames
                .as_ref()
                .and_then(|frames| frames.first())
                .map(|frame| frame.seq);
            json!({
                "tail": layout.tail,
                "head": layout.head,
                "first": first,
                "frames": layout.frames.map(|frames| {
                    frames.iter().map(|frame| [frame.offset, frame.len]).collect::<Vec<_>>()
                }),
            })
        }
        Err(_) => Value::Null,
    }
}

type UiPoolCache = BTreeMap<PathBuf, (PoolInfo, Value)>;

/// Pool info without message ages. Ages grow on every read, so comparing them
/// would count every pool that holds messages as changed on every scan.
fn settled(info: &PoolInfo) -> PoolInfo {
    let mut info = info.clone();
    if let Some(metrics) = info.metrics.as_mut() {
        metrics.age.oldest_age_ms = None;
        metrics.age.newest_age_ms = None;
    }
    info
}

/// A directory scan reads current headers. Ring layouts are measured when a
/// pool appears or its info changes, including a same-name replacement.
fn ui_pool_snapshot(client: &LocalClient, cache: &mut UiPoolCache) -> Result<(Value, bool), Error> {
    let mut infos = client.list_pools()?;
    infos.sort_by(|a, b| a.path.cmp(&b.path));
    let mut next = UiPoolCache::new();
    let mut pools = Vec::with_capacity(infos.len());
    let mut changed = infos.len() != cache.len();
    for info in infos {
        let ring = match cache.get(&info.path) {
            Some((prior, ring)) if settled(prior) == settled(&info) => ring.clone(),
            _ => {
                changed = true;
                Pool::open(&info.path)
                    .map(|pool| ring_json(&pool))
                    .unwrap_or(Value::Null)
            }
        };
        let mut pool = pool_info_json(&pool_file_name(&info.path), &info);
        pool["ring"] = ring.clone();
        next.insert(info.path.clone(), (info, ring));
        pools.push(pool);
    }
    *cache = next;
    Ok((json!({ "pools": pools }), changed))
}

/// The pool list plus each pool's ring layout, so the map can draw each message where it sits.
async fn ui_list_pools(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    match state
        .storage_executor
        .run_authorized(grant, move || {
            ui_pool_snapshot(&client, &mut UiPoolCache::new()).map(|(snapshot, _)| snapshot)
        })
        .await
    {
        Ok(snapshot) => json_response(snapshot),
        Err(err) => error_response(err),
    }
}

async fn ui_activity(State(state): State<Arc<AppState>>) -> Response {
    if !state.local_admin {
        return error_response_with_status(
            Error::new(ErrorKind::Permission)
                .with_message("pool activity requires local administration"),
            StatusCode::FORBIDDEN,
        );
    }
    let client = state.client.clone();
    let activity = state.activity.clone();
    match state
        .storage_executor
        .run(move || {
            let pools = client
                .list_pools()?
                .into_iter()
                .map(|info| (pool_file_name(&info.path), info.path))
                .collect::<Vec<_>>();
            Ok(activity.snapshot(&pools))
        })
        .await
    {
        Ok(snapshot) => json_response(snapshot),
        Err(err) => error_response(err),
    }
}

async fn ui_pool_stream(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let existing = {
        let guard = state.ui_pool_watch.lock().unwrap();
        guard
            .as_ref()
            .filter(|tx| tx.receiver_count() > 0)
            .map(watch::Sender::subscribe)
    };
    let mut rx = if let Some(rx) = existing {
        rx
    } else {
        let client = state.client.clone();
        let initial = state
            .storage_executor
            .run_authorized(grant.clone(), move || {
                let mut cache = UiPoolCache::new();
                let (snapshot, _) = ui_pool_snapshot(&client, &mut cache)?;
                Ok((snapshot, cache))
            })
            .await;
        let (snapshot, cache) = match initial {
            Ok(initial) => initial,
            Err(err) => return error_response(err),
        };
        let mut guard = state.ui_pool_watch.lock().unwrap();
        if let Some(tx) = guard.as_ref().filter(|tx| tx.receiver_count() > 0) {
            tx.subscribe()
        } else {
            let (tx, rx) = watch::channel(snapshot);
            *guard = Some(tx.clone());
            tokio::spawn(watch_ui_pools(state.clone(), tx, cache));
            rx
        }
    };

    let (tx, body_rx) = mpsc::channel::<Bytes>(8);
    tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        let mut revocation = tokio::time::interval(Duration::from_secs(1));
        loop {
            if grant.as_ref().is_some_and(AccessGrant::is_revoked) {
                break;
            }
            let snapshot = rx.borrow_and_update().clone();
            let frame = match serde_json::to_vec(&snapshot) {
                Ok(mut payload) => {
                    let mut frame = b"event: pools\ndata: ".to_vec();
                    frame.append(&mut payload);
                    frame.extend_from_slice(b"\n\n");
                    Bytes::from(frame)
                }
                Err(_) => break,
            };
            if tx.send(frame).await.is_err() {
                break;
            }
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() { return; }
                        break;
                    }
                    _ = heartbeat.tick() => {
                        if tx.send(Bytes::from_static(b": keep-alive\n\n")).await.is_err() { return; }
                    }
                    _ = revocation.tick() => {
                        if grant.as_ref().is_some_and(AccessGrant::is_revoked) { return; }
                    }
                    _ = tx.closed() => return,
                }
            }
        }
    });
    let mut response = Response::new(Body::from_stream(
        ReceiverStream::new(body_rx).map(Ok::<_, std::io::Error>),
    ));
    apply_tail_response_headers(&mut response, TailStreamEncoding::Sse);
    response
}

async fn watch_ui_pools(state: Arc<AppState>, tx: watch::Sender<Value>, cache: UiPoolCache) {
    let cache = Arc::new(Mutex::new(cache));
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    interval.tick().await;
    loop {
        tokio::select! {
            _ = tx.closed() => break,
            _ = interval.tick() => {}
        }
        let client = state.client.clone();
        let cache = cache.clone();
        let result = state
            .storage_executor
            .run(move || ui_pool_snapshot(&client, &mut cache.lock().unwrap()))
            .await;
        if let Ok((snapshot, true)) = result {
            tx.send_replace(snapshot);
        }
    }
}

async fn delete_pool(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
) -> Response {
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    match state
        .storage_executor
        .run_authorized(grant, move || client.delete_pool(&pool_ref))
        .await
    {
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
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let durability = durability_from_str(payload.durability.as_deref());
    let tags = payload.tags.unwrap_or_default();
    let data = payload.data;

    let client = state.client.clone();
    let result = state
        .storage_executor
        .run_authorized(grant, move || {
            client
                .open_pool(&pool_ref)
                .and_then(|mut pool| pool.append_json_now(&data, &tags, durability))
        })
        .await;
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
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
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
    let client = state.client.clone();
    let result = state
        .storage_executor
        .run_authorized(grant, move || {
            client.open_pool(&pool_ref).and_then(|mut pool| {
                let seq = pool.append_lite3_now(&payload, durability)?;
                pool.get_message(seq)
            })
        })
        .await;
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
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    let result = state
        .storage_executor
        .run_authorized(grant, move || {
            client
                .open_pool(&pool_ref)
                .and_then(|pool| pool.get_message(seq))
        })
        .await;

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
    let grant = match authorize(&headers, &state) {
        Ok(grant) => grant,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&pool) {
        Ok(pool_ref) => pool_ref,
        Err(err) => return error_response(err),
    };
    let client = state.client.clone();
    let result = state
        .storage_executor
        .run_authorized(grant, move || {
            client.open_pool(&pool_ref).and_then(|pool| {
                let frame = pool.get_lite3(seq)?;
                let payload = frame.payload;
                lite3::validate_bytes(&payload)?;
                Ok(payload)
            })
        })
        .await;
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
    RawQuery(raw_query): RawQuery,
) -> Response {
    let (pool_ref, grant) = match tail_pool_ref_from_request(&state, &headers, &pool) {
        Ok(value) => value,
        Err(err) => return error_response(err),
    };
    let runtime = match prepare_tail_runtime(
        &state,
        &query,
        raw_query.as_deref(),
        TailStreamEncoding::Jsonl,
    ) {
        Ok(runtime) => runtime,
        Err(err) => return error_response(err),
    };
    spawn_tail_stream_response(&state, pool_ref, runtime, TailStreamEncoding::Jsonl, grant)
}

async fn tail_lite3(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
    Query(query): Query<TailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let (pool_ref, grant) = match tail_pool_ref_from_request(&state, &headers, &pool) {
        Ok(value) => value,
        Err(err) => return error_response(err),
    };
    let runtime = match prepare_tail_runtime(
        &state,
        &query,
        raw_query.as_deref(),
        TailStreamEncoding::Lite3,
    ) {
        Ok(runtime) => runtime,
        Err(err) => return error_response(err),
    };
    if let Err(err) = precheck_lite3_since_seq(&state, &pool_ref, query.since_seq).await {
        return error_response(err);
    }
    spawn_tail_stream_response(&state, pool_ref, runtime, TailStreamEncoding::Lite3, grant)
}

async fn ui_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pool): AxumPath<String>,
    Query(query): Query<TailQuery>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let (pool_ref, grant) = match tail_pool_ref_from_request(&state, &headers, &pool) {
        Ok(value) => value,
        Err(err) => return error_response(err),
    };
    let runtime = match prepare_tail_runtime(
        &state,
        &query,
        raw_query.as_deref(),
        TailStreamEncoding::Sse,
    ) {
        Ok(runtime) => runtime,
        Err(err) => return error_response(err),
    };
    spawn_tail_stream_response(&state, pool_ref, runtime, TailStreamEncoding::Sse, grant)
}

fn tail_pool_ref_from_request(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    pool: &str,
) -> Result<(PoolRef, Option<AccessGrant>), Error> {
    let grant = authorize(headers, state)?;
    Ok((pool_ref_from_request(pool)?, grant))
}

async fn precheck_lite3_since_seq(
    state: &Arc<AppState>,
    pool_ref: &PoolRef,
    since_seq: Option<u64>,
) -> Result<(), Error> {
    let Some(since_seq) = since_seq else {
        return Ok(());
    };
    let client = state.client.clone();
    let pool_ref = pool_ref.clone();
    let precheck = state
        .storage_executor
        .run(move || {
            client.open_pool(&pool_ref).and_then(|pool| {
                let frame = pool.get_lite3(since_seq)?;
                lite3::validate_bytes(&frame.payload)?;
                Ok(())
            })
        })
        .await;
    match precheck {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn prepare_tail_runtime(
    state: &Arc<AppState>,
    query: &TailQuery,
    raw_query: Option<&str>,
    encoding: TailStreamEncoding,
) -> Result<TailRuntime, Error> {
    let gap_policy = match query.gap_policy.as_deref() {
        None | Some("continue") => GapPolicy::Continue,
        Some("error") => GapPolicy::Error,
        Some(_) => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid tail gap policy")
                .with_hint("Use gap_policy=continue or gap_policy=error."));
        }
    };
    if encoding == TailStreamEncoding::Lite3 && gap_policy == GapPolicy::Error {
        return Err(Error::new(ErrorKind::Usage).with_message(
            "remote Lite3 tails do not support gap_policy=error because the stream has no error frame",
        ));
    }
    if let Some(timeout_ms) = query.timeout_ms
        && timeout_ms > state.max_tail_timeout_ms
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("tail timeout exceeds server limit")
            .with_hint(format!("Use timeout_ms <= {}.", state.max_tail_timeout_ms)));
    }
    let permit = acquire_tail_permit(state)?;
    let timeout_ms = query.timeout_ms.unwrap_or(state.max_tail_timeout_ms);
    let timeout = Duration::from_millis(timeout_ms);
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("tail timeout is out of range"))?;
    let options = TailOptions {
        since_seq: query.since_seq,
        max_messages: query.max.map(|value| value as usize),
        tags: parse_tags_from_query(raw_query),
        timeout: Some(timeout),
        gap_policy,
        ..TailOptions::default()
    };
    Ok(TailRuntime {
        permit,
        options,
        deadline,
    })
}

fn acquire_tail_permit(state: &Arc<AppState>) -> Result<OwnedSemaphorePermit, Error> {
    state
        .tail_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| tail_busy_error())
}

fn tail_busy_error() -> Error {
    Error::new(ErrorKind::Busy)
        .with_message("too many concurrent tail requests")
        .with_hint("Try again later or reduce tail concurrency.")
}

fn spawn_tail_stream_response(
    state: &Arc<AppState>,
    pool_ref: PoolRef,
    runtime: TailRuntime,
    encoding: TailStreamEncoding,
    grant: Option<AccessGrant>,
) -> Response {
    let client = state.client.clone();
    let TailRuntime {
        permit,
        mut options,
        deadline,
    } = runtime;
    options.cancel = grant.as_ref().map(AccessGrant::cancellation_flag);
    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(16);
    let producer_grant = grant.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if Instant::now() >= deadline
            || producer_grant.as_ref().is_some_and(AccessGrant::is_revoked)
        {
            return;
        }
        let result = client.open_pool(&pool_ref).and_then(|pool| {
            stream_tail_bytes(
                &pool,
                options,
                encoding,
                tx.clone(),
                producer_grant.clone(),
                deadline,
            )
        });
        if let Err(err) = result
            && !producer_grant.as_ref().is_some_and(AccessGrant::is_revoked)
        {
            let _ = send_tail_result(&tx, Err(err), producer_grant.as_ref(), deadline);
        }
    });

    let stream = ReceiverStream::new(rx)
        .take_while(move |_| !grant.as_ref().is_some_and(AccessGrant::is_revoked))
        .map(move |result| match result {
            Ok(bytes) => Ok(bytes),
            Err(err) => match encode_tail_terminal_error(&err, encoding) {
                Some(bytes) => Ok(bytes),
                None => Err(std::io::Error::other(error_json_string(&err))),
            },
        });
    let mut response = Response::new(Body::from_stream(stream));
    apply_tail_response_headers(&mut response, encoding);
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

fn stream_tail_bytes(
    pool: &plasmite::api::Pool,
    mut options: TailOptions,
    encoding: TailStreamEncoding,
    tx: mpsc::Sender<Result<Bytes, Error>>,
    grant: Option<AccessGrant>,
    deadline: Instant,
) -> Result<(), Error> {
    // Admission, producer scheduling, reads, and backpressure share one budget.
    options.timeout = Some(deadline.saturating_duration_since(Instant::now()));
    match encoding {
        TailStreamEncoding::Jsonl | TailStreamEncoding::Sse => {
            let mut tail = pool.tail(options);
            while let Some(message) = tail.next_message()? {
                if grant.as_ref().is_some_and(AccessGrant::is_revoked) {
                    break;
                }
                let encoded = match encoding {
                    TailStreamEncoding::Jsonl => encode_jsonl_message(&message)?,
                    TailStreamEncoding::Sse => encode_sse_message(&message)?,
                    TailStreamEncoding::Lite3 => unreachable!("handled in separate branch"),
                };
                if !send_tail_result(&tx, Ok(encoded), grant.as_ref(), deadline) {
                    break;
                }
            }
        }
        TailStreamEncoding::Lite3 => {
            let mut tail = pool.tail_lite3(options);
            while let Some(frame) = tail.next_frame()? {
                if grant.as_ref().is_some_and(AccessGrant::is_revoked) {
                    break;
                }
                lite3::validate_bytes(&frame.payload)?;
                let encoded = encode_lite3_stream_frame(&frame)?;
                if !send_tail_result(&tx, Ok(encoded), grant.as_ref(), deadline) {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn send_tail_result(
    tx: &mpsc::Sender<Result<Bytes, Error>>,
    mut result: Result<Bytes, Error>,
    grant: Option<&AccessGrant>,
    deadline: Instant,
) -> bool {
    loop {
        if Instant::now() >= deadline || grant.is_some_and(AccessGrant::is_revoked) {
            return false;
        }
        match tx.try_send(result) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(remaining)) => {
                result = remaining;
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    }
}

fn encode_message_payload(message: &plasmite::api::Message) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&message_json(message)).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to encode message")
            .with_source(err)
    })
}

fn encode_jsonl_message(message: &plasmite::api::Message) -> Result<Bytes, Error> {
    let mut payload = encode_message_payload(message)?;
    payload.push(b'\n');
    Ok(Bytes::from(payload))
}

fn encode_sse_message(message: &plasmite::api::Message) -> Result<Bytes, Error> {
    let mut payload = encode_message_payload(message)?;
    // SSE event frame: clients parse one JSON message per event.
    let mut frame = b"event: message\ndata: ".to_vec();
    frame.append(&mut payload);
    frame.extend_from_slice(b"\n\n");
    Ok(Bytes::from(frame))
}

fn encode_tail_terminal_error(err: &Error, encoding: TailStreamEncoding) -> Option<Bytes> {
    match encoding {
        TailStreamEncoding::Jsonl => {
            let mut payload = error_json_string(err).into_bytes();
            payload.push(b'\n');
            Some(Bytes::from(payload))
        }
        TailStreamEncoding::Sse => {
            // SSE terminal error frame keeps machine-readable error semantics after streaming starts.
            let mut frame = b"event: error\ndata: ".to_vec();
            frame.extend_from_slice(error_json_string(err).as_bytes());
            frame.extend_from_slice(b"\n\n");
            Some(Bytes::from(frame))
        }
        TailStreamEncoding::Lite3 => None,
    }
}

fn apply_tail_response_headers(response: &mut Response, encoding: TailStreamEncoding) {
    match encoding {
        TailStreamEncoding::Jsonl => {
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("application/jsonl"),
            );
        }
        TailStreamEncoding::Lite3 => {
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("application/x-plasmite-lite3-stream"),
            );
        }
        TailStreamEncoding::Sse => {
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("text/event-stream"),
            );
            response.headers_mut().insert(
                "cache-control",
                HeaderValue::from_static("no-cache, no-transform"),
            );
            response
                .headers_mut()
                .insert("connection", HeaderValue::from_static("keep-alive"));
        }
    }
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
    serde_json::to_value(MessageWire::new(
        message.seq,
        message.time.clone(),
        message.meta.tags.clone(),
        message.data.clone(),
    ))
    .expect("message wire data is serializable")
}

fn normalize_tags(raw: Vec<String>) -> Vec<String> {
    raw.into_iter()
        .map(|value| value.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn parse_tags_from_query(raw_query: Option<&str>) -> Vec<String> {
    let Some(raw_query) = raw_query else {
        return Vec::new();
    };
    let tags = url::form_urlencoded::parse(raw_query.as_bytes())
        .filter_map(|(key, value)| (key == "tag").then(|| value.into_owned()))
        .collect::<Vec<_>>();
    normalize_tags(tags)
}

fn json_response(payload: serde_json::Value) -> Response {
    let mut response = Json(payload).into_response();
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

fn html_response(body: &str) -> Response {
    let mut response = Response::new(Body::from(body.to_owned()));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'unsafe-inline'; font-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
    ));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn encode_lite3_stream_frame(frame: &plasmite::api::FrameRef) -> Result<Bytes, Error> {
    let payload_len: u32 = frame.payload.len().try_into().map_err(|_| {
        Error::new(ErrorKind::Usage).with_message("lite3 payload exceeds max frame length")
    })?;
    let mut buf = Vec::with_capacity(8 + 8 + 4 + payload_len as usize);
    buf.extend_from_slice(&frame.seq.to_be_bytes());
    buf.extend_from_slice(&frame.timestamp_ns.to_be_bytes());
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.extend_from_slice(&frame.payload);
    Ok(Bytes::from(buf))
}

fn error_json_string(err: &Error) -> String {
    serde_json::to_string(&json!({ "error": error_body(err) }))
        .unwrap_or_else(|_| "{\"error\":{\"kind\":\"Internal\",\"message\":\"error\"}}".to_string())
}

fn durability_from_str(value: Option<&str>) -> Durability {
    match value {
        Some("flush") => Durability::Flush,
        _ => Durability::Fast,
    }
}

fn error_response(err: Error) -> Response {
    let status = StatusCode::from_u16(error_policy(interface_error_kind(err.kind())).http_status)
        .expect("error policy contains valid HTTP status");
    error_response_with_status(err, status)
}

fn error_response_with_status(err: Error, status: StatusCode) -> Response {
    let body = ErrorEnvelope {
        error: error_body(&err),
    };
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert("plasmite-version", HeaderValue::from_static("0"));
    response
}

fn error_body(err: &Error) -> ErrorBody {
    ErrorBody {
        kind: error_policy(interface_error_kind(err.kind()))
            .mcp_error_kind
            .to_string(),
        message: err.message().unwrap_or("error").to_string(),
        path: err.path().map(|path| path.to_string_lossy().to_string()),
        seq: err.seq(),
        offset: err.offset(),
    }
}

#[cfg(test)]
mod pool_name_tests {
    use super::pool_file_name;
    use std::path::Path;

    #[test]
    fn pool_name_drops_only_one_extension() {
        assert_eq!(pool_file_name(Path::new("/pools/chat.plasmite")), "chat");
        assert_eq!(
            pool_file_name(Path::new("/pools/a.plasmite.plasmite")),
            "a.plasmite"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccessStore, AppState, BrowserSessions, Error, ErrorKind, LocalClient, McpHandler,
        ServeConfig, ServeMcpHandler, StorageExecutor, ToolCallRequest, error_response, healthz,
        list_pools, mcp_post, normalize_tags, parse_tags_from_query, same_origin, ui_pool_snapshot,
        validate_config, validate_mcp_origin_header, validate_mcp_protocol_version,
    };
    use axum::Json;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, Uri, header};
    use plasmite::api::PoolApiExt;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Semaphore;

    #[test]
    fn tail_backpressure_expires_and_releases_permit_without_draining_queued_data() {
        use bytes::Bytes;
        use std::time::{Duration, Instant};
        use tokio::sync::mpsc;

        // Both ordinary frames and terminal errors use the same bounded send.
        for result in [
            Ok(Bytes::from_static(b"next")),
            Err(Error::new(ErrorKind::Corrupt).with_message("terminal error")),
        ] {
            let semaphore = Arc::new(Semaphore::new(1));
            let permit = semaphore.clone().try_acquire_owned().unwrap();
            let (tx, mut rx) = mpsc::channel(1);
            tx.try_send(Ok(Bytes::from_static(b"queued"))).unwrap();
            let deadline = Instant::now() + Duration::from_millis(20);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let producer = std::thread::spawn(move || {
                let sent = {
                    let _permit = permit;
                    super::send_tail_result(&tx, result, None, deadline)
                };
                done_tx.send(sent).unwrap();
            });
            let completion = done_rx.recv_timeout(Duration::from_millis(300));
            let available_before_drain = semaphore.available_permits();
            assert_eq!(
                rx.try_recv().unwrap().unwrap(),
                Bytes::from_static(b"queued")
            );
            // Cleanup also bounds this regression when the sender fails to expire.
            drop(rx);
            producer.join().unwrap();
            assert!(
                !completion.unwrap(),
                "full sender must stop at its deadline"
            );
            assert_eq!(
                available_before_drain, 1,
                "stalled client must release its tail slot"
            );
        }
    }

    #[test]
    fn activity_ignores_map_reads_and_identifies_mcp_pool_use() {
        assert!(super::observes_pool_route("/v0/ui/pools/:pool/events"));
        assert!(super::observes_pool_route("/v0/pools/:pool/tail"));
        for route in [
            "/v0/ui/activity",
            "/v0/ui/pools",
            "/v0/ui/pools/stream",
            "/v0/ui/pools/:pool/info",
            "/v0/pools/:pool/info",
        ] {
            assert!(!super::observes_pool_route(route));
        }
        assert_eq!(
            super::mcp_activity_pool(
                &json!({"method":"tools/call","params":{"name":"plasmite_wait","arguments":{"pool":"events"}}})
            ),
            Some(("events".to_owned(), "plasmite_wait".to_owned()))
        );
        assert_eq!(
            super::mcp_activity_pool(
                &json!({"method":"resources/read","params":{"uri":"plasmite:///pools/events"}})
            ),
            Some(("events".to_owned(), "resources/read".to_owned()))
        );
        assert!(super::mcp_activity_pool(&json!({"method":"tools/list"})).is_none());
        assert!(super::mcp_activity_pool(&json!({"method":"tools/call","params":{"name":"plasmite_pool_info","arguments":{"pool":"events"}}})).is_none());
    }

    #[tokio::test]
    async fn activity_route_observes_browser_stream_and_removes_dropped_response() {
        use axum::body::{Body, to_bytes};
        use axum::extract::ConnectInfo;
        use axum::http::Request;
        use plasmite::api::{PoolOptions, PoolRef};
        use tower_service::Service;

        let temp = tempfile::tempdir().unwrap();
        LocalClient::new()
            .with_pool_dir(temp.path())
            .create_pool(&PoolRef::name("events"), PoolOptions::new(64 * 1024))
            .unwrap();
        let mut cfg = config();
        cfg.pool_dir = temp.path().to_owned();
        cfg.max_tail_timeout_ms = 1_000;
        let activity = super::ActivityRegistry::default();
        let mut app = super::prepare_server_with_access(&cfg, None, true, activity.clone())
            .await
            .unwrap()
            .app;
        let mut follow = Request::builder()
            .uri("/v0/ui/pools/events/events")
            .header("host", "localhost")
            .header("user-agent", "Browser test")
            .body(Body::empty())
            .unwrap();
        follow.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response = app.call(follow).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let snapshot = app
            .call(
                Request::builder()
                    .uri("/v0/ui/activity")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let snapshot: serde_json::Value =
            serde_json::from_slice(&to_bytes(snapshot.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        let entries = snapshot["pools"]["events"]["entries"].as_array().unwrap();
        let browser = entries
            .iter()
            .find(|entry| entry["kind"] == "browser")
            .unwrap();
        assert_eq!(browser["peer_ip"], "127.0.0.1");
        assert_eq!(browser["user_agent"], "Browser test");
        assert!(browser["age_ms"].is_number());
        drop(response);
        let snapshot =
            activity.snapshot(&[("events".to_owned(), temp.path().join("events.plasmite"))]);
        assert!(
            !snapshot["pools"]["events"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["kind"] == "browser")
        );

        let mut remote = super::prepare_server_with_access(&cfg, None, false, activity)
            .await
            .unwrap()
            .app;
        let forbidden = remote
            .call(
                Request::builder()
                    .uri("/v0/ui/activity")
                    .header("host", "example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn pool_snapshot_notices_same_name_replacement() {
        use plasmite::api::{PoolOptions, PoolRef};

        let dir = tempfile::tempdir().expect("tempdir");
        let client = LocalClient::new().with_pool_dir(dir.path());
        let name = PoolRef::name("replaced");
        client
            .create_pool(&name, PoolOptions::new(64 * 1024))
            .expect("create first pool");
        let mut cache = Default::default();
        let (before, _) = ui_pool_snapshot(&client, &mut cache).expect("first snapshot");

        client.delete_pool(&name).expect("delete first pool");
        client
            .create_pool(&name, PoolOptions::new(128 * 1024))
            .expect("replace before next scan");
        let (after, changed) = ui_pool_snapshot(&client, &mut cache).expect("next snapshot");
        assert!(changed);
        assert_ne!(before, after);
        assert_eq!(after["pools"][0]["file_size"], 128 * 1024);
    }

    fn config() -> ServeConfig {
        ServeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            pool_dir: std::path::PathBuf::from("/tmp/plasmite-test"),
            tls_cert: None,
            tls_key: None,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 1,
        }
    }

    #[test]
    fn browser_origin_accepts_http2_authority_and_rejects_conflicts() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://localhost:9743"),
        );
        let uri: Uri = "https://localhost:9743/v0/browser/session".parse().unwrap();
        assert!(same_origin(&headers, &uri, "https"));

        headers.insert(header::HOST, HeaderValue::from_static("localhost:9743"));
        assert!(same_origin(&headers, &uri, "https"));
        assert!(same_origin(
            &headers,
            &"/v0/browser/session".parse().unwrap(),
            "https"
        ));
        headers.insert(header::HOST, HeaderValue::from_static("evil.example"));
        assert!(!same_origin(&headers, &uri, "https"));
        headers.insert(header::HOST, HeaderValue::from_static("localhost:9743"));
        headers.append(header::HOST, HeaderValue::from_static("localhost:9743"));
        assert!(!same_origin(&headers, &uri, "https"));
        headers.remove(header::HOST);
        headers.append(
            header::ORIGIN,
            HeaderValue::from_static("https://localhost:9743"),
        );
        assert!(!same_origin(&headers, &uri, "https"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!same_origin(&headers, &uri, "https"));
        assert!(!same_origin(
            &headers,
            &"/v0/browser/session".parse().unwrap(),
            "https"
        ));
    }

    #[test]
    fn config_requires_complete_tls_pair_and_positive_limits() {
        let mut cfg = config();
        cfg.tls_cert = Some("cert.pem".into());
        assert_eq!(validate_config(&cfg).unwrap_err().kind(), ErrorKind::Usage);
        cfg.tls_cert = None;
        cfg.max_body_bytes = 0;
        assert_eq!(validate_config(&cfg).unwrap_err().kind(), ErrorKind::Usage);
        cfg.max_body_bytes = 1;
        cfg.max_tail_timeout_ms = 0;
        assert_eq!(validate_config(&cfg).unwrap_err().kind(), ErrorKind::Usage);
        cfg.max_tail_timeout_ms = 1;
        cfg.max_concurrent_tails = 0;
        assert_eq!(validate_config(&cfg).unwrap_err().kind(), ErrorKind::Usage);
    }

    #[test]
    fn mcp_origin_must_be_a_plain_http_origin() {
        let mut headers = HeaderMap::new();
        for invalid in [
            "null",
            "https://example.com/path",
            "https://example.com?x=1",
        ] {
            headers.insert(header::ORIGIN, HeaderValue::from_str(invalid).unwrap());
            assert_eq!(
                validate_mcp_origin_header(&headers).unwrap_err().kind(),
                ErrorKind::Permission
            );
        }
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://example.com"),
        );
        validate_mcp_origin_header(&headers).unwrap();
    }

    #[test]
    fn mcp_http_requires_the_negotiated_version_after_initialize() {
        let payload = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
        let mut headers = HeaderMap::new();
        headers.insert(
            "MCP-Protocol-Version",
            HeaderValue::from_static("2025-11-25"),
        );
        validate_mcp_protocol_version(&headers, &payload).unwrap();
        headers.remove("MCP-Protocol-Version");
        assert_eq!(
            validate_mcp_protocol_version(&headers, &payload)
                .unwrap_err()
                .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        let initialize = json!({"jsonrpc":"2.0","id":2,"method":"initialize"});
        validate_mcp_protocol_version(&headers, &initialize).unwrap();
        headers.insert(
            "MCP-Protocol-Version",
            HeaderValue::from_static("not-supported"),
        );
        assert_eq!(
            validate_mcp_protocol_version(&headers, &initialize)
                .unwrap_err()
                .status(),
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn storage_executor_rejects_saturation_without_queueing() {
        let executor = StorageExecutor::new(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let active = {
            let executor = executor.clone();
            tokio::spawn(async move {
                executor
                    .run(move || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    })
                    .await
            })
        };
        started_rx.await.unwrap();
        let err = executor.run(|| Ok::<_, Error>(())).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Busy);
        release_tx.send(()).unwrap();
        active.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn admitted_storage_work_may_finish_after_revocation_but_new_work_fails() {
        let temp = tempfile::tempdir().unwrap();
        let access = AccessStore::open(temp.path(), None, None, None).unwrap();
        let key = access.issue("writer").unwrap();
        let grant = access
            .authorize_secret(key.rsplit('.').next().unwrap())
            .unwrap();
        let id = serde_json::to_value(access.list().unwrap()).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let client = LocalClient::new().with_pool_dir(temp.path());
        let pool_ref = plasmite::api::PoolRef::name("events");
        client
            .create_pool(&pool_ref, plasmite::api::PoolOptions::new(1024 * 1024))
            .unwrap();
        let executor = StorageExecutor::new(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let admitted = {
            let executor = executor.clone();
            let grant = grant.clone();
            let client = client.clone();
            let pool_ref = pool_ref.clone();
            tokio::spawn(async move {
                executor
                    .run_authorized(Some(grant), move || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        let mut pool = client.open_pool(&pool_ref)?;
                        pool.append_json_now(
                            &json!({"admitted": true}),
                            &[],
                            plasmite::api::Durability::Fast,
                        )
                        .map(|message| message.seq)
                    })
                    .await
            })
        };
        started_rx.await.unwrap();
        access.revoke(&id).unwrap();
        assert!(grant.is_revoked());
        release_tx.send(()).unwrap();
        let seq = admitted.await.unwrap().unwrap();
        assert_eq!(
            client
                .open_pool(&pool_ref)
                .unwrap()
                .get_message(seq)
                .unwrap()
                .data,
            json!({"admitted": true})
        );
        let rejected = executor
            .run_authorized(Some(grant), || Ok(8))
            .await
            .unwrap_err();
        assert_eq!(rejected.kind(), ErrorKind::Permission);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_storage_keeps_health_and_mcp_responsive() {
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState {
            client: LocalClient::new().with_pool_dir(temp.path()),
            secure_access: None,
            oauth: None,
            browser_sessions: Arc::new(BrowserSessions::new(None).unwrap()),
            local_admin: true,
            max_tail_timeout_ms: 30_000,
            tail_semaphore: Arc::new(Semaphore::new(1)),
            mcp_semaphore: Arc::new(Semaphore::new(1)),
            storage_executor: StorageExecutor::new(1),
            ui_pool_watch: Arc::new(Mutex::new(None)),
            activity: Default::default(),
        });
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let active = {
            let executor = state.storage_executor.clone();
            tokio::spawn(async move {
                executor
                    .run(move || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    })
                    .await
            })
        };
        started_rx.await.unwrap();
        assert_eq!(
            list_pools(State(state.clone()), HeaderMap::new())
                .await
                .status(),
            axum::http::StatusCode::LOCKED
        );
        let payload = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":super::MCP_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}});
        let mut headers = HeaderMap::new();
        headers.insert(
            "MCP-Protocol-Version",
            HeaderValue::from_static(super::MCP_PROTOCOL_VERSION),
        );
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            assert_eq!(healthz().await.status(), axum::http::StatusCode::OK);
            assert_eq!(
                mcp_post(
                    State(state.clone()),
                    "/mcp".parse().unwrap(),
                    headers.clone(),
                    None,
                    Json(payload.clone()),
                )
                .await
                .status(),
                axum::http::StatusCode::OK
            );
        })
        .await
        .unwrap();
        let _mcp_permit = state.mcp_semaphore.clone().try_acquire_owned().unwrap();
        assert_eq!(
            mcp_post(
                State(state),
                "/mcp".parse().unwrap(),
                headers,
                None,
                Json(payload)
            )
            .await
            .status(),
            axum::http::StatusCode::LOCKED
        );
        release_tx.send(()).unwrap();
        active.await.unwrap().unwrap();
    }

    #[test]
    fn remote_state_without_access_store_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = AppState {
            client: LocalClient::new().with_pool_dir(temp.path()),
            secure_access: None,
            oauth: None,
            browser_sessions: Arc::new(BrowserSessions::new(None).unwrap()),
            local_admin: false,
            max_tail_timeout_ms: 30_000,
            tail_semaphore: Arc::new(Semaphore::new(1)),
            mcp_semaphore: Arc::new(Semaphore::new(1)),
            storage_executor: StorageExecutor::new(1),
            ui_pool_watch: Arc::new(Mutex::new(None)),
            activity: Default::default(),
        };
        assert_eq!(
            super::authorize(&HeaderMap::new(), &state)
                .unwrap_err()
                .kind(),
            ErrorKind::Permission
        );
        state.local_admin = true;
        super::authorize(&HeaderMap::new(), &state).unwrap();
    }

    #[test]
    fn mcp_wait_shares_reader_budget() {
        let temp = tempfile::tempdir().unwrap();
        let semaphore = Arc::new(Semaphore::new(1));
        let _permit = semaphore.clone().try_acquire_owned().unwrap();
        let mut handler =
            ServeMcpHandler::new(LocalClient::new().with_pool_dir(temp.path()), semaphore);
        let result = handler
            .call_tool(ToolCallRequest {
                name: "plasmite_wait".to_string(),
                arguments: json!({"pool":"events","after_seq":0,"timeout_ms":10})
                    .as_object()
                    .unwrap()
                    .clone(),
            })
            .unwrap();
        assert!(result.is_error);
        assert_eq!(
            result.structured_content.unwrap()["error_kind"],
            json!("Busy")
        );
    }

    #[test]
    fn revocation_wakes_an_idle_mcp_wait() {
        let temp = tempfile::tempdir().unwrap();
        let access = AccessStore::open(temp.path(), None, None, None).unwrap();
        let key = access.issue("reader").unwrap();
        let grant = access
            .authorize_secret(key.rsplit('.').next().unwrap())
            .unwrap();
        let id = serde_json::to_value(access.list().unwrap()).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let client = LocalClient::new().with_pool_dir(temp.path());
        client
            .create_pool(
                &plasmite::api::PoolRef::name("events"),
                plasmite::api::PoolOptions::new(1024 * 1024),
            )
            .unwrap();
        let wait_semaphore = Arc::new(Semaphore::new(1));
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let thread_semaphore = wait_semaphore.clone();
        std::thread::spawn(move || {
            let mut handler =
                ServeMcpHandler::new(client, thread_semaphore).with_grant(Some(grant));
            let result = handler.call_tool(ToolCallRequest {
                name: "plasmite_wait".into(),
                arguments: json!({"pool":"events","after_seq":0,"timeout_ms":60_000})
                    .as_object()
                    .unwrap()
                    .clone(),
            });
            result_tx.send(result).unwrap();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while wait_semaphore.available_permits() != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "MCP wait was not admitted"
            );
            std::thread::yield_now();
        }
        assert!(matches!(
            result_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        access.revoke(&id).unwrap();
        let result = result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("revocation should wake the idle wait")
            .unwrap();
        assert!(result.is_error);
        assert_eq!(
            result.structured_content.unwrap()["error_kind"],
            "Permission"
        );
    }

    #[test]
    fn http_error_presenter_uses_policy_status() {
        assert_eq!(
            error_response(Error::new(ErrorKind::Permission))
                .status()
                .as_u16(),
            401
        );
        assert_eq!(
            error_response(Error::new(ErrorKind::Busy))
                .status()
                .as_u16(),
            423
        );
    }

    #[test]
    fn tags_preserve_values_and_drop_empty_entries() {
        assert_eq!(
            normalize_tags(vec![
                "keep".into(),
                " prod ".into(),
                "a,b".into(),
                "".into()
            ]),
            vec!["keep", "prod", "a,b"]
        );
        assert_eq!(
            parse_tags_from_query(Some("tag=keep%2Cprod")),
            vec!["keep,prod"]
        );
    }
}
