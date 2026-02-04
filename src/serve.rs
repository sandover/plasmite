//! Purpose: Provide the HTTP/JSON remote server for Plasmite.
//! Exports: `ServeConfig`, `serve`.
//! Role: Axum-based loopback server implementing the remote v0 spec.
//! Invariants: JSON envelopes match spec/v0/SPEC.md; error kinds remain stable.
//! Invariants: Loopback-only unless explicitly allowed (v0 policy).
//! Notes: Streaming uses JSONL; tail is at-least-once and resumable.

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use plasmite::api::{
    Bounds, Durability, Error, ErrorKind, LocalClient, PoolApiExt, PoolInfo, PoolOptions, PoolRef,
    TailOptions,
};

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub bind: SocketAddr,
    pub pool_dir: PathBuf,
    pub token: Option<String>,
}

#[derive(Clone)]
struct AppState {
    client: LocalClient,
    token: Option<String>,
}

pub async fn serve(config: ServeConfig) -> Result<(), Error> {
    if !is_loopback(config.bind.ip()) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote bind is not supported in v0; use loopback"));
    }

    let state = Arc::new(AppState {
        client: LocalClient::new().with_pool_dir(config.pool_dir),
        token: config.token,
    });

    let app = Router::new()
        .route("/v0/pools", post(create_pool).get(list_pools))
        .route("/v0/pools/open", post(open_pool))
        .route("/v0/pools/:pool/info", get(pool_info))
        .route("/v0/pools/:pool", delete(delete_pool))
        .route("/v0/pools/:pool/append", post(append_message))
        .route("/v0/pools/:pool/messages/:seq", get(get_message))
        .route("/v0/pools/:pool/tail", get(tail_messages))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to bind server")
                .with_source(err)
        })?;

    axum::serve(listener, app).await.map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("server failed")
            .with_source(err)
    })
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback(),
    }
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
    let size_bytes = payload.size_bytes.unwrap_or(1024 * 1024);
    let result = state.client.create_pool(
        &pool_ref_from_path(&payload.pool),
        PoolOptions::new(size_bytes),
    );
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
    match state.client.pool_info(&pool_ref_from_path(&payload.pool)) {
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
    match state.client.pool_info(&pool_ref_from_path(&pool)) {
        Ok(info) => json_response(json!({ "pool": pool_info_json(&pool, &info) })),
        Err(err) => error_response(err),
    }
}

async fn list_pools(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(err) = authorize(&headers, &state) {
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
    match state.client.delete_pool(&pool_ref_from_path(&pool)) {
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
    let durability = match payload.durability.as_deref() {
        Some("flush") => Durability::Flush,
        _ => Durability::Fast,
    };
    let descrips = payload.descrips.unwrap_or_default();

    let result = state
        .client
        .open_pool(&pool_ref_from_path(&pool))
        .and_then(|mut pool| pool.append_json_now(&payload.data, &descrips, durability));
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
    let result = state
        .client
        .open_pool(&pool_ref_from_path(&pool))
        .and_then(|pool| pool.get_message(seq));

    match result {
        Ok(message) => json_response(json!({ "message": message_json(&message) })),
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
    let pool_ref = pool_ref_from_path(&pool);
    let client = state.client.clone();

    let (tx, rx) = mpsc::channel::<Result<Bytes, Error>>(16);
    tokio::task::spawn_blocking(move || {
        let result = client.open_pool(&pool_ref).and_then(|pool| {
            let options = TailOptions {
                since_seq: query.since_seq,
                max_messages: query.max.map(|value| value as usize),
                timeout: query.timeout_ms.map(std::time::Duration::from_millis),
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

fn pool_ref_from_path(pool: &str) -> PoolRef {
    if pool.contains('/') {
        PoolRef::path(pool)
    } else {
        PoolRef::name(pool)
    }
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

fn error_response(err: Error) -> Response {
    let status = match err.kind() {
        ErrorKind::Usage => StatusCode::BAD_REQUEST,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        ErrorKind::Busy => StatusCode::LOCKED,
        ErrorKind::Permission => StatusCode::UNAUTHORIZED,
        ErrorKind::Corrupt | ErrorKind::Io | ErrorKind::Internal => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
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
