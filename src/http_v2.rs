//! Version 2 pool routes and stream endings.

use super::*;
use crate::interface_wire::{
    LITE3_CONTROL_RECORD, LITE3_MESSAGE_ID_LEN, LITE3_MESSAGE_RECORD, encode_lite3_record_header,
};
use plasmite::api::{Read, ReadStatus, Since};
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
    let permit = acquire_tail_permit(&state)?;
    let client = state.client.clone();
    let timeout = Duration::from_millis(state.max_tail_timeout_ms);
    let (send, receive) = mpsc::channel(16);
    let (opened, opening) = tokio::sync::oneshot::channel();
    let revoked = grant.as_ref().map(AccessGrant::cancellation_flag);
    let worker = ReadWorker {
        send,
        timeout,
        shutdown,
        revoked,
        binary,
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        worker.run::<LIVE>(
            client,
            pool_ref,
            options,
            query.until,
            query.incarnation,
            opened,
        );
    });
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
    fn run<const LIVE: bool>(
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
        let stream = if LIVE {
            pool.follow_lite3(options)
        } else if let Some(until) = until {
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
        if !LIVE && started.checked_add(self.timeout).is_none() {
            let _ = opened.send(Err(
                Error::new(ErrorKind::Usage).with_message("stream limit is out of range")
            ));
            return;
        }
        if opened.send(Ok(incarnation)).is_err() {
            return;
        }

        const CHECK_INTERVAL: Duration = Duration::from_millis(50);
        let mut stalled_since = None;
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
            let full = self.send.capacity() <= 1;
            if !full {
                stalled_since = None;
            }
            let wait = if !LIVE {
                self.timeout.saturating_sub(started.elapsed())
            } else if full {
                let since = stalled_since.get_or_insert_with(Instant::now);
                self.timeout.saturating_sub(since.elapsed())
            } else {
                CHECK_INTERVAL
            };
            if wait.is_zero() {
                if LIVE {
                    self.error(&Error::new(ErrorKind::Io).with_message("follow output stalled"));
                } else {
                    self.cutoff(reader.position());
                }
                return;
            }
            let wait = wait.min(CHECK_INTERVAL);
            if full {
                // Wake as soon as the response makes progress. Cap each wait
                // so the worker also checks revocation and server shutdown.
                let room = tokio::runtime::Handle::current().block_on(async {
                    tokio::time::timeout(wait, self.send.reserve_many(2)).await
                });
                match room {
                    Ok(Ok(_)) => stalled_since = None,
                    Ok(Err(_)) => return,
                    Err(_) => {}
                }
                continue;
            }
            let record = if self.binary {
                reader.poll(wait).map(|poll| poll.map(encode_binary_frame))
            } else {
                reader.poll_json(wait)
            };
            match record {
                Ok(ReadStatus::Message(mut bytes)) => {
                    if !self.binary {
                        bytes.push(b'\n');
                    }
                    if !self.revoked() && self.send.try_send(bytes.into()).is_err() {
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

    #[tokio::test]
    async fn quiet_read_releases_its_slot_when_response_closes() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let reference = PoolRef::name("quiet");
        client
            .create_pool(&reference, PoolOptions::new(65536))
            .unwrap();
        let permits = Arc::new(Semaphore::new(1));
        let permit = permits.clone().try_acquire_owned().unwrap();
        let (send, receive) = mpsc::channel(2);
        let (opened, opening) = tokio::sync::oneshot::channel();
        let done = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            ReadWorker {
                send,
                timeout: Duration::from_secs(5),
                shutdown: watch::channel(false).1,
                revoked: None,
                binary: false,
            }
            .run::<true>(client, reference, Read::new(), None, None, opened);
        });
        opening.await.unwrap().unwrap();
        drop(receive);
        tokio::time::timeout(Duration::from_millis(500), done)
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
        let (send, mut receive) = mpsc::channel(2);
        let (opened, opening) = tokio::sync::oneshot::channel();
        let revoked = Arc::new(AtomicBool::new(false));
        let worker = ReadWorker {
            send,
            timeout: Duration::from_secs(5),
            shutdown: watch::channel(false).1,
            revoked: Some(revoked.clone()),
            binary: false,
        };
        let done = tokio::task::spawn_blocking(move || {
            worker.run::<true>(client, reference, Read::new(), None, None, opened);
        });
        opening.await.unwrap().unwrap();
        revoked.store(true, Ordering::Release);
        let bytes = tokio::time::timeout(Duration::from_millis(500), receive.recv())
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
        let (send, mut receive) = mpsc::channel(2);
        let (opened, opening) = tokio::sync::oneshot::channel();
        let (stop, shutdown) = watch::channel(false);
        let done = tokio::task::spawn_blocking(move || {
            ReadWorker {
                send,
                timeout: Duration::from_secs(5),
                shutdown,
                revoked: None,
                binary: false,
            }
            .run::<true>(client, reference, Read::new(), None, None, opened);
        });
        opening.await.unwrap().unwrap();
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
        let permit = permits.clone().try_acquire_owned().unwrap();
        let (send, mut receive) = mpsc::channel(2);
        let (opened, opening) = tokio::sync::oneshot::channel();
        let done = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            ReadWorker {
                send,
                timeout: Duration::from_millis(100),
                shutdown: watch::channel(false).1,
                revoked: None,
                binary: false,
            }
            .run::<true>(
                client,
                pool_ref,
                Read::since(Since::Oldest),
                None,
                None,
                opened,
            );
        });
        opening.await.unwrap().unwrap();
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
            worker.run::<false>(client, pool_ref, Read::new(), None, None, opened);
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
