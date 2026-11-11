//! Generic duplex input, independent replies, and session termination.
pub mod support;
use plasmite::api::{Durability, Pool, PoolApiExt, PoolOptions};
use support::cli::*;

#[test]
fn duplex_receives_its_first_send_and_an_independent_reply_after_input_eof() {
    for remote in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.plasmite");
        let mut writer = Pool::create(&path, PoolOptions::new(1024 * 1024)).unwrap();
        let server =
            remote.then(|| ServeProcess::start_with_args_and_scheme(temp.path(), &[], "http"));
        let target = server
            .as_ref()
            .map_or_else(|| "events".to_owned(), |s| format!("{}/events", s.base_url));
        let mut child = KillOnDrop(
            cmd()
                .arg("--dir")
                .arg(temp.path())
                .args(["duplex", &target, "--timeout", "300ms", "--format", "json"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        writeln!(child.0.stdin.take().unwrap(), "{}", json!({"seq":900,"time":"2000-01-01T00:00:00Z","meta":{"tags":["request"]},"data":{"from":"alice","request":1}})).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        let request = loop {
            match writer.get_message(1) {
                Ok(message) => break message,
                Err(error)
                    if error.kind() == plasmite::api::ErrorKind::NotFound
                        && Instant::now() < until =>
                {
                    sleep(Duration::from_millis(5))
                }
                other => panic!("request was not appended: {other:?}"),
            }
        };
        assert_eq!(request.meta.tags, ["request"]);
        // The independent application replies only after seeing the request.
        writer
            .append_json_now(
                &json!({"from":"alice","reply":1}),
                &["reply".into()],
                Durability::Fast,
            )
            .unwrap();
        let status = wait_within(
            &mut child.0,
            Duration::from_secs(5),
            "duplex receive timeout after EOF",
        );
        let mut bytes = Vec::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(status.code(), Some(124), "remote={remote}");
        let messages = parse_json_lines(&bytes);
        assert_eq!(messages.len(), 2, "remote={remote}: {messages:?}");
        assert_eq!(messages[0]["seq"], 1);
        assert_eq!(messages[0]["data"], json!({"from":"alice","request":1}));
        assert_eq!(messages[0]["meta"]["tags"], json!(["request"]));
        assert_eq!(messages[1]["seq"], 2);
        assert_eq!(messages[1]["data"], json!({"from":"alice","reply":1}));
        assert_eq!(messages[1]["meta"]["tags"], json!(["reply"]));
    }
}

#[cfg(unix)]
#[test]
fn duplex_accepts_json_from_a_terminal_without_an_identity() {
    use std::os::fd::FromRawFd;
    let temp = tempfile::tempdir().unwrap();
    Pool::create(
        temp.path().join("events.plasmite"),
        PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let mut child = KillOnDrop(
        cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(["duplex", "events", "--timeout", "300ms", "--format", "json"])
            .stdin(slave)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    writeln!(master, "{{\"from\":\"application\",\"n\":1}}").unwrap();
    let status = wait_within(&mut child.0, Duration::from_secs(5), "terminal duplex");
    let mut bytes = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(status.code(), Some(124));
    assert_eq!(
        parse_json_lines(&bytes)[0]["data"],
        json!({"from":"application","n":1})
    );
}

#[test]
fn duplex_input_error_preserves_prior_appends_and_stops_reception() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("events.plasmite");
    let pool = Pool::create(&path, PoolOptions::new(1024 * 1024)).unwrap();
    let mut child = KillOnDrop(
        cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(["duplex", "events", "--format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"n\":1}\n{broken}\n")
        .unwrap();
    assert_eq!(
        wait_within(
            &mut child.0,
            Duration::from_secs(5),
            "malformed duplex input"
        )
        .code(),
        Some(2)
    );
    assert_eq!(pool.get_message(1).unwrap().data, json!({"n":1}));
    assert_eq!(pool.info().unwrap().bounds.newest_seq, Some(1));
}

#[test]
fn duplex_closed_stdout_ends_reception_with_input_still_open() {
    let temp = tempfile::tempdir().unwrap();
    let mut pool = Pool::create(
        temp.path().join("events.plasmite"),
        PoolOptions::new(1024 * 1024),
    )
    .unwrap();
    pool.append_json_now(&json!({"ready":true}), &[], Durability::Fast)
        .unwrap();
    let mut child = KillOnDrop(
        cmd()
            .arg("--dir")
            .arg(temp.path())
            .args(["duplex", "events", "--tail", "1", "--format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // Publish the seed only after closing stdout's read end. Stdin stays open.
    let stdout = child.0.stdout.take().unwrap();
    let (closed_tx, closed_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        drop(reader);
        closed_tx.send(line).unwrap();
    });
    let seed = closed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(parse_json(seed.trim())["data"], json!({"ready":true}));
    pool.append_json_now(&json!({"after_close":true}), &[], Durability::Fast)
        .unwrap();
    assert_eq!(
        wait_within(&mut child.0, Duration::from_secs(5), "duplex closed stdout").code(),
        Some(0)
    );
    assert_eq!(pool.info().unwrap().bounds.newest_seq, Some(2));
}
