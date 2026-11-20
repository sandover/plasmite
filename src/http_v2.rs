//! Version 2 pool routes and stream endings.

use super::*;
use crate::interface_wire::{
    LITE3_CONTROL_RECORD, LITE3_MESSAGE_ID_LEN, LITE3_MESSAGE_RECORD, encode_lite3_record_header,
};
use plasmite::api::{HostedRead, Read, ReadStatus, Since};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn routes(shutdown: watch::Receiver<bool>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/pools", post(create_pool).get(list_pools))
        .route("/v2/pools/:pool", get(pool_info).delete(delete_pool))
        .route("/v2/pools/:pool/open", post(open))
        .route("/v2/pools/:pool/feed", post(feed))
        .route("/v2/pools/:pool/fetch", get(read::<false>))
        .route("/v2/pools/:pool/follow", get(read::<true>))
        .layer(Extension(shutdown))
        .layer(middleware::map_response(|mut response: Response| async {
            response
                .headers_mut()
                .insert("plasmite-version", HeaderValue::from_static("2"));
            response
        }))
}

#[derive(Deserialize)]
struct OpenRequest {
    #[serde(default)]
    create: bool,
}

async fn open(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Json(request): Json<OpenRequest>,
) -> Result<Response, HttpError> {
    let grant = authorize(&headers, &state)?;
    let pool_ref = pool_ref_from_request(&name)?;
    let client = state.client.clone();
    let info = state
        .storage_executor
        .run_authorized(grant, move || {
            client.open_pool(&pool_ref, request.create)?.info()
        })
        .await?;
    Ok(json_response(
        json!({ "pool": pool_info_json(&name, &info) }),
    ))
}

async fn feed(
    state: State<Arc<AppState>>,
    headers: HeaderMap,
    path: AxumPath<String>,
    query: Query<AppendLite3Query>,
    body: Bytes,
) -> Result<Response, HttpError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        == Some("application/x-plasmite-lite3")
    {
        if !matches!(query.durability.as_deref(), None | Some("fast" | "flush")) {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("durability must be fast or flush")
                .into());
        }
        return append_lite3(state, headers, path, query, body).await;
    }
    // Check access before interpreting input, just as the other write routes do.
    authorize(&headers, &state)?;
    let request: AppendRequest = serde_json::from_slice(&body).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid feed input")
            .with_source(err)
    })?;
    if !request.data.is_object() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("message data must be an object")
            .into());
    }
    if !matches!(request.durability.as_deref(), None | Some("fast" | "flush")) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("durability must be fast or flush")
            .into());
    }
    append_message(state, headers, path, Json(request)).await
}

#[derive(Deserialize)]
struct ReadQuery {
    since: Option<String>,
    last: Option<u64>,
    until: Option<u64>,
    incarnation: Option<String>,
}

async fn read<const LIVE: bool>(
    State(state): State<Arc<AppState>>,
    Extension(shutdown): Extension<watch::Receiver<bool>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<ReadQuery>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response, HttpError> {
    let (pool_ref, grant) = tail_pool_ref_from_request(&state, &headers, &name)?;
    let mut options = match (query.since.as_deref(), query.last) {
        (Some(_), Some(_)) => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("use either since or last")
                .into());
        }
        (Some(since), None) => Read::since(since.parse::<Since>()?),
        (None, Some(last)) => Read::last(last),
        (None, None) => Read::new(),
    };
    if query.until.is_some()
        && (LIVE
            || !query
                .since
                .as_ref()
                .is_some_and(|since| since.parse::<u64>().is_ok()))
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("until requires an explicit continuation sequence on fetch")
            .into());
    }
    if query.incarnation.is_some() && query.until.is_none() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("incarnation requires a fetch continuation")
            .into());
    }
    let tags = url::form_urlencoded::parse(raw_query.as_deref().unwrap_or_default().as_bytes())
        .filter_map(|(key, value)| (key == "tag").then(|| value.into_owned()));
    options = options.tags(tags);
    let binary = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        == Some("application/x-plasmite-lite3-stream");
    let permit = if LIVE {
        acquire_tail_permit(&state)?
    } else {
        state
            .fetch_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                Error::new(ErrorKind::Busy)
                    .with_message("the server is at its limit of finite fetches")
                    .with_hint("Retry after an in-flight fetch completes.")
            })?
    };
    let client = state.client.clone();
    let timeout = Duration::from_millis(state.max_tail_timeout_ms);
    // Live reads keep one message ready for the connection and one slot for
    // the terminal record. A large message cannot multiply across a queue.
    let (send, receive) = mpsc::channel(if LIVE { 2 } else { 16 });
    let (opened, opening) = tokio::sync::oneshot::channel();
    let worker = ReadWorker {
        send,
        timeout,
        shutdown,
        revoked: grant.as_ref().map(AccessGrant::cancellation_flag),
        binary,
    };
    if LIVE {
        let waiters = state.pool_waiters.clone();
        let grant = grant.clone();
        tokio::spawn(async move {
            let _permit = permit;
            worker
                .run_follow(client, pool_ref, options, waiters, grant, opened)
                .await;
        });
    } else {
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker.run_fetch(
                client,
                pool_ref,
                options,
                query.until,
                query.incarnation,
                opened,
            );
        });
    }
    let incarnation = tokio::time::timeout(timeout, opening)
        .await
        .map_err(|_| Error::new(ErrorKind::Busy).with_message("stream opening took too long"))?
        .map_err(|_| Error::new(ErrorKind::Io).with_message("stream could not open"))??;
    let mut finished = false;
    let body = ReceiverStream::new(receive)
        .map_while(move |bytes| {
            if finished {
                return None;
            }
            if grant.as_ref().is_some_and(AccessGrant::is_revoked) {
                finished = true;
                let error = Error::new(ErrorKind::Permission).with_message("access key revoked");
                return Some(encode_control(
                    binary,
                    json!({"type":"error","error":error_body(&error)}),
                ));
            }
            Some(bytes)
        })
        .map(Ok::<_, std::convert::Infallible>);
    let mut response = Response::new(Body::from_stream(body));
    response.headers_mut().insert(
        "plasmite-incarnation",
        HeaderValue::from_str(&incarnation).expect("native identity is ASCII"),
    );
    let media_type = if binary {
        "application/x-plasmite-lite3-stream"
    } else {
        "application/jsonl"
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    Ok(response)
}

struct ReadWorker {
    send: mpsc::Sender<Bytes>,
    timeout: Duration,
    shutdown: watch::Receiver<bool>,
    revoked: Option<Arc<AtomicBool>>,
    binary: bool,
}

impl ReadWorker {
    async fn run_follow(
        self,
        client: LocalClient,
        pool_ref: PoolRef,
        options: Read,
        waiters: PoolWaiters,
        grant: Option<AccessGrant>,
        opened: tokio::sync::oneshot::Sender<Result<String, Error>>,
    ) {
        let mut opened = Some(opened);
        let result = tokio::select! {
            biased;
            _ = wait_for_revocation(grant.as_ref()) => {
                Err(Error::new(ErrorKind::Permission).with_message("access key revoked"))
            }
            _ = self.send.closed() => return,
            _ = wait_for_shutdown(self.shutdown.clone()) => {
                Err(Error::new(ErrorKind::Io).with_message("server is shutting down"))
            }
            result = self.follow(client, pool_ref, options, &waiters, &mut opened) => result,
        };
        if let Err(error) = result {
            if let Some(opened) = opened {
                let _ = opened.send(Err(error));
            } else {
                self.error(&error);
            }
        }
    }

    async fn follow(
        &self,
        client: LocalClient,
        pool_ref: PoolRef,
        options: Read,
        waiters: &PoolWaiters,
        opened: &mut Option<tokio::sync::oneshot::Sender<Result<String, Error>>>,
    ) -> Result<(), Error> {
        let registry = waiters.clone();
        let (incarnation, initial) = waiters
            .read(move || {
                let pool = client.open_pool(&pool_ref, false)?;
                let incarnation = pool.incarnation()?;
                let (pool, changes) = registry.subscribe_owned(pool)?;
                let initial = match HostedRead::new(pool, options) {
                    Ok(reader) => Ok((reader, changes)),
                    Err(error) if error.kind() == ErrorKind::RetentionGap => Err(error),
                    Err(error) => return Err(error),
                };
                Ok((incarnation, initial))
            })
            .await?;
        if opened
            .take()
            .expect("opening signal available")
            .send(Ok(incarnation))
            .is_err()
        {
            return Ok(());
        }
        let (mut reader, mut changes) = initial?;
        let binary = self.binary;
        loop {
            // Wait for the next message slot without holding a storage worker.
            // Reserve two slots so an error still fits after the message.
            if self.send.capacity() <= 1 {
                match tokio::time::timeout(self.timeout, self.send.reserve_many(2)).await {
                    Ok(Ok(available)) => drop(available),
                    Ok(Err(_)) => return Ok(()),
                    Err(_) => {
                        return Err(Error::new(ErrorKind::Io).with_message("follow output stalled"));
                    }
                }
            }
            changes.acknowledge();
            let (returned, record, remaining) = waiters
                .read(move || {
                    let mut remaining = 32;
                    let record = if binary {
                        match reader.turn(&mut remaining)? {
                            ReadStatus::Message(frame) => {
                                ReadStatus::Message(encode_binary_frame(frame))
                            }
                            ReadStatus::Waiting => ReadStatus::Waiting,
                            ReadStatus::Complete => ReadStatus::Complete,
                            ReadStatus::Cancelled => ReadStatus::Cancelled,
                        }
                    } else {
                        reader.turn_json(&mut remaining)?
                    };
                    Ok((reader, record, remaining))
                })
                .await?;
            reader = returned;
            match record {
                ReadStatus::Message(bytes) => {
                    if self.revoked() {
                        return Err(
                            Error::new(ErrorKind::Permission).with_message("access key revoked")
                        );
                    }
                    if self.send.try_send(self.record(bytes)).is_err() {
                        return Ok(());
                    }
                }
                ReadStatus::Waiting if remaining == 0 => tokio::task::yield_now().await,
                ReadStatus::Waiting => changes.changed().await,
                ReadStatus::Complete => {
                    self.control(json!({"type":"complete"}));
                    return Ok(());
                }
                ReadStatus::Cancelled => return Ok(()),
            }
        }
    }

    fn run_fetch(
        self,
        client: LocalClient,
        pool_ref: PoolRef,
        options: Read,
        until: Option<u64>,
        expected_incarnation: Option<String>,
        opened: tokio::sync::oneshot::Sender<Result<String, Error>>,
    ) {
        let pool = match client.open_pool(&pool_ref, false) {
            Ok(pool) => pool,
            Err(error) => {
                let _ = opened.send(Err(error));
                return;
            }
        };
        let incarnation = match pool.incarnation() {
            Ok(incarnation) => incarnation,
            Err(error) => {
                let _ = opened.send(Err(error));
                return;
            }
        };
        if expected_incarnation.is_some_and(|expected| expected != incarnation) {
            let _ = opened
                .send(Err(Error::new(ErrorKind::NotFound)
                    .with_message("pool was replaced during the read")));
            return;
        }
        let stream = if let Some(until) = until {
            pool.continue_fetch_lite3(options, until)
        } else {
            pool.fetch_lite3(options)
        };
        let mut reader = match stream {
            Ok(reader) => reader,
            Err(error) if error.kind() == ErrorKind::RetentionGap => {
                if opened.send(Ok(incarnation)).is_ok() {
                    self.error(&error);
                }
                return;
            }
            Err(error) => {
                let _ = opened.send(Err(error));
                return;
            }
        };
        let started = Instant::now();
        if started.checked_add(self.timeout).is_none() {
            let _ = opened.send(Err(
                Error::new(ErrorKind::Usage).with_message("stream limit is out of range")
            ));
            return;
        }
        if opened.send(Ok(incarnation)).is_err() {
            return;
        }

        const CHECK_INTERVAL: Duration = Duration::from_millis(50);
        loop {
            if self.send.is_closed() {
                return;
            }
            if self.revoked() {
                self.error(&Error::new(ErrorKind::Permission).with_message("access key revoked"));
                return;
            }
            if *self.shutdown.borrow() {
                self.error(&Error::new(ErrorKind::Io).with_message("server is shutting down"));
                return;
            }
            // Keep one channel slot for a terminal record. Advance the read only
            // when there is room to hand off the next matching frame.
            let wait = self.timeout.saturating_sub(started.elapsed());
            if wait.is_zero() {
                self.cutoff(reader.position());
                return;
            }
            let wait = wait.min(CHECK_INTERVAL);
            if self.send.capacity() <= 1 {
                // Check for revocation and shutdown while the client is stalled.
                let room = tokio::runtime::Handle::current().block_on(async {
                    tokio::time::timeout(wait, self.send.reserve_many(2)).await
                });
                if matches!(room, Ok(Err(_))) {
                    return;
                }
                continue;
            }
            let record = if self.binary {
                reader.poll(wait).map(|poll| poll.map(encode_binary_frame))
            } else {
                reader.poll_json(wait)
            };
            match record {
                Ok(ReadStatus::Message(bytes)) => {
                    if !self.revoked() && self.send.try_send(self.record(bytes)).is_err() {
                        return;
                    }
                }
                Ok(ReadStatus::Waiting) => {}
                Ok(ReadStatus::Complete) => {
                    self.control(json!({"type":"complete"}));
                    return;
                }
                Ok(ReadStatus::Cancelled) => return,
                Err(error) => {
                    self.error(&error);
                    return;
                }
            }
        }
    }

    fn record(&self, mut bytes: Vec<u8>) -> Bytes {
        if !self.binary {
            bytes.push(b'\n');
        }
        bytes.into()
    }

    fn revoked(&self) -> bool {
        self.revoked
            .as_ref()
            .is_some_and(|revoked| revoked.load(Ordering::Acquire))
    }

    fn cutoff(&self, position: (Option<u64>, Option<u64>)) {
        match position {
            (Some(next_seq), Some(until)) if next_seq <= until => {
                self.control(json!({"type":"cutoff","next_seq":next_seq,"until":until}));
            }
            _ => self.control(json!({"type":"complete"})),
        }
    }

    fn error(&self, error: &Error) {
        self.control(json!({"type":"error","error":error_body(error)}));
    }

    fn control(&self, record: Value) {
        let _ = self.send.try_send(encode_control(self.binary, record));
    }
}

fn encode_binary_frame(frame: plasmite::api::FrameRef) -> Vec<u8> {
    let length = u32::try_from(frame.payload.len() + LITE3_MESSAGE_ID_LEN)
        .expect("a stored frame fits a record length");
    let mut bytes = Vec::with_capacity(length as usize + 5);
    bytes.extend_from_slice(&encode_lite3_record_header(LITE3_MESSAGE_RECORD, length));
    bytes.extend_from_slice(&frame.seq.to_be_bytes());
    bytes.extend_from_slice(&frame.time.to_be_bytes());
    bytes.extend_from_slice(&frame.payload);
    bytes
}

fn encode_control(binary: bool, record: Value) -> Bytes {
    let payload = serde_json::to_vec(&record).expect("JSON control object");
    let mut bytes = Vec::with_capacity(payload.len() + 5);
    if binary {
        let len = u32::try_from(payload.len()).expect("control record length");
        bytes.extend_from_slice(&encode_lite3_record_header(LITE3_CONTROL_RECORD, len));
    }
    bytes.extend_from_slice(&payload);
    if !binary {
        bytes.push(b'\n');
    }
    bytes.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower_service::Service;

    async fn follow_worker(
        client: LocalClient,
        pool_ref: PoolRef,
        options: Read,
        timeout: Duration,
        grant: Option<AccessGrant>,
        shutdown: watch::Receiver<bool>,
        permit: OwnedSemaphorePermit,
    ) -> (mpsc::Receiver<Bytes>, tokio::task::JoinHandle<()>) {
        let (send, receive) = mpsc::channel(2);
        let (opened, opening) = tokio::sync::oneshot::channel();
        let done = tokio::spawn(async move {
            let _permit = permit;
            ReadWorker {
                send,
                timeout,
                shutdown,
                revoked: grant.as_ref().map(AccessGrant::cancellation_flag),
                binary: false,
            }
            .run_follow(
                client,
                pool_ref,
                options,
                PoolWaiters::default(),
                grant,
                opened,
            )
            .await;
        });
        opening.await.unwrap().unwrap();
        (receive, done)
    }

    #[tokio::test]
    async fn quiet_read_releases_its_slot_when_response_closes() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let reference = PoolRef::name("quiet");
        client
            .create_pool(&reference, PoolOptions::new(65536))
            .unwrap();
        let permits = Arc::new(Semaphore::new(1));
        let (_stop, shutdown) = watch::channel(false);
        let (receive, done) = follow_worker(
            client,
            reference,
            Read::new(),
            Duration::from_secs(5),
            None,
            shutdown,
            permits.clone().try_acquire_owned().unwrap(),
        )
        .await;
        drop(receive);
        tokio::time::timeout(Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn revoked_quiet_read_sends_an_error_record() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let reference = PoolRef::name("revoked");
        client
            .create_pool(&reference, PoolOptions::new(65536))
            .unwrap();
        let access = AccessStore::open(dir.path(), None, None, None).unwrap();
        let key = access.issue("reader").unwrap();
        let grant = access
            .authorize_secret(key.rsplit('.').next().unwrap())
            .unwrap();
        let id = serde_json::to_value(access.list().unwrap()).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (_stop, shutdown) = watch::channel(false);
        let (mut receive, done) = follow_worker(
            client,
            reference,
            Read::new(),
            Duration::from_secs(5),
            Some(grant),
            shutdown,
            Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
        )
        .await;
        access.revoke(&id).unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(2), receive.recv())
            .await
            .unwrap()
            .unwrap();
        let error: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["kind"], "Permission");
        done.await.unwrap();
    }

    #[tokio::test]
    async fn quiet_follow_ends_on_server_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let reference = PoolRef::name("quiet");
        client
            .create_pool(&reference, PoolOptions::new(65536))
            .unwrap();
        let (stop, shutdown) = watch::channel(false);
        let (mut receive, done) = follow_worker(
            client,
            reference,
            Read::new(),
            Duration::from_secs(5),
            None,
            shutdown,
            Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
        )
        .await;
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap();
        let error: Value = serde_json::from_slice(&receive.try_recv().unwrap()).unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["message"], "server is shutting down");
        assert!(receive.recv().await.is_none());
    }

    #[tokio::test]
    async fn stalled_follow_reports_io_and_releases_its_slot() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let pool_ref = PoolRef::name("stalled");
        let mut pool = client
            .create_pool(&pool_ref, PoolOptions::new(65536))
            .unwrap();
        for n in 1..=3 {
            pool.feed(&json!({"n": n}), Default::default()).unwrap();
        }
        let permits = Arc::new(Semaphore::new(1));
        let (_stop, shutdown) = watch::channel(false);
        let (mut receive, done) = follow_worker(
            client,
            pool_ref,
            Read::since(Since::Oldest),
            Duration::from_millis(100),
            None,
            shutdown,
            permits.clone().try_acquire_owned().unwrap(),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(permits.available_permits(), 1);
        let message: Value = serde_json::from_slice(&receive.try_recv().unwrap()).unwrap();
        assert_eq!(message["seq"], 1);
        let error: Value = serde_json::from_slice(&receive.try_recv().unwrap()).unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["error"]["kind"], "Io");
        assert_eq!(error["error"]["message"], "follow output stalled");
        assert!(receive.recv().await.is_none());
    }

    #[tokio::test]
    async fn fetch_and_feed_keep_their_slots_when_a_large_follow_stalls() {
        let dir = tempfile::tempdir().unwrap();
        let reference = PoolRef::name("stalled");
        let mut pool = LocalClient::new()
            .with_pool_dir(dir.path())
            .create_pool(&reference, PoolOptions::new(2 * 1024 * 1024))
            .unwrap();
        for n in 1..=3 {
            pool.feed(
                &json!({"n": n, "text": "x".repeat(256 * 1024)}),
                Default::default(),
            )
            .unwrap();
        }
        let config = ServeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            pool_dir: dir.path().to_owned(),
            tls_cert: None,
            tls_key: None,
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 5_000,
            max_concurrent_tails: 1,
        };
        let (_stop, shutdown) = watch::channel(false);
        let mut app = prepare_server_with_access(
            &config,
            None,
            true,
            ActivityRegistry::default(),
            shutdown,
            PoolWaiters::default(),
        )
        .await
        .unwrap()
        .app;
        let get = |uri| {
            Request::builder()
                .uri(uri)
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap()
        };
        let follow = app
            .call(get("/v2/pools/stalled/follow?since=oldest"))
            .await
            .unwrap();
        assert_eq!(follow.status(), StatusCode::OK);
        let second = app.call(get("/v2/pools/stalled/follow")).await.unwrap();
        assert_eq!(second.status(), StatusCode::LOCKED);
        let started = Instant::now();
        let fetch = app
            .call(get("/v2/pools/stalled/fetch?last=1"))
            .await
            .unwrap();
        assert_eq!(fetch.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(fetch.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let mut lines = bytes.split(|byte| *byte == b'\n');
        let fetched: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
        assert_eq!(fetched["seq"], 3);
        assert_eq!(lines.next().unwrap(), br#"{"type":"complete"}"#);
        let fetch_elapsed = started.elapsed();
        let started = Instant::now();
        let feed = app
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/v2/pools/stalled/feed")
                    .header("host", "localhost")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"data":{"ok":true}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(feed.status(), StatusCode::OK);
        eprintln!(
            "HTTP with a stalled follow: fetch {fetch_elapsed:?}, feed {:?}",
            started.elapsed()
        );
        drop(follow);
    }

    #[tokio::test]
    async fn full_read_queue_keeps_room_for_cutoff_at_the_next_unsent_record() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let pool_ref = PoolRef::name("history");
        let mut pool = client
            .create_pool(&pool_ref, plasmite::api::PoolOptions::new(65536))
            .unwrap();
        for n in 1..=3 {
            pool.feed(&json!({"n": n}), Default::default()).unwrap();
        }

        let (send, mut receive) = mpsc::channel(2);
        let (opened, opening) = tokio::sync::oneshot::channel();
        let done = tokio::task::spawn_blocking(move || {
            let worker = ReadWorker {
                send,
                timeout: Duration::from_millis(250),
                shutdown: watch::channel(false).1,
                revoked: None,
                binary: false,
            };
            worker.run_fetch(client, pool_ref, Read::new(), None, None, opened);
        });
        opening.await.unwrap().unwrap();
        done.await.unwrap();

        let message: Value = serde_json::from_slice(&receive.try_recv().unwrap()).unwrap();
        assert_eq!(message["seq"], 1);
        let cutoff: Value = serde_json::from_slice(&receive.try_recv().unwrap()).unwrap();
        assert_eq!(cutoff, json!({"type": "cutoff", "next_seq": 2, "until": 3}));
        assert!(receive.try_recv().is_err());
    }
}
