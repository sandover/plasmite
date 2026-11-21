//! Route UDP appends and subscriptions over the shared pool directory.

use crate::pool_wait::{PoolChange, PoolWaiters};
use crate::udp_wire as wire;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use plasmite::api::internal::LocalPoolTransport;
use plasmite::api::{Cursor, CursorResult, FrameRef, Pool, PoolPathGuard};
use plasmite::api::{Error, ErrorKind, Feed, LocalClient, PoolRef};
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
        let mut pool = client.open_pool(&PoolRef::name(name), false)?;
        pool.feed_lite3_receipt(&packet, Feed::default())?;
    } else {
        let input: JsonInput = serde_json::from_slice(body).map_err(|_| invalid())?;
        if !input.data.is_object() {
            return Err(invalid());
        }
        let mut pool = client.open_pool(&PoolRef::name(name), false)?;
        pool.feed(&input.data, Feed::default().tags(input.tags))?;
    }
    Ok(())
}

type Key = (SocketAddr, wire::Id);
struct Subscription {
    selection: wire::Selection,
    lease: watch::Sender<Lease>,
}

#[derive(Clone, Copy)]
struct Lease {
    expiry: Option<Instant>,
    progress_requests: u64,
}

impl Lease {
    fn active(self) -> bool {
        self.expiry.is_some_and(|expiry| Instant::now() < expiry)
    }
}

impl Subscription {
    fn new(selection: wire::Selection) -> Self {
        let (lease, _) = watch::channel(Lease {
            expiry: Some(Self::new_expiry()),
            progress_requests: 0,
        });
        Self { selection, lease }
    }

    fn new_expiry() -> Instant {
        Instant::now() + Duration::from_millis(wire::LEASE_MS)
    }

    fn expiry(&self) -> Option<Instant> {
        self.lease.borrow().expiry
    }

    fn active(&self) -> bool {
        self.lease.borrow().active()
    }

    fn cancel(&self) {
        self.lease.send_modify(|lease| lease.expiry = None);
    }

    fn request_progress(&self) {
        // A repeat of the original subscription requests progress without renewing.
        self.lease.send_modify(|lease| lease.progress_requests += 1);
    }

    fn renew(&self) {
        self.lease.send_modify(|lease| {
            lease.expiry = Some(Self::new_expiry());
            lease.progress_requests += 1;
        });
    }
}

struct Completed {
    sub: Arc<Subscription>,
    pool: PoolPathGuard,
    ending: Result<wire::Progress, Error>,
}

impl Completed {
    fn reply(&self, socket: &std::net::UdpSocket, key: Key) -> Result<(), Error> {
        self.pool.check_current_path()?;
        match &self.ending {
            Ok(progress) => reply(socket, key, wire::PROGRESS, progress),
            Err(error) => failure(socket, key, error),
        }
        Ok(())
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
            missing: error.missing(),
            next_seq: error.next_seq(),
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
        wire::Start::New if !sub.selection.live => Ok((oldest, upper, cursor, None)),
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
            if *seq > after {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("since is after the next unwritten message"));
            }
            if *seq != 0 && *seq < oldest {
                return Err(Error::retention_gap(*seq, oldest));
            }
            Ok((if *seq == 0 { oldest } else { *seq }, upper, cursor, None))
        }
        wire::Start::Since(_) | wire::Start::Ago(_) => {
            let timestamp = match start {
                wire::Start::Since(time) => *time,
                wire::Start::Ago(nanos) => crate::cli::support::now_ns()?.saturating_sub(*nanos),
                _ => unreachable!(),
            };
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
                        if frame.time >= timestamp {
                            return Ok((frame.seq, upper, cursor, Some(frame)));
                        }
                    }
                    // A concurrent append can pass the fixed selection bound.
                    // Keep that frame for live delivery instead of consuming it.
                    CursorResult::Message(frame) => return Ok((after, upper, cursor, Some(frame))),
                    CursorResult::FellBehind => {
                        let current = pool.header_from_mmap()?;
                        cursor.seek_to(current.tail_off as usize);
                    }
                    _ => return Ok((after, upper, cursor, None)),
                }
            }
        }
    }
}
async fn follow_worker(
    socket: Arc<std::net::UdpSocket>,
    client: LocalClient,
    key: Key,
    sub: Arc<Subscription>,
    waiters: PoolWaiters,
    lease: watch::Receiver<Lease>,
) -> (Key, Option<Completed>) {
    let completion = match follow_pool(socket.clone(), client, key, &sub, &waiters, lease).await {
        Ok(completion) => completion,
        Err(err) => {
            if sub.active() {
                failure(&socket, key, &err);
            }
            None
        }
    };
    (key, completion)
}

fn completed_worker(
    socket: Arc<std::net::UdpSocket>,
    key: Key,
    completed: Completed,
) -> (Key, Option<Completed>) {
    let completion = if completed.sub.active() {
        match completed.reply(&socket, key) {
            Ok(()) => Some(completed),
            Err(err) => {
                failure(&socket, key, &err);
                None
            }
        }
    } else {
        None
    };
    (key, completion)
}

async fn follow_pool(
    socket: Arc<std::net::UdpSocket>,
    client: LocalClient,
    key: Key,
    sub: &Arc<Subscription>,
    waiters: &PoolWaiters,
    lease: watch::Receiver<Lease>,
) -> Result<Option<Completed>, Error> {
    let opening_sub = sub.clone();
    let registry = waiters.clone();
    let (follow, change) = waiters
        .read(move || {
            let pool = client.open_pool(&PoolRef::name(&opening_sub.selection.pool), false)?;
            let (start_seq, upper, cursor, pending) =
                select_start(&pool, &opening_sub.selection.start, &opening_sub)?;
            let change = opening_sub
                .selection
                .live
                .then(|| registry.subscribe(&pool))
                .transpose()?;
            let progress = wire::Progress {
                start_seq,
                next_seq: start_seq,
                end_seq: (!opening_sub.selection.live).then_some(upper),
                lease_ms: wire::LEASE_MS,
                terminal: false,
            };
            Ok((
                Follow {
                    pool,
                    cursor,
                    pending,
                    progress,
                },
                change,
            ))
        })
        .await?;
    follow_selected(socket, key, sub, follow, change, waiters, lease).await
}

struct Follow {
    pool: Pool,
    cursor: Cursor,
    pending: Option<FrameRef>,
    progress: wire::Progress,
}

enum Turn {
    Ready(Follow),
    Waiting(Follow),
    Finished(Completed),
    Closed,
}

impl Follow {
    fn turn(
        mut self,
        socket: &std::net::UdpSocket,
        key: Key,
        sub: Arc<Subscription>,
        progress_requested: bool,
    ) -> Result<Turn, Error> {
        if !sub.active() {
            return Ok(Turn::Closed);
        }
        self.pool.check_current_path()?;
        if progress_requested {
            reply(socket, key, wire::PROGRESS, &self.progress);
        }
        // Limit examined records, including older records skipped during selection.
        for _ in 0..32 {
            if !sub.active() {
                return Ok(Turn::Closed);
            }
            if self
                .progress
                .end_seq
                .is_some_and(|end| self.progress.next_seq > end)
            {
                self.progress.terminal = true;
                let progress = self.progress.clone();
                return self.finish(socket, key, sub, Ok(progress));
            }
            let next = match self.pending.take() {
                Some(frame) => CursorResult::Message(frame),
                None => self.cursor.next(&self.pool)?,
            };
            let next_retained = match next {
                CursorResult::Message(frame) if frame.seq < self.progress.next_seq => continue,
                CursorResult::Message(frame) if frame.seq > self.progress.next_seq => {
                    Some(frame.seq)
                }
                CursorResult::Message(frame) => {
                    self.pool.check_current_path()?;
                    send_frame(socket, key, &frame)?;
                    self.progress.next_seq = frame.seq.saturating_add(1);
                    None
                }
                CursorResult::FellBehind => self.pool.bounds()?.oldest_seq,
                CursorResult::WouldBlock => return Ok(Turn::Waiting(self)),
            };
            if let Some(oldest) = next_retained.filter(|seq| *seq > self.progress.next_seq) {
                let gap = Error::retention_gap(self.progress.next_seq, oldest);
                return self.finish(socket, key, sub, Err(gap));
            }
        }
        Ok(Turn::Ready(self))
    }

    fn finish(
        self,
        socket: &std::net::UdpSocket,
        key: Key,
        sub: Arc<Subscription>,
        ending: Result<wire::Progress, Error>,
    ) -> Result<Turn, Error> {
        let completed = Completed {
            sub,
            pool: self.pool.into_path_guard(),
            ending,
        };
        completed.reply(socket, key)?;
        Ok(Turn::Finished(completed))
    }
}

fn send_frame(socket: &std::net::UdpSocket, key: Key, frame: &FrameRef) -> Result<(), Error> {
    let reason = match wire::data(key.1, frame.seq, frame.time, &frame.payload) {
        Some(packet) => match socket.send_to(&packet, key.0) {
            Ok(_) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => wire::Reason::Pressure,
            Err(err) => {
                return Err(Error::new(ErrorKind::Io)
                    .with_message("UDP data send failed")
                    .with_source(err));
            }
        },
        None => wire::Reason::Oversized,
    };
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
    Ok(())
}

async fn follow_selected(
    socket: Arc<std::net::UdpSocket>,
    key: Key,
    sub: &Arc<Subscription>,
    mut follow: Follow,
    mut change: Option<PoolChange>,
    waiters: &PoolWaiters,
    mut lease: watch::Receiver<Lease>,
) -> Result<Option<Completed>, Error> {
    let mut last_progress_request = None;
    loop {
        let lease_state = *lease.borrow_and_update();
        let Some(expiry) = lease_state.expiry.filter(|expiry| Instant::now() < *expiry) else {
            return Ok(None);
        };
        let progress_requested = last_progress_request != Some(lease_state.progress_requests);
        last_progress_request = Some(lease_state.progress_requests);
        if let Some(change) = &mut change {
            change.acknowledge();
        }
        let task_socket = socket.clone();
        let task_sub = sub.clone();
        match waiters
            .read(move || follow.turn(&task_socket, key, task_sub, progress_requested))
            .await?
        {
            Turn::Ready(reader) => {
                follow = reader;
                tokio::task::yield_now().await;
            }
            Turn::Waiting(reader) => {
                follow = reader;
                let Some(change) = &mut change else {
                    // A finite read has no later writes to wait for.
                    return Err(Error::new(ErrorKind::Io).with_message("UDP fetch ended early"));
                };
                tokio::select! {
                    _ = lease.changed() => {},
                    _ = tokio::time::sleep_until(expiry.into()) => {},
                    _ = change.changed() => {},
                }
            }
            Turn::Finished(completed) => return Ok(Some(completed)),
            Turn::Closed => return Ok(None),
        }
    }
}

pub(crate) async fn run(
    socket: UdpSocket,
    client: LocalClient,
    shutdown: watch::Receiver<bool>,
    follow_enabled: bool,
    capacity: usize,
    waiters: PoolWaiters,
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
    let mut completed: HashMap<Key, Completed> = HashMap::new();
    let mut workers: tokio::task::JoinSet<(Key, Option<Completed>)> = tokio::task::JoinSet::new();
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
        let next_expiry = completed
            .values()
            .filter_map(|done| done.sub.expiry())
            .min()
            .unwrap_or_else(Instant::now);
        let packet = tokio::select! { biased;
            _ = &mut stop => break Ok(()),
            outcome = workers.join_next(), if !workers.is_empty() => {
                match outcome { Some(Ok((key, completion))) => {
                    subscriptions.remove(&key);
                    if let Some(done) = completion.filter(|done| done.sub.active()) {
                        completed.insert(key, done);
                    }
                },
                    Some(Err(err)) => break Err(Error::new(ErrorKind::Internal).with_message("UDP follow worker failed").with_source(err)),
                    None => {} } continue;
            }
            _ = tokio::time::sleep_until(next_expiry.into()), if !completed.is_empty() => {
                completed.retain(|_, done| done.sub.active());
                continue;
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
            completed.remove(&key);
            if let Some(sub) = subscriptions.get(&key) {
                sub.cancel();
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
                sub.renew();
            } else if let Some(done) = completed.remove(&key).filter(|done| done.sub.active()) {
                done.sub.renew();
                subscriptions.insert(key, done.sub.clone());
                let socket = outgoing.clone();
                workers.spawn_blocking(move || completed_worker(socket, key, done));
            }
            continue;
        };
        if crate::pool_paths::resolve_named_pool_path(&selection.pool, client.pool_dir()?).is_err()
        {
            continue;
        }
        let Some(cookie) = request.cookie else {
            if length >= 256
                && let Some(cookie) = control.cookie(key, &selection)
            {
                let body = wire::Challenge { cookie };
                if let Some(response) = wire::control(wire::CHALLENGE, id, &body)
                    .filter(|packet| packet.len() <= length)
                {
                    let _ = outgoing.send_to(&response, source);
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
                sub.request_progress();
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
        if let Some(done) = completed.get(&key) {
            if done.sub.selection != selection || !done.sub.active() {
                failure(
                    &outgoing,
                    key,
                    &Error::new(ErrorKind::Usage)
                        .with_message("UDP subscription selection changed or expired"),
                );
                continue;
            }
            let done = completed.remove(&key).expect("completed subscription");
            subscriptions.insert(key, done.sub.clone());
            let socket = outgoing.clone();
            workers.spawn_blocking(move || completed_worker(socket, key, done));
            continue;
        }
        if subscriptions.len() + completed.len() >= capacity {
            failure(
                &outgoing,
                key,
                &Error::new(ErrorKind::Busy).with_message("UDP follower capacity reached"),
            );
            continue;
        }
        let sub = Arc::new(Subscription::new(selection));
        let lease = sub.lease.subscribe();
        subscriptions.insert(key, sub.clone());
        workers.spawn(follow_worker(
            outgoing.clone(),
            client.clone(),
            key,
            sub,
            waiters.clone(),
            lease,
        ));
    };
    for sub in subscriptions.values() {
        sub.cancel();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_cursor_sends_a_replayable_terminal_gap() {
        let directory = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(directory.path());
        let name = PoolRef::name("events");
        let mut pool = client
            .create_pool(&name, plasmite::api::PoolOptions::new(16 * 1024))
            .unwrap();
        pool.feed(&serde_json::json!({"seed": true}), Feed::new())
            .unwrap();
        let sub = Arc::new(Subscription::new(wire::Selection {
            pool: "events".into(),
            start: wire::Start::New,
            live: true,
        }));
        let (start, _, cursor, pending) = select_start(&pool, &wire::Start::New, &sub).unwrap();
        let padding = serde_json::json!({"padding": "x".repeat(1536)});
        let oldest = (0..64)
            .find_map(|_| {
                pool.feed(&padding, Feed::new()).unwrap();
                let oldest = pool.bounds().unwrap().oldest_seq.unwrap();
                (oldest > start
                    && matches!(cursor.clone().next(&pool), Ok(CursorResult::FellBehind)))
                .then_some(oldest)
            })
            .expect("overwrite the selected cursor offset");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let key = (receiver.local_addr().unwrap(), [1; 16]);
        let progress = wire::Progress {
            start_seq: start,
            next_seq: start,
            end_seq: None,
            lease_ms: wire::LEASE_MS,
            terminal: false,
        };
        let waiters = PoolWaiters::default();
        let change = waiters.subscribe(&pool).unwrap();
        let sender = Arc::new(sender);
        let completed = follow_selected(
            sender.clone(),
            key,
            &sub,
            Follow {
                pool,
                cursor,
                pending,
                progress,
            },
            Some(change),
            &waiters,
            sub.lease.subscribe(),
        )
        .await
        .unwrap()
        .expect("cache the terminal error for renewal");
        let mut packet = [0; 1024];
        let count = receiver.recv(&mut packet).unwrap();
        assert_eq!(wire::parse(&packet[..count]).unwrap().0, wire::PROGRESS);
        receiver.recv(&mut packet).unwrap(); // Simulate losing the first error datagram.
        completed.reply(&sender, key).unwrap();
        let count = receiver.recv(&mut packet).unwrap();
        let (op, _, body) = wire::parse(&packet[..count]).unwrap();
        assert_eq!(op, wire::ERROR);
        let error: wire::Failure = serde_json::from_slice(body).unwrap();
        assert_eq!(error.kind, "RetentionGap");
        assert_eq!(error.missing, Some(start));
        assert_eq!(error.next_seq, Some(oldest));
    }

    #[tokio::test]
    async fn finite_follow_releases_worker_and_mapping_before_lease_ends() {
        let directory = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(directory.path());
        client
            .create_pool(
                &PoolRef::name("events"),
                plasmite::api::PoolOptions::new(1024 * 1024),
            )
            .unwrap();
        let sender = Arc::new(std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let key = (receiver.local_addr().unwrap(), [1; 16]);
        let sub = Arc::new(Subscription::new(wire::Selection {
            pool: "events".into(),
            start: wire::Start::Last(1),
            live: false,
        }));
        let lease = sub.lease.subscribe();
        let (_, completion) = tokio::time::timeout(
            Duration::from_secs(2),
            follow_worker(sender, client, key, sub, PoolWaiters::default(), lease),
        )
        .await
        .expect("finite worker must return before lease expiry");
        let completion = completion.expect("completion record");
        assert!(completion.ending.unwrap().terminal);
        completion.pool.check_current_path().unwrap();
    }
    #[test]
    fn idle_udp_followers_share_waiting_and_keep_storage_workers_available() {
        // One blocking thread runs the synchronous client; the other must
        // remain available to open readers, append, and serve every follower.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .unwrap()
            .block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let client = LocalClient::new().with_pool_dir(directory.path());
                let pool = PoolRef::name("events");
                client
                    .create_pool(&pool, plasmite::api::PoolOptions::new(1024 * 1024))
                    .unwrap();
                let socket = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
                let address = socket.local_addr().unwrap();
                let (shutdown, receiver) = watch::channel(false);
                let server = tokio::spawn(run(
                    socket,
                    client,
                    receiver,
                    true,
                    64,
                    PoolWaiters::default(),
                ));
                let result = tokio::task::spawn_blocking(move || {
                    let udp = plasmite::api::UdpClient::new(format!("udp://{address}"))
                        .unwrap()
                        .open_pool(&pool)
                        .unwrap();
                    let read = || plasmite::api::Read::new().tags(["sample"]);
                    let mut followers: Vec<_> =
                        (0..48).map(|_| udp.follow(read()).unwrap()).collect();
                    udp.feed(
                        &serde_json::json!({"sample": 42}),
                        Feed::new().tags(["sample"]),
                    )
                    .unwrap();
                    let deadline = Instant::now() + Duration::from_secs(3);
                    for follower in &mut followers {
                        loop {
                            assert!(Instant::now() < deadline, "UDP delivery timed out");
                            if let Some(plasmite::api::UdpEvent::Frame(frame)) = follower
                                .next_event_timeout(Duration::from_millis(50))
                                .unwrap()
                            {
                                assert_eq!(frame.seq, 1);
                                break;
                            }
                        }
                    }
                })
                .await;
                shutdown.send_replace(true);
                server.await.unwrap().unwrap();
                result.unwrap();
            });
    }
}
