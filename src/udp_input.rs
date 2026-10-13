//! Append one routed JSON or Lite3 message per UDP datagram.

use crate::udp_wire as wire;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use plasmite::api::{Cursor, CursorResult, FrameRef, Pool};
use plasmite::api::{Durability, Error, ErrorKind, LocalClient, PoolApiExt, PoolRef};
use ring::hmac;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::watch;

pub(crate) const MAX_DATAGRAM_BYTES: usize = 65_507;

pub(crate) async fn bind(address: SocketAddr) -> Result<UdpSocket, Error> {
    let socket = UdpSocket::bind(address).await.map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to bind UDP input listener")
            .with_source(err)
    })?;
    // Both directions must fit one complete protocol-maximum datagram.
    let settings = socket2::SockRef::from(&socket);
    settings
        .set_send_buffer_size(128 * 1024)
        .and_then(|()| settings.set_recv_buffer_size(128 * 1024))
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to configure UDP socket buffers")
                .with_source(err)
        })?;
    Ok(socket)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonInput {
    data: Value,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Counters {
    received: u64,
    appended: u64,
    oversized: u64,
    errors: u64,
}

impl Counters {
    fn report(self) {
        tracing::info!(
            received = self.received,
            appended = self.appended,
            oversized = self.oversized,
            errors = self.errors,
            "UDP input summary"
        );
    }
}

fn append(client: &LocalClient, mut packet: Vec<u8>) -> Result<(), Error> {
    let invalid = || Error::new(ErrorKind::Usage).with_message("invalid UDP message");
    let separator = packet
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?;
    let name = std::str::from_utf8(&packet[..separator])
        .map_err(|_| invalid())?
        .to_owned();
    let body = &packet[separator + 1..];
    let first = *body.first().ok_or_else(invalid)?;
    // Lite3 object documents begin with their object-root type byte.
    if first == 0x06 {
        // Lite3 readers require aligned storage. A variable-length routing prefix
        // can misalign the body, so move its unchanged bytes to the allocation start.
        let body_len = body.len();
        packet.copy_within(separator + 1.., 0);
        packet.truncate(body_len);
        let mut pool = client.open_pool(&PoolRef::name(name))?;
        pool.append_lite3_now(&packet, Durability::Fast)?;
    } else {
        let input: JsonInput = serde_json::from_slice(body).map_err(|_| invalid())?;
        if !input.data.is_object() {
            return Err(invalid());
        }
        let mut pool = client.open_pool(&PoolRef::name(name))?;
        pool.append_json_now(&input.data, &input.tags, Durability::Fast)?;
    }
    Ok(())
}

type Key = (SocketAddr, wire::Id);
struct Subscription {
    selection: wire::Selection,
    cancelled: AtomicBool,
    expiry: Mutex<Instant>,
    renewal_requested: AtomicBool,
}
impl Subscription {
    fn active(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire)
            && self
                .expiry
                .lock()
                .is_ok_and(|expiry| Instant::now() < *expiry)
    }
}

struct Control {
    secret: hmac::Key,
    boot: Instant,
    window: Instant,
    challenges: usize,
}
impl Control {
    fn new() -> Result<Self, Error> {
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message(format!("UDP cookie entropy failed: {err}"))
        })?;
        Ok(Self {
            secret: hmac::Key::new(hmac::HMAC_SHA256, &secret),
            boot: Instant::now(),
            window: Instant::now(),
            challenges: 0,
        })
    }
    fn signed_bytes(key: Key, selection: &wire::Selection, expires: u64) -> Vec<u8> {
        let mut data = key.0.to_string().into_bytes();
        data.push(0);
        data.extend_from_slice(&key.1);
        data.extend_from_slice(&expires.to_be_bytes());
        // Serializing a typed selection makes padding and JSON key order irrelevant.
        data.extend_from_slice(&serde_json::to_vec(selection).expect("typed UDP selection"));
        data
    }
    fn cookie(&mut self, key: Key, selection: &wire::Selection) -> Option<wire::Cookie> {
        if self.window.elapsed() >= Duration::from_secs(1) {
            self.window = Instant::now();
            self.challenges = 0;
        }
        if self.challenges >= 128 {
            return None;
        }
        self.challenges += 1;
        let expires_ms = self.boot.elapsed().as_millis() as u64 + 10_000;
        let mac = hmac::sign(
            &self.secret,
            &Self::signed_bytes(key, selection, expires_ms),
        );
        Some(wire::Cookie {
            expires_ms,
            mac: URL_SAFE_NO_PAD.encode(mac.as_ref()),
        })
    }
    fn verify(&self, key: Key, selection: &wire::Selection, cookie: &wire::Cookie) -> bool {
        let now = self.boot.elapsed().as_millis() as u64;
        if cookie.expires_ms < now || cookie.expires_ms > now.saturating_add(10_000) {
            return false;
        }
        let Ok(mac) = URL_SAFE_NO_PAD.decode(&cookie.mac) else {
            return false;
        };
        hmac::verify(
            &self.secret,
            &Self::signed_bytes(key, selection, cookie.expires_ms),
            &mac,
        )
        .is_ok()
    }
}
fn reply(socket: &std::net::UdpSocket, key: Key, op: u8, body: &impl serde::Serialize) {
    if let Some(packet) = wire::control(op, key.1, body) {
        let _ = socket.send_to(&packet, key.0);
    }
}
fn failure(socket: &std::net::UdpSocket, key: Key, error: &Error) {
    reply(
        socket,
        key,
        wire::ERROR,
        &wire::Failure {
            kind: format!("{:?}", error.kind()),
            message: error.message().unwrap_or("UDP stream failed").to_string(),
        },
    );
}
fn select_start(
    pool: &Pool,
    start: &wire::Start,
    sub: &Subscription,
) -> Result<(u64, u64, Cursor, Option<FrameRef>), Error> {
    let header = pool.header_from_mmap()?;
    let upper = header.newest_seq;
    let oldest = header.oldest_seq.max(1);
    let after = upper
        .checked_add(1)
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("pool sequence exhausted"))?;
    let mut cursor = Cursor::new();
    match start {
        wire::Start::New => {
            cursor.seek_to(header.head_off as usize);
            Ok((after, upper, cursor, None))
        }
        wire::Start::Last(n) => {
            if *n == 0 {
                cursor.seek_to(header.head_off as usize);
            }
            Ok((after.saturating_sub(*n).max(oldest), upper, cursor, None))
        }
        wire::Start::Sequence(seq) => {
            if *seq == 0 {
                return Err(Error::new(ErrorKind::Usage).with_message("sequence must be positive"));
            }
            Ok((*seq, upper, cursor, None))
        }
        wire::Start::Since(timestamp) => {
            let deadline = Instant::now() + Duration::from_millis(wire::STARTUP_MS);
            loop {
                if !sub.active() || Instant::now() >= deadline {
                    return Err(
                        Error::new(ErrorKind::Io).with_message("UDP history selection timed out")
                    );
                }
                pool.check_current_path()?;
                match cursor.next(pool)? {
                    CursorResult::Message(frame) if frame.seq <= upper => {
                        if frame.timestamp_ns >= *timestamp {
                            return Ok((frame.seq, upper, cursor, Some(frame)));
                        }
                    }
                    // A concurrent append can pass the fixed selection bound.
                    // Keep that frame for live delivery instead of consuming it.
                    CursorResult::Message(frame) => return Ok((after, upper, cursor, Some(frame))),
                    CursorResult::FellBehind => {
                        return Err(Error::new(ErrorKind::RetentionGap)
                            .with_message("selected UDP history is no longer retained"));
                    }
                    _ => return Ok((after, upper, cursor, None)),
                }
            }
        }
    }
}
fn follow_worker(
    socket: Arc<std::net::UdpSocket>,
    client: LocalClient,
    key: Key,
    sub: Arc<Subscription>,
) -> Key {
    let result = follow_pool(&socket, &client, key, &sub);
    if let Err(err) = result {
        if sub.active() {
            failure(&socket, key, &err);
        }
    }
    key
}
fn follow_pool(
    socket: &std::net::UdpSocket,
    client: &LocalClient,
    key: Key,
    sub: &Subscription,
) -> Result<(), Error> {
    let pool = client.open_pool(&PoolRef::name(&sub.selection.pool))?;
    let (start_seq, upper, mut cursor, mut pending) =
        select_start(&pool, &sub.selection.start, sub)?;
    let mut progress = wire::Progress {
        start_seq,
        next_seq: start_seq,
        end_seq: (!sub.selection.live).then_some(upper),
        lease_ms: wire::LEASE_MS,
        terminal: false,
        retention_gap: false,
    };
    let path = crate::pool_paths::resolve_named_pool_path(&sub.selection.pool, client.pool_dir())
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("invalid UDP pool name"))?;
    let mut notify = plasmite::api::notify::open_for_path(&path);
    pool.check_current_path()?;
    reply(socket, key, wire::PROGRESS, &progress);
    let mut batch = 0;
    while sub.active() {
        pool.check_current_path()?;
        if sub.renewal_requested.swap(false, Ordering::AcqRel) {
            reply(socket, key, wire::PROGRESS, &progress);
        }
        if !sub.selection.live && progress.next_seq > upper {
            progress.terminal = true;
            reply(socket, key, wire::PROGRESS, &progress);
            // Retain only terminal metadata; the opened pool remains identity-checked until close/expiry.
            while sub.active() {
                pool.check_current_path()?;
                if sub.renewal_requested.swap(false, Ordering::AcqRel) {
                    reply(socket, key, wire::PROGRESS, &progress);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            return Ok(());
        }
        let next = match pending.take() {
            Some(frame) => CursorResult::Message(frame),
            None => cursor.next(&pool)?,
        };
        match next {
            CursorResult::Message(frame) => {
                if frame.seq < progress.next_seq {
                    continue;
                }
                if frame.seq > progress.next_seq {
                    progress.retention_gap = true;
                    reply(
                        socket,
                        key,
                        wire::SKIP,
                        &wire::Skip {
                            first: progress.next_seq,
                            last: if sub.selection.live {
                                frame.seq - 1
                            } else {
                                (frame.seq - 1).min(upper)
                            },
                            reason: wire::Reason::Retention,
                        },
                    );
                    if !sub.selection.live {
                        return Err(Error::new(ErrorKind::RetentionGap)
                            .with_message("selected UDP history is no longer retained"));
                    }
                }
                let reason = match wire::data(key.1, frame.seq, frame.timestamp_ns, &frame.payload)
                {
                    Some(packet) => match socket.send_to(&packet, key.0) {
                        Ok(_) => None,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            Some(wire::Reason::Pressure)
                        }
                        Err(err) => {
                            return Err(Error::new(ErrorKind::Io)
                                .with_message("UDP data send failed")
                                .with_source(err));
                        }
                    },
                    None => Some(wire::Reason::Oversized),
                };
                if let Some(reason) = reason {
                    reply(
                        socket,
                        key,
                        wire::SKIP,
                        &wire::Skip {
                            first: frame.seq,
                            last: frame.seq,
                            reason,
                        },
                    );
                }
                progress.next_seq = frame.seq.saturating_add(1);
                batch += 1;
                if batch == 32 {
                    std::thread::yield_now();
                    batch = 0;
                }
            }
            CursorResult::FellBehind => {
                progress.retention_gap = true;
                if !sub.selection.live {
                    return Err(Error::new(ErrorKind::RetentionGap)
                        .with_message("selected UDP history is no longer retained"));
                }
            }
            CursorResult::WouldBlock => {
                if let Some(notify) = notify.as_mut() {
                    if notify.wait(Duration::from_millis(50))
                        == plasmite::api::notify::NotifyWait::Unavailable
                    {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn run(
    socket: UdpSocket,
    client: LocalClient,
    shutdown: watch::Receiver<bool>,
    follow_enabled: bool,
    capacity: usize,
) -> Result<(), Error> {
    let std_socket = socket
        .into_std()
        .map_err(|err| Error::new(ErrorKind::Io).with_source(err))?;
    let outgoing = Arc::new(
        std_socket
            .try_clone()
            .map_err(|err| Error::new(ErrorKind::Io).with_source(err))?,
    );
    let socket = UdpSocket::from_std(std_socket)
        .map_err(|err| Error::new(ErrorKind::Io).with_source(err))?;
    let mut control = Control::new()?;
    let mut subscriptions: HashMap<Key, Arc<Subscription>> = HashMap::new();
    let mut workers = tokio::task::JoinSet::new();
    let counts = Arc::new(Mutex::new(Counters::default()));
    let (append_tx, mut append_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let writer_stop = Arc::new(AtomicBool::new(false));
    let append_stop = writer_stop.clone();
    let append_client = client.clone();
    let append_counts = counts.clone();
    let append_shutdown = shutdown.clone();
    let writer = tokio::spawn(async move {
        let stop = crate::serve::wait_for_shutdown(append_shutdown);
        tokio::pin!(stop);
        loop {
            if append_stop.load(Ordering::Acquire) {
                break;
            }
            let packet = tokio::select! { biased; _ = &mut stop => break,
            packet = append_rx.recv() => match packet { Some(packet) => packet, None => break } };
            let client = append_client.clone();
            let outcome = tokio::task::spawn_blocking(move || append(&client, packet)).await;
            let mut counts = append_counts.lock().expect("UDP counters");
            match outcome {
                Ok(Ok(())) => counts.appended += 1,
                _ => counts.errors += 1,
            }
        }
    });
    let mut buffer = [0; 65_536];
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let stop = crate::serve::wait_for_shutdown(shutdown);
    tokio::pin!(stop);
    let result = loop {
        let packet = tokio::select! { biased;
            _ = &mut stop => break Ok(()),
            completed = workers.join_next(), if !workers.is_empty() => {
                match completed { Some(Ok(key)) => { subscriptions.remove(&key); },
                    Some(Err(err)) => break Err(Error::new(ErrorKind::Internal).with_message("UDP follow worker failed").with_source(err)),
                    None => {} } continue;
            }
            _ = interval.tick() => { counts.lock().expect("UDP counters").report(); continue; }
            received = socket.recv_from(&mut buffer) => match received {
                Ok(packet) => packet,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => break Err(Error::new(ErrorKind::Io).with_message("UDP receive failed").with_source(err)),
            }
        };
        let (length, source) = packet;
        {
            let mut counts = counts.lock().expect("UDP counters");
            counts.received += 1;
            if length > MAX_DATAGRAM_BYTES {
                counts.oversized += 1;
                continue;
            }
        }
        let packet = &buffer[..length];
        if packet.first() != Some(&0) {
            if append_tx.try_send(packet.to_vec()).is_err() {
                counts.lock().expect("UDP counters").errors += 1;
            }
            continue;
        }
        let Some((op, id, body)) = wire::parse(packet) else {
            counts.lock().expect("UDP counters").errors += 1;
            continue;
        };
        let key = (source, id);
        if op == wire::CLOSE && body.is_empty() {
            if let Some(sub) = subscriptions.get(&key) {
                sub.cancelled.store(true, Ordering::Release);
            }
            continue;
        }
        if op != wire::SUBSCRIBE {
            continue;
        }
        let Some(request) = wire::decode::<wire::Subscribe>(body) else {
            continue;
        };
        let Some(selection) = request.selection else {
            if let Some(sub) = subscriptions.get(&key).filter(|sub| sub.active()) {
                *sub.expiry.lock().expect("UDP lease") =
                    Instant::now() + Duration::from_millis(wire::LEASE_MS);
                sub.renewal_requested.store(true, Ordering::Release);
            }
            continue;
        };
        if crate::pool_paths::resolve_named_pool_path(&selection.pool, client.pool_dir()).is_err() {
            continue;
        }
        let Some(cookie) = request.cookie else {
            if length >= 256 {
                if let Some(cookie) = control.cookie(key, &selection) {
                    let body = wire::Challenge { cookie };
                    if let Some(response) = wire::control(wire::CHALLENGE, id, &body)
                        .filter(|packet| packet.len() <= length)
                    {
                        let _ = outgoing.send_to(&response, source);
                    }
                }
            }
            continue;
        };
        if !control.verify(key, &selection, &cookie) {
            continue;
        }
        if !follow_enabled {
            failure(
                &outgoing,
                key,
                &Error::new(ErrorKind::Permission)
                    .with_message("UDP follow is disabled; enable --udp-follow"),
            );
            continue;
        }
        if let Some(sub) = subscriptions.get(&key) {
            if sub.selection == selection && sub.active() {
                sub.renewal_requested.store(true, Ordering::Release);
            } else {
                failure(
                    &outgoing,
                    key,
                    &Error::new(ErrorKind::Usage)
                        .with_message("UDP subscription selection changed or expired"),
                );
            }
            continue;
        }
        if subscriptions.len() >= capacity {
            failure(
                &outgoing,
                key,
                &Error::new(ErrorKind::Busy).with_message("UDP follower capacity reached"),
            );
            continue;
        }
        let sub = Arc::new(Subscription {
            selection,
            cancelled: AtomicBool::new(false),
            expiry: Mutex::new(Instant::now() + Duration::from_millis(wire::LEASE_MS)),
            renewal_requested: AtomicBool::new(false),
        });
        subscriptions.insert(key, sub.clone());
        let socket = outgoing.clone();
        let client = client.clone();
        workers.spawn_blocking(move || follow_worker(socket, client, key, sub));
    };
    for sub in subscriptions.values() {
        sub.cancelled.store(true, Ordering::Release);
    }
    writer_stop.store(true, Ordering::Release);
    drop(append_tx);
    // Let the active append finish; no follower retains its pool beyond this join.
    writer
        .await
        .map_err(|err| Error::new(ErrorKind::Internal).with_source(err))?;
    while let Some(outcome) = workers.join_next().await {
        outcome.map_err(|err| Error::new(ErrorKind::Internal).with_source(err))?;
    }
    counts.lock().expect("UDP counters").report();
    result
}
