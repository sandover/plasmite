//! Open and continue HTTP readers using explicit terminal records.
use super::*;
use crate::api::message::{Read as Selection, ReadStart, Since, format_ts};
use crate::interface_wire::{
    LITE3_CONTROL_RECORD, LITE3_MESSAGE_ID_LEN, LITE3_MESSAGE_RECORD, LITE3_RECORD_HEADER_LEN,
};
use serde_json::Value;
use std::sync::atomic::Ordering;

pub struct RemoteRead(Stream);
pub struct RemoteLite3Read(Stream);

pub trait RemotePoolTransport {
    fn follow_lite3(&self, read: Selection) -> ApiResult<RemoteLite3Read>;

    fn fetch_lite3(&self, read: Selection) -> ApiResult<RemoteLite3Read>;

    fn feed_lite3(&self, bytes: &[u8], options: crate::api::Feed) -> ApiResult<DecodedMessage>;

    fn feed_lite3_receipt(
        &self,
        bytes: &[u8],
        options: crate::api::Feed,
    ) -> ApiResult<crate::api::FeedReceipt>;
}

impl RemotePoolTransport for RemotePool {
    fn follow_lite3(&self, read: Selection) -> ApiResult<RemoteLite3Read> {
        Stream::open(self, read, true, true).map(RemoteLite3Read)
    }

    fn fetch_lite3(&self, read: Selection) -> ApiResult<RemoteLite3Read> {
        Stream::open(self, read, false, true).map(RemoteLite3Read)
    }

    fn feed_lite3(&self, bytes: &[u8], options: crate::api::Feed) -> ApiResult<DecodedMessage> {
        let envelope: MessageEnvelope = read_json_response(self.request_lite3_feed(
            bytes,
            options.encoded_durability()?,
            "application/json",
        )?)?;
        message_from_remote(envelope.message)
    }

    fn feed_lite3_receipt(
        &self,
        bytes: &[u8],
        options: crate::api::Feed,
    ) -> ApiResult<crate::api::FeedReceipt> {
        let response = self.request_lite3_feed(
            bytes,
            options.encoded_durability()?,
            "application/x-plasmite-identity",
        )?;
        if response.status() != 204 {
            return Err(protocol_error("expected a numeric feed receipt"));
        }
        let field = |name| {
            response
                .header(name)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| protocol_error(&format!("invalid feed receipt header {name}")))
        };
        Ok(crate::api::FeedReceipt {
            seq: field("plasmite-seq")?,
            time: field("plasmite-timestamp-ns")?,
        })
    }
}

impl RemotePool {
    pub fn follow(&self, read: Selection) -> ApiResult<RemoteRead> {
        Stream::open(self, read, true, true).map(RemoteRead)
    }

    pub fn fetch(&self, read: Selection) -> ApiResult<RemoteRead> {
        Stream::open(self, read, false, true).map(RemoteRead)
    }

    pub fn feed<'a>(
        &self,
        input: impl Into<crate::api::FeedInput<'a>>,
        options: crate::api::Feed,
    ) -> ApiResult<Message> {
        let (view, durability) = options.prepare(input.into())?;
        self.feed_committed_view(view.bytes(), durability)
            .map(Message::from_view)
    }
}

impl RemoteRead {
    pub fn next_message(&mut self) -> ApiResult<Option<Message>> {
        match self.0.next_record()? {
            Some(Record::Lite3(frame)) => {
                crate::view::MessageView::from_owned(frame.payload, frame.seq, frame.time, 0)
                    .map(Message::from_view)
                    .map(Some)
            }
            Some(Record::Json(message)) => {
                let tags = message
                    .meta
                    .tags
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let view = crate::view::MessageView::encode_legacy_value(&message.data, &tags)?
                    .for_feed(message.meta.sender.as_deref())?
                    .with_identity(message.seq, message.time, 0);
                Ok(Some(Message::from_view(view)))
            }
            None => Ok(None),
            Some(Record::Control(_)) => unreachable!("controls are consumed by the stream"),
        }
    }

    pub fn close(&mut self) {
        self.0.reader = None;
        self.0.failure = None;
    }
}

impl Iterator for RemoteRead {
    type Item = ApiResult<Message>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_message().transpose()
    }
}

impl RemoteLite3Read {
    pub fn next_frame(&mut self) -> ApiResult<Option<RemoteLite3Frame>> {
        match self.0.next_record()? {
            Some(Record::Lite3(frame)) => Ok(Some(frame)),
            None => Ok(None),
            Some(_) => unreachable!("binary reader only returns Lite3 frames"),
        }
    }

    pub fn close(&mut self) {
        self.0.reader = None;
        self.0.failure = None;
    }
}

impl Iterator for RemoteLite3Read {
    type Item = ApiResult<RemoteLite3Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
    }
}

struct Stream {
    pool: RemotePool,
    selection: Selection,
    live: bool,
    binary: bool,
    reader: Option<TailReader>,
    incarnation: Option<hyper::header::HeaderValue>,
    resumed: Option<(u64, u64)>,
    last_message: Option<u64>,
    failure: Option<Box<Error>>,
}

enum Record {
    Json(DecodedMessage),
    Lite3(RemoteLite3Frame),
    Control(Ending),
}

enum RecordAction {
    Message(Record),
    Continue,
    Complete,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Ending {
    Complete,
    Error { error: HttpErrorWire },
    Cutoff { next_seq: u64, until: u64 },
}

impl Stream {
    fn open(pool: &RemotePool, selection: Selection, live: bool, binary: bool) -> ApiResult<Self> {
        let mut stream = Self::new(pool, selection, live, binary);
        stream.reopen()?;
        Ok(stream)
    }

    fn new(pool: &RemotePool, selection: Selection, live: bool, binary: bool) -> Self {
        Self {
            pool: pool.clone(),
            selection,
            live,
            binary,
            reader: None,
            incarnation: None,
            resumed: None,
            last_message: None,
            failure: None,
        }
    }

    fn request(&self) -> ApiResult<(Url, Option<crate::api::access::AccessKey>)> {
        let route = if self.live { "follow" } else { "fetch" };
        let mut url = build_url(&self.pool.base_url, &["pools", &self.pool.pool, route])?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some((next, end)) = self.resumed {
                pairs.append_pair("since", &next.to_string());
                pairs.append_pair("until", &end.to_string());
                let incarnation = self
                    .incarnation
                    .as_ref()
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        protocol_error("missing pool identity on stream continuation")
                    })?;
                pairs.append_pair("incarnation", incarnation);
            } else {
                match &self.selection.start {
                    ReadStart::Default => {}
                    ReadStart::Last(last) => {
                        pairs.append_pair("last", &last.to_string());
                    }
                    ReadStart::Since(since) => {
                        let value = match since {
                            Since::Oldest => "oldest".into(),
                            Since::Seq(seq) => seq.to_string(),
                            Since::At(nanos) => format_ts(*nanos)?,
                            Since::Ago(duration) => {
                                format!("{}ns", duration.as_nanos().min(u64::MAX as u128))
                            }
                        };
                        pairs.append_pair("since", &value);
                    }
                }
            }
            for tag in &self.selection.tags {
                pairs.append_pair("tag", tag);
            }
        }
        if url.query() == Some("") {
            url.set_query(None);
        }
        let key = self
            .pool
            .client
            .key_for_request(&url)?
            .map(std::borrow::Cow::into_owned);
        Ok((url, key))
    }

    fn opened(&mut self, incarnation: Option<hyper::header::HeaderValue>) -> ApiResult<()> {
        if self.resumed.is_some() {
            match (&self.incarnation, &incarnation) {
                (Some(original), Some(current)) if original == current => {}
                (Some(_), Some(_)) => {
                    return Err(Error::new(ErrorKind::NotFound)
                        .with_message("pool was replaced during the read"));
                }
                _ => {
                    return Err(protocol_error(
                        "missing pool identity on stream continuation",
                    ));
                }
            }
        } else {
            self.incarnation = incarnation;
        }
        Ok(())
    }

    fn reopen(&mut self) -> ApiResult<()> {
        let (url, key) = self.request()?;
        let accept = if self.binary {
            "application/x-plasmite-lite3-stream"
        } else {
            "application/jsonl"
        };
        if let Some(body) = cancellable_read::open(url, accept, key, self.selection.cancel.clone())?
        {
            self.opened(body.incarnation.clone())?;
            self.reader = Some(BufReader::new(Box::new(body)));
        }
        Ok(())
    }

    fn accept(&mut self, record: Record) -> ApiResult<RecordAction> {
        match record {
            Record::Control(Ending::Complete) if self.live => {
                Err(protocol_error("unexpected completion of live read"))
            }
            Record::Control(Ending::Complete) => Ok(RecordAction::Complete),
            Record::Control(Ending::Error { error }) => Err(error_from_remote(error)),
            Record::Control(Ending::Cutoff { next_seq, until }) => {
                if self.live
                    || next_seq == 0
                    || self.last_message.is_some_and(|last| next_seq <= last)
                    || next_seq > until
                {
                    return Err(protocol_error("invalid stream cutoff"));
                }
                if let Some((previous, end)) = self.resumed
                    && (end != until || next_seq < previous)
                {
                    return Err(protocol_error("stream cutoff failed to advance the read"));
                }
                self.resumed = Some((next_seq, until));
                Ok(RecordAction::Continue)
            }
            Record::Json(ref message) => {
                self.last_message = Some(message.seq);
                Ok(RecordAction::Message(record))
            }
            Record::Lite3(ref frame) => {
                self.last_message = Some(frame.seq);
                Ok(RecordAction::Message(record))
            }
        }
    }

    fn next_record(&mut self) -> ApiResult<Option<Record>> {
        if let Some(error) = &self.failure {
            return Err((**error).clone());
        }
        let result = self.advance();
        if let Err(error) = &result {
            self.reader = None;
            self.failure = Some(Box::new(error.clone()));
        }
        result
    }

    fn advance(&mut self) -> ApiResult<Option<Record>> {
        while self.reader.is_some() {
            if self
                .selection
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
            {
                self.reader = None;
                return Ok(None);
            }
            let record = if self.binary {
                self.binary_record()
            } else {
                self.json_record()
            };
            if self
                .selection
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
            {
                self.reader = None;
                return Ok(None);
            }
            match self.accept(record?)? {
                RecordAction::Complete => {
                    self.reader = None;
                    return Ok(None);
                }
                RecordAction::Continue => {
                    self.reader = None;
                    self.reopen()?;
                }
                RecordAction::Message(record) => return Ok(Some(record)),
            }
        }
        Ok(None)
    }

    fn json_record(&mut self) -> ApiResult<Record> {
        let line = read_jsonl_line(&mut self.reader, MAX_REMOTE_RESPONSE_BYTES)?
            .ok_or_else(|| protocol_error("stream ended without a terminal record"))?;
        let mut value: Value = serde_json::from_str(&line)
            .map_err(|error| protocol_error("invalid JSON stream record").with_source(error))?;
        if value.get("type").is_some() {
            let ending = serde_json::from_value(value)
                .map_err(|error| protocol_error("invalid stream ending").with_source(error))?;
            return Ok(Record::Control(ending));
        }
        let data = value.get_mut("data").map(Value::take);
        let mut wire: MessageWire = serde_json::from_value(value)
            .map_err(|error| protocol_error("invalid message envelope").with_source(error))?;
        if let Some(data) = data {
            wire.data = data;
        }
        message_from_remote(wire).map(Record::Json)
    }

    fn binary_record(&mut self) -> ApiResult<Record> {
        let reader = self.reader.as_mut().expect("stream opened");
        let mut header = [0; LITE3_RECORD_HEADER_LEN];
        if !read_exact_or_eof(reader, &mut header)? {
            return Err(protocol_error("stream ended without a terminal record"));
        }
        let (kind, length) = binary_header(header)?;
        let mut identity = [0; LITE3_MESSAGE_ID_LEN];
        if kind == LITE3_MESSAGE_RECORD {
            reader.read_exact(&mut identity).map_err(|error| {
                protocol_error("incomplete binary message identity").with_source(error)
            })?;
        }
        let mut payload = vec![0; length];
        reader
            .read_exact(&mut payload)
            .map_err(|error| protocol_error("incomplete binary record").with_source(error))?;
        binary_record(kind, identity, payload)
    }
}

fn binary_header(header: [u8; LITE3_RECORD_HEADER_LEN]) -> ApiResult<(u8, usize)> {
    let length = u32::from_be_bytes(header[1..].try_into().expect("record length")) as usize;
    match header[0] {
        LITE3_MESSAGE_RECORD => length
            .checked_sub(LITE3_MESSAGE_ID_LEN)
            .filter(|length| *length <= MAX_REMOTE_RESPONSE_BYTES)
            .map(|length| (LITE3_MESSAGE_RECORD, length))
            .ok_or_else(|| protocol_error("invalid binary message length")),
        LITE3_CONTROL_RECORD if length <= 64 * 1024 => Ok((LITE3_CONTROL_RECORD, length)),
        _ => Err(protocol_error("invalid binary record type or length")),
    }
}

fn binary_record(
    kind: u8,
    identity: [u8; LITE3_MESSAGE_ID_LEN],
    payload: Vec<u8>,
) -> ApiResult<Record> {
    if kind == LITE3_MESSAGE_RECORD {
        let seq = u64::from_be_bytes(identity[..8].try_into().expect("sequence"));
        let time = u64::from_be_bytes(identity[8..].try_into().expect("timestamp"));
        return Ok(Record::Lite3(RemoteLite3Frame { seq, time, payload }));
    }
    serde_json::from_slice(&payload)
        .map(Record::Control)
        .map_err(|error| protocol_error("invalid binary control record").with_source(error))
}

pub(crate) struct AsyncRead {
    stream: Stream,
    body: Option<axum::body::BodyDataStream>,
    chunk: bytes::Bytes,
}

impl AsyncRead {
    pub(crate) async fn open(pool: &RemotePool, read: Selection, live: bool) -> ApiResult<Self> {
        let mut reader = Self {
            stream: Stream::new(pool, read, live, true),
            body: None,
            chunk: bytes::Bytes::new(),
        };
        reader.reopen().await?;
        Ok(reader)
    }

    async fn reopen(&mut self) -> ApiResult<()> {
        let (url, key) = self.stream.request()?;
        let (incarnation, body) = tokio::time::timeout(
            REQUEST_TIMEOUT,
            cancellable_read::connect(url, "application/x-plasmite-lite3-stream", key),
        )
        .await
        .map_err(|error| protocol_error("stream opening timed out").with_source(error))??;
        self.stream.opened(incarnation)?;
        self.body = Some(body.into_data_stream());
        self.chunk = bytes::Bytes::new();
        Ok(())
    }

    async fn read_exact(&mut self, mut output: &mut [u8]) -> ApiResult<()> {
        use bytes::Buf;
        use tokio_stream::StreamExt;
        while !output.is_empty() {
            if self.chunk.is_empty() {
                self.chunk = self
                    .body
                    .as_mut()
                    .expect("opened body")
                    .next()
                    .await
                    .ok_or_else(|| protocol_error("stream ended without a terminal record"))?
                    .map_err(|error| {
                        protocol_error("could not read stream record").with_source(error)
                    })?;
            }
            let count = self.chunk.len().min(output.len());
            let (head, rest) = output.split_at_mut(count);
            self.chunk.copy_to_slice(head);
            output = rest;
        }
        Ok(())
    }

    pub(crate) async fn next(&mut self) -> ApiResult<Option<crate::view::MessageView>> {
        if let Some(error) = &self.stream.failure {
            return Err((**error).clone());
        }
        let result = self.advance().await;
        if let Err(error) = &result {
            self.body = None;
            self.stream.failure = Some(Box::new(error.clone()));
        }
        result
    }

    async fn advance(&mut self) -> ApiResult<Option<crate::view::MessageView>> {
        while self.body.is_some() {
            let mut header = [0; LITE3_RECORD_HEADER_LEN];
            self.read_exact(&mut header).await?;
            let (kind, length) = binary_header(header)?;
            let mut identity = [0; LITE3_MESSAGE_ID_LEN];
            if kind == LITE3_MESSAGE_RECORD {
                self.read_exact(&mut identity).await?;
            }
            let mut payload = vec![0; length];
            self.read_exact(&mut payload).await?;
            match self
                .stream
                .accept(binary_record(kind, identity, payload)?)?
            {
                RecordAction::Complete => self.body = None,
                RecordAction::Continue => {
                    self.body = None;
                    self.reopen().await?;
                }
                RecordAction::Message(Record::Lite3(frame)) => {
                    return crate::view::MessageView::from_owned(
                        frame.payload,
                        frame.seq,
                        frame.time,
                        0,
                    )
                    .map(Some);
                }
                RecordAction::Message(_) => unreachable!("binary stream"),
            }
        }
        Ok(None)
    }
}

fn protocol_error(message: &str) -> Error {
    Error::new(ErrorKind::Io).with_message(message)
}

pub(super) fn request_result_v2(
    response: Result<ureq::Response, ureq::Error>,
) -> ApiResult<ureq::Response> {
    match response {
        Err(ureq::Error::Status(404, response))
            if response.header("plasmite-version") != Some("2") =>
        {
            Err(
                protocol_error("server does not support Plasmite HTTP API v2")
                    .with_hint("Upgrade the server to Plasmite 2.0."),
            )
        }
        response => request_result(response),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn responses<const N: usize, B: AsRef<[u8]> + Send + 'static>(
        scripts: [(&'static str, &'static str, B); N],
    ) -> (RemotePool, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen on loopback");
        listener.set_nonblocking(true).expect("bounded accept");
        let base_url = Url::parse(&format!(
            "http://{}",
            listener.local_addr().expect("address")
        ))
        .expect("loopback URL");
        let client = RemoteClient::new(base_url.to_string()).expect("remote client");
        let pool = RemotePool {
            client,
            base_url,
            pool: "events".into(),
            pool_ref: PoolRef::name("events"),
        };
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            for (path, incarnation, body) in scripts {
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => assert!(
                            std::time::Instant::now() < deadline,
                            "no request for {path}"
                        ),
                        Err(error) => panic!("accept request: {error}"),
                    }
                    std::thread::sleep(Duration::from_millis(5));
                };
                socket
                    .set_nonblocking(false)
                    .expect("blocking request socket");
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .expect("request deadline");
                let mut request = BufReader::new(&mut socket);
                let mut line = String::new();
                request.read_line(&mut line).expect("read request path");
                assert_eq!(line.trim_end(), format!("GET {path} HTTP/1.1"));
                loop {
                    line.clear();
                    let count = request.read_line(&mut line).expect("read request headers");
                    if count == 0 || line == "\r\n" {
                        break;
                    }
                }
                let body = body.as_ref();
                write!(socket, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nPlasmite-Version: 2\r\nPlasmite-Incarnation: {incarnation}\r\n\r\n", body.len())
                    .expect("send response");
                socket.write_all(body).expect("send response body");
            }
        });
        (pool, worker)
    }

    fn open_json(pool: &RemotePool, selection: Selection, live: bool) -> ApiResult<RemoteRead> {
        Stream::open(pool, selection, live, false).map(RemoteRead)
    }

    #[tokio::test]
    async fn async_binary_fetch_keeps_its_opening_range_after_a_cutoff() {
        fn record(kind: u8, body: &[u8]) -> Vec<u8> {
            let mut bytes = vec![kind];
            bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
            bytes.extend_from_slice(body);
            bytes
        }
        let message = crate::view::MessageView::from_json(b"{}", &[]).unwrap();
        let mut body = 2u64.to_be_bytes().to_vec();
        body.extend_from_slice(&123u64.to_be_bytes());
        body.extend_from_slice(message.bytes());
        let mut ending = record(LITE3_MESSAGE_RECORD, &body);
        ending.extend_from_slice(&record(LITE3_CONTROL_RECORD, br#"{"type":"complete"}"#));
        let (pool, server) = responses([
            (
                "/pools/events/fetch",
                "original",
                record(
                    LITE3_CONTROL_RECORD,
                    br#"{"type":"cutoff","next_seq":2,"until":3}"#,
                ),
            ),
            (
                "/pools/events/fetch?since=2&until=3&incarnation=original",
                "original",
                ending,
            ),
        ]);
        let mut reader = AsyncRead::open(&pool, Selection::new(), false)
            .await
            .unwrap();
        let received = reader.next().await.unwrap().unwrap();
        assert_eq!((received.seq, received.time), (2, 123));
        assert_eq!(received.bytes(), message.bytes());
        assert!(reader.next().await.unwrap().is_none());
        server.join().unwrap();
    }

    #[test]
    fn cutoff_continues_from_raw_position_even_without_matching_messages() {
        let (pool, server) = responses([
            (
                "/pools/events/fetch?tag=keep",
                "original",
                "{\"type\":\"cutoff\",\"next_seq\":8,\"until\":10}\n",
            ),
            (
                "/pools/events/fetch?since=8&until=10&incarnation=original&tag=keep",
                "original",
                "{\"type\":\"cutoff\",\"next_seq\":8,\"until\":10}\n",
            ),
            (
                "/pools/events/fetch?since=8&until=10&incarnation=original&tag=keep",
                "original",
                concat!(
                    "{\"seq\":9,\"time\":\"2026-11-17T00:00:00.000000123Z\",\"data\":{},\"meta\":{\"tags\":[\"keep\"]}}\n",
                    "{\"type\":\"complete\"}\n"
                ),
            ),
        ]);
        let messages = open_json(&pool, Selection::new().tags(["keep"]), false)
            .expect("open")
            .collect::<ApiResult<Vec<_>>>()
            .expect("resume finite read");
        assert_eq!(
            messages
                .iter()
                .map(|message| message.seq)
                .collect::<Vec<_>>(),
            [9]
        );
        server.join().expect("mock server exit");
    }

    #[test]
    fn fetch_rejects_a_same_name_replacement_after_cutoff() {
        let (pool, server) = responses([
            (
                "/pools/events/fetch",
                "original",
                "{\"type\":\"cutoff\",\"next_seq\":2,\"until\":3}\n",
            ),
            (
                "/pools/events/fetch?since=2&until=3&incarnation=original",
                "replacement",
                "{\"seq\":2,\"time\":\"2026-11-17T00:00:00Z\",\"data\":{},\"meta\":{\"tags\":[]}}\n",
            ),
        ]);
        let mut reader = open_json(&pool, Selection::new(), false).expect("open");
        let error = reader.next_message().expect_err("pool identity changed");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.message(), Some("pool was replaced during the read"));
        server.join().expect("mock server exit");
    }

    #[test]
    fn follow_rejects_a_cutoff() {
        for (body, detail) in [
            (
                "{\"type\":\"cutoff\",\"next_seq\":2}\n",
                "invalid stream ending",
            ),
            (
                "{\"type\":\"cutoff\",\"next_seq\":2,\"until\":3}\n",
                "invalid stream cutoff",
            ),
        ] {
            let (pool, server) = responses([("/pools/events/follow", "original", body)]);
            let mut reader = open_json(&pool, Selection::new(), true).expect("open");
            let error = reader.next_message().expect_err("invalid live cutoff");
            assert_eq!(error.kind(), ErrorKind::Io);
            assert_eq!(error.message(), Some(detail));
            server.join().expect("mock server exit");
        }
    }

    #[test]
    fn a_404_without_a_version_reports_an_old_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = RemoteClient::new(&origin).unwrap();
        let pool = RemotePool {
            client,
            base_url: Url::parse(&origin).unwrap(),
            pool: "events".into(),
            pool_ref: PoolRef::name("events"),
        };
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = BufReader::new(&mut socket);
            let mut line = String::new();
            while request.read_line(&mut line).unwrap() != 0 && line != "\r\n" {
                line.clear();
            }
            socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let error = pool.fetch(Selection::new()).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::Io);
        assert_eq!(error.hint(), Some("Upgrade the server to Plasmite 2.0."));
        server.join().unwrap();
    }

    #[test]
    fn missing_ending_is_a_terminal_io_error() {
        let (pool, server) = responses([(
            "/pools/events/fetch",
            "original",
            "{\"seq\":1,\"time\":\"2026-11-17T00:00:00Z\",\"data\":{},\"meta\":{\"tags\":[]}}\n",
        )]);
        let mut reader = open_json(&pool, Selection::new(), false).expect("open");
        assert_eq!(
            reader.next_message().expect("read").expect("message").seq,
            1
        );
        assert_eq!(
            reader.next_message().expect_err("incomplete read").kind(),
            ErrorKind::Io
        );
        assert_eq!(reader.next_message().unwrap_err().kind(), ErrorKind::Io);
        server.join().expect("mock server exit");
    }
}
