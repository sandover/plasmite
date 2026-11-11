//! Purpose: Provide best-effort UDP send and follow clients.
//! Exports: `UdpClient`, `UdpPool`, `UdpFollow`, and UDP event types.
//! Role: Keep the UDP transport separate from committed HTTP append semantics.
//! Invariants: Resolve one server endpoint per client; deliver DATA in arrival order.
#![expect(
    clippy::result_large_err,
    reason = "the public API uses the shared Error type like sibling API modules"
)]

use super::{Message, PoolRef};
use crate::core::error::{Error, ErrorKind};
use crate::core::lite3::{encode_message, validate_bytes};
use crate::pool_paths::{PoolNameResolveError, resolve_named_pool_path};
use crate::udp_wire as wire;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::io;
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs, UdpSocket,
};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::{Host, Url};

pub type UdpStart = wire::Start;
pub type UdpSkipReason = wire::Reason;

const CONTROL_RETRY: Duration = Duration::from_secs(1);
const INITIAL_REQUEST_MIN: usize = 256;
const MAX_OPENING_SKIPS: usize = 64;

#[derive(Clone, Debug)]
pub struct UdpClient {
    inner: Arc<UdpClientInner>,
}

#[derive(Debug)]
struct UdpClientInner {
    endpoint: SocketAddr,
    host: String,
    port: u16,
    send_socket: UdpSocket,
}

#[derive(Clone, Debug)]
pub struct UdpPool {
    client: UdpClient,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UdpFollowOptions {
    pub start: UdpStart,
    pub live: bool,
}

impl UdpFollowOptions {
    pub fn new() -> Self {
        Self {
            start: UdpStart::New,
            live: true,
        }
    }
}

impl Default for UdpFollowOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UdpFrame {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub payload: Vec<u8>,
}

impl UdpFrame {
    pub fn message(&self) -> Result<Message, Error> {
        super::message::message_from_udp_payload(self.seq, self.timestamp_ns, &self.payload)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UdpEvent {
    Frame(UdpFrame),
    /// Compatibility name for an explicit server SKIP, never inferred network loss.
    Gap {
        first: u64,
        last: u64,
        reason: UdpSkipReason,
    },
    Progress {
        start_seq: u64,
        next_seq: u64,
        end_seq: Option<u64>,
        terminal: bool,
        retention_gap: bool,
    },
}

pub struct UdpFollow {
    socket: Option<UdpSocket>,
    id: wire::Id,
    selection: wire::Selection,
    pending: VecDeque<UdpEvent>,
    initial_bounds: Option<FollowBounds>,
    last_activity: Instant,
    next_renewal: Instant,
    terminal_seen: bool,
}

struct FollowBounds {
    start_seq: u64,
    end_seq: Option<u64>,
}

impl UdpClient {
    /// Parse and resolve an explicit `udp://HOST:PORT` endpoint once.
    pub fn new(endpoint: impl AsRef<str>) -> Result<Self, Error> {
        let (host, port, address) = parse_endpoint(endpoint.as_ref())?;
        let send_socket = connected_socket(address).map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message("failed to open UDP client socket")
                .with_source(error)
        })?;
        Ok(Self {
            inner: Arc::new(UdpClientInner {
                endpoint: address,
                host,
                port,
                send_socket,
            }),
        })
    }

    /// Split a UDP pool URL into its endpoint and decoded pool name without resolving DNS.
    pub fn parse_pool_url(uri: &str) -> Result<(String, String), Error> {
        let (host, port, name) = parse_pool_uri(uri)?;
        Ok((format!("udp://{host}:{port}"), name))
    }

    /// Validate a pool reference locally. This does not check whether the pool exists.
    pub fn open_pool(&self, pool_ref: &PoolRef) -> Result<UdpPool, Error> {
        let name = match pool_ref {
            PoolRef::Name(name) => name.clone(),
            PoolRef::Path(_) => {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("UDP pool refs must use a pool name, not a filesystem path"));
            }
            PoolRef::Uri(uri) => {
                let (host, port, name) = parse_pool_uri(uri)?;
                if !host.eq_ignore_ascii_case(&self.inner.host) || port != self.inner.port {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("UDP pool URL names a different server")
                        .with_hint("Create a UdpClient for that server's UDP endpoint."));
                }
                name
            }
        };
        validate_pool_name(&name)?;
        Ok(UdpPool {
            client: self.clone(),
            name,
        })
    }
}

impl UdpPool {
    /// Encode and send one message. Success means the operating system accepted the datagram.
    pub fn send_json(&self, data: &Value, tags: &[String]) -> Result<(), Error> {
        let document = encode_message(tags, data)?;
        self.send_lite3(document.as_slice())
    }

    /// Validate and send one complete Lite3 message without changing its bytes.
    pub fn send_lite3(&self, document: &[u8]) -> Result<(), Error> {
        check_datagram_sizes(self.name.as_bytes(), document.len())?;

        // Lite3's native reader needs aligned storage. A caller can pass a slice
        // starting at any byte, so validate a copy before forwarding the exact bytes.
        let aligned = document.to_vec();
        validate_bytes(&aligned)?;

        let mut packet = Vec::with_capacity(self.name.len() + 1 + document.len());
        packet.extend_from_slice(self.name.as_bytes());
        packet.push(0);
        packet.extend_from_slice(document);
        let sent = self
            .client
            .inner
            .send_socket
            .send(&packet)
            .map_err(|error| {
                Error::new(ErrorKind::Io)
                    .with_message("UDP send failed; storage is unknown")
                    .with_source(error)
            })?;
        if sent != packet.len() {
            return Err(Error::new(ErrorKind::Io)
                .with_message("UDP send accepted only part of a datagram; storage is unknown"));
        }
        Ok(())
    }

    /// Subscribe to best-effort DATA in arrival order, including late messages and duplicates.
    pub fn follow(&self, options: UdpFollowOptions) -> Result<UdpFollow, Error> {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|error| {
            Error::new(ErrorKind::Internal)
                .with_message(format!("failed to create UDP subscription id: {error}"))
        })?;
        let socket = connected_socket(self.client.inner.endpoint).map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message("failed to open UDP follow socket")
                .with_source(error)
        })?;
        let selection = wire::Selection {
            pool: self.name.clone(),
            start: options.start,
            live: options.live,
        };
        let now = Instant::now();
        let mut follow = UdpFollow {
            socket: Some(socket),
            id,
            selection,
            pending: VecDeque::new(),
            initial_bounds: None,
            last_activity: now,
            next_renewal: now + Duration::from_millis(wire::RENEW_MS),
            terminal_seen: false,
        };
        follow.open()?;
        Ok(follow)
    }
}

impl UdpFollow {
    /// Wait for one event. Return `None` when a finished or closed follow has no queued events.
    pub fn next_event(&mut self) -> Result<Option<UdpEvent>, Error> {
        self.next_event_until(None, false)
    }

    /// Wait for one event up to `timeout`. `None` can also mean the wait expired;
    /// call `is_finished` to distinguish that case from the end of a finite follow.
    pub fn next_event_timeout(&mut self, timeout: Duration) -> Result<Option<UdpEvent>, Error> {
        let deadline = Instant::now().checked_add(timeout);
        self.next_event_until(deadline, timeout.is_zero())
    }

    /// Return true once terminal progress arrives or the socket closes.
    /// The caller can still read queued events.
    pub fn is_finished(&self) -> bool {
        self.terminal_seen || self.socket.is_none()
    }

    /// Send one best-effort close and release the socket.
    pub fn close(&mut self) {
        if let Some(socket) = self.socket.take() {
            let _ = socket.send(&wire::header(wire::CLOSE, self.id));
        }
    }

    fn open(&mut self) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_millis(wire::STARTUP_MS);
        let mut answered_challenge = false;
        let mut request = self.subscribe_packet(None, Some(self.selection.clone()))?;
        self.send_control(&request)?;
        let mut retry_at = Instant::now() + CONTROL_RETRY;
        let mut buffer = [0u8; 65_536];

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(startup_timeout());
            }
            if now >= retry_at {
                self.send_control(&request)?;
                retry_at = now + CONTROL_RETRY;
            }
            let wait = retry_at
                .min(deadline)
                .saturating_duration_since(Instant::now());
            self.set_read_timeout(wait)?;
            let length = match self.socket_ref()?.recv(&mut buffer) {
                Ok(length) => length,
                Err(error) if is_timeout(&error) || error.kind() == io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(error) => return Err(socket_error("UDP follow receive failed", error)),
            };
            let Some((op, body)) = self.response(&buffer[..length]) else {
                continue;
            };
            match op {
                wire::CHALLENGE if !answered_challenge => {
                    let challenge: wire::Challenge = decode_control(body, "UDP challenge")?;
                    if challenge.cookie.mac.is_empty() || challenge.cookie.expires_ms == 0 {
                        return Err(protocol_error("UDP server sent an invalid challenge"));
                    }
                    request = self
                        .subscribe_packet(Some(challenge.cookie), Some(self.selection.clone()))?;
                    self.send_control(&request)?;
                    answered_challenge = true;
                    retry_at = Instant::now() + CONTROL_RETRY;
                }
                wire::PROGRESS | wire::DATA => {
                    self.accept_response(op, body)?;
                    break;
                }
                _ => self.accept_response(op, body)?,
            }
        }
        self.refresh_liveness();
        self.next_renewal = self.last_activity + Duration::from_millis(wire::RENEW_MS);
        Ok(())
    }

    fn next_event_until(
        &mut self,
        deadline: Option<Instant>,
        mut poll_once: bool,
    ) -> Result<Option<UdpEvent>, Error> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        if self.is_finished() {
            return Ok(None);
        }
        let mut buffer = [0u8; 65_536];

        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            if self.is_finished() {
                return Ok(None);
            }
            let now = Instant::now();
            let liveness_deadline = self.last_activity + Duration::from_millis(wire::LEASE_MS);
            if now >= liveness_deadline {
                return Err(
                    Error::new(ErrorKind::Io).with_message("UDP follow lost server liveness")
                );
            }
            if now >= self.next_renewal {
                self.send_renewal()?;
                self.next_renewal = now + Duration::from_millis(wire::RENEW_MS);
            }

            if !poll_once && deadline.is_some_and(|deadline| now >= deadline) {
                return Ok(None);
            }

            let mut receive_until = self.next_renewal.min(liveness_deadline);
            if let Some(deadline) = deadline {
                receive_until = receive_until.min(deadline);
            }
            let received = if poll_once {
                // A zero timeout must check the socket without blocking the caller.
                let socket = self.socket_ref()?;
                socket
                    .set_nonblocking(true)
                    .map_err(|error| socket_error("UDP poll setup failed", error))?;
                let received = socket.recv(&mut buffer);
                socket
                    .set_nonblocking(false)
                    .map_err(|error| socket_error("UDP poll cleanup failed", error))?;
                poll_once = false;
                received
            } else {
                let wait = receive_until.saturating_duration_since(Instant::now());
                if wait.is_zero() {
                    continue;
                }
                self.set_read_timeout(wait)?;
                self.socket_ref()?.recv(&mut buffer)
            };
            let length = match received {
                Ok(length) => length,
                Err(error) if is_timeout(&error) || error.kind() == io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(error) => return Err(socket_error("UDP follow receive failed", error)),
            };
            let Some((op, body)) = self.response(&buffer[..length]) else {
                continue;
            };

            self.accept_response(op, body)?;
        }
    }

    fn response<'a>(&self, packet: &'a [u8]) -> Option<(u8, &'a [u8])> {
        let (op, id, body) = wire::parse(packet)?;
        (id == self.id).then_some((op, body))
    }

    fn accept_response(&mut self, op: u8, body: &[u8]) -> Result<(), Error> {
        match op {
            wire::PROGRESS => self.accept_progress(decode_control(body, "UDP progress")?)?,
            wire::DATA => self.accept_data(body)?,
            wire::SKIP => {
                // Opening may receive SKIPs before PROGRESS; preserve each exact range.
                if self.pending.len() >= MAX_OPENING_SKIPS {
                    return Err(Error::new(ErrorKind::Busy)
                        .with_message("UDP startup skip queue is full before opening progress")
                        .with_hint("Try following again or use HTTP for selected history."));
                }
                self.accept_skip(decode_control(body, "UDP skip")?)?;
            }
            wire::ERROR => return Err(decode_failure(body)?),
            _ => return Ok(()),
        }
        self.refresh_liveness();
        Ok(())
    }

    fn subscribe_packet(
        &self,
        cookie: Option<wire::Cookie>,
        selection: Option<wire::Selection>,
    ) -> Result<Vec<u8>, Error> {
        let mut packet = wire::control(
            wire::SUBSCRIBE,
            self.id,
            &wire::Subscribe { selection, cookie },
        )
        .ok_or_else(|| Error::new(ErrorKind::Internal).with_message("UDP request is too large"))?;
        if packet.len() < INITIAL_REQUEST_MIN {
            packet.resize(INITIAL_REQUEST_MIN, b' ');
        }
        Ok(packet)
    }

    fn send_control(&self, packet: &[u8]) -> Result<(), Error> {
        let sent = self
            .socket_ref()?
            .send(packet)
            .map_err(|error| socket_error("UDP follow request failed", error))?;
        if sent != packet.len() {
            return Err(Error::new(ErrorKind::Io)
                .with_message("UDP follow request sent only part of a datagram"));
        }
        Ok(())
    }

    fn send_renewal(&self) -> Result<(), Error> {
        let packet = wire::control(
            wire::SUBSCRIBE,
            self.id,
            &wire::Subscribe {
                selection: None,
                cookie: None,
            },
        )
        .ok_or_else(|| Error::new(ErrorKind::Internal).with_message("UDP renewal is too large"))?;
        self.send_control(&packet)
    }

    fn accept_progress(&mut self, progress: wire::Progress) -> Result<(), Error> {
        if progress.start_seq == 0
            || progress.start_seq > progress.next_seq
            || self.selection.live != progress.end_seq.is_none()
            || (progress.terminal && progress.end_seq.is_none())
            || (progress.terminal && progress.end_seq.is_some_and(|end| progress.next_seq <= end))
            || progress.lease_ms == 0
            || progress.lease_ms > wire::LEASE_MS
        {
            return Err(protocol_error("UDP progress sequence range is invalid"));
        }
        if let Some(bounds) = &self.initial_bounds {
            if progress.end_seq != bounds.end_seq {
                return Err(protocol_error("UDP server changed the follow end sequence"));
            }
            if progress.start_seq != bounds.start_seq {
                return Err(protocol_error(
                    "UDP server changed the follow start sequence",
                ));
            }
        } else {
            self.initial_bounds = Some(FollowBounds {
                start_seq: progress.start_seq,
                end_seq: progress.end_seq,
            });
        }
        self.pending.push_back(UdpEvent::Progress {
            start_seq: progress.start_seq,
            next_seq: progress.next_seq,
            end_seq: progress.end_seq,
            terminal: progress.terminal,
            retention_gap: progress.retention_gap,
        });
        if progress.terminal {
            self.terminal_seen = true;
        }
        Ok(())
    }

    fn accept_data(&mut self, body: &[u8]) -> Result<(), Error> {
        if body.len() <= 16 {
            return Err(protocol_error("UDP data packet has no Lite3 payload"));
        }
        let seq = u64::from_be_bytes(body[..8].try_into().expect("fixed sequence field"));
        let timestamp_ns =
            u64::from_be_bytes(body[8..16].try_into().expect("fixed timestamp field"));
        let payload = body[16..].to_vec();
        validate_bytes(&payload).map_err(|error| {
            Error::new(ErrorKind::Corrupt)
                .with_message("UDP data packet contains invalid Lite3")
                .with_source(error)
        })?;

        let since = match &self.selection.start {
            UdpStart::Since(since) => Some(*since),
            _ => None,
        };
        if since.is_none_or(|since| timestamp_ns >= since) {
            self.pending.push_back(UdpEvent::Frame(UdpFrame {
                seq,
                timestamp_ns,
                payload,
            }));
        }
        Ok(())
    }

    fn accept_skip(&mut self, skip: wire::Skip) -> Result<(), Error> {
        if skip.first > skip.last || matches!(skip.reason, UdpSkipReason::Unknown) {
            return Err(protocol_error("UDP skip range or reason is invalid"));
        }
        self.pending.push_back(UdpEvent::Gap {
            first: skip.first,
            last: skip.last,
            reason: skip.reason,
        });
        Ok(())
    }

    fn refresh_liveness(&mut self) {
        self.last_activity = Instant::now();
    }

    fn set_read_timeout(&self, duration: Duration) -> Result<(), Error> {
        let timeout = duration.max(Duration::from_millis(1));
        self.socket_ref()?
            .set_read_timeout(Some(timeout))
            .map_err(|error| socket_error("failed to set UDP receive timeout", error))
    }

    fn socket_ref(&self) -> Result<&UdpSocket, Error> {
        self.socket
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Io).with_message("UDP follow is closed"))
    }
}

impl Drop for UdpFollow {
    fn drop(&mut self) {
        self.close();
    }
}

fn parse_endpoint(endpoint: &str) -> Result<(String, u16, SocketAddr), Error> {
    let url = parse_udp_url(endpoint, false)?;
    if !url.path().is_empty() && url.path() != "/" {
        return Err(
            Error::new(ErrorKind::Usage).with_message("UDP endpoint must not include a pool path")
        );
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP endpoint has no host"))?
        .to_ascii_lowercase();
    let port = explicit_port(&url)?;
    if port == 0 {
        return Err(Error::new(ErrorKind::Usage).with_message("UDP endpoint port must be nonzero"));
    }
    let address = resolve_host(&url, port)?;
    validate_destination(address.ip())?;
    Ok((host, port, address))
}

fn parse_pool_uri(uri: &str) -> Result<(String, u16, String), Error> {
    let url = parse_udp_url(uri, true)?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP pool URL has no host"))?
        .to_ascii_lowercase();
    let port = explicit_port(&url)?;
    if port == 0 {
        return Err(Error::new(ErrorKind::Usage).with_message("UDP URL port must be nonzero"));
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 1 || segments[0].is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("UDP pool URL must contain one pool-name path segment"));
    }
    let name = decode_path_segment(segments[0])?;
    if name.contains(['/', '\\']) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("UDP pool name must not contain path separators"));
    }
    validate_pool_name(&name)?;
    Ok((host, port, name))
}

fn parse_udp_url(input: &str, pool_url: bool) -> Result<Url, Error> {
    let url = Url::parse(input).map_err(|error| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid UDP URL")
            .with_source(error)
    })?;
    if url.scheme() != "udp" {
        return Err(Error::new(ErrorKind::Usage).with_message("UDP URL must use the udp scheme"));
    }
    let has_user_info = input
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .is_some_and(|authority| authority.contains('@'));
    if has_user_info || !url.username().is_empty() || url.password().is_some() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("UDP URL must not contain credentials")
        );
    }
    if url.query().is_some() {
        return Err(Error::new(ErrorKind::Usage).with_message("UDP URL must not contain a query"));
    }
    if url.fragment().is_some() {
        return Err(
            Error::new(ErrorKind::Usage).with_message("UDP URL must not contain a fragment")
        );
    }
    if !pool_url && !matches!(url.path(), "" | "/") {
        return Err(
            Error::new(ErrorKind::Usage).with_message("UDP endpoint must not include a pool path")
        );
    }
    Ok(url)
}

fn explicit_port(url: &Url) -> Result<u16, Error> {
    url.port()
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP URL needs an explicit port"))
}

fn resolve_host(url: &Url, port: u16) -> Result<SocketAddr, Error> {
    let host = url
        .host()
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("UDP endpoint has no host"))?;
    let addresses: Vec<SocketAddr> = match host {
        Host::Ipv4(address) => vec![SocketAddr::V4(SocketAddrV4::new(address, port))],
        Host::Ipv6(address) => vec![SocketAddr::V6(SocketAddrV6::new(address, port, 0, 0))],
        Host::Domain(domain) => (domain, port)
            .to_socket_addrs()
            .map_err(|error| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to resolve UDP endpoint host")
                    .with_source(error)
            })?
            .collect(),
    };
    addresses
        .into_iter()
        .filter(|address| validate_destination(address.ip()).is_ok())
        // Match the server's IPv4 default when a hostname names both families.
        .min_by_key(SocketAddr::is_ipv6)
        .ok_or_else(|| {
            Error::new(ErrorKind::Usage)
                .with_message("UDP endpoint must resolve to a unicast address")
        })
}

fn validate_destination(address: IpAddr) -> Result<(), Error> {
    let rejected = match address {
        IpAddr::V4(address) => {
            address == Ipv4Addr::BROADCAST || address.is_multicast() || address.is_unspecified()
        }
        IpAddr::V6(address) => address.is_multicast() || address.is_unspecified(),
    };
    if rejected {
        Err(Error::new(ErrorKind::Usage).with_message("UDP endpoint must be a unicast address"))
    } else {
        Ok(())
    }
}

fn validate_pool_name(name: &str) -> Result<(), Error> {
    // A URL must route to the same name on Unix and Windows hosts.
    if name.contains('\\') || name.as_bytes().get(1) == Some(&b':') {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("UDP pool name must not contain a path prefix or separator"));
    }
    resolve_named_pool_path(name, Path::new("."))
        .map(|_| ())
        .map_err(|error| match error {
            PoolNameResolveError::InvalidName => Error::new(ErrorKind::Usage).with_message(
                "pool name must be nonempty and must not contain NUL or a path prefix",
            ),
            PoolNameResolveError::ContainsPathSeparator => Error::new(ErrorKind::Usage)
                .with_message("pool name must not contain path separators"),
        })
}

fn decode_path_segment(segment: &str) -> Result<String, Error> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("invalid percent escape in UDP pool name"));
            }
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("invalid percent escape in UDP pool name"));
            };
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|error| {
        Error::new(ErrorKind::Usage)
            .with_message("UDP pool name must be valid UTF-8")
            .with_source(error)
    })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn check_datagram_sizes(pool: &[u8], payload_len: usize) -> Result<(), Error> {
    let follow_size = wire::DATA_HEADER.saturating_add(payload_len);
    if follow_size > wire::MAX_DATAGRAM {
        return Err(Error::new(ErrorKind::Usage).with_message(format!(
            "encoded UDP follow datagram is {follow_size} bytes; maximum is {}",
            wire::MAX_DATAGRAM
        )));
    }
    let append_size = pool.len().saturating_add(1).saturating_add(payload_len);
    if append_size > wire::MAX_DATAGRAM {
        return Err(Error::new(ErrorKind::Usage).with_message(format!(
            "encoded UDP append datagram is {append_size} bytes; maximum is {}",
            wire::MAX_DATAGRAM
        )));
    }
    Ok(())
}

fn connected_socket(endpoint: SocketAddr) -> io::Result<UdpSocket> {
    let local = match endpoint {
        SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
    };
    let socket = UdpSocket::bind(local)?;
    // macOS's default send buffer rejects datagrams larger than 9216 bytes.
    // Both directions must fit one complete protocol-maximum datagram.
    let settings = socket2::SockRef::from(&socket);
    settings.set_send_buffer_size(128 * 1024)?;
    settings.set_recv_buffer_size(128 * 1024)?;
    socket.connect(endpoint)?;
    Ok(socket)
}

fn decode_control<T: for<'de> Deserialize<'de>>(body: &[u8], name: &str) -> Result<T, Error> {
    wire::decode(body).ok_or_else(|| {
        Error::new(ErrorKind::Corrupt).with_message(format!("UDP server sent invalid {name}"))
    })
}

fn decode_failure(body: &[u8]) -> Result<Error, Error> {
    let failure: wire::Failure = decode_control(body, "error")?;
    let kind = match failure.kind.as_str() {
        "Internal" => ErrorKind::Internal,
        "Usage" => ErrorKind::Usage,
        "NotFound" => ErrorKind::NotFound,
        "AlreadyExists" => ErrorKind::AlreadyExists,
        "Busy" => ErrorKind::Busy,
        "Permission" => ErrorKind::Permission,
        "Corrupt" => ErrorKind::Corrupt,
        "Io" => ErrorKind::Io,
        "RetentionGap" => ErrorKind::RetentionGap,
        _ => return Err(protocol_error("UDP server sent an unknown error kind")),
    };
    Ok(Error::new(kind).with_message(failure.message))
}

fn startup_timeout() -> Error {
    Error::new(ErrorKind::Io)
        .with_message("UDP follow did not start before the startup deadline")
        .with_hint("The server may support UDP input only; enable UDP follow on the server.")
}

fn protocol_error(message: &str) -> Error {
    Error::new(ErrorKind::Corrupt).with_message(message)
}

fn socket_error(message: &str, error: io::Error) -> Error {
    Error::new(ErrorKind::Io)
        .with_message(message)
        .with_source(error)
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}
