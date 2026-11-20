//! MCP Events push delivery for retained pool messages.
use super::*;
use crate::pool_wait::PoolChange;
use plasmite::api::{HostedRead, Read, ReadStatus, Since};
use plasmite::mcp::{JsonRpcId, parse_modern_request};
use std::sync::LazyLock;

const EVENT: &str = "pool.message";
const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

type Authorization = (AccessGrant, (Arc<OauthService>, String));

pub(super) fn catalog() -> &'static Value {
    static CATALOG: LazyLock<Value> = LazyLock::new(|| {
        json!([{
            "name": EVENT,
            "description": "A new message in a named Plasmite pool, optionally filtered by tags. Fetch its contents by sequence.",
            "delivery": ["push"],
            "inputSchema": {
                "type": "object", "required": ["pool"], "additionalProperties": false,
                "properties": {
                    "pool": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                }
            },
            "payloadSchema": {
                "type": "object", "required": ["pool", "seq"], "additionalProperties": false,
                "properties": {
                    "pool": {"type": "string"},
                    "seq": {"type": "string", "description": "Decimal sequence number to pass as since"}
                }
            }
        }])
    });
    &CATALOG
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamRequest {
    name: String,
    arguments: Arguments,
    cursor: Option<String>,
    heartbeat_ms: Option<u64>,
    max_age_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    pool: String,
    #[serde(default)]
    tags: Vec<String>,
}

pub(super) async fn open(
    state: Arc<AppState>,
    shutdown: watch::Receiver<bool>,
    authorization: Option<Authorization>,
    activity: Option<Extension<RequestActivity>>,
    payload: Value,
) -> Response {
    let error_response = |err: McpJsonRpcError| {
        let status = if matches!(err.code, -32600 | -32602) {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::OK
        };
        mcp_protocol_error(&payload, status, err)
    };
    let (id, params) = match parse_modern_request(payload.clone()) {
        Ok(request) => request,
        Err(err) => return error_response(err),
    };
    let request: StreamRequest = match serde_json::from_value(Value::Object(params)) {
        Ok(request) => request,
        Err(_) => {
            return error_response(McpJsonRpcError::invalid_params(
                "invalid event stream parameters",
            ));
        }
    };
    if request.name != EVENT {
        return error_response(McpJsonRpcError {
            code: -32023,
            message: "Unknown event".into(),
            data: Some(json!({"kind": "event"})),
        });
    }
    let cursor = match request.cursor.as_deref().map(parse_cursor).transpose() {
        Ok(cursor) => cursor,
        Err(err) => return error_response(err),
    };
    let pool_ref = match pool_ref_from_request(&request.arguments.pool) {
        Ok(pool) => pool,
        Err(err) => return error_response(McpJsonRpcError::invalid_params(err.to_string())),
    };
    let permit = match acquire_tail_permit(&state) {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(McpJsonRpcError {
                code: -32025,
                message: "Too many live reads".into(),
                data: Some(json!({"limit":"streams"})),
            });
        }
    };
    if let Some(Extension(activity)) = &activity {
        activity.pool(request.arguments.pool.clone(), "events/stream".to_string());
    }
    let timeout = Duration::from_millis(state.max_tail_timeout_ms);
    let (send, receive) = mpsc::channel(16);
    let (opened, opening) = tokio::sync::oneshot::channel();
    let worker = StreamWorker {
        id,
        send,
        authorization,
        timeout,
        waiters: state.pool_waiters.clone(),
    };
    let client = state.client.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _activity = activity;
        let mut opened = Some(opened);
        let mut revoked = std::pin::pin!(wait_for_revocation(
            worker.authorization.as_ref().map(|(grant, _)| grant),
        ));
        let result = tokio::select! {
            biased;
            _ = &mut revoked => Err(McpJsonRpcError::new(-32024, "MCP access was revoked")),
            _ = worker.send.closed() => return,
            _ = wait_for_shutdown(shutdown) => return,
            result = worker.follow(client, pool_ref, request, cursor, &mut opened) => result,
        };
        if let Some(opened) = opened {
            let _ = opened.send(result);
            return;
        }
        if let Err(error) = result {
            let _ = worker
                .send
                .try_send(worker.notification("terminated", json!({"error": error})));
        }
        let _ = worker
            .send
            .try_send(StreamWorker::frame(json!({"jsonrpc":"2.0","id":worker.id,
            "result":{"resultType":"complete","_meta":{(SUBSCRIPTION_ID):worker.id}}})));
    });
    match tokio::time::timeout(timeout, opening).await {
        Ok(Ok(Ok(()))) => {
            let body = ReceiverStream::new(receive).map(Ok::<_, std::convert::Infallible>);
            let mut response = Response::new(Body::from_stream(body));
            apply_sse_headers(&mut response);
            response
                .headers_mut()
                .insert("x-accel-buffering", HeaderValue::from_static("no"));
            response
        }
        Ok(Ok(Err(err))) => error_response(err),
        _ => error_response(McpJsonRpcError::internal_error("event stream did not open")),
    }
}

fn parse_cursor(value: &str) -> Result<(String, u64), McpJsonRpcError> {
    value
        .rsplit_once('/')
        .and_then(|(identity, seq)| Some((identity, seq.parse::<u64>().ok()?)))
        .filter(|(identity, seq)| !identity.is_empty() && *seq > 0)
        .map(|(identity, seq)| (identity.to_owned(), seq))
        .ok_or_else(|| McpJsonRpcError::invalid_params("invalid pool event cursor"))
}

struct StreamWorker {
    id: JsonRpcId,
    send: mpsc::Sender<Bytes>,
    authorization: Option<Authorization>,
    timeout: Duration,
    waiters: PoolWaiters,
}

struct EventReader {
    reader: HostedRead,
    changes: PoolChange,
    identity: String,
    truncated: bool,
}

fn open_reader(
    pool: Pool,
    waiters: &PoolWaiters,
    cursor: Option<(String, u64)>,
    max_age_ms: Option<u64>,
    tags: &[String],
) -> Result<EventReader, Error> {
    let (pool, changes) = waiters.subscribe_owned(pool)?;
    let identity = pool.incarnation()?;
    let read = |start| make_read(start, tags);
    let (start, mut truncated) = match cursor {
        None => (None, false),
        Some((previous, seq)) => {
            let floor = max_age_ms.map(|age| Since::Ago(Duration::from_millis(age)));
            if previous != identity {
                (Some(floor.unwrap_or(Since::Oldest)), true)
            } else if let Some(floor) = floor {
                let floor =
                    HostedRead::start_position(&pool, Read::since(floor))?.unwrap_or(u64::MAX);
                (Some(Since::Seq(seq.max(floor))), floor > seq)
            } else {
                (Some(Since::Seq(seq)), false)
            }
        }
    };
    let reader = match HostedRead::new(pool.clone(), read(start)) {
        Err(err) if err.kind() == ErrorKind::RetentionGap => {
            truncated = true;
            HostedRead::new(pool, read(Some(Since::Oldest)))?
        }
        result => result?,
    };
    Ok(EventReader {
        reader,
        changes,
        identity,
        truncated,
    })
}

fn make_read(start: Option<Since>, tags: &[String]) -> Read {
    start
        .map_or_else(Read::new, Read::since)
        .tags(tags.iter().cloned())
}

impl StreamWorker {
    async fn follow(
        &self,
        client: LocalClient,
        pool_ref: PoolRef,
        request: StreamRequest,
        cursor: Option<(String, u64)>,
        opened: &mut Option<tokio::sync::oneshot::Sender<Result<(), McpJsonRpcError>>>,
    ) -> Result<(), McpJsonRpcError> {
        if !self.access_valid() {
            return Err(McpJsonRpcError::new(
                -32024,
                "MCP credentials are no longer valid",
            ));
        }
        let tags = Arc::new(request.arguments.tags);
        let open_tags = tags.clone();
        let waiters = self.waiters.clone();
        let EventReader {
            mut reader,
            mut changes,
            identity,
            truncated,
        } = self
            .waiters
            .read(move || {
                let pool = client.open_pool(&pool_ref, false)?;
                open_reader(pool, &waiters, cursor, request.max_age_ms, &open_tags)
            })
            .await
            .map_err(pool_error)?;
        if opened
            .take()
            .expect("opening signal available")
            .send(Ok(()))
            .is_err()
        {
            return Ok(());
        }
        let heartbeat_ms = request.heartbeat_ms.unwrap_or(30_000).clamp(1000, 30_000);
        let heartbeat = Duration::from_millis(heartbeat_ms);
        let cursor_at = |seq: Option<u64>| format!("{identity}/{}", seq.unwrap_or(u64::MAX));
        let active = |position, truncated| {
            self.notification("active", json!({
            "cursor": cursor_at(position), "truncated": truncated, "heartbeatMs": heartbeat_ms
        }))
        };
        if !self.deliver(active(reader.position(), truncated)).await {
            return Ok(());
        }

        let mut deadline = tokio::time::Instant::now() + heartbeat;
        loop {
            changes.acknowledge();
            let read_tags = tags.clone();
            let (returned, result, remaining) = self
                .waiters
                .read(move || {
                    let mut remaining = 32;
                    let result = match reader.turn(&mut remaining) {
                        Err(err) if err.kind() == ErrorKind::RetentionGap => reader
                            .restart(make_read(Some(Since::Oldest), &read_tags))
                            .map(|()| None),
                        result => result.map(Some),
                    };
                    Ok((reader, result, remaining))
                })
                .await
                .map_err(pool_error)?;
            reader = returned;
            match result.map_err(pool_error)? {
                Some(ReadStatus::Message(frame)) => {
                    if !self.access_valid() {
                        return Ok(());
                    }
                    let timestamp =
                        time::OffsetDateTime::from_unix_timestamp_nanos(frame.time.into())
                            .expect("stored timestamps fit OffsetDateTime")
                            .format(&time::format_description::well_known::Rfc3339)
                            .expect("stored timestamps fit RFC 3339");
                    if !self.deliver(self.notification(
                        "event",
                        json!({
                            "eventId": format!("{identity}/{}", frame.seq),
                            "name": EVENT,
                            "timestamp": timestamp,
                            "data": {"pool": request.arguments.pool, "seq": frame.seq.to_string()},
                            "cursor": cursor_at(reader.position())
                        }),
                    )).await {
                        return Ok(());
                    }
                    deadline = tokio::time::Instant::now() + heartbeat;
                }
                Some(ReadStatus::Waiting) if remaining == 0 => {}
                Some(ReadStatus::Waiting) => {
                    tokio::select! {
                        _ = changes.changed() => {}
                        _ = tokio::time::sleep_until(deadline) => {
                            if !self.access_valid() || !self.deliver(self.notification(
                                "heartbeat", json!({"cursor": cursor_at(reader.position())}),
                            )).await {
                                return Ok(());
                            }
                            deadline = tokio::time::Instant::now() + heartbeat;
                        }
                    }
                }
                Some(ReadStatus::Complete | ReadStatus::Cancelled) => return Ok(()),
                None => {
                    if !self.access_valid() || !self.deliver(active(reader.position(), true)).await
                    {
                        return Ok(());
                    }
                    deadline = tokio::time::Instant::now() + heartbeat;
                }
            }
        }
    }

    fn access_valid(&self) -> bool {
        self.authorization
            .as_ref()
            .is_none_or(|(grant, (oauth, bearer))| {
                !grant.is_revoked() && oauth.grant(bearer).is_some()
            })
    }

    fn notification(&self, method: &str, mut params: Value) -> Bytes {
        params["_meta"] = json!({(SUBSCRIPTION_ID):self.id});
        Self::frame(
            json!({"jsonrpc":"2.0","method":format!("notifications/events/{method}"),"params":params}),
        )
    }

    fn frame(value: Value) -> Bytes {
        Bytes::from(format!("data: {value}\n\n"))
    }

    async fn deliver(&self, bytes: Bytes) -> bool {
        matches!(
            tokio::time::timeout(self.timeout, self.send.send(bytes)).await,
            Ok(Ok(()))
        )
    }
}

fn pool_error(err: Error) -> McpJsonRpcError {
    match err.kind() {
        ErrorKind::NotFound => McpJsonRpcError::new(-32023, "Pool not found"),
        ErrorKind::Usage => McpJsonRpcError::invalid_params(err.to_string()),
        _ => McpJsonRpcError::internal_error(err.to_string()),
    }
}
