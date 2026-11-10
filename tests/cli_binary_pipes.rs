//! Verify exact Lite3 payloads across local and HTTP CLI streams.
pub mod support;
use plasmite::api::{AppendOptions, Durability, Pool, PoolApiExt, PoolOptions, lite3};
use support::cli::*;

fn append(pool: &mut Pool, value: Value, tags: &[String]) -> Vec<u8> {
    let mut payload = lite3::encode_message(tags, &value)
        .unwrap()
        .as_slice()
        .to_vec();
    // Valid documents may retain unused bytes; a copying pipe must preserve them.
    payload.extend_from_slice(&[0, 0, 0]);
    pool.append_lite3(&payload, AppendOptions::new(12, Durability::Fast))
        .unwrap();
    payload
}

fn pipe(directory: &Path, source: &str, destination: &str, format: &str) -> std::process::Output {
    let mut reader = KillOnDrop(
        cmd()
            .args([
                "--dir",
                directory.to_str().unwrap(),
                "follow",
                source,
                "--tail",
                "2",
                "--no-follow",
                "--format",
                "lite3",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let writer = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "feed",
            destination,
            "--format",
            format,
        ])
        .stdin(Stdio::from(reader.0.stdout.take().unwrap()))
        .output()
        .unwrap();
    assert!(wait_within(&mut reader.0, Duration::from_secs(5), "binary reader").success());
    assert!(
        writer.status.success(),
        "{}",
        String::from_utf8_lossy(&writer.stderr)
    );
    writer
}

#[test]
fn local_and_http_binary_pipes_preserve_exact_bytes_and_assign_identity() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path();
    let mut source = Pool::create(
        directory.join("source.plasmite"),
        PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    let tags = vec!["sensor".to_string()];
    let original = [
        append(&mut source, json!({"temperature":20}), &tags),
        append(&mut source, json!({"temperature":21}), &tags),
    ];
    let server = ServeProcess::start(directory);
    for (name, source_ref, destination_ref) in [
        ("local", "source".to_string(), "local".to_string()),
        (
            "remote",
            format!("{}/source", server.local_url),
            format!("{}/remote", server.local_url),
        ),
    ] {
        let mut destination = Pool::create(
            directory.join(format!("{name}.plasmite")),
            PoolOptions::new(1024 * 1024),
        )
        .unwrap();
        append(&mut destination, json!({"existing":true}), &[]);
        let output = pipe(directory, &source_ref, &destination_ref, "json");
        let messages = parse_json_lines(&output.stdout);
        assert_eq!(messages.len(), 2);
        for (index, payload) in original.iter().enumerate() {
            let seq = index as u64 + 2;
            assert_eq!(destination.get_lite3(seq).unwrap().payload, *payload);
            assert_eq!(messages[index]["seq"], seq);
            assert_eq!(messages[index]["meta"]["tags"], json!(["sensor"]));
            assert_eq!(messages[index]["data"]["temperature"], 20 + index as u64);
            assert_ne!(messages[index]["time"], "1970-01-01T00:00:00.000000012Z");
        }
    }
}

#[test]
fn binary_feed_receipts_report_the_committed_identity_and_original_payload() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path();
    let mut source = Pool::create(
        directory.join("source.plasmite"),
        PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    let tags = vec!["sensor".to_string()];
    let payloads = [
        append(&mut source, json!({"reading":20}), &tags),
        append(&mut source, json!({"reading":21}), &tags),
    ];
    let server = ServeProcess::start(directory);
    for (name, target) in [
        ("local", "local".to_owned()),
        ("remote", format!("{}/remote", server.local_url)),
    ] {
        let mut pool = Pool::create(
            directory.join(format!("{name}.plasmite")),
            PoolOptions::new(1024 * 1024),
        )
        .unwrap();
        append(&mut pool, json!({"existing":true}), &[]);
        let output = pipe(directory, "source", &target, "lite3");
        let mut expected = b"PLS1".to_vec();
        for (seq, payload) in (2..).zip(&payloads) {
            let stored = pool.get_lite3(seq).unwrap();
            assert_eq!(&stored.payload, payload);
            assert_ne!(stored.timestamp_ns, 12, "feed assigns destination time");
            expected.extend_from_slice(&seq.to_be_bytes());
            expected.extend_from_slice(&stored.timestamp_ns.to_be_bytes());
            expected.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            expected.extend_from_slice(payload);
        }
        assert_eq!(output.stdout, expected);
    }
}

#[test]
fn framed_fetch_matches_local_and_http_and_truncation_keeps_prior_appends() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path();
    let mut source = Pool::create(
        directory.join("source.plasmite"),
        PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    let payload = append(&mut source, json!({"hello":"world"}), &[]);
    let destination = Pool::create(
        directory.join("destination.plasmite"),
        PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    let server = ServeProcess::start(directory);
    let local = cmd()
        .args([
            "--dir",
            directory.to_str().unwrap(),
            "fetch",
            "source",
            "1",
            "--format",
            "lite3",
        ])
        .output()
        .unwrap();
    let remote = cmd()
        .args([
            "fetch",
            &format!("{}/source", server.local_url),
            "1",
            "--format",
            "lite3",
        ])
        .output()
        .unwrap();
    assert!(local.status.success());
    assert!(remote.status.success());
    assert_eq!(local.stdout, remote.stdout);
    assert_eq!(&local.stdout[..4], b"PLS1");
    let stored = source.get_lite3(1).unwrap();
    assert_eq!(&local.stdout[4..12], &1u64.to_be_bytes());
    assert_eq!(&local.stdout[12..20], &stored.timestamp_ns.to_be_bytes());
    assert_eq!(&local.stdout[20..24], &(payload.len() as u32).to_be_bytes());
    assert_eq!(&local.stdout[24..], payload.as_slice());
    let mut truncated = local.stdout.clone();
    truncated.extend_from_slice(&[0, 0]); // The next frame's header ends early.
    let mut feed = KillOnDrop(
        cmd()
            .args(["--dir", directory.to_str().unwrap(), "feed", "destination"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    feed.0.stdin.take().unwrap().write_all(&truncated).unwrap();
    assert!(!wait_within(&mut feed.0, Duration::from_secs(5), "truncated stream").success());
    assert_eq!(destination.get_lite3(1).unwrap().payload, payload);
    assert!(destination.get_lite3(2).is_err());
}
