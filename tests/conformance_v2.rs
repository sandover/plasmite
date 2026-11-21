//! Run shared 2.0 cases through the installed CLI and the Rust API.
pub mod support;

use plasmite::api::{Durability, Feed, LocalClient, Pool, PoolOptions, PoolRef, Read, Since};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use support::cli::{cmd, parse_json_lines};

#[derive(Deserialize)]
struct Cases {
    seed: Vec<Seed>,
    fetch: Vec<ReadCase>,
    single_fetch: Vec<ReadCase>,
    follow: Vec<ReadCase>,
    fetch_snapshot: ReadCase,
    retention: RetentionCase,
    pools: PoolCases,
    previous_release: PreviousRelease,
}

#[derive(Deserialize)]
struct Seed {
    data: Value,
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ReadCase {
    name: String,
    since: Option<Value>,
    last: Option<u64>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    expect: Vec<u64>,
    error: Option<String>,
    cli_error: Option<String>,
    #[serde(default)]
    after_open: Vec<Seed>,
}

#[derive(Deserialize)]
struct RetentionCase {
    size: u64,
    inserts: u64,
    padding_bytes: usize,
    since: u64,
    tags: Vec<String>,
    error: String,
}

#[derive(Deserialize)]
struct PoolCases {
    missing: String,
    existing: String,
    non_object_feed: String,
}

#[derive(Deserialize)]
struct PreviousRelease {
    fixture: String,
    messages: Vec<Seed>,
}

fn cases() -> Cases {
    serde_json::from_str(include_str!("fixtures/conformance/reads.json"))
        .expect("parse shared conformance cases")
}

fn seed_cli(dir: &Path, seeds: &[Seed]) {
    let created = cmd()
        .arg("--dir")
        .arg(dir)
        .args(["pool", "create", "events"])
        .output()
        .expect("create a conformance pool");
    assert!(created.status.success(), "{created:?}");
    for seed in seeds {
        let mut writer = cmd();
        writer
            .arg("--dir")
            .arg(dir)
            .args(["feed", "events", &seed.data.to_string()]);
        for tag in &seed.tags {
            writer.args(["--tag", tag]);
        }
        let output = writer.output().expect("seed conformance pool");
        assert!(output.status.success(), "{output:?}");
    }
}

fn check_cli_fetch(dir: &Path, case: &ReadCase, single: bool) {
    let mut fetch = cmd();
    fetch.arg("--dir").arg(dir).args(["fetch", "events"]);
    if let Some(since) = &case.since {
        if !single {
            fetch.arg("--since");
        }
        let since_arg = since
            .as_str()
            .map_or_else(|| since.to_string(), str::to_owned);
        fetch.arg(since_arg);
    }
    if let Some(last) = case.last {
        fetch.args(["--last", &last.to_string()]);
    }
    for tag in &case.tags {
        fetch.args(["--tag", tag]);
    }
    let output = fetch.output().expect("run CLI fetch");
    match case.cli_error.as_ref().or(case.error.as_ref()) {
        Some(kind) => {
            assert!(!output.status.success(), "{}", case.name);
            let errors = parse_json_lines(&output.stderr);
            assert_eq!(errors[0]["error"]["kind"], *kind, "{}", case.name);
        }
        None => {
            assert!(output.status.success(), "{}: {output:?}", case.name);
            let got: Vec<_> = parse_json_lines(&output.stdout)
                .iter()
                .map(|message| message["seq"].as_u64().expect("message sequence"))
                .collect();
            assert_eq!(got, case.expect, "{}", case.name);
        }
    }
}

#[test]
fn cli_fetch_contract() {
    let cases = cases();
    let dir = tempfile::tempdir().expect("conformance directory");
    seed_cli(dir.path(), &cases.seed);
    for case in &cases.fetch {
        check_cli_fetch(dir.path(), case, false);
    }
    for case in &cases.single_fetch {
        check_cli_fetch(dir.path(), case, true);
    }
}

#[test]
fn cli_empty_oldest_and_single_sequence_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut pool =
        Pool::create(dir.path().join("events.plasmite"), PoolOptions::new(16384)).unwrap();
    let run = |args: &[&str]| {
        cmd()
            .arg("--dir")
            .arg(dir.path())
            .args(args)
            .output()
            .unwrap()
    };
    let empty = run(&["fetch", "events", "--since", "oldest"]);
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let too_far = run(&["fetch", "events", "5"]);
    let error = &parse_json_lines(&too_far.stderr)[0]["error"];
    assert_eq!(error["kind"], "Usage");
    assert!(error["message"].as_str().unwrap().starts_with("SEQ"));
    let large = serde_json::json!({"padding":"x".repeat(2048)});
    for _ in 0..32 {
        pool.feed(&large, Feed::new()).unwrap();
    }
    let dropped = run(&["fetch", "events", "1"]);
    let error = &parse_json_lines(&dropped.stderr)[0]["error"];
    assert_eq!(error["kind"], "RetentionGap");
    assert_eq!(error["missing"], 1);
    assert_eq!(
        error["next_seq"].as_u64(),
        pool.info().unwrap().bounds.oldest_seq
    );
}

#[test]
fn cli_forwarding_accepts_offset_times_and_rejects_zero_sequence() {
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::create(dir.path().join("events.plasmite"), PoolOptions::new(65536)).unwrap();
    for (seq, time, valid) in [
        (7, "2000-01-01T05:00:00+05:00", true),
        (8, "2000-01-01T00:00:00Z", true),
        (0, "2000-01-01T00:00:00Z", false),
        (7, "yesterday", false),
    ] {
        let input = json!({"seq":seq,"time":time,"meta":{"tags":["source"]},"data":{"value":9}});
        let output = cmd()
            .arg("--dir")
            .arg(dir.path())
            .args(["feed", "events", &input.to_string()])
            .output()
            .unwrap();
        assert_eq!(output.status.success(), valid, "{output:?}");
    }
    let records = pool
        .fetch(Read::new())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        records.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        records
            .iter()
            .all(|m| *m.data().unwrap() == json!({"value":9}) && m.tags().unwrap() == ["source"])
    );
}

#[test]
fn cli_follow_one_resumes_and_exits_on_first_matching_message() {
    use std::{process::Stdio, time::Duration};
    use support::cli::{KillOnDrop, read_line_with_timeout, wait_within};

    let cases = cases();
    let dir = tempfile::tempdir().expect("conformance directory");
    seed_cli(dir.path(), &cases.seed);
    let mut child = KillOnDrop(
        cmd()
            .arg("--dir")
            .arg(dir.path())
            .args(["follow", "events", "--since", "2", "--tag", "gfx", "--one"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("start follow"),
    );
    let line = read_line_with_timeout(
        child.0.stdout.take().expect("stdout"),
        Duration::from_secs(5),
    );
    let messages = parse_json_lines(line.as_bytes());
    assert_eq!(messages[0]["seq"], 3);
    assert!(wait_within(&mut child.0, Duration::from_secs(5), "follow --one").success());
}

fn rust_options(case: &ReadCase) -> Read {
    let start = match (&case.since, case.last) {
        (None, None) => Read::new(),
        (None, Some(count)) => Read::last(count),
        (Some(position), None) => {
            let since = if let Some(seq) = position.as_u64() {
                Since::Seq(seq)
            } else {
                position
                    .as_str()
                    .expect("position in conformance case")
                    .parse::<Since>()
                    .expect("valid textual starting position")
            };
            Read::since(since)
        }
        (Some(_), Some(_)) => unreachable!("Rust can express only one start"),
    };
    start.tags(case.tags.iter().cloned())
}

fn seed_rust(writer: &mut Pool, seeds: &[Seed]) {
    for seed in seeds {
        writer
            .feed(&seed.data, Feed::new().tags(seed.tags.clone()))
            .expect("seed pool");
    }
}

#[test]
fn rust_read_contract() {
    let cases = cases();
    let dir = tempfile::tempdir().expect("conformance directory");
    let path = dir.path().join("events.plasmite");
    let mut writer = Pool::create(&path, PoolOptions::new(65536)).expect("create pool");
    seed_rust(&mut writer, &cases.seed);
    let reader = Pool::open(&path).expect("open pool");
    for (case, single) in cases
        .fetch
        .iter()
        .map(|case| (case, false))
        .chain(cases.single_fetch.iter().map(|case| (case, true)))
    {
        if case.since.is_some() && case.last.is_some() {
            continue;
        }
        match (reader.fetch(rust_options(case)), &case.error) {
            (Ok(stream), None) => {
                let limit = if single { 1 } else { usize::MAX };
                let seqs: Vec<_> = stream
                    .take(limit)
                    .map(|message| message.expect("read").seq)
                    .collect();
                assert_eq!(seqs, case.expect, "{}", case.name);
            }
            (Err(error), Some(kind)) => {
                assert_eq!(format!("{:?}", error.kind()), *kind, "{}", case.name)
            }
            (Err(error), None) => panic!("{}: {error}", case.name),
            (Ok(_), Some(kind)) => panic!("{}: expected {kind}", case.name),
        }
    }

    let snapshot = &cases.fetch_snapshot;
    let stream = reader
        .fetch(rust_options(snapshot))
        .expect("open finite reader");
    seed_rust(&mut writer, &snapshot.after_open);
    let seqs: Vec<_> = stream.map(|message| message.expect("read").seq).collect();
    assert_eq!(seqs, snapshot.expect, "{}", snapshot.name);

    // Each follow case starts with the same seed and may add its own live messages.
    for (index, case) in cases.follow.iter().enumerate() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        use std::time::Duration;

        let path = dir.path().join(format!("follow-{index}.plasmite"));
        let mut writer = Pool::create(&path, PoolOptions::new(65536)).expect("create pool");
        seed_rust(&mut writer, &cases.seed);
        let reader = Pool::open(&path).expect("open pool");
        let stop = Arc::new(AtomicBool::new(false));
        let stream = reader
            .follow(rust_options(case).cancel(stop.clone()))
            .expect("open live reader");
        seed_rust(&mut writer, &case.after_open);
        let (done, wait) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if wait.recv_timeout(Duration::from_secs(3)).is_err() {
                stop.store(true, Ordering::Release);
            }
        });
        let seqs: Vec<_> = stream
            .take(case.expect.len())
            .map(|message| message.expect("read").seq)
            .collect();
        let _ = done.send(());
        watchdog.join().expect("wait for follow watchdog");
        assert_eq!(seqs, case.expect, "{}", case.name);
    }
}

#[test]
fn rust_retention_does_not_hide_loss_on_a_filtered_reader() {
    let case = cases().retention;
    let dir = tempfile::tempdir().expect("conformance directory");
    let path = dir.path().join("events.plasmite");
    let mut writer = Pool::create(&path, PoolOptions::new(case.size)).expect("create pool");
    writer
        .feed(
            &serde_json::json!({"point": [10, 20]}),
            Feed::new().tags(case.tags.clone()),
        )
        .expect("seed pool");
    let reader = Pool::open(&path).expect("open pool");
    let read = Read::since(Since::Seq(case.since)).tags(case.tags.clone());
    let mut open = reader.fetch(read.clone()).expect("open before overwrite");
    let padding = serde_json::json!({"padding": "x".repeat(case.padding_bytes)});
    for _ in 0..case.inserts {
        writer
            .feed(&padding, Feed::new())
            .expect("overwrite history");
    }
    let oldest = reader
        .info()
        .expect("inspect pool")
        .bounds
        .oldest_seq
        .expect("retained data");
    assert!(oldest > case.since);
    let late_error = open
        .next()
        .expect("report lost snapshot")
        .expect_err("snapshot gap");
    let repeated = open
        .next()
        .expect("keep the failure visible")
        .expect_err("snapshot gap");
    assert_eq!(repeated.kind(), late_error.kind());
    assert_eq!(repeated.next_seq(), late_error.next_seq());
    let start_error = match reader.follow(read.clone()) {
        Err(error) => error,
        Ok(_) => panic!("opening a stale cursor should report a gap"),
    };
    let lookup_error = match reader.fetch(read) {
        Err(error) => error,
        Ok(_) => panic!("looking up a stale sequence should report a gap"),
    };
    for error in [late_error, start_error, lookup_error] {
        assert_eq!(format!("{:?}", error.kind()), case.error);
        assert_eq!(error.missing(), Some(case.since));
        assert_eq!(error.next_seq(), Some(oldest));
    }
}

#[test]
fn rust_feed_and_pool_contract() {
    let cases = cases();
    let dir = tempfile::tempdir().expect("conformance directory");
    let client = LocalClient::new().with_pool_dir(dir.path().join("pools"));
    let source = PoolRef::name("source");
    let destination = PoolRef::name("destination");
    let missing = match client.open_pool(&source, false) {
        Err(error) => error,
        Ok(_) => panic!("opening a missing pool should fail"),
    };
    assert_eq!(format!("{:?}", missing.kind()), cases.pools.missing);

    let mut source_pool = client
        .create_pool(&source, PoolOptions::new(65536))
        .expect("create source pool");
    let mut destination_pool = client
        .open_pool(&destination, true)
        .expect("create missing destination");
    let existing = match client.create_pool(&source, PoolOptions::new(65536)) {
        Err(error) => error,
        Ok(_) => panic!("creating an existing pool should fail"),
    };
    assert_eq!(format!("{:?}", existing.kind()), cases.pools.existing);
    let source_size = client
        .open_pool(&source, true)
        .expect("open existing pool")
        .info()
        .expect("inspect source")
        .file_size;
    assert_eq!(source_size, 65536);

    let rejected = source_pool
        .feed(&Value::Bool(true), Feed::new())
        .expect_err("reject scalar");
    assert_eq!(
        format!("{:?}", rejected.kind()),
        cases.pools.non_object_feed
    );
    let seed = &cases.seed[0];
    let sent = source_pool
        .feed(&seed.data, Feed::new().tags(seed.tags.clone()))
        .expect("store canvas message");
    assert_eq!(sent.seq, 1);
    assert_eq!(sent.data().unwrap(), &seed.data);
    assert_eq!(sent.tags().unwrap(), seed.tags);
    assert!(sent.time > 0);
    seed_rust(&mut destination_pool, &cases.seed);
    let copied = destination_pool
        .feed(&sent, Feed::new().durability(Durability::Flush))
        .expect("forward message");
    assert_eq!(copied.seq, 4);
    assert_eq!(copied.data().unwrap(), sent.data().unwrap());
    assert_eq!(copied.tags().unwrap(), sent.tags().unwrap());
    assert_eq!(
        destination_pool
            .fetch(Read::since(Since::Seq(copied.seq)))
            .expect("open destination")
            .next()
            .expect("read copy")
            .expect("copy"),
        copied
    );
    assert_eq!(client.list_pools().expect("list pools").len(), 2);
    drop(source_pool);
    client.delete_pool(&source).expect("delete source");
    assert_eq!(client.list_pools().expect("list pools").len(), 1);
}

#[test]
fn reads_pool_from_last_1x_release() {
    let fixture = cases().previous_release;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture.fixture);
    let dir = tempfile::tempdir().expect("conformance directory");
    let copy = dir.path().join("compat.plasmite");
    std::fs::copy(path, &copy).expect("copy 1.x pool");
    let mut pool = LocalClient::new()
        .open_pool(&PoolRef::Path(copy), false)
        .expect("open 1.x pool");
    let messages: Vec<_> = pool
        .fetch(Read::new())
        .expect("open 1.x read")
        .collect::<Result<_, _>>()
        .expect("read 1.x records");
    assert_eq!(messages.len(), fixture.messages.len());
    for (i, (message, seed)) in messages.iter().zip(fixture.messages).enumerate() {
        assert_eq!(message.seq, i as u64 + 1);
        assert_eq!(message.data().unwrap(), &seed.data);
        assert_eq!(message.tags().unwrap(), seed.tags);
        assert!(message.time > 0);
    }
    let forwarded = pool.feed(&messages[0], Feed::new()).unwrap();
    assert_eq!(forwarded.seq, messages.len() as u64 + 1);
    assert_eq!(forwarded.data().unwrap(), messages[0].data().unwrap());
    assert_eq!(forwarded.tags().unwrap(), messages[0].tags().unwrap());
    let error = pool
        .feed(&messages[0], Feed::new().tags(["override"]))
        .unwrap_err();
    assert_eq!(error.kind(), plasmite::api::ErrorKind::Usage);
    let mut history = pool.fetch(Read::last(1)).unwrap();
    assert_eq!(history.next_message().unwrap().unwrap(), forwarded);
}
