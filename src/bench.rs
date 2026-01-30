// Benchmark harness for Plasmite.
//
// Purpose:
// - Provide a simple, repeatable baseline for core operations (append, follow read, get scan, multi-writer contention).
// - Emit machine-readable JSON to stdout and a human-readable table to stderr.
//
// Design notes:
// - Uses child processes for contention/follow to exercise cross-process file locking.
// - Avoids extra dependencies; keep benchmarks "good enough" for trend tracking, not lab-grade profiling.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use plasmite::core::cursor::{Cursor, CursorResult};
use plasmite::core::error::{Error, ErrorKind};
use plasmite::core::lite3;
use plasmite::core::pool::{Pool, PoolOptions};

#[derive(Clone, Debug)]
pub struct BenchArgs {
    pub work_dir: Option<PathBuf>,
    pub pool_sizes: Vec<u64>,
    pub payload_sizes: Vec<usize>,
    pub messages: u64,
    pub writers: Vec<usize>,
    pub format: BenchFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BenchFormat {
    Json,
    Table,
    Both,
}

impl BenchFormat {
    pub fn parse(input: &str) -> Result<Self, Error> {
        match input.trim() {
            "json" => Ok(Self::Json),
            "table" => Ok(Self::Table),
            "both" => Ok(Self::Both),
            _ => Err(Error::new(ErrorKind::Usage)
                .with_message("invalid --format (use json|table|both)")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkerArgs {
    pub pool_path: PathBuf,
    pub role: WorkerRole,
    pub messages: u64,
    pub payload_bytes: usize,
    pub out_json: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerRole {
    Writer,
    Follower,
}

impl WorkerRole {
    pub fn parse(input: &str) -> Result<Self, Error> {
        match input.trim() {
            "writer" => Ok(Self::Writer),
            "follower" => Ok(Self::Follower),
            _ => Err(Error::new(ErrorKind::Usage)
                .with_message("invalid worker role (use writer|follower)")),
        }
    }
}

pub fn run_bench(args: BenchArgs, program_version: &str) -> Result<(), Error> {
    let start = SystemTime::now();
    let work_dir = args
        .work_dir
        .clone()
        .unwrap_or_else(default_work_dir);
    std::fs::create_dir_all(&work_dir)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(&work_dir).with_source(err))?;

    let mut results = Vec::new();
    for pool_size in &args.pool_sizes {
        for payload_bytes in &args.payload_sizes {
            let base_name = format!("bench-{}-{}", pool_size, payload_bytes);
            let pool_path = work_dir.join(format!("{base_name}.plasmite"));

            let append = bench_append(&pool_path, *pool_size, *payload_bytes, args.messages)?;
            results.extend(append);

            let follow = bench_follow(&work_dir, &pool_path, *pool_size, *payload_bytes, args.messages)?;
            results.push(follow);

            let get_scan = bench_get_scan(&pool_path, *pool_size, *payload_bytes, args.messages)?;
            results.extend(get_scan);

            for writers in &args.writers {
                if *writers <= 1 {
                    continue;
                }
                let contention = bench_multi_writer(
                    &work_dir,
                    &pool_path,
                    *pool_size,
                    *payload_bytes,
                    args.messages,
                    *writers,
                )?;
                results.push(contention);
            }
        }
    }

    let output = json!({
        "name": "plasmite",
        "version": program_version,
        "ts": rfc3339_now(start),
        "system": system_json(),
        "params": {
            "pool_sizes": args.pool_sizes,
            "payload_sizes": args.payload_sizes,
            "messages": args.messages,
            "writers": args.writers,
            "work_dir": work_dir.display().to_string(),
            "debug_build": cfg!(debug_assertions),
        },
        "results": results,
    });

    emit_bench_output(output, args.format)
}

pub fn run_worker(args: WorkerArgs) -> Result<(), Error> {
    match args.role {
        WorkerRole::Writer => run_writer_worker(args),
        WorkerRole::Follower => run_follower_worker(args),
    }
}

fn emit_bench_output(value: Value, format: BenchFormat) -> Result<(), Error> {
    match format {
        BenchFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string()));
            Ok(())
        }
        BenchFormat::Table => {
            emit_table(&value)?;
            Ok(())
        }
        BenchFormat::Both => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string()));
            emit_table(&value)?;
            Ok(())
        }
    }
}

fn emit_table(value: &Value) -> Result<(), Error> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "plasmite bench (table)").map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write bench table")
            .with_source(err)
    })?;

    let results = value
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::new(ErrorKind::Internal).with_message("bench results missing"))?;

    writeln!(
        stderr,
        "{:>10}  {:>10}  {:>10}  {:>8}  {:>9}  {:>10}  {}",
        "bench", "pool", "payload", "msgs", "writers", "ms/msg", "notes"
    )
    .map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write bench table header")
            .with_source(err)
    })?;

    for item in results {
        let bench = item.get("bench").and_then(|v| v.as_str()).unwrap_or("?");
        let pool = item.get("pool_size").and_then(|v| v.as_u64()).unwrap_or(0);
        let payload = item.get("payload_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let msgs = item.get("messages").and_then(|v| v.as_u64()).unwrap_or(0);
        let writers = item.get("writers").and_then(|v| v.as_u64()).unwrap_or(1);
        let ms_per_msg = item
            .get("ms_per_msg")
            .and_then(|v| v.as_f64())
            .unwrap_or(f64::NAN);
        let notes = item.get("notes").and_then(|v| v.as_str()).unwrap_or("");

        writeln!(
            stderr,
            "{:>10}  {:>10}  {:>10}  {:>8}  {:>9}  {:>10.3}  {}",
            bench,
            format_bytes(pool),
            format_bytes(payload),
            msgs,
            writers,
            ms_per_msg,
            notes
        )
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to write bench table row")
                .with_source(err)
        })?;
    }

    Ok(())
}

fn bench_append(
    pool_path: &Path,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
) -> Result<Vec<Value>, Error> {
    let _ = std::fs::remove_file(pool_path);
    let mut pool = Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let payload_once = payload_for_bytes(payload_bytes, None, false)?;
    let start = Instant::now();
    for _ in 0..messages {
        pool.append(payload_once.as_slice())?;
    }
    let dur = start.elapsed();
    let core = result_entry(
        "append",
        pool_size,
        payload_bytes,
        messages,
        1,
        dur,
        Some("core payload reused"),
    );

    let _ = std::fs::remove_file(pool_path);
    let mut pool = Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let start = Instant::now();
    for i in 0..messages {
        let is_done = i + 1 == messages;
        let payload = payload_for_bytes(payload_bytes, Some(i), is_done)?;
        pool.append(payload.as_slice())?;
    }
    let dur = start.elapsed();
    let end_to_end = result_entry(
        "append",
        pool_size,
        payload_bytes,
        messages,
        1,
        dur,
        Some("includes Lite3 encode per msg"),
    );

    Ok(vec![core, end_to_end])
}

fn bench_follow(
    work_dir: &Path,
    pool_path: &Path,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
) -> Result<Value, Error> {
    let _ = std::fs::remove_file(pool_path);
    Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let follower_out = work_dir.join("follow-follower.json");
    let writer_out = work_dir.join("follow-writer.json");

    let mut follower = spawn_worker(WorkerArgs {
        pool_path: pool_path.to_path_buf(),
        role: WorkerRole::Follower,
        messages,
        payload_bytes,
        out_json: follower_out.clone(),
    })?;

    let mut writer = spawn_worker(WorkerArgs {
        pool_path: pool_path.to_path_buf(),
        role: WorkerRole::Writer,
        messages,
        payload_bytes,
        out_json: writer_out.clone(),
    })?;

    let writer_status = writer
        .wait()
        .map_err(|err| Error::new(ErrorKind::Io).with_message("writer wait failed").with_source(err))?;
    if !writer_status.success() {
        return Err(Error::new(ErrorKind::Internal).with_message("writer worker failed"));
    }

    let follower_status = follower.wait().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("follower wait failed")
            .with_source(err)
    })?;
    if !follower_status.success() {
        return Err(Error::new(ErrorKind::Internal).with_message("follower worker failed"));
    }

    let follower_json = read_json_file(&follower_out)?;
    let writer_json = read_json_file(&writer_out)?;

    let seen = follower_json
        .get("messages_seen")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let dur_ms = follower_json
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let ms_per_msg = if seen == 0 {
        0.0
    } else {
        dur_ms as f64 / seen as f64
    };

    let mut entry = BTreeMap::new();
    entry.insert("bench".to_string(), json!("follow"));
    entry.insert("pool_size".to_string(), json!(pool_size));
    entry.insert("payload_bytes".to_string(), json!(payload_bytes));
    entry.insert("messages".to_string(), json!(seen));
    entry.insert("writers".to_string(), json!(1));
    entry.insert("duration_ms".to_string(), json!(dur_ms));
    entry.insert("ms_per_msg".to_string(), json!(ms_per_msg));
    entry.insert(
        "latency_ms".to_string(),
        follower_json.get("latency_ms").cloned().unwrap_or(json!({})),
    );
    entry.insert("notes".to_string(), json!("cross-process: writer+follower"));
    entry.insert("writer".to_string(), writer_json);

    Ok(Value::Object(entry.into_iter().collect()))
}

fn bench_get_scan(
    pool_path: &Path,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
) -> Result<Vec<Value>, Error> {
    let _ = std::fs::remove_file(pool_path);
    let mut pool = Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let payload_once = payload_for_bytes(payload_bytes, None, false)?;
    for _ in 0..messages {
        pool.append(payload_once.as_slice())?;
    }

    let bounds = pool.bounds()?;
    let Some(oldest) = bounds.oldest_seq else {
        return Ok(vec![]);
    };
    let Some(newest) = bounds.newest_seq else {
        return Ok(vec![]);
    };
    let mid = oldest.saturating_add((newest.saturating_sub(oldest)) / 2);

    let targets = [
        ("near_newest", newest),
        ("mid", mid),
        ("near_oldest", oldest),
    ];

    let mut out = Vec::new();
    for (label, seq) in targets {
        let start = Instant::now();
        let _frame = pool.get(seq)?;
        let dur = start.elapsed();
        out.push(result_entry(
            "get_scan",
            pool_size,
            payload_bytes,
            1,
            1,
            dur,
            Some(label),
        ));
    }
    Ok(out)
}

fn bench_multi_writer(
    work_dir: &Path,
    pool_path: &Path,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
    writers: usize,
) -> Result<Value, Error> {
    let _ = std::fs::remove_file(pool_path);
    Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let mut children = Vec::new();
    let suite_start = Instant::now();

    for idx in 0..writers {
        let out_json = work_dir.join(format!("writer-{writers}-{idx}.json"));
        children.push(spawn_worker(WorkerArgs {
            pool_path: pool_path.to_path_buf(),
            role: WorkerRole::Writer,
            messages,
            payload_bytes,
            out_json,
        })?);
    }

    for mut child in children {
        let status = child.wait().map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("writer worker wait failed")
                .with_source(err)
        })?;
        if !status.success() {
            return Err(Error::new(ErrorKind::Internal).with_message("writer worker failed"));
        }
    }

    let dur = suite_start.elapsed();
    Ok(result_entry(
        "multi_writer",
        pool_size,
        payload_bytes,
        messages,
        writers as u64,
        dur,
        Some("cross-process writers"),
    ))
}

fn run_writer_worker(args: WorkerArgs) -> Result<(), Error> {
    let mut pool = Pool::open(&args.pool_path)?;

    let start = Instant::now();
    for i in 0..args.messages {
        let is_done = i + 1 == args.messages;
        let payload = payload_for_bytes(args.payload_bytes, Some(now_ns()? ^ (i as u64)), is_done)?;
        pool.append(payload.as_slice())?;
    }
    let dur = start.elapsed();

    let output = json!({
        "role": "writer",
        "messages": args.messages,
        "payload_bytes": args.payload_bytes,
        "duration_ms": dur.as_millis() as u64,
    });
    write_json_file(&args.out_json, &output)?;
    Ok(())
}

fn run_follower_worker(args: WorkerArgs) -> Result<(), Error> {
    let pool = Pool::open(&args.pool_path)?;

    let mut cursor = Cursor::new();
    let start = Instant::now();
    let mut latencies_ms = Vec::new();
    let mut seen = 0u64;
    let deadline = start + Duration::from_secs(300);

    loop {
        if Instant::now() > deadline {
            return Err(Error::new(ErrorKind::Internal).with_message("follower timed out"));
        }
        match cursor.next(&pool)? {
            CursorResult::Message(frame) => {
                let data = decode_payload_data(frame.payload)?;
                if let Some(sent_ns) = data.get("sent_ns").and_then(|v| v.as_u64()) {
                    let now = now_ns()?;
                    let delta = now.saturating_sub(sent_ns);
                    latencies_ms.push(delta as f64 / 1_000_000.0);
                }
                seen += 1;
                if data.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                    break;
                }
            }
            CursorResult::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            CursorResult::FellBehind => {
                // For baseline benches we tolerate this; it indicates the pool was too small.
                // Resync is handled by Cursor internally; continue.
            }
        }
    }

    let dur = start.elapsed();
    latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let output = json!({
        "role": "follower",
        "messages_upper_bound": args.messages,
        "messages_seen": seen,
        "payload_bytes": args.payload_bytes,
        "duration_ms": dur.as_millis() as u64,
        "latency_ms": latency_summary(&latencies_ms),
    });
    write_json_file(&args.out_json, &output)?;
    Ok(())
}

fn decode_payload_data(payload: &[u8]) -> Result<BTreeMap<String, Value>, Error> {
    let doc = lite3::Lite3DocRef::new(payload);
    let json_str = doc.to_json(false)?;
    let value: Value = serde_json::from_str(&json_str).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("payload is not object"))?;
    let data = obj
        .get("data")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("payload data missing"))?;

    let mut out = BTreeMap::new();
    for (key, value) in data {
        out.insert(key.clone(), value.clone());
    }
    Ok(out)
}

fn payload_for_bytes(payload_bytes: usize, sent_ns: Option<u64>, done: bool) -> Result<lite3::Lite3Buf, Error> {
    let mut filler = "x".repeat(payload_bytes.saturating_sub(32));
    if filler.is_empty() {
        filler = "x".to_string();
    }

    let data = match sent_ns {
        Some(ns) => json!({"sent_ns": ns, "done": done, "filler": filler}),
        None => json!({"filler": filler}),
    };
    lite3::encode_message(&["bench".to_string()], &data)
}

fn spawn_worker(args: WorkerArgs) -> Result<std::process::Child, Error> {
    let exe = std::env::current_exe().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to resolve current executable")
            .with_source(err)
    })?;

    let mut cmd = Command::new(exe);
    cmd.arg("bench-worker")
        .arg(args.role.to_string())
        .arg("--pool")
        .arg(&args.pool_path)
        .arg("--messages")
        .arg(args.messages.to_string())
        .arg("--payload-bytes")
        .arg(args.payload_bytes.to_string())
        .arg("--out-json")
        .arg(&args.out_json)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    cmd.spawn().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to spawn bench worker")
            .with_source(err)
    })
}

fn read_json_file(path: &Path) -> Result<Value, Error> {
    let bytes = std::fs::read(path)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    serde_json::from_slice(&bytes).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("invalid worker json")
            .with_path(path)
            .with_source(err)
    })
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| Error::new(ErrorKind::Internal).with_message("json encode failed").with_source(err))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| Error::new(ErrorKind::Io).with_path(parent).with_source(err))?;
    }
    std::fs::write(path, bytes)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))
}

fn result_entry(
    bench: &str,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
    writers: u64,
    duration: Duration,
    notes: Option<&str>,
) -> Value {
    let dur_ms = duration.as_secs_f64() * 1000.0;
    let msgs = if messages == 0 { 1 } else { messages } as f64;
    let ms_per_msg = dur_ms / msgs;
    let mb = (payload_bytes as f64 * messages as f64) / (1024.0 * 1024.0);
    let secs = duration.as_secs_f64().max(1e-9);
    let mb_per_sec = mb / secs;
    let msgs_per_sec = (messages as f64) / secs;

    let mut map = BTreeMap::new();
    map.insert("bench".to_string(), json!(bench));
    map.insert("pool_size".to_string(), json!(pool_size));
    map.insert("payload_bytes".to_string(), json!(payload_bytes));
    map.insert("messages".to_string(), json!(messages));
    map.insert("writers".to_string(), json!(writers));
    map.insert("duration_ms".to_string(), json!(dur_ms));
    map.insert("ms_per_msg".to_string(), json!(ms_per_msg));
    map.insert("msgs_per_sec".to_string(), json!(msgs_per_sec));
    map.insert("mb_per_sec".to_string(), json!(mb_per_sec));
    if let Some(notes) = notes {
        map.insert("notes".to_string(), json!(notes));
    }
    Value::Object(map.into_iter().collect())
}

fn latency_summary(sorted_ms: &[f64]) -> Value {
    if sorted_ms.is_empty() {
        return json!({});
    }
    json!({
        "min": sorted_ms.first().copied().unwrap_or(0.0),
        "p50": quantile(sorted_ms, 0.50),
        "p95": quantile(sorted_ms, 0.95),
        "max": sorted_ms.last().copied().unwrap_or(0.0),
    })
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let clamped = q.clamp(0.0, 1.0);
    let idx = ((sorted.len() - 1) as f64 * clamped).round() as usize;
    sorted.get(idx).copied().unwrap_or_else(|| sorted[sorted.len() - 1])
}

fn system_json() -> Value {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": cpus,
    })
}

fn default_work_dir() -> PathBuf {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis();
    PathBuf::from(".scratch").join(format!("plasmite-bench-{pid}-{ts}"))
}

fn now_ns() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_nanos() as u64)
        .map_err(|err| Error::new(ErrorKind::Internal).with_message("clock went backwards").with_source(err))
}

fn rfc3339_now(ts: SystemTime) -> String {
    let dur = ts.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let nsec = dur.subsec_nanos();
    let tm = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or_else(|_| time::OffsetDateTime::UNIX_EPOCH);
    let tm = tm.replace_nanosecond(nsec).unwrap_or(tm);
    tm.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn format_bytes(value: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let v = value as f64;
    if v >= GB {
        format!("{:.1}GiB", v / GB)
    } else if v >= MB {
        format!("{:.1}MiB", v / MB)
    } else if v >= KB {
        format!("{:.1}KiB", v / KB)
    } else {
        format!("{value}B")
    }
}

impl fmt::Display for WorkerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkerRole::Writer => write!(f, "writer"),
            WorkerRole::Follower => write!(f, "follower"),
        }
    }
}
