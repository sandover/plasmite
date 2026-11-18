//! Version 2 pool routes and stream endings.

use super::*;
use crate::interface_wire::{
    LITE3_CONTROL_RECORD, LITE3_MESSAGE_ID_LEN, LITE3_MESSAGE_RECORD, encode_lite3_record_header,
};
use plasmite::api::{Read, ReadStatus, Since};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/pools", post(create_pool).get(list_pools))
        .route("/v2/pools/:pool", get(pool_info).delete(delete_pool))
        .route("/v2/pools/:pool/open", post(open))
        .route("/v2/pools/:pool/feed", post(feed))
        .route("/v2/pools/:pool/fetch", get(read::<false>))
        .route("/v2/pools/:pool/follow", get(read::<true>))
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
}

async fn read<const LIVE: bool>(
    State(state): State<Arc<AppState>>,
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
    if let Some(end) = query.until {
        if LIVE
            || !query
                .since
                .as_ref()
                .is_some_and(|since| since.parse::<u64>().is_ok())
        {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("until requires an explicit continuation sequence on fetch")
                .into());
        }
        options = options.until(end);
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
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("stream limit is out of range"))?;
    let (send, receive) = mpsc::channel(16);
    let (opened, opening) = tokio::sync::oneshot::channel();
    let revoked = grant.as_ref().map(AccessGrant::cancellation_flag);
    let worker = ReadWorker {
        send,
        deadline,
        revoked,
        binary,
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        worker.run::<LIVE>(client, pool_ref, options, opened);
    });
    tokio::time::timeout(timeout, opening)
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
    deadline: Instant,
    revoked: Option<Arc<AtomicBool>>,
    binary: bool,
}

impl ReadWorker {
    fn run<const LIVE: bool>(
        self,
        client: LocalClient,
        pool_ref: PoolRef,
        options: Read,
        opened: tokio::sync::oneshot::Sender<Result<(), Error>>,
    ) {
        let pool = match client.open_pool(&pool_ref, false) {
            Ok(pool) => pool,
            Err(error) => {
                let _ = opened.send(Err(error));
                return;
            }
        };
        let stream = if LIVE {
            pool.follow_lite3(options)
        } else {
            pool.fetch_lite3(options)
        };
        let mut reader = match stream {
            Ok(reader) => reader,
            Err(error) if error.kind() == ErrorKind::RetentionGap => {
                if opened.send(Ok(())).is_ok() {
                    self.error(&error);
                }
                return;
            }
            Err(error) => {
                let _ = opened.send(Err(error));
                return;
            }
        };
        if opened.send(Ok(())).is_err() {
            return;
        }

        loop {
            if self.send.is_closed() {
                return;
            }
            if self.revoked() {
                self.error(&Error::new(ErrorKind::Permission).with_message("access key revoked"));
                return;
            }
            // Keep one channel slot for a terminal record. Advance the read only
            // when there is room to hand off the next matching frame.
            if self.send.capacity() <= 1 {
                // The worker already runs on a blocking thread. Let the receiver
                // wake it as soon as there is room while checking revocation and
                // the cutoff even if the consumer has stopped reading.
                let wait = self.deadline.saturating_duration_since(Instant::now());
                let room = tokio::runtime::Handle::current().block_on(async {
                    tokio::time::timeout(
                        wait.min(Duration::from_millis(50)),
                        self.send.reserve_many(2),
                    )
                    .await
                });
                match room {
                    Ok(Ok(permits)) => {
                        drop(permits);
                        continue;
                    }
                    Ok(Err(_)) => return,
                    Err(_) if Instant::now() >= self.deadline => {
                        self.cutoff(reader.position());
                        return;
                    }
                    Err(_) => continue,
                }
            }
            let wait = self.deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                self.cutoff(reader.position());
                return;
            }
            let wait = wait.min(Duration::from_millis(50));
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
            (Some(next_seq), until) if until.is_none_or(|end| next_seq <= end) => {
                let mut record = json!({"type":"cutoff","next_seq":next_seq});
                if let Some(until) = until {
                    record["until"] = until.into();
                }
                self.control(record);
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
                deadline: Instant::now() + Duration::from_millis(250),
                revoked: None,
                binary: false,
            };
            worker.run::<false>(client, pool_ref, Read::new(), opened);
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
