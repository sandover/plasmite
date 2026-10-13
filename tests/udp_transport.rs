//! Full UDP transport over actual server processes and deterministic packet loss.
use plasmite::api::{
    AppendOptions, Durability, ErrorKind, LocalClient, PoolApiExt, PoolOptions, PoolRef, UdpClient,
    UdpEvent, UdpFollow, UdpFollowOptions, UdpStart,
};
use serde_json::json;
use std::{
    fs,
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[path = "../src/udp_wire.rs"]
#[expect(
    dead_code,
    reason = "Network tests use a subset of the shared wire codec."
)]
mod wire;
type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Server {
    child: Child,
    _temp: tempfile::TempDir,
    pools: PathBuf,
    address: SocketAddr,
}
impl Server {
    fn start(follow: bool, capacity: usize) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_on(follow, capacity, SocketAddr::from(([127, 0, 0, 1], 0)))
    }
    fn start_on(
        follow: bool,
        capacity: usize,
        udp_bind: SocketAddr,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let pools = temp.path().join("pools");
        let home = temp.path().join("home");
        fs::create_dir_all(&pools)?;
        fs::create_dir_all(&home)?;
        let client = LocalClient::new().with_pool_dir(&pools);
        client.create_pool(&PoolRef::name("events"), PoolOptions::new(4 * 1024 * 1024))?;
        let held = UdpSocket::bind(udp_bind)?;
        let address = held.local_addr()?;
        drop(held);
        let ready = temp.path().join("ready");
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .env("HOME", &home)
            .env("PLASMITE_ACCESS_HOME", &home)
            .env("PLASMITE_SERVE_READY_FILE", &ready)
            .arg("--dir")
            .arg(&pools)
            .args([
                "serve",
                "--bind",
                "127.0.0.1:0",
                "--remote-bind",
                "127.0.0.1:0",
            ])
            .arg(format!("--udp={address}"))
            .arg("--max-tail-concurrency")
            .arg(capacity.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if follow {
            command.arg("--udp-follow");
        }
        let child = command.spawn()?;
        let mut server = Self {
            child,
            _temp: temp,
            pools,
            address,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                server.child.try_wait()?.is_none(),
                "server exited before readiness"
            );
            assert!(Instant::now() < deadline, "server startup timed out");
            thread::sleep(Duration::from_millis(10));
        }
        Ok(server)
    }
    fn local(&self) -> LocalClient {
        LocalClient::new().with_pool_dir(&self.pools)
    }
    fn udp(&self) -> Result<plasmite::api::UdpPool, Box<dyn std::error::Error>> {
        Ok(UdpClient::new(format!("udp://{}", self.address))?
            .open_pool(&PoolRef::name("events"))?)
    }
    fn append(&self, value: serde_json::Value, time: u64) -> TestResult {
        self.local()
            .open_pool(&PoolRef::name("events"))?
            .append_json(&value, &[], AppendOptions::new(time, Durability::Fast))?;
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn frame(stream: &mut UdpFollow) -> Result<plasmite::api::UdpFrame, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(UdpEvent::Frame(frame)) =
            stream.next_event_timeout(Duration::from_millis(50))?
        {
            return Ok(frame);
        }
        assert!(Instant::now() < deadline, "frame did not arrive");
    }
}
#[test]
fn send_live_follow_retains_original_attachment_and_real_identity() -> TestResult {
    let server = Server::start(true, 64)?;
    let udp = server.udp()?;
    let mut stream = udp.follow(UdpFollowOptions::new())?;
    let payload = plasmite::api::lite3::encode_message(&["test".into()], &json!({"sample":42}))?;
    udp.send_lite3(payload.as_slice())?;
    let received = frame(&mut stream)?;
    assert_eq!(received.seq, 1);
    assert!(received.timestamp_ns > 0);
    assert_eq!(received.payload, payload.as_slice());
    assert_eq!(received.message()?.data, json!({"sample":42}));
    let stored = server.local().open_pool(&PoolRef::name("events"))?.get(1)?;
    assert_eq!(received.timestamp_ns, stored.timestamp_ns);
    stream.close();
    Ok(())
}
#[test]
fn localhost_uses_ipv4_and_explicit_ipv6_supports_udp_feed_and_follow() -> TestResult {
    let ipv4 = Server::start(true, 64)?;
    let local = UdpClient::new(format!("udp://localhost:{}", ipv4.address.port()))?
        .open_pool(&PoolRef::name("events"))?;
    let mut stream = local.follow(UdpFollowOptions::new())?;
    let ipv4_value = json!({"family":"ipv4"});
    local.send_json(&ipv4_value, &[])?;
    assert_eq!(frame(&mut stream)?.message()?.data, ipv4_value);

    let bind = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 0));
    let ipv6 = match Server::start_on(true, 64, bind) {
        Ok(server) => server,
        Err(err)
            if err.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
                )
            }) =>
        {
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    let endpoint = format!("udp://[::1]:{}", ipv6.address.port());
    let local = UdpClient::new(endpoint)?.open_pool(&PoolRef::name("events"))?;
    let mut stream = local.follow(UdpFollowOptions::new())?;
    let ipv6_value = json!({"family":"ipv6"});
    local.send_json(&ipv6_value, &[])?;
    assert_eq!(frame(&mut stream)?.message()?.data, ipv6_value);
    Ok(())
}
#[test]
fn maximum_sized_lite3_message_is_stored_and_followed_byte_for_byte() -> TestResult {
    let server = Server::start(true, 64)?;
    let udp = server.udp()?;
    let max_payload = wire::MAX_DATAGRAM - wire::DATA_HEADER;
    let base = plasmite::api::lite3::encode_message(&[], &json!({"blob":""}))?;
    let guess = max_payload - base.len();
    let mut at_limit = None;
    let mut one_over = None;
    for length in guess.saturating_sub(8)..=guess + 8 {
        let payload =
            plasmite::api::lite3::encode_message(&[], &json!({"blob":"x".repeat(length)}))?;
        match payload.len() {
            size if size == max_payload => at_limit = Some(payload),
            size if size == max_payload + 1 => one_over = Some(payload),
            _ => {}
        }
    }
    let payload = at_limit.expect("Lite3 string length should reach the exact UDP payload limit");
    let oversized =
        one_over.expect("Lite3 string length should reach one byte over the UDP payload limit");
    assert_eq!(wire::DATA_HEADER + payload.len(), wire::MAX_DATAGRAM);
    assert_eq!(
        udp.send_lite3(oversized.as_slice()).unwrap_err().kind(),
        ErrorKind::Usage
    );

    let mut stream = udp.follow(UdpFollowOptions::new())?;
    udp.send_lite3(payload.as_slice())?;
    let received = frame(&mut stream)?;
    assert_eq!(received.seq, 1);
    assert_eq!(
        wire::DATA_HEADER + received.payload.len(),
        wire::MAX_DATAGRAM
    );
    assert_eq!(received.payload, payload.as_slice());
    let stored = server.local().open_pool(&PoolRef::name("events"))?.get(1)?;
    assert_eq!(stored.payload, payload.as_slice());
    Ok(())
}
#[test]
fn pool_created_after_server_start_accepts_udp_append_and_follow() -> TestResult {
    let server = Server::start(true, 64)?;
    let local = server.local();
    local.create_pool(&PoolRef::name("later"), PoolOptions::new(1024 * 1024))?;
    let udp =
        UdpClient::new(format!("udp://{}", server.address))?.open_pool(&PoolRef::name("later"))?;
    let mut stream = udp.follow(UdpFollowOptions::new())?;
    let value = json!({"created_after_start":true});
    udp.send_json(&value, &[])?;
    let received = frame(&mut stream)?;
    assert_eq!(received.seq, 1);
    assert_eq!(received.message()?.data, value);
    let stored = local.open_pool(&PoolRef::name("later"))?.get(1)?;
    assert_eq!(received.payload, stored.payload);
    Ok(())
}
#[test]
fn retention_overrun_fails_finite_follow_and_reports_live_gap() -> TestResult {
    let server = Server::start(true, 64)?;
    let local = server.local();
    let pool_ref = PoolRef::name("retained");
    local.create_pool(&pool_ref, PoolOptions::new(16 * 1024))?;
    let mut native = local.open_pool(&pool_ref)?;
    let mut oldest = 1;
    let mut newest = 0;
    for seq in 1..=256 {
        native.append_json(
            &json!({"seq":seq,"text":"x".repeat(128)}),
            &[],
            AppendOptions::new(seq, Durability::Fast),
        )?;
        let bounds = native.info()?.bounds;
        oldest = bounds.oldest_seq.unwrap_or(1);
        newest = bounds.newest_seq.unwrap_or(0);
        if oldest > 1 {
            break;
        }
    }
    assert!(oldest > 1, "native appends should evict the first message");
    assert!(newest >= oldest);

    let udp = UdpClient::new(format!("udp://{}", server.address))?.open_pool(&pool_ref)?;
    let mut finite = udp.follow(UdpFollowOptions {
        start: UdpStart::Sequence(1),
        live: false,
    })?;
    let mut finite_gap = false;
    let mut finite_error = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match finite.next_event_timeout(Duration::from_millis(100)) {
            Ok(Some(UdpEvent::Gap {
                first,
                last,
                reason: plasmite::api::UdpSkipReason::Retention,
            })) => {
                assert_eq!((first, last), (1, oldest - 1));
                finite_gap = true;
            }
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(err) => {
                assert_eq!(err.kind(), ErrorKind::RetentionGap);
                finite_error = true;
                break;
            }
        }
    }
    assert!(
        finite_error,
        "finite history follow should terminate with RetentionGap"
    );
    assert!(
        finite_gap,
        "finite history follow should report the evicted range"
    );

    let mut live = udp.follow(UdpFollowOptions {
        start: UdpStart::Sequence(1),
        live: true,
    })?;
    let mut live_gap = false;
    let mut frames = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && frames.last().copied() != Some(newest) {
        match live.next_event_timeout(Duration::from_millis(100))? {
            Some(UdpEvent::Gap {
                first,
                last,
                reason: plasmite::api::UdpSkipReason::Retention,
            }) => {
                assert_eq!((first, last), (1, oldest - 1));
                live_gap = true;
            }
            Some(UdpEvent::Frame(frame)) => frames.push(frame.seq),
            _ => {}
        }
    }
    assert!(live_gap, "live follow should report the evicted range");
    assert_eq!(frames, (oldest..=newest).collect::<Vec<_>>());
    Ok(())
}
#[test]
fn input_only_server_refuses_reads_and_verified_capacity_is_bounded() -> TestResult {
    let input = Server::start(false, 1)?;
    assert_eq!(
        input
            .udp()?
            .follow(UdpFollowOptions::new())
            .err()
            .unwrap()
            .kind(),
        ErrorKind::Permission
    );
    let server = Server::start(true, 1)?;
    let mut first = server.udp()?.follow(UdpFollowOptions::new())?;
    assert_eq!(
        server
            .udp()?
            .follow(UdpFollowOptions::new())
            .err()
            .unwrap()
            .kind(),
        ErrorKind::Busy
    );
    first.close();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match server.udp()?.follow(UdpFollowOptions::new()) {
            Ok(mut stream) => {
                stream.close();
                break;
            }
            Err(err) if err.kind() == ErrorKind::Busy && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50))
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}
#[test]
fn since_handles_nonmonotone_timestamps_and_oversized_stored_messages() -> TestResult {
    let server = Server::start(true, 64)?;
    server.append(json!({"sample":1}), 200)?;
    server.append(json!({"sample":2}), 10)?;
    server.append(json!({"sample":3}), 300)?;
    let mut stream = server.udp()?.follow(UdpFollowOptions {
        start: UdpStart::Since(100),
        live: false,
    })?;
    let mut sequences = Vec::new();
    while let Some(event) = stream.next_event()? {
        if let UdpEvent::Frame(frame) = event {
            sequences.push(frame.seq);
        }
    }
    assert_eq!(sequences, vec![1, 3]);
    server.append(json!({"text":"x".repeat(80_000)}), 400)?;
    let mut stream = server.udp()?.follow(UdpFollowOptions {
        start: UdpStart::Last(1),
        live: false,
    })?;
    let mut oversized = false;
    while let Some(event) = stream.next_event()? {
        if let UdpEvent::Gap {
            first: 4,
            last: 4,
            reason: plasmite::api::UdpSkipReason::Oversized,
        } = event
        {
            oversized = true;
        }
    }
    assert!(oversized);
    Ok(())
}
#[test]
fn replacing_a_pool_ends_the_old_stream_instead_of_resetting_sequence() -> TestResult {
    let server = Server::start(true, 64)?;
    let udp = server.udp()?;
    let mut stream = udp.follow(UdpFollowOptions::new())?;
    server.append(json!({"old":true}), 100)?;
    assert_eq!(frame(&mut stream)?.seq, 1);
    let old = server.pools.join("events.plasmite");
    fs::rename(&old, server.pools.join("old.plasmite"))?;
    server
        .local()
        .create_pool(&PoolRef::name("events"), PoolOptions::new(1024 * 1024))?;
    server.append(json!({"replacement":true}), 200)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match stream.next_event_timeout(Duration::from_millis(100)) {
            Err(err) => {
                assert_eq!(err.kind(), ErrorKind::NotFound);
                break;
            }
            Ok(Some(UdpEvent::Frame(_))) => panic!("old stream received replacement data"),
            _ => assert!(Instant::now() < deadline, "old stream did not terminate"),
        }
    }
    Ok(())
}
#[test]
fn challenge_requires_return_proof_before_reading_and_binds_the_endpoint() -> TestResult {
    let server = Server::start(true, 64)?;
    server.append(json!({"private":42}), 100)?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let id = [7; 16];
    let selection = wire::Selection {
        pool: "events".into(),
        start: wire::Start::Last(1),
        live: true,
    };
    let mut request = wire::control(
        wire::SUBSCRIBE,
        id,
        &wire::Subscribe {
            selection: Some(selection.clone()),
            cookie: None,
        },
    )
    .unwrap();
    request.resize(256, b' ');
    socket.send_to(&request, server.address)?;
    let mut buffer = [0; 65536];
    let (len, _) = socket.recv_from(&mut buffer)?;
    let (op, _, body) = wire::parse(&buffer[..len]).unwrap();
    assert_eq!(op, wire::CHALLENGE);
    assert!(len <= request.len());
    let challenge: wire::Challenge = wire::decode(body).unwrap();
    assert!(
        socket.recv_from(&mut buffer).is_err(),
        "challenge must not start DATA"
    );
    let other = UdpSocket::bind("127.0.0.1:0")?;
    other.set_read_timeout(Some(Duration::from_millis(200)))?;
    let request = wire::control(
        wire::SUBSCRIBE,
        id,
        &wire::Subscribe {
            selection: Some(selection),
            cookie: Some(challenge.cookie),
        },
    )
    .unwrap();
    other.send_to(&request, server.address)?;
    assert!(
        other.recv_from(&mut buffer).is_err(),
        "cookie must bind source endpoint"
    );
    Ok(())
}
fn raw_socket() -> Result<UdpSocket, Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    Ok(socket)
}
fn challenge(
    socket: &UdpSocket,
    server: SocketAddr,
    id: [u8; 16],
    selection: &wire::Selection,
) -> Result<wire::Cookie, Box<dyn std::error::Error>> {
    let mut packet = wire::control(
        wire::SUBSCRIBE,
        id,
        &wire::Subscribe {
            selection: Some(selection.clone()),
            cookie: None,
        },
    )
    .unwrap();
    packet.resize(256, b' ');
    socket.send_to(&packet, server)?;
    let mut buffer = [0; 65536];
    let (len, source) = socket.recv_from(&mut buffer)?;
    assert_eq!(source, server);
    let (op, response_id, body) = wire::parse(&buffer[..len]).unwrap();
    assert_eq!(op, wire::CHALLENGE);
    assert_eq!(response_id, id);
    Ok(wire::decode::<wire::Challenge>(body).unwrap().cookie)
}
fn prove(
    socket: &UdpSocket,
    server: SocketAddr,
    id: [u8; 16],
    selection: &wire::Selection,
    cookie: wire::Cookie,
) -> Result<(), Box<dyn std::error::Error>> {
    let packet = wire::control(
        wire::SUBSCRIBE,
        id,
        &wire::Subscribe {
            selection: Some(selection.clone()),
            cookie: Some(cookie),
        },
    )
    .unwrap();
    socket.send_to(&packet, server)?;
    Ok(())
}
fn receive_control(
    socket: &UdpSocket,
    id: [u8; 16],
    expected: u8,
) -> Result<(u8, Vec<u8>), Box<dyn std::error::Error>> {
    let mut buffer = [0; 65536];
    let (len, _) = socket.recv_from(&mut buffer)?;
    let (op, response_id, body) = wire::parse(&buffer[..len]).unwrap();
    assert_eq!(response_id, id);
    assert_eq!(op, expected);
    Ok((op, body.to_vec()))
}
fn quiet_for(socket: &UdpSocket) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = [0; 65536];
    match socket.recv_from(&mut buffer) {
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        Ok((len, _)) => Err(format!("unexpected UDP packet: {len} bytes").into()),
        Err(err) => Err(err.into()),
    }
}
#[test]
fn cookie_expiry_and_subscription_id_and_source_binding_are_enforced() -> TestResult {
    let server = Server::start(true, 64)?;
    let selection = wire::Selection {
        pool: "events".into(),
        start: wire::Start::New,
        live: true,
    };
    let socket = raw_socket()?;
    let expired_id = [8; 16];
    let expired = challenge(&socket, server.address, expired_id, &selection)?;
    thread::sleep(Duration::from_millis(10_150));
    prove(&socket, server.address, expired_id, &selection, expired)?;
    quiet_for(&socket)?;

    let id = [9; 16];
    let cookie = challenge(&socket, server.address, id, &selection)?;
    prove(
        &socket,
        server.address,
        [10; 16],
        &selection,
        cookie.clone(),
    )?;
    quiet_for(&socket)?;
    let other = raw_socket()?;
    prove(&other, server.address, id, &selection, cookie.clone())?;
    quiet_for(&other)?;
    prove(&socket, server.address, id, &selection, cookie)?;
    let (_, body) = receive_control(&socket, id, wire::PROGRESS)?;
    let progress: wire::Progress = wire::decode(&body).unwrap();
    assert_eq!(progress.lease_ms, wire::LEASE_MS);
    Ok(())
}
#[test]
fn quiet_subscriptions_renew_and_expire_on_their_lease() -> TestResult {
    let server = Server::start(true, 64)?;
    let selection = wire::Selection {
        pool: "events".into(),
        start: wire::Start::New,
        live: true,
    };
    let renewed_socket = raw_socket()?;
    let expired_socket = raw_socket()?;
    let renewed_id = [11; 16];
    let expired_id = [12; 16];
    let renewed_cookie = challenge(&renewed_socket, server.address, renewed_id, &selection)?;
    prove(
        &renewed_socket,
        server.address,
        renewed_id,
        &selection,
        renewed_cookie,
    )?;
    receive_control(&renewed_socket, renewed_id, wire::PROGRESS)?;
    let expired_cookie = challenge(&expired_socket, server.address, expired_id, &selection)?;
    prove(
        &expired_socket,
        server.address,
        expired_id,
        &selection,
        expired_cookie,
    )?;
    receive_control(&expired_socket, expired_id, wire::PROGRESS)?;

    thread::sleep(Duration::from_secs(5));
    let renew = wire::control(
        wire::SUBSCRIBE,
        renewed_id,
        &wire::Subscribe {
            selection: None,
            cookie: None,
        },
    )
    .unwrap();
    renewed_socket.send_to(&renew, server.address)?;
    let (_, body) = receive_control(&renewed_socket, renewed_id, wire::PROGRESS)?;
    let progress: wire::Progress = wire::decode(&body).unwrap();
    assert_eq!(progress.lease_ms, wire::LEASE_MS);

    // This check lands after the unrenewed lease expires but before the renewed one.
    thread::sleep(Duration::from_secs(26));
    let renew = wire::control(
        wire::SUBSCRIBE,
        renewed_id,
        &wire::Subscribe {
            selection: None,
            cookie: None,
        },
    )
    .unwrap();
    renewed_socket.send_to(&renew, server.address)?;
    receive_control(&renewed_socket, renewed_id, wire::PROGRESS)?;
    let renew = wire::control(
        wire::SUBSCRIBE,
        expired_id,
        &wire::Subscribe {
            selection: None,
            cookie: None,
        },
    )
    .unwrap();
    expired_socket.send_to(&renew, server.address)?;
    quiet_for(&expired_socket)?;
    Ok(())
}
struct Proxy {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    dropped_terminal: Option<Arc<AtomicBool>>,
}
impl Proxy {
    fn impaired(server: SocketAddr) -> Result<Self, Box<dyn std::error::Error>> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let address = socket.local_addr()?;
        socket.set_read_timeout(Some(Duration::from_millis(20)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let worker = thread::spawn(move || {
            let mut client = None;
            let mut buffer = [0; 65536];
            let mut delayed = None;
            let mut opening = true;
            while !done.load(Ordering::Acquire) {
                let Ok((len, source)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                let bytes = &buffer[..len];
                if source != server {
                    client = Some(source);
                    let _ = socket.send_to(bytes, server);
                    continue;
                }
                let Some(client) = client else {
                    continue;
                };
                if let Some((op, _, body)) = wire::parse(bytes) {
                    if op == wire::PROGRESS && opening {
                        opening = false;
                        continue;
                    }
                    if op == wire::DATA {
                        let seq = u64::from_be_bytes(body[..8].try_into().unwrap());
                        match seq {
                            1 => {
                                let _ = socket.send_to(bytes, client);
                                let _ = socket.send_to(bytes, client);
                                continue;
                            }
                            2 => continue,
                            3 => {
                                delayed = Some(bytes.to_vec());
                                continue;
                            }
                            4 => {
                                let _ = socket.send_to(bytes, client);
                                if let Some(bytes) = delayed.take() {
                                    let _ = socket.send_to(&bytes, client);
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                let _ = socket.send_to(bytes, client);
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
            dropped_terminal: None,
        })
    }
    fn dropping_terminal_progress(server: SocketAddr) -> Result<Self, Box<dyn std::error::Error>> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let address = socket.local_addr()?;
        socket.set_read_timeout(Some(Duration::from_millis(20)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let dropped_terminal = Arc::new(AtomicBool::new(false));
        let dropped = dropped_terminal.clone();
        let worker = thread::spawn(move || {
            let mut client = None;
            let mut buffer = [0; 65536];
            while !done.load(Ordering::Acquire) {
                let Ok((len, source)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                let bytes = &buffer[..len];
                if source != server {
                    client = Some(source);
                    let _ = socket.send_to(bytes, server);
                    continue;
                }
                let Some(client) = client else {
                    continue;
                };
                if let Some((op, _, body)) = wire::parse(bytes)
                    && op == wire::PROGRESS
                    && wire::decode::<wire::Progress>(body)
                        .is_some_and(|progress| progress.terminal)
                    && !dropped.swap(true, Ordering::AcqRel)
                {
                    continue;
                }
                let _ = socket.send_to(bytes, client);
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
            dropped_terminal: Some(dropped_terminal),
        })
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[test]
fn loss_duplicate_reorder_and_lost_opening_progress_never_repair_or_repeat_data() -> TestResult {
    let server = Server::start(true, 64)?;
    for n in 1..=4 {
        server.append(json!({"sample":n}), n)?;
    }
    let proxy = Proxy::impaired(server.address)?;
    let pool =
        UdpClient::new(format!("udp://{}", proxy.address))?.open_pool(&PoolRef::name("events"))?;
    let mut stream = pool.follow(UdpFollowOptions {
        start: UdpStart::Last(4),
        live: false,
    })?;
    let mut frames = Vec::new();
    let mut gaps = Vec::new();
    while let Some(event) = stream.next_event()? {
        match event {
            UdpEvent::Frame(frame) => frames.push(frame.seq),
            UdpEvent::Gap { first, last, .. } => gaps.push((first, last)),
            _ => {}
        }
    }
    assert_eq!(frames, vec![1, 4]);
    assert!(gaps.contains(&(2, 3)), "{gaps:?}");
    Ok(())
}
#[test]
fn dropped_terminal_progress_returns_on_the_next_renewal() -> TestResult {
    let server = Server::start(true, 64)?;
    server.append(json!({"sample":1}), 1)?;
    let proxy = Proxy::dropping_terminal_progress(server.address)?;
    let pool =
        UdpClient::new(format!("udp://{}", proxy.address))?.open_pool(&PoolRef::name("events"))?;
    let started = Instant::now();
    let mut stream = pool.follow(UdpFollowOptions {
        start: UdpStart::Last(1),
        live: false,
    })?;
    let deadline = Instant::now() + Duration::from_secs(14);
    let mut frames = Vec::new();
    let mut terminal = false;
    while Instant::now() < deadline {
        match stream.next_event_timeout(Duration::from_millis(250))? {
            Some(UdpEvent::Frame(frame)) => frames.push(frame.seq),
            Some(UdpEvent::Progress { terminal: true, .. }) => {
                terminal = true;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(frames, vec![1]);
    assert!(terminal, "terminal progress did not return after renewal");
    assert!(
        proxy
            .dropped_terminal
            .as_ref()
            .unwrap()
            .load(Ordering::Acquire)
    );
    assert!(
        started.elapsed() >= Duration::from_secs(8),
        "terminal state should come from renewal"
    );
    Ok(())
}
