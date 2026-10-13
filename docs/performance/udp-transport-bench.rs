//! Loopback Lite3 feed/follow comparison. See udp-transport.md for the command.
//! Build this standalone file against the release library; it changes no crate targets.

use plasmite::api::{
    Durability, LocalClient, PoolOptions, PoolRef, RemoteClient, TailOptions, UdpClient, UdpEvent,
    UdpFollowOptions, lite3,
};
use std::error::Error;
use std::fs;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COUNT: usize = 200;
const PACE: Duration = Duration::from_millis(5);
const SIZES: [usize; 3] = [128, 1024, 16384];

struct Server {
    child: Child,
    root: PathBuf,
    pools: PathBuf,
    http: String,
    udp: String,
}

impl Server {
    fn start(binary: &str) -> Result<Self, Box<dyn Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root =
            std::env::temp_dir().join(format!("plasmite-udp-bench-{}-{stamp}", std::process::id()));
        let pools = root.join("pools");
        let home = root.join("home");
        fs::create_dir_all(&pools)?;
        fs::create_dir_all(&home)?;
        let held = UdpSocket::bind("127.0.0.1:0")?;
        let udp = format!("udp://{}", held.local_addr()?);
        drop(held);
        let ready = root.join("ready");
        let child = Command::new(binary)
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
            .arg(format!("--udp={}", udp.trim_start_matches("udp://")))
            .arg("--udp-follow")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut server = Self {
            child,
            root,
            pools,
            http: String::new(),
            udp,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = server.child.try_wait()? {
                return Err(format!("server exited before readiness: {status}").into());
            }
            if Instant::now() >= deadline {
                return Err("server readiness timed out".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let local: String =
            serde_json::from_slice(&fs::read(server.pools.join(".plasmite-serve/local.json"))?)?;
        server.http = format!("http://{local}");
        Ok(server)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Seen {
    frames: Vec<(usize, Instant)>,
    gaps: usize,
    errors: usize,
}

fn percentile(values: &mut [f64], pct: usize) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) * pct + 99) / 100]
}

fn run(
    server: &Server,
    transport: &str,
    size: usize,
    fans: usize,
    repeat: usize,
) -> Result<(), Box<dyn Error>> {
    let name = format!("bench_{transport}_{size}_{fans}_{repeat}");
    let pool_ref = PoolRef::name(&name);
    LocalClient::new()
        .with_pool_dir(&server.pools)
        .create_pool(&pool_ref, PoolOptions::new(8 * 1024 * 1024))?;
    let messages: Arc<Vec<Vec<u8>>> = Arc::new(
        (0..COUNT)
            .map(|i| {
                let json = format!(
                    "{{\"meta\":{{\"tags\":[]}},\"data\":{{\"id\":{i},\"blob\":\"{}\"}}}}",
                    "x".repeat(size)
                );
                Ok::<_, plasmite::api::Error>(
                    lite3::Lite3Buf::from_json_str(&json)?.as_slice().to_vec(),
                )
            })
            .collect::<Result<_, _>>()?,
    );
    let encoded = messages[0].len();
    let sends = Arc::new(Mutex::new(vec![None::<Instant>; COUNT]));
    let done = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    if transport == "udp" {
        let client = UdpClient::new(&server.udp)?;
        let pool = client.open_pool(&pool_ref)?;
        for _ in 0..fans {
            let mut follow = pool.follow(UdpFollowOptions::new())?;
            let tx = ready_tx.clone();
            let done = done.clone();
            let messages = messages.clone();
            handles.push(thread::spawn(move || {
                let mut seen = Seen {
                    frames: Vec::new(),
                    gaps: 0,
                    errors: 0,
                };
                tx.send(()).unwrap();
                let mut finish_at = None;
                loop {
                    match follow.next_event_timeout(Duration::from_millis(50)) {
                        Ok(Some(UdpEvent::Frame(frame))) => {
                            let at = Instant::now();
                            let seq = frame.seq as usize;
                            if seq == 0 || seq > COUNT || frame.payload != messages[seq - 1] {
                                seen.errors += 1;
                            } else {
                                seen.frames.push((seq - 1, at));
                            }
                        }
                        Ok(Some(UdpEvent::Gap { first, last, .. })) => {
                            seen.gaps += (last - first + 1) as usize
                        }
                        Ok(_) => (),
                        Err(error) => {
                            eprintln!("UDP follow error: {error}");
                            seen.errors += 1;
                            break;
                        }
                    }
                    if seen.frames.len() == COUNT {
                        break;
                    }
                    if done.load(Ordering::Relaxed) {
                        let deadline = finish_at
                            .get_or_insert_with(|| Instant::now() + Duration::from_secs(2));
                        if Instant::now() >= *deadline {
                            break;
                        }
                    }
                }
                follow.close();
                seen
            }));
        }
        for _ in 0..fans {
            ready_rx.recv()?;
        }
        let mut send_errors = 0;
        let mut send_durations = Vec::new();
        let batch_start = Instant::now();
        for (i, message) in messages.iter().enumerate() {
            let planned = batch_start + PACE * i as u32;
            if Instant::now() < planned {
                thread::sleep(planned - Instant::now());
            }
            let start = Instant::now();
            sends.lock().unwrap()[i] = Some(start);
            match pool.send_lite3(message) {
                Ok(()) => (),
                Err(error) => {
                    eprintln!("UDP send error: {error}");
                    send_errors += 1;
                }
            }
            send_durations.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        done.store(true, Ordering::Relaxed);
        report(
            transport,
            size,
            encoded,
            fans,
            repeat,
            send_errors,
            &mut send_durations,
            &sends,
            handles,
            batch_start,
        )?;
    } else {
        let client = RemoteClient::new(&server.http)?;
        let pool = client.open_pool(&pool_ref)?;
        for _ in 0..fans {
            let mut opts = TailOptions::new();
            opts.since_seq = Some(1);
            opts.max_messages = Some(COUNT);
            opts.timeout = Some(Duration::from_secs(4));
            let mut tail = pool.tail_lite3(opts)?;
            let tx = ready_tx.clone();
            let messages = messages.clone();
            handles.push(thread::spawn(move || {
                let mut seen = Seen {
                    frames: Vec::new(),
                    gaps: 0,
                    errors: 0,
                };
                tx.send(()).unwrap();
                loop {
                    match tail.next_frame() {
                        Ok(Some(frame)) => {
                            let at = Instant::now();
                            let seq = frame.seq as usize;
                            if seq == 0 || seq > COUNT || frame.payload != messages[seq - 1] {
                                seen.errors += 1;
                            } else {
                                seen.frames.push((seq - 1, at));
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("HTTP follow error: {error}");
                            seen.errors += 1;
                            break;
                        }
                    }
                }
                tail.cancel();
                seen
            }));
        }
        for _ in 0..fans {
            ready_rx.recv()?;
        }
        let mut send_errors = 0;
        let mut send_durations = Vec::new();
        let batch_start = Instant::now();
        for (i, message) in messages.iter().enumerate() {
            let planned = batch_start + PACE * i as u32;
            if Instant::now() < planned {
                thread::sleep(planned - Instant::now());
            }
            let start = Instant::now();
            sends.lock().unwrap()[i] = Some(start);
            match pool.append_lite3_now(message, Durability::Fast) {
                Ok(seq) if seq == i as u64 + 1 => (),
                Ok(seq) => {
                    eprintln!("unexpected HTTP sequence: {seq}");
                    send_errors += 1;
                }
                Err(error) => {
                    eprintln!("HTTP append error: {error}");
                    send_errors += 1;
                }
            }
            send_durations.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        done.store(true, Ordering::Relaxed);
        report(
            transport,
            size,
            encoded,
            fans,
            repeat,
            send_errors,
            &mut send_durations,
            &sends,
            handles,
            batch_start,
        )?;
    }
    Ok(())
}

fn report(
    transport: &str,
    size: usize,
    encoded: usize,
    fans: usize,
    repeat: usize,
    send_errors: usize,
    send_durations: &mut [f64],
    sends: &Arc<Mutex<Vec<Option<Instant>>>>,
    handles: Vec<thread::JoinHandle<Seen>>,
    batch_start: Instant,
) -> Result<(), Box<dyn Error>> {
    let send_p50 = percentile(send_durations, 50);
    let send_p95 = percentile(send_durations, 95);
    let starts = sends.lock().unwrap();
    for (follower, handle) in handles.into_iter().enumerate() {
        let seen = handle.join().map_err(|_| "follower panicked")?;
        let mut latencies: Vec<f64> = seen
            .frames
            .iter()
            .map(|(id, at)| {
                at.duration_since(starts[*id].expect("send start"))
                    .as_secs_f64()
                    * 1000.0
            })
            .collect();
        let last = seen
            .frames
            .iter()
            .map(|(_, at)| *at)
            .max()
            .unwrap_or(batch_start);
        let duration = last.duration_since(batch_start).as_secs_f64();
        let p50 = percentile(&mut latencies, 50);
        let p95 = percentile(&mut latencies, 95);
        let p99 = percentile(&mut latencies, 99);
        println!(
            "{transport},{size},{encoded},{fans},{repeat},{follower},{COUNT},{send_errors},{},{},{},{},{:.1},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
            seen.frames.len(),
            COUNT - seen.frames.len(),
            seen.gaps,
            seen.errors,
            seen.frames.len() as f64 / duration,
            send_p50,
            send_p95,
            p50,
            p95,
            p99,
            duration
        );
    }
    Ok(())
}

fn max_probe(server: &Server) -> Result<(), Box<dyn Error>> {
    let pool_ref = PoolRef::name("max_probe");
    LocalClient::new()
        .with_pool_dir(&server.pools)
        .create_pool(&pool_ref, PoolOptions::new(1024 * 1024))?;
    let pool = UdpClient::new(&server.udp)?.open_pool(&pool_ref)?;
    let mut largest = Vec::new();
    let mut rejected = Vec::new();
    for blob_len in 65_000..65_100 {
        let json = format!(
            "{{\"meta\":{{\"tags\":[]}},\"data\":{{\"blob\":\"{}\"}}}}",
            "x".repeat(blob_len)
        );
        let message = lite3::Lite3Buf::from_json_str(&json)?.as_slice().to_vec();
        if message.len() + 40 <= 65_507 {
            largest = message;
        } else {
            rejected = message;
            break;
        }
    }
    if largest.is_empty() || rejected.is_empty() {
        return Err("max-size probe did not bracket the limit".into());
    }
    let mut follow = pool.follow(UdpFollowOptions::new())?;
    pool.send_lite3(&largest)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut received = false;
    while Instant::now() < deadline {
        match follow.next_event_timeout(Duration::from_millis(50))? {
            Some(UdpEvent::Frame(frame)) => {
                received = frame.payload == largest;
                break;
            }
            _ => (),
        }
    }
    follow.close();
    let oversized_rejected = pool.send_lite3(&rejected).is_err();
    println!(
        "max_datagram=65507 data_header=40 lite3_bytes={} follow_datagram_bytes={} received_exact={} next_lite3_bytes={} oversized_rejected={}",
        largest.len(),
        largest.len() + 40,
        received,
        rejected.len(),
        oversized_rejected
    );
    if !received || !oversized_rejected {
        return Err("maximum UDP datagram proof failed".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let binary = std::env::args()
        .nth(1)
        .ok_or("pass the release plasmite binary")?;
    let server = Server::start(&binary)?;
    eprintln!(
        "HTTP={} UDP={} count={} pace_ms={} (loopback)",
        server.http,
        server.udp,
        COUNT,
        PACE.as_millis()
    );
    if std::env::args().nth(2).as_deref() == Some("--max-probe") {
        return max_probe(&server);
    }
    println!(
        "transport,blob_bytes,lite3_bytes,fanout,run,follower,sent,send_errors,received,missing,gap_events_messages,follow_errors,useful_messages_per_s,send_p50_ms,send_p95_ms,receipt_p50_ms,receipt_p95_ms,receipt_p99_ms,duration_s"
    );
    for repeat in 1..=3 {
        for size in SIZES {
            for fans in [1, 4] {
                for transport in ["udp", "http"] {
                    run(&server, transport, size, fans, repeat)?;
                }
            }
        }
    }
    Ok(())
}
