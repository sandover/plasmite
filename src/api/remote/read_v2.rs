//! Open and continue HTTP readers using explicit terminal records.
use super::*;
use crate::api::message::{Read as Selection, ReadStart, Since, format_ts};
use crate::interface_wire::{
    LITE3_CONTROL_RECORD, LITE3_MESSAGE_ID_LEN, LITE3_MESSAGE_RECORD, LITE3_RECORD_HEADER_LEN,
};
use std::sync::atomic::Ordering;

pub struct RemoteRead(Stream);
pub struct RemoteLite3Read(Stream);

impl RemotePool {
    pub fn follow(&self, read: Selection) -> ApiResult<RemoteRead> {
        Stream::open(self, read, true, false).map(RemoteRead)
    }

    pub fn fetch(&self, read: Selection) -> ApiResult<RemoteRead> {
        Stream::open(self, read, false, false).map(RemoteRead)
    }

    pub fn follow_lite3(&self, read: Selection) -> ApiResult<RemoteLite3Read> {
        Stream::open(self, read, true, true).map(RemoteLite3Read)
    }

    pub fn fetch_lite3(&self, read: Selection) -> ApiResult<RemoteLite3Read> {
        Stream::open(self, read, false, true).map(RemoteLite3Read)
    }

    pub fn feed<'a>(
        &self,
        input: impl Into<crate::api::FeedInput<'a>>,
        options: crate::api::Feed,
    ) -> ApiResult<Message> {
        let (data, tags, durability) = options.resolve(input.into())?;
        let url = build_url(&self.base_url, &["v2", "pools", &self.pool, "feed"])?;
        let request = AppendRequest {
            data,
            tags,
            durability: durability_to_str(durability),
        };
        let envelope: MessageEnvelope = self.client.request_json("POST", &url, &request)?;
        message_from_remote(envelope.message)
    }

    pub fn feed_lite3(&self, bytes: &[u8], options: crate::api::Feed) -> ApiResult<Message> {
        let envelope: MessageEnvelope = read_json_response(self.request_lite3_feed(
            bytes,
            options.encoded_durability()?,
            "application/json",
        )?)?;
        message_from_remote(envelope.message)
    }

    pub fn feed_lite3_receipt(
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

impl RemoteRead {
    pub fn next_message(&mut self) -> ApiResult<Option<Message>> {
        match self.0.next_record()? {
            Some(Record::Json(message)) => Ok(Some(message)),
            None => Ok(None),
            Some(_) => unreachable!("JSON reader only returns JSON messages"),
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
    resumed: Option<(u64, Option<u64>)>,
    last_message: Option<u64>,
    failure: Option<Box<Error>>,
}

enum Record {
    Json(Message),
    Lite3(RemoteLite3Frame),
    Control(Ending),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Ending {
    Complete,
    Error { error: HttpErrorWire },
    Cutoff { next_seq: u64, until: Option<u64> },
}

impl Stream {
    fn open(pool: &RemotePool, selection: Selection, live: bool, binary: bool) -> ApiResult<Self> {
        let mut stream = Self {
            pool: pool.clone(),
            selection,
            live,
            binary,
            reader: None,
            incarnation: None,
            resumed: None,
            last_message: None,
            failure: None,
        };
        stream.reopen()?;
        Ok(stream)
    }

    fn reopen(&mut self) -> ApiResult<()> {
        let route = if self.live { "follow" } else { "fetch" };
        let mut url = build_url(
            &self.pool.base_url,
            &["v2", "pools", &self.pool.pool, route],
        )?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some((next, end)) = self.resumed {
                pairs.append_pair("since", &next.to_string());
                if let Some(end) = end {
                    pairs.append_pair("until", &end.to_string());
                }
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
        let accept = if self.binary {
            "application/x-plasmite-lite3-stream"
        } else {
            "application/jsonl"
        };
        let key = self
            .pool
            .client
            .key_for_request(&url)?
            .map(std::borrow::Cow::into_owned);
        if let Some(body) = cancellable_read::open(url, accept, key, self.selection.cancel.clone())?
        {
            if self.resumed.is_some() {
                match (&self.incarnation, &body.incarnation) {
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
                self.incarnation = body.incarnation.clone();
            }
            self.reader = Some(BufReader::new(Box::new(body)));
        }
        Ok(())
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
            let record = record?;
            match record {
                Record::Control(Ending::Complete) => {
                    if self.live {
                        return Err(protocol_error("unexpected completion of live read"));
                    }
                    self.reader = None;
                    return Ok(None);
                }
                Record::Control(Ending::Error { error }) => return Err(error_from_remote(error)),
                Record::Control(Ending::Cutoff { next_seq, until }) => {
                    if next_seq == 0
                        || self.last_message.is_some_and(|last| next_seq <= last)
                        || self.live == until.is_some()
                        || until.is_some_and(|end| next_seq > end)
                    {
                        return Err(protocol_error("invalid stream cutoff"));
                    }
                    if let Some((previous, end)) = self.resumed
                        && (end != until || next_seq < previous)
                    {
                        return Err(protocol_error("stream cutoff failed to advance the read"));
                    }
                    self.reader = None;
                    self.resumed = Some((next_seq, until));
                    self.reopen()?;
                }
                Record::Json(ref message) => {
                    self.last_message = Some(message.seq);
                    return Ok(Some(record));
                }
                Record::Lite3(ref frame) => {
                    self.last_message = Some(frame.seq);
                    return Ok(Some(record));
                }
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
        let length = u32::from_be_bytes(header[1..].try_into().expect("record length")) as usize;
        match header[0] {
            LITE3_MESSAGE_RECORD => {
                let length = length
                    .checked_sub(LITE3_MESSAGE_ID_LEN)
                    .filter(|length| *length <= MAX_REMOTE_RESPONSE_BYTES)
                    .ok_or_else(|| protocol_error("invalid binary message length"))?;
                let mut identity = [0; LITE3_MESSAGE_ID_LEN];
                reader.read_exact(&mut identity).map_err(|error| {
                    protocol_error("incomplete binary message identity").with_source(error)
                })?;
                let seq = u64::from_be_bytes(identity[..8].try_into().expect("sequence"));
                let time = u64::from_be_bytes(identity[8..].try_into().expect("timestamp"));
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload).map_err(|error| {
                    protocol_error("incomplete binary message payload").with_source(error)
                })?;
                Ok(Record::Lite3(RemoteLite3Frame { seq, time, payload }))
            }
            LITE3_CONTROL_RECORD if length <= 64 * 1024 => {
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).map_err(|error| {
                    protocol_error("incomplete binary control record").with_source(error)
                })?;
                let ending = serde_json::from_slice(&bytes).map_err(|error| {
                    protocol_error("invalid binary control record").with_source(error)
                })?;
                Ok(Record::Control(ending))
            }
            _ => Err(protocol_error("invalid binary record type or length")),
        }
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

    fn responses<const N: usize>(
        scripts: [(&'static str, &'static str, &'static str); N],
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
                write!(socket, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nPlasmite-Version: 2\r\nPlasmite-Incarnation: {incarnation}\r\n\r\n{body}", body.len())
                    .expect("send response");
            }
        });
        (pool, worker)
    }

    #[test]
    fn cutoff_continues_from_raw_position_even_without_matching_messages() {
        let (pool, server) = responses([
            (
                "/v2/pools/events/fetch?tag=keep",
                "original",
                "{\"type\":\"cutoff\",\"next_seq\":8,\"until\":10}\n",
            ),
            (
                "/v2/pools/events/fetch?since=8&until=10&tag=keep",
                "original",
                "{\"type\":\"cutoff\",\"next_seq\":8,\"until\":10}\n",
            ),
            (
                "/v2/pools/events/fetch?since=8&until=10&tag=keep",
                "original",
                concat!(
                    "{\"seq\":9,\"time\":\"2026-11-17T00:00:00.000000123Z\",\"data\":{},\"meta\":{\"tags\":[\"keep\"]}}\n",
                    "{\"type\":\"complete\"}\n"
                ),
            ),
        ]);
        let messages = pool
            .fetch(Selection::new().tags(["keep"]))
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
                "/v2/pools/events/fetch",
                "original",
                "{\"type\":\"cutoff\",\"next_seq\":2,\"until\":3}\n",
            ),
            (
                "/v2/pools/events/fetch?since=2&until=3",
                "replacement",
                "{\"seq\":2,\"time\":\"2026-11-17T00:00:00Z\",\"data\":{},\"meta\":{\"tags\":[]}}\n",
            ),
        ]);
        let mut reader = pool.fetch(Selection::new()).expect("open");
        let error = reader.next_message().expect_err("pool identity changed");
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.message(), Some("pool was replaced during the read"));
        server.join().expect("mock server exit");
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
            "/v2/pools/events/fetch",
            "original",
            "{\"seq\":1,\"time\":\"2026-11-17T00:00:00Z\",\"data\":{},\"meta\":{\"tags\":[]}}\n",
        )]);
        let mut reader = pool.fetch(Selection::new()).expect("open");
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
