//! MCP Events push delivery for retained pool messages.
use super::*;
use plasmite::api::{Read, ReadPoll, Since};
use plasmite::mcp::{JsonRpcId, parse_modern_request};
use std::sync::LazyLock;

const EVENT: &str = "pool.message";
const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
const STREAM_CHECK: Duration = Duration::from_millis(100);

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
    let client = state.client.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _activity = activity;
        let worker = StreamWorker {
            id,
            send,
            shutdown,
            authorization,
            timeout,
        };
        worker.run(client, pool_ref, request, cursor, opened);
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
    shutdown: watch::Receiver<bool>,
    authorization: Option<Authorization>,
    timeout: Duration,
}

impl StreamWorker {
    fn run(
        self,
        client: LocalClient,
        pool_ref: PoolRef,
        request: StreamRequest,
        cursor: Option<(String, u64)>,
        opened: tokio::sync::oneshot::Sender<Result<(), McpJsonRpcError>>,
    ) {
        let mut opened = Some(opened);
        let result = self.follow(client, pool_ref, request, cursor, &mut opened);
        if let Some(opened) = opened {
            let _ = opened.send(result);
            return;
        }
        if let Err(error) = result {
            self.notification("terminated", json!({"error": error}));
        }
        self.frame(json!({"jsonrpc":"2.0","id":self.id,
            "result":{"resultType":"complete","_meta":{(SUBSCRIPTION_ID):self.id}}}));
    }

    fn follow(
        &self,
        client: LocalClient,
        pool_ref: PoolRef,
        request: StreamRequest,
        cursor: Option<(String, u64)>,
        opened: &mut Option<tokio::sync::oneshot::Sender<Result<(), McpJsonRpcError>>>,
    ) -> Result<(), McpJsonRpcError> {
        if !self.access_valid()? {
            return Err(McpJsonRpcError::new(
                -32024,
                "MCP credentials are no longer valid",
            ));
        }
        let pool = client.open_pool(&pool_ref, false).map_err(pool_error)?;
        let identity = pool.incarnation().map_err(pool_error)?;
        let tags = request.arguments.tags;
        let read = |start| {
            match start {
                Some(since) => Read::since(since),
                None => Read::new(),
            }
            .tags(tags.iter().cloned())
        };
        let (start, mut truncated) = match cursor {
            None => (None, false),
            Some((previous, seq)) => {
                let floor = request
                    .max_age_ms
                    .map(|age| Since::Ago(Duration::from_millis(age)));
                if previous != identity {
                    (Some(floor.unwrap_or(Since::Oldest)), true)
                } else if let Some(floor) = floor {
                    let floor = pool
                        .follow_lite3(Read::since(floor))
                        .map_err(pool_error)?
                        .position()
                        .0
                        .unwrap_or(u64::MAX);
                    (Some(Since::Seq(seq.max(floor))), floor > seq)
                } else {
                    (Some(Since::Seq(seq)), false)
                }
            }
        };
        let mut reader = match pool.follow_lite3(read(start)) {
            Err(err) if err.kind() == ErrorKind::RetentionGap => {
                truncated = true;
                pool.follow_lite3(read(Some(Since::Oldest)))
                    .map_err(pool_error)?
            }
            result => result.map_err(pool_error)?,
        };
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
        let active = |reader: &plasmite::api::Lite3Follow<'_>, truncated| {
            self.notification("active", json!({
                "cursor": cursor_at(reader.position().0), "truncated": truncated, "heartbeatMs": heartbeat_ms
            }))
        };
        if !active(&reader, truncated) {
            return Ok(());
        }

        let mut last_sent = Instant::now();
        while !self.send.is_closed() && !*self.shutdown.borrow() {
            if self
                .authorization
                .as_ref()
                .is_some_and(|(grant, _)| grant.is_revoked())
            {
                return Err(McpJsonRpcError::new(-32024, "MCP access was revoked"));
            }
            let wait = heartbeat
                .saturating_sub(last_sent.elapsed())
                .min(STREAM_CHECK);
            match reader.poll(wait) {
                Ok(ReadPoll::Message(frame)) => {
                    if !self.access_valid()? {
                        return Ok(());
                    }
                    let timestamp =
                        time::OffsetDateTime::from_unix_timestamp_nanos(frame.time.into())
                            .expect("stored timestamps fit OffsetDateTime")
                            .format(&time::format_description::well_known::Rfc3339)
                            .expect("stored timestamps fit RFC 3339");
                    if !self.notification(
                        "event",
                        json!({
                            "eventId": format!("{identity}/{}", frame.seq),
                            "name": EVENT,
                            "timestamp": timestamp,
                            "data": {"pool": request.arguments.pool, "seq": frame.seq.to_string()},
                            "cursor": cursor_at(reader.position().0)
                        }),
                    ) {
                        return Ok(());
                    }
                    last_sent = Instant::now();
                }
                Ok(ReadPoll::Waiting) if last_sent.elapsed() >= heartbeat => {
                    if !self.access_valid()? {
                        return Ok(());
                    }
                    pool.check_current_path().map_err(pool_error)?;
                    if !self.notification(
                        "heartbeat",
                        json!({"cursor": cursor_at(reader.position().0)}),
                    ) {
                        return Ok(());
                    }
                    last_sent = Instant::now();
                }
                Ok(ReadPoll::Waiting) => {}
                Ok(ReadPoll::Complete | ReadPoll::Cancelled) => break,
                Err(err) if err.kind() == ErrorKind::RetentionGap => {
                    if !self.access_valid()? {
                        return Ok(());
                    }
                    reader = pool
                        .follow_lite3(read(Some(Since::Oldest)))
                        .map_err(pool_error)?;
                    if !active(&reader, true) {
                        return Ok(());
                    }
                    last_sent = Instant::now();
                }
                Err(err) => return Err(pool_error(err)),
            }
        }
        Ok(())
    }

    fn access_valid(&self) -> Result<bool, McpJsonRpcError> {
        let Some((grant, (oauth, bearer))) = &self.authorization else {
            return Ok(true);
        };
        if grant.is_revoked() {
            return Err(McpJsonRpcError::new(-32024, "MCP access was revoked"));
        }
        // A credential can expire or rotate while the key still permits access.
        // Close the stream so the client can reconnect with fresh credentials.
        Ok(oauth.grant(bearer).is_some())
    }

    fn notification(&self, method: &str, mut params: Value) -> bool {
        params["_meta"] = json!({(SUBSCRIPTION_ID):self.id});
        self.frame(json!({"jsonrpc":"2.0","method":format!("notifications/events/{method}"),"params":params}))
    }

    fn frame(&self, value: Value) -> bool {
        let bytes = Bytes::from(format!("data: {value}\n\n"));
        matches!(
            tokio::runtime::Handle::current().block_on(async {
                tokio::time::timeout(self.timeout, self.send.send(bytes)).await
            }),
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
