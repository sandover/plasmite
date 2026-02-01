//! Purpose: Benchmark harness for core operations and multi-process contention scenarios.
//! Exports: `run_bench`, `run_worker`, `BenchArgs`, `BenchFormat`, `WorkerArgs`, `WorkerRole`.
//! Role: Dev-only runner used by the `plasmite-bench` binary (not shipped to end users).
//! Invariants: Uses child processes to exercise cross-process file locking and follow semantics.
//! Invariants: Intended for trend tracking; not lab-grade profiling.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use plasmite::core::cursor::{Cursor, CursorResult};
use plasmite::core::error::{Error, ErrorKind};
use plasmite::core::lite3;
use plasmite::core::pool::{AppendOptions, Durability, Pool, PoolOptions};

#[derive(Clone, Debug)]
pub struct BenchArgs {
    pub work_dir: Option<PathBuf>,
    pub pool_sizes: Vec<u64>,
    pub payload_sizes: Vec<usize>,
    pub messages: u64,
    pub writers: Vec<usize>,
    pub format: BenchFormat,
    pub durabilities: Vec<Durability>,
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
            _ => {
                Err(Error::new(ErrorKind::Usage)
                    .with_message("invalid --format (use json|table|both)"))
            }
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
    pub durability: Durability,
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
    warn_if_debug_build()?;
    let work_dir = args.work_dir.clone().unwrap_or_else(default_work_dir);
    std::fs::create_dir_all(&work_dir).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_path(&work_dir)
            .with_source(err)
    })?;

    let rep_pool = *args
        .pool_sizes
        .last()
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("no pool sizes provided"))?;
    let rep_payload = *args
        .payload_sizes
        .get(args.payload_sizes.len() / 2)
        .ok_or_else(|| Error::new(ErrorKind::Usage).with_message("no payload sizes provided"))?;
    let rep_writers = args
        .writers
        .iter()
        .copied()
        .find(|value| *value == 4)
        .or_else(|| args.writers.iter().copied().find(|value| *value > 1));

    let mut results = Vec::new();
    for pool_size in &args.pool_sizes {
        for payload_bytes in &args.payload_sizes {
            for durability in &args.durabilities {
                if *durability == Durability::Flush
                    && (*pool_size != rep_pool || *payload_bytes != rep_payload)
                {
                    continue;
                }
                let durability_label = durability_label(*durability);
                let base_name = format!("bench-{pool_size}-{payload_bytes}-{durability_label}");
                let pool_path = work_dir.join(format!("{base_name}.plasmite"));

                let append = bench_append(
                    &pool_path,
                    *pool_size,
                    *payload_bytes,
                    args.messages,
                    *durability,
                )?;
                results.extend(append);

                let follow = bench_follow(
                    &work_dir,
                    &pool_path,
                    *pool_size,
                    *payload_bytes,
                    args.messages,
                    *durability,
                )?;
                results.push(follow);

                let get_scan = bench_get_scan(
                    &pool_path,
                    *pool_size,
                    *payload_bytes,
                    args.messages,
                    *durability,
                )?;
                results.extend(get_scan);

                for writers in &args.writers {
                    if *writers <= 1 {
                        continue;
                    }
                    if *durability == Durability::Flush && rep_writers != Some(*writers) {
                        continue;
                    }
                    let contention = bench_multi_writer(
                        &work_dir,
                        &pool_path,
                        *pool_size,
                        *payload_bytes,
                        args.messages,
                        *writers,
                        *durability,
                    )?;
                    results.push(contention);
                }
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
            "durabilities": args
                .durabilities
                .iter()
                .map(|d| durability_label(*d).to_string())
                .collect::<Vec<_>>(),
            "flush_sample": {
                "pool_size": rep_pool,
                "payload_bytes": rep_payload,
                "multi_writer_writers": rep_writers.map(|value| value as u64),
            },
            "work_dir": work_dir.display().to_string(),
            "debug_build": cfg!(debug_assertions),
            "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "results": results,
    });

    emit_bench_output(output, args.format)
}

fn warn_if_debug_build() -> Result<(), Error> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }

    let mut stderr = io::stderr().lock();
    writeln!(
        stderr,
        "plasmite-bench: debug build detected; for baseline numbers, run a release build"
    )
    .map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write debug build warning")
            .with_source(err)
    })?;
    writeln!(
        stderr,
        "  cargo build --release --example plasmite-bench && ./target/release/examples/plasmite-bench"
    )
    .map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write debug build hint")
            .with_source(err)
    })?;
    Ok(())
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
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
            );
            Ok(())
        }
        BenchFormat::Table => {
            emit_table(&value)?;
            Ok(())
        }
        BenchFormat::Both => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
            );
            emit_table(&value)?;
            Ok(())
        }
    }
}

fn emit_table(value: &Value) -> Result<(), Error> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "plasmite-bench (table)").map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write bench table")
            .with_source(err)
    })?;

    let results = value
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::new(ErrorKind::Internal).with_message("bench results missing"))?;

    let mut rows = Vec::new();
    for item in results {
        if let Some(row) = BenchRow::from_value(item) {
            rows.push(row);
        }
    }

    rows.sort_by(|a, b| {
        a.pool_size
            .cmp(&b.pool_size)
            .then(a.payload_bytes.cmp(&b.payload_bytes))
            .then(a.bench.cmp(&b.bench))
            .then(a.notes.cmp(&b.notes))
            .then(a.writers.cmp(&b.writers))
            .then(durability_rank(&a.durability).cmp(&durability_rank(&b.durability)))
    });

    let mut fast_baseline = BTreeMap::new();
    for row in &rows {
        if row.durability == "fast" {
            fast_baseline.insert(row.baseline_key(), row.ms_per_msg);
        }
    }

    let mut last_group: Option<(u64, u64)> = None;
    for row in &rows {
        let group_key = (row.pool_size, row.payload_bytes);
        if last_group != Some(group_key) {
            if last_group.is_some() {
                writeln!(stderr).map_err(|err| {
                    Error::new(ErrorKind::Io)
                        .with_message("failed to write bench table spacer")
                        .with_source(err)
                })?;
            }
            writeln!(
                stderr,
                "pool={} payload={}",
                format_bytes(row.pool_size),
                format_bytes(row.payload_bytes)
            )
            .map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to write bench table group header")
                    .with_source(err)
            })?;
            writeln!(
                stderr,
                "{:>16}  {:>6}  {:>7}  {:>10}  {:>10}  {:>8}  notes",
                "scenario", "dur", "writers", "ms/msg", "msgs/s", "x_fast"
            )
            .map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to write bench table header")
                    .with_source(err)
            })?;
            last_group = Some(group_key);
        }

        let rel = fast_baseline
            .get(&row.baseline_key())
            .and_then(|fast_ms| {
                if row.durability == "fast" || *fast_ms == 0.0 || fast_ms.is_nan() {
                    None
                } else {
                    Some(row.ms_per_msg / fast_ms)
                }
            })
            .map(|ratio| format!("{ratio:.2}x"))
            .unwrap_or_else(|| "-".to_string());

        let (ms_display, msgs_display) = format_rate(row.ms_per_msg, row.msgs_per_sec);
        writeln!(
            stderr,
            "{:>16}  {:>6}  {:>7}  {:>10}  {:>10}  {:>8}  {}",
            row.scenario(),
            row.durability,
            row.writers,
            ms_display,
            msgs_display,
            rel,
            row.notes_label()
        )
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to write bench table row")
                .with_source(err)
        })?;
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct BenchRow {
    bench: String,
    pool_size: u64,
    payload_bytes: u64,
    writers: u64,
    durability: String,
    ms_per_msg: f64,
    msgs_per_sec: f64,
    notes: String,
}

impl BenchRow {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            bench: value.get("bench")?.as_str()?.to_string(),
            pool_size: value.get("pool_size")?.as_u64()?,
            payload_bytes: value.get("payload_bytes")?.as_u64()?,
            writers: value.get("writers")?.as_u64()?,
            durability: value.get("durability")?.as_str()?.to_string(),
            ms_per_msg: value.get("ms_per_msg")?.as_f64()?,
            msgs_per_sec: value.get("msgs_per_sec")?.as_f64()?,
            notes: value
                .get("notes")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    fn scenario(&self) -> String {
        match self.bench.as_str() {
            "append" => {
                if self.notes.contains("Lite3 encode") {
                    "append+enc".to_string()
                } else {
                    "append".to_string()
                }
            }
            "get_scan" => {
                if self.notes.is_empty() {
                    "get_scan".to_string()
                } else {
                    match self.notes.as_str() {
                        "near_newest" => "get_scan:newest".to_string(),
                        "near_oldest" => "get_scan:oldest".to_string(),
                        "mid" => "get_scan:mid".to_string(),
                        other => format!("get_scan:{other}"),
                    }
                }
            }
            "multi_writer" => "multi_writer".to_string(),
            "follow" => "follow".to_string(),
            other => other.to_string(),
        }
    }

    fn notes_label(&self) -> String {
        match self.bench.as_str() {
            "append" => {
                if self.notes.contains("Lite3 encode") {
                    "Lite3 encode/msg".to_string()
                } else if self.notes.contains("core payload reused") {
                    "payload reused".to_string()
                } else {
                    self.notes.clone()
                }
            }
            "get_scan" => String::new(),
            "multi_writer" => "cross-process".to_string(),
            "follow" => "writer+follower".to_string(),
            _ => self.notes.clone(),
        }
    }

    fn baseline_key(&self) -> (u64, u64, String, u64, String) {
        (
            self.pool_size,
            self.payload_bytes,
            self.bench.clone(),
            self.writers,
            self.notes.clone(),
        )
    }
}

fn durability_rank(durability: &str) -> u8 {
    match durability {
        "fast" => 0,
        "flush" => 1,
        _ => 2,
    }
}

fn format_rate(ms_per_msg: f64, msgs_per_sec: f64) -> (String, String) {
    if ms_per_msg.is_nan() {
        return ("nan".to_string(), "-".to_string());
    }
    if ms_per_msg < 0.0005 {
        return ("<0.001".to_string(), "-".to_string());
    }
    (format!("{ms_per_msg:.3}"), format!("{msgs_per_sec:.0}"))
}

fn bench_append(
    pool_path: &Path,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
    durability: Durability,
) -> Result<Vec<Value>, Error> {
    let _ = std::fs::remove_file(pool_path);
    let mut pool = Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let payload_once = payload_for_bytes(payload_bytes, None, false)?;
    let start = Instant::now();
    for _ in 0..messages {
        append_with_durability(&mut pool, payload_once.as_slice(), durability)?;
    }
    let dur = start.elapsed();
    let core = result_entry(
        "append",
        pool_size,
        payload_bytes,
        messages,
        1,
        dur,
        durability,
        Some("core payload reused"),
    );

    let _ = std::fs::remove_file(pool_path);
    let mut pool = Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let start = Instant::now();
    for i in 0..messages {
        let is_done = i + 1 == messages;
        let payload = payload_for_bytes(payload_bytes, Some(i), is_done)?;
        append_with_durability(&mut pool, payload.as_slice(), durability)?;
    }
    let dur = start.elapsed();
    let end_to_end = result_entry(
        "append",
        pool_size,
        payload_bytes,
        messages,
        1,
        dur,
        durability,
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
    durability: Durability,
) -> Result<Value, Error> {
    let _ = std::fs::remove_file(pool_path);
    Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let durability_tag = durability_label(durability);
    let follower_out = work_dir.join(format!("follow-follower-{durability_tag}.json"));
    let writer_out = work_dir.join(format!("follow-writer-{durability_tag}.json"));

    let mut follower = spawn_worker(WorkerArgs {
        pool_path: pool_path.to_path_buf(),
        role: WorkerRole::Follower,
        messages,
        payload_bytes,
        out_json: follower_out.clone(),
        durability,
    })?;

    let mut writer = spawn_worker(WorkerArgs {
        pool_path: pool_path.to_path_buf(),
        role: WorkerRole::Writer,
        messages,
        payload_bytes,
        out_json: writer_out.clone(),
        durability,
    })?;

    let writer_status = writer.wait().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("writer wait failed")
            .with_source(err)
    })?;
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
    entry.insert(
        "durability".to_string(),
        json!(durability_label(durability)),
    );
    entry.insert("duration_ms".to_string(), json!(dur_ms));
    entry.insert("ms_per_msg".to_string(), json!(ms_per_msg));
    entry.insert(
        "latency_ms".to_string(),
        follower_json
            .get("latency_ms")
            .cloned()
            .unwrap_or(json!({})),
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
    durability: Durability,
) -> Result<Vec<Value>, Error> {
    let _ = std::fs::remove_file(pool_path);
    let mut pool = Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let payload_once = payload_for_bytes(payload_bytes, None, false)?;
    for _ in 0..messages {
        append_with_durability(&mut pool, payload_once.as_slice(), durability)?;
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
            durability,
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
    durability: Durability,
) -> Result<Value, Error> {
    let _ = std::fs::remove_file(pool_path);
    Pool::create(pool_path, PoolOptions::new(pool_size))?;

    let mut children = Vec::new();
    let suite_start = Instant::now();

    for idx in 0..writers {
        let durability_tag = durability_label(durability);
        let out_json = work_dir.join(format!("writer-{writers}-{idx}-{durability_tag}.json"));
        children.push(spawn_worker(WorkerArgs {
            pool_path: pool_path.to_path_buf(),
            role: WorkerRole::Writer,
            messages,
            payload_bytes,
            out_json,
            durability,
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
        durability,
        Some("cross-process writers"),
    ))
}

fn run_writer_worker(args: WorkerArgs) -> Result<(), Error> {
    let mut pool = Pool::open(&args.pool_path)?;

    let start = Instant::now();
    for i in 0..args.messages {
        let is_done = i + 1 == args.messages;
        let payload = payload_for_bytes(args.payload_bytes, Some(now_ns()? ^ i), is_done)?;
        append_with_durability(&mut pool, payload.as_slice(), args.durability)?;
    }
    let dur = start.elapsed();

    let output = json!({
        "role": "writer",
        "messages": args.messages,
        "payload_bytes": args.payload_bytes,
        "duration_ms": dur.as_millis() as u64,
        "durability": durability_label(args.durability),
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

fn payload_for_bytes(
    payload_bytes: usize,
    sent_ns: Option<u64>,
    done: bool,
) -> Result<lite3::Lite3Buf, Error> {
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
        .arg("--durability")
        .arg(durability_label(args.durability))
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
    let bytes = serde_json::to_vec_pretty(value).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("json encode failed")
            .with_source(err)
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| Error::new(ErrorKind::Io).with_path(parent).with_source(err))?;
    }
    std::fs::write(path, bytes)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))
}

#[allow(clippy::too_many_arguments)]
fn result_entry(
    bench: &str,
    pool_size: u64,
    payload_bytes: usize,
    messages: u64,
    writers: u64,
    duration: Duration,
    durability: Durability,
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
    map.insert(
        "durability".to_string(),
        json!(durability_label(durability)),
    );
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
    sorted
        .get(idx)
        .copied()
        .unwrap_or_else(|| sorted[sorted.len() - 1])
}

fn system_json() -> Value {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
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
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("clock went backwards")
                .with_source(err)
        })
}

fn rfc3339_now(ts: SystemTime) -> String {
    let dur = ts.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let nsec = dur.subsec_nanos();
    let tm =
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
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
        format!("{:.1}G", v / GB)
    } else if v >= MB {
        format!("{:.1}M", v / MB)
    } else if v >= KB {
        format!("{:.1}K", v / KB)
    } else {
        format!("{value}B")
    }
}

fn append_with_durability(
    pool: &mut Pool,
    payload: &[u8],
    durability: Durability,
) -> Result<u64, Error> {
    pool.append_with_options(payload, AppendOptions::new(0, durability))
}

fn durability_label(durability: Durability) -> &'static str {
    match durability {
        Durability::Fast => "fast",
        Durability::Flush => "flush",
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
