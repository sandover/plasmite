//! Real-process UDP input checks using loopback sockets and ordinary pool readers.

#[cfg(unix)]
use fs2::FileExt;
use plasmite::api::{Pool, PoolApiExt};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputStream {
    Stdout,
    Stderr,
}

struct OutputLine {
    stream: OutputStream,
    text: String,
}

struct TestHome {
    _temp: TempDir,
    home: PathBuf,
    pool_dir: PathBuf,
    ready_file: PathBuf,
}

impl TestHome {
    fn new() -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        let pool_dir = temp.path().join("pools");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&pool_dir)?;
        let ready_file = temp.path().join("remote-address");
        Ok(Self {
            _temp: temp,
            home,
            pool_dir,
            ready_file,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .env("HOME", &self.home)
            .env("PLASMITE_ACCESS_HOME", &self.home)
            .env("RUST_LOG", "info");
        command
    }

    fn create_pool(&self, pool_dir: &Path, name: &str) -> TestResult<()> {
        self.create_pool_with_size(pool_dir, name, None)
    }

    fn create_pool_with_size(
        &self,
        pool_dir: &Path,
        name: &str,
        size: Option<&str>,
    ) -> TestResult<()> {
        let mut command = self.command();
        command.arg("--dir").arg(pool_dir).arg("pool").arg("create");
        if let Some(size) = size {
            command.args(["--size", size]);
        }
        let output = command.args(["--format", "json", name]).output()?;
        if !output.status.success() {
            return Err(format!(
                "pool create {name} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    }

    fn pool_path(&self, pool_dir: &Path, name: &str) -> PathBuf {
        pool_dir.join(format!("{name}.plasmite"))
    }

    fn serve_args(&self, with_dir: bool, udp_bind: Option<&str>) -> Command {
        let mut command = self.command();
        if with_dir {
            command.arg("--dir").arg(&self.pool_dir);
        }
        command
            .arg("serve")
            .args(["--bind", "127.0.0.1:0"])
            .args(["--remote-bind", "127.0.0.1:0"]);
        if let Some(udp_bind) = udp_bind {
            command.arg(format!("--udp={udp_bind}"));
        }
        command
    }
}

struct UdpServer {
    child: Child,
    lines: Receiver<OutputLine>,
    combined_log: String,
    stderr_log: String,
    stdout_log: String,
    udp_addr: Option<SocketAddr>,
}

impl UdpServer {
    fn start(home: &TestHome, with_dir: bool, udp_bind: Option<&str>) -> TestResult<Self> {
        let mut command = home.serve_args(with_dir, udp_bind);
        command
            .env("PLASMITE_SERVE_READY_FILE", &home.ready_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let (sender, lines) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            spawn_line_reader(stdout, OutputStream::Stdout, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_line_reader(stderr, OutputStream::Stderr, sender.clone());
        }
        drop(sender);

        let mut server = Self {
            child,
            lines,
            combined_log: String::new(),
            stderr_log: String::new(),
            stdout_log: String::new(),
            udp_addr: None,
        };
        if udp_bind.is_some() {
            server.udp_addr = Some(server.wait_for_udp_banner(Duration::from_secs(10))?);
        }
        let remote_addr = server.wait_for_ready_file(&home.ready_file, Duration::from_secs(10))?;
        std::net::TcpStream::connect_timeout(&remote_addr, Duration::from_secs(1))?;
        if udp_bind.is_none() {
            server.collect_output(Duration::from_millis(50));
        }
        Ok(server)
    }

    fn wait_for_udp_banner(&mut self, timeout: Duration) -> TestResult<SocketAddr> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = self.receive_until(deadline)? {
                if line.stream == OutputStream::Stderr
                    && let Some(address) = parse_udp_banner(&line.text)
                {
                    return Ok(address);
                }
            }
            self.ensure_running("UDP startup banner")?;
        }
    }

    fn wait_for_ready_file(&mut self, path: &Path, timeout: Duration) -> TestResult<SocketAddr> {
        let deadline = Instant::now() + timeout;
        loop {
            match fs::read_to_string(path) {
                Ok(text) => return Ok(text.trim().parse()?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.ensure_running("remote ready file")?;
            if Instant::now() >= deadline {
                return Err(format!(
                    "server did not publish its remote ready file; output:\n{}",
                    self.combined_log
                )
                .into());
            }
            self.receive_until((Instant::now() + Duration::from_millis(20)).min(deadline))?;
        }
    }

    fn receive_until(&mut self, deadline: Instant) -> TestResult<Option<OutputLine>> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        match self.lines.recv_timeout(remaining) {
            Ok(line) => {
                self.combined_log.push_str(&line.text);
                self.combined_log.push('\n');
                match line.stream {
                    OutputStream::Stdout => {
                        self.stdout_log.push_str(&line.text);
                        self.stdout_log.push('\n');
                    }
                    OutputStream::Stderr => {
                        self.stderr_log.push_str(&line.text);
                        self.stderr_log.push('\n');
                    }
                }
                Ok(Some(line))
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn ensure_running(&mut self, phase: &str) -> TestResult<()> {
        if let Some(status) = self.child.try_wait()? {
            self.drain_available();
            return Err(format!(
                "server exited during {phase} ({status}); output:\n{}",
                self.combined_log
            )
            .into());
        }
        Ok(())
    }

    fn drain_available(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.combined_log.push_str(&line.text);
            self.combined_log.push('\n');
            match line.stream {
                OutputStream::Stdout => {
                    self.stdout_log.push_str(&line.text);
                    self.stdout_log.push('\n');
                }
                OutputStream::Stderr => {
                    self.stderr_log.push_str(&line.text);
                    self.stderr_log.push('\n');
                }
            }
        }
    }

    fn collect_output(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    self.combined_log.push_str(&line.text);
                    self.combined_log.push('\n');
                    match line.stream {
                        OutputStream::Stdout => {
                            self.stdout_log.push_str(&line.text);
                            self.stdout_log.push('\n');
                        }
                        OutputStream::Stderr => {
                            self.stderr_log.push_str(&line.text);
                            self.stderr_log.push('\n');
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn assert_running(&mut self) -> TestResult<()> {
        self.ensure_running("running check")
    }

    #[cfg(unix)]
    fn stop_gracefully(&mut self, timeout: Duration) -> TestResult<()> {
        self.request_shutdown()?;
        self.wait_for_exit(timeout)
    }

    #[cfg(unix)]
    fn request_shutdown(&mut self) -> TestResult<()> {
        let result = unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    #[cfg(unix)]
    fn wait_for_exit(&mut self, timeout: Duration) -> TestResult<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.child.try_wait()?.is_some() {
                self.collect_output(Duration::from_millis(200));
                return Ok(());
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.collect_output(Duration::from_millis(200));
                return Err(format!(
                    "server did not stop after SIGTERM; output:\n{}",
                    self.combined_log
                )
                .into());
            }
            self.receive_until((Instant::now() + Duration::from_millis(20)).min(deadline))?;
        }
    }

    #[cfg(not(unix))]
    fn stop_gracefully(&mut self, _timeout: Duration) -> TestResult<()> {
        self.child.kill()?;
        self.child.wait()?;
        self.collect_output(Duration::from_millis(200));
        Ok(())
    }
}

impl Drop for UdpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_line_reader(
    reader: impl std::io::Read + Send + 'static,
    stream: OutputStream,
    sender: mpsc::Sender<OutputLine>,
) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(text) = line else { break };
            if sender.send(OutputLine { stream, text }).is_err() {
                break;
            }
        }
    });
}

fn parse_udp_banner(line: &str) -> Option<SocketAddr> {
    let rest = line.split_once("UDP input: ")?.1;
    rest.split_whitespace().next()?.parse().ok()
}

fn create_fixture(home: &TestHome, pool_dir: &Path, name: &str) -> TestResult<()> {
    home.create_pool(pool_dir, name)
}

fn send(socket: &UdpSocket, address: SocketAddr, pool: &str, body: &[u8]) -> TestResult<()> {
    let mut payload = Vec::with_capacity(pool.len() + 1 + body.len());
    payload.extend_from_slice(pool.as_bytes());
    payload.push(0);
    payload.extend_from_slice(body);
    let written = socket.send_to(&payload, address).map_err(|error| {
        format!(
            "sending a {}-byte UDP payload to {address} failed: {error}",
            payload.len()
        )
    })?;
    assert_eq!(
        written,
        payload.len(),
        "the UDP sender truncated a datagram"
    );
    Ok(())
}

fn allow_large_udp_datagrams(socket: &UdpSocket) -> TestResult<()> {
    socket2::SockRef::from(socket).set_send_buffer_size(128 * 1024)?;
    Ok(())
}

fn wait_for_sequence(path: &Path, expected: u64, timeout: Duration) -> TestResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let pool = Pool::open(path)?;
        if pool.bounds()?.newest_seq == Some(expected) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pool {} did not reach sequence {expected}; bounds were {:?}",
                path.display(),
                pool.bounds()?
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn newest_sequence(path: &Path) -> TestResult<Option<u64>> {
    Ok(Pool::open(path)?.bounds()?.newest_seq)
}

fn message(path: &Path, seq: u64) -> TestResult<plasmite::api::Message> {
    Ok(Pool::open(path)?.get_message(seq)?)
}

fn raw_lite3(path: &Path, seq: u64) -> TestResult<Vec<u8>> {
    Ok(Pool::open(path)?.get_lite3(seq)?.payload)
}

fn cli_lite3_fixture(home: &TestHome, pool_dir: &Path) -> TestResult<Vec<u8>> {
    create_fixture(home, pool_dir, "lite3-source")?;
    let feed = home
        .command()
        .arg("--dir")
        .arg(pool_dir)
        .args([
            "feed",
            "lite3-source",
            "--data",
            r#"{"fixture":"udp","sample":41}"#,
            "--tag",
            "fixture",
            "--tag",
            "binary",
            "--format",
            "json",
        ])
        .output()?;
    if !feed.status.success() {
        return Err(format!(
            "feed Lite3 fixture failed: {}",
            String::from_utf8_lossy(&feed.stderr)
        )
        .into());
    }
    let fetch = home
        .command()
        .arg("--dir")
        .arg(pool_dir)
        .args(["fetch", "lite3-source", "1", "--format", "lite3-raw"])
        .output()?;
    if !fetch.status.success() {
        return Err(format!(
            "fetch raw Lite3 fixture failed: {}",
            String::from_utf8_lossy(&fetch.stderr)
        )
        .into());
    }
    assert_eq!(fetch.stdout.first(), Some(&0x06));
    plasmite::api::lite3::validate_bytes(&fetch.stdout)?;
    Ok(fetch.stdout)
}

fn json_body_with_length(body_len: usize) -> Vec<u8> {
    const PREFIX: &[u8] = b"{\"data\":{\"padding\":\"";
    const SUFFIX: &[u8] = b"\"},\"tags\":[\"boundary\"]}";
    assert!(body_len >= PREFIX.len() + SUFFIX.len());
    let padding_len = body_len - PREFIX.len() - SUFFIX.len();
    let mut body = Vec::with_capacity(body_len);
    body.extend_from_slice(PREFIX);
    body.resize(body.len() + padding_len, b'a');
    body.extend_from_slice(SUFFIX);
    assert_eq!(body.len(), body_len);
    body
}

fn lite3_body_with_length(body_len: usize) -> TestResult<Vec<u8>> {
    let tags = vec!["boundary".to_string()];
    let encoded_len = |padding_len: usize| -> TestResult<usize> {
        let data = json!({"padding": "a".repeat(padding_len)});
        Ok(plasmite::api::lite3::encode_message(&tags, &data)?.len())
    };

    let (mut low, mut high) = (0usize, body_len);
    while low < high {
        let mid = low + (high - low) / 2;
        if encoded_len(mid)? < body_len {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let start = low.saturating_sub(24);
    let end = (low + 24).min(body_len);
    for padding_len in start..=end {
        let data = json!({"padding": "a".repeat(padding_len)});
        let encoded = plasmite::api::lite3::encode_message(&tags, &data)?;
        if encoded.len() == body_len {
            return Ok(encoded.as_slice().to_vec());
        }
    }
    Err(format!("could not encode a Lite3 object of exactly {body_len} bytes").into())
}

fn local_http_address(pool_dir: &Path) -> TestResult<SocketAddr> {
    let local_file = pool_dir.join(".plasmite-serve/local.json");
    let local_text: String = serde_json::from_slice(&fs::read(local_file)?)?;
    Ok(local_text.parse()?)
}

fn assert_local_http_health(pool_dir: &Path) -> TestResult<()> {
    let local_address = local_http_address(pool_dir)?;
    let response = ureq::get(&format!("http://{local_address}/healthz")).call()?;
    assert_eq!(response.status(), 200);
    let value: Value = serde_json::from_str(&response.into_string()?)?;
    assert_eq!(value["ok"], json!(true));
    Ok(())
}

fn assert_http_pool_readers(pool_dir: &Path, lite3: &[u8]) -> TestResult<()> {
    let address = local_http_address(pool_dir)?;
    let json_response = ureq::get(&format!("http://{address}/v0/pools/json/messages/1")).call()?;
    let json_message: Value = serde_json::from_str(&json_response.into_string()?)?;
    assert_eq!(json_message["message"]["data"]["sample"], 81);
    assert_eq!(
        json_message["message"]["meta"]["tags"],
        json!(["sensor", " udp ", "sensor"])
    );

    let lite_response =
        ureq::get(&format!("http://{address}/v0/pools/lite/messages/1/lite3")).call()?;
    assert_eq!(lite_response.header("plasmite-seq"), Some("1"));
    assert_eq!(
        lite_response.header("content-type"),
        Some("application/x-plasmite-lite3")
    );
    let mut lite_bytes = Vec::new();
    lite_response.into_reader().read_to_end(&mut lite_bytes)?;
    assert_eq!(lite_bytes, lite3);
    Ok(())
}

#[cfg(unix)]
fn log_counter(log: &str, field: &str) -> Option<u64> {
    let log = strip_ansi_codes(log);
    let marker = format!("{field}=");
    log.split_whitespace()
        .find_map(|part| part.strip_prefix(&marker))
        .and_then(|value| {
            value
                .trim_matches(|character: char| !character.is_ascii_digit())
                .parse()
                .ok()
        })
}

#[cfg(unix)]
fn strip_ansi_codes(text: &str) -> String {
    let mut characters = text.chars().peekable();
    let mut plain = String::with_capacity(text.len());
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && characters.peek() == Some(&'[') {
            characters.next();
            for character in characters.by_ref() {
                if ('@'..='~').contains(&character) {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

#[test]
fn one_server_appends_mixed_formats_and_keeps_receiving_after_bad_datagrams() -> TestResult<()> {
    let home = TestHome::new()?;
    for pool in [
        "json",
        "lite",
        "canary",
        "温度",
        "replace",
        "replace-barrier",
        "outside",
    ] {
        let dir = if pool == "outside" {
            home.pool_dir.parent().expect("fixture parent")
        } else {
            &home.pool_dir
        };
        create_fixture(&home, dir, pool)?;
    }
    #[cfg(unix)]
    for pool in [r"back\slash", "C:outside"] {
        create_fixture(&home, &home.pool_dir, pool)?;
    }
    home.create_pool_with_size(&home.pool_dir, "small", Some("64K"))?;
    let fixture = cli_lite3_fixture(&home, &home.pool_dir)?;
    let mut server = UdpServer::start(&home, true, Some("127.0.0.1:0"))?;
    let address = server.udp_addr.expect("UDP address from startup banner");
    assert_eq!(address.ip().to_string(), "127.0.0.1");
    assert!(server.stderr_log.contains("UDP input:"));
    assert_local_http_health(&home.pool_dir)?;

    let socket = UdpSocket::bind("127.0.0.1:0")?;
    allow_large_udp_datagrams(&socket)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let json_body = br#"{"data":{"temperature_c":22.4,"sample":81,"text":"before\u0000after"},"tags":["sensor"," udp ","sensor"]}"#;
    send(&socket, address, "json", json_body)?;
    wait_for_sequence(
        &home.pool_path(&home.pool_dir, "json"),
        1,
        Duration::from_secs(3),
    )?;
    send(&socket, address, "lite", &fixture)?;
    if let Err(error) = wait_for_sequence(
        &home.pool_path(&home.pool_dir, "lite"),
        1,
        Duration::from_secs(3),
    ) {
        let stop_result = server.stop_gracefully(Duration::from_secs(5));
        return Err(format!(
            "Lite3 UDP append did not arrive: {error}; stop result: {stop_result:?}; logs:\n{}",
            server.combined_log
        )
        .into());
    }

    let json_message = message(&home.pool_path(&home.pool_dir, "json"), 1)?;
    assert_eq!(json_message.seq, 1);
    assert_eq!(json_message.data["temperature_c"], json!(22.4));
    assert_eq!(json_message.data["sample"], json!(81));
    assert_eq!(json_message.data["text"], "before\0after");
    assert_eq!(json_message.meta.tags, ["sensor", " udp ", "sensor"]);
    assert!(!json_message.time.is_empty());
    let lite_message = message(&home.pool_path(&home.pool_dir, "lite"), 1)?;
    assert_eq!(lite_message.data["fixture"], "udp");
    assert_eq!(lite_message.data["sample"], 41);
    assert_eq!(lite_message.meta.tags, ["fixture", "binary"]);
    assert_eq!(
        raw_lite3(&home.pool_path(&home.pool_dir, "lite"), 1)?,
        fixture
    );
    assert_http_pool_readers(&home.pool_dir, &fixture)?;

    #[cfg(unix)]
    for pool in [r"back\slash", "C:outside"] {
        send(&socket, address, pool, br#"{"data":{"native_name":true}}"#)?;
        let path = home.pool_path(&home.pool_dir, pool);
        wait_for_sequence(&path, 1, Duration::from_secs(3))?;
        assert_eq!(message(&path, 1)?.data["native_name"], true);
    }

    send(
        &socket,
        address,
        "温度",
        br#"{"data":{"unicode_pool":true}}"#,
    )?;
    let unicode_path = home.pool_path(&home.pool_dir, "温度");
    wait_for_sequence(&unicode_path, 1, Duration::from_secs(3))?;
    assert_eq!(message(&unicode_path, 1)?.data["unicode_pool"], true);

    let missing = br#"{"data":{"must_not_create":true}}"#;
    send(&socket, address, "missing", missing)?;
    let array_lite3 = plasmite::api::lite3::Lite3Buf::from_json_str("[1,2]")?;
    let mut invalid = vec![
        b"json".to_vec(),
        [b"\0".as_slice(), json_body].concat(),
        [b"\xff\0".as_slice(), json_body].concat(),
        [b"../outside\0".as_slice(), json_body].concat(),
        [b"folder/name\0".as_slice(), json_body].concat(),
        b"json\0".to_vec(),
        [b"json\0\xff".as_slice()].concat(),
        b"json\0{".to_vec(),
        b"json\0[]".to_vec(),
        b"json\0{}".to_vec(),
        b"json\0{\"data\":[]}".to_vec(),
        b"json\0{\"data\":{},\"tags\":\"wrong\"}".to_vec(),
        b"json\0{\"data\":{},\"tags\":[\"good\",7]}".to_vec(),
        b"json\0{\"data\":{},\"data\":{}}".to_vec(),
        b"json\0{\"data\":{},\"tags\":[],\"tags\":[]}".to_vec(),
        b"json\0{\"data\":{},\"other\":true}".to_vec(),
        b"json\0{\"data\":{}} {\"data\":{}}".to_vec(),
        b"json\0{\"data\":{\"nul\":\"\0\"}}".to_vec(),
        b"json\0\x06not-lite3".to_vec(),
        b"json\0\x06{\"data\":{\"fallback\":true}}".to_vec(),
        [b"json\0".as_slice(), array_lite3.as_slice()].concat(),
    ];
    if cfg!(windows) {
        invalid.push([b"folder\\name\0".as_slice(), json_body].concat());
        invalid.push([b"C:outside\0".as_slice(), json_body].concat());
    }
    for datagram in &invalid {
        let written = socket.send_to(datagram, address)?;
        assert_eq!(written, datagram.len());
    }

    send(
        &socket,
        address,
        "json",
        br#" { "data" : { "canary" : true }, "tags" : ["after-invalid"] } "#,
    )?;
    let json_path = home.pool_path(&home.pool_dir, "json");
    wait_for_sequence(&json_path, 2, Duration::from_secs(3))?;
    assert_eq!(newest_sequence(&json_path)?, Some(2));
    assert_eq!(message(&json_path, 2)?.data["canary"], true);
    assert!(!home.pool_path(&home.pool_dir, "missing").exists());
    assert_eq!(
        newest_sequence(&home.pool_path(home.pool_dir.parent().unwrap(), "outside"))?,
        None
    );

    create_fixture(&home, &home.pool_dir, "created-after-start")?;
    let later_path = home.pool_path(&home.pool_dir, "created-after-start");
    send(
        &socket,
        address,
        "created-after-start",
        br#"{"data":{"created_after_start":true}}"#,
    )?;
    wait_for_sequence(&later_path, 1, Duration::from_secs(3))?;
    assert_eq!(message(&later_path, 1)?.data["created_after_start"], true);

    let replacement_path = home.pool_path(&home.pool_dir, "replace");
    send(&socket, address, "replace", br#"{"data":{"generation":1}}"#)?;
    wait_for_sequence(&replacement_path, 1, Duration::from_secs(3))?;
    fs::remove_file(&replacement_path)?;
    send(
        &socket,
        address,
        "replace",
        br#"{"data":{"must_not_exist":true}}"#,
    )?;
    send(
        &socket,
        address,
        "replace-barrier",
        br#"{"data":{"barrier":true}}"#,
    )?;
    let replacement_barrier = home.pool_path(&home.pool_dir, "replace-barrier");
    wait_for_sequence(&replacement_barrier, 1, Duration::from_secs(3))?;
    assert!(!replacement_path.exists());
    create_fixture(&home, &home.pool_dir, "replace")?;
    send(&socket, address, "replace", br#"{"data":{"generation":2}}"#)?;
    wait_for_sequence(&replacement_path, 1, Duration::from_secs(3))?;
    assert_eq!(message(&replacement_path, 1)?.data["generation"], 2);

    let small_path = home.pool_path(&home.pool_dir, "small");
    let large_body = json_body_with_length(60_000 - "small".len() - 1);
    let large_request: Value = serde_json::from_slice(&large_body)?;
    let tags = large_request["tags"]
        .as_array()
        .expect("large request tags")
        .iter()
        .map(|tag| tag.as_str().expect("string tag").to_owned())
        .collect::<Vec<_>>();
    let encoded = plasmite::api::lite3::encode_message(&tags, &large_request["data"])?;
    let ring_size = Pool::open(&small_path)?.info()?.ring_size as usize;
    assert!(
        encoded.len() > ring_size,
        "fixture must exceed the small pool ring"
    );
    send(&socket, address, "small", &large_body)?;
    send(
        &socket,
        address,
        "small",
        br#"{"data":{"after_capacity_error":true}}"#,
    )?;
    wait_for_sequence(&small_path, 1, Duration::from_secs(3))?;
    assert_eq!(message(&small_path, 1)?.data["after_capacity_error"], true);

    let repeated = br#"{"data":{"sample":8},"tags":["sender"]}"#;
    for expected in 3..=4 {
        send(&socket, address, "json", repeated)?;
        wait_for_sequence(&json_path, expected, Duration::from_secs(3))?;
    }
    send(
        &socket,
        address,
        "json",
        br#"{"data":{"sample":7},"tags":["sender"]}"#,
    )?;
    wait_for_sequence(&json_path, 5, Duration::from_secs(3))?;
    assert_eq!(message(&json_path, 3)?.data["sample"], 8);
    assert_eq!(message(&json_path, 4)?.data["sample"], 8);
    assert_eq!(message(&json_path, 5)?.data["sample"], 7);

    let mut reply = [0; 1];
    assert!(matches!(
        socket.recv_from(&mut reply),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
    ));
    assert_local_http_health(&home.pool_dir)?;
    server.assert_running()?;
    server.stop_gracefully(Duration::from_secs(5))?;

    #[cfg(unix)]
    {
        let summary = server
            .combined_log
            .lines()
            .find(|line| line.contains("UDP input summary"))
            .ok_or_else(|| format!("missing shutdown UDP summary:\n{}", server.combined_log))?;
        assert!(log_counter(summary, "received").is_some(), "{summary}");
        assert!(log_counter(summary, "appended").is_some(), "{summary}");
        assert!(log_counter(summary, "oversized").is_some(), "{summary}");
        assert!(log_counter(summary, "errors").is_some(), "{summary}");
        assert!(
            log_counter(summary, "appended").unwrap_or_default() >= 12,
            "expected counters to include all observed appends: {summary}"
        );
        assert!(
            log_counter(summary, "errors").unwrap_or_default() >= invalid.len() as u64 + 3,
            "expected counters to include missing and invalid datagrams: {summary}"
        );
    }
    Ok(())
}

#[test]
fn occupied_udp_socket_fails_startup_before_banner_or_readiness() -> TestResult<()> {
    let home = TestHome::new()?;
    let held = UdpSocket::bind("127.0.0.1:0")?;
    let address = held.local_addr()?.to_string();
    let output = home
        .serve_args(true, Some(&address))
        .env("PLASMITE_SERVE_READY_FILE", &home.ready_file)
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to bind UDP input listener"),
        "unexpected startup error: {stderr}"
    );
    assert!(!stderr.contains("UDP input:"), "{stderr}");
    assert!(!home.ready_file.exists());
    drop(held);
    Ok(())
}

#[cfg(unix)]
#[test]
fn shutdown_waits_for_an_in_progress_pool_lock_and_does_not_append_queued_packets() -> TestResult<()>
{
    let home = TestHome::new()?;
    for name in ["locked", "queued"] {
        create_fixture(&home, &home.pool_dir, name)?;
    }
    let locked_path = home.pool_path(&home.pool_dir, "locked");
    let queued_path = home.pool_path(&home.pool_dir, "queued");
    let mut server = UdpServer::start(&home, true, Some("127.0.0.1:0"))?;
    let address = server.udp_addr.expect("UDP address from startup banner");
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    send(
        &socket,
        address,
        "queued",
        br#"{"data":{"before_lock":true}}"#,
    )?;
    wait_for_sequence(&queued_path, 1, Duration::from_secs(3))?;

    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&locked_path)?;
    lock_file.lock_exclusive()?;
    send(
        &socket,
        address,
        "locked",
        br#"{"data":{"lock_holder":true}}"#,
    )?;
    thread::sleep(Duration::from_millis(200));
    send(
        &socket,
        address,
        "queued",
        br#"{"data":{"must_not_drain":true}}"#,
    )?;

    server.request_shutdown()?;
    thread::sleep(Duration::from_millis(100));
    assert!(
        server.child.try_wait()?.is_none(),
        "server exited before the held pool lock was released"
    );

    fs2::FileExt::unlock(&lock_file)?;
    wait_for_sequence(&locked_path, 1, Duration::from_secs(3))?;
    server.wait_for_exit(Duration::from_secs(5))?;
    assert_eq!(message(&locked_path, 1)?.data["lock_holder"], true);
    assert_eq!(newest_sequence(&queued_path)?, Some(1));
    let summary = server
        .combined_log
        .lines()
        .find(|line| line.contains("UDP input summary"))
        .ok_or_else(|| format!("missing shutdown UDP summary:\n{}", server.combined_log))?;
    assert_eq!(log_counter(summary, "appended"), Some(2), "{summary}");
    // Reception now runs independently of a blocked append. Queued reception
    // may increase received, but shutdown must not commit that queued message.
    assert!(
        log_counter(summary, "received").unwrap_or_default() >= 2,
        "{summary}"
    );
    Ok(())
}

#[test]
fn ipv4_sender_accepts_the_shared_limit_and_rejects_a_larger_datagram() -> TestResult<()> {
    let home = TestHome::new()?;
    create_fixture(&home, &home.pool_dir, "limit")?;
    let mut server = UdpServer::start(&home, true, Some("127.0.0.1:0"))?;
    let address = server.udp_addr.expect("UDP address from startup banner");
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    allow_large_udp_datagrams(&socket)?;

    let name = "limit";
    let body = json_body_with_length(65_507 - name.len() - 1);
    let path = home.pool_path(&home.pool_dir, name);
    send(&socket, address, name, &body)?;
    wait_for_sequence(&path, 1, Duration::from_secs(3))?;
    assert!(message(&path, 1)?.data["padding"].as_str().unwrap().len() > 64_000);

    let mut too_large = Vec::with_capacity(65_508);
    too_large.extend_from_slice(name.as_bytes());
    too_large.push(0);
    too_large.extend_from_slice(&body);
    too_large.push(b'x');
    assert_eq!(too_large.len(), 65_508);
    assert!(
        socket.send_to(&too_large, address).is_err(),
        "the IPv4 sender accepted a UDP payload larger than 65,507 bytes"
    );
    assert_eq!(newest_sequence(&path)?, Some(1));
    assert_local_http_health(&home.pool_dir)?;
    server.assert_running()?;
    server.stop_gracefully(Duration::from_secs(5))?;
    #[cfg(unix)]
    {
        let summary = server
            .combined_log
            .lines()
            .find(|line| line.contains("UDP input summary"))
            .ok_or_else(|| format!("missing shutdown UDP summary:\n{}", server.combined_log))?;
        assert_eq!(log_counter(summary, "appended"), Some(1), "{summary}");
        assert_eq!(log_counter(summary, "oversized"), Some(0), "{summary}");
    }
    Ok(())
}

#[test]
fn ipv6_listener_accepts_both_formats_at_the_limit_and_discards_oversized_datagrams()
-> TestResult<()> {
    let home = TestHome::new()?;
    for pool in ["json-limit", "lite-limit", "barrier"] {
        create_fixture(&home, &home.pool_dir, pool)?;
    }
    let mut server = UdpServer::start(&home, true, Some("[::1]:0"))?;
    let address = server.udp_addr.expect("UDP address from startup banner");
    assert_eq!(address.ip().to_string(), "::1");
    assert_local_http_health(&home.pool_dir)?;

    let socket = UdpSocket::bind("[::1]:0")?;
    allow_large_udp_datagrams(&socket)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let json_name = "json-limit";
    let json_prefix_len = json_name.len() + 1;
    let max_json = json_body_with_length(65_507 - json_prefix_len);
    let json_path = home.pool_path(&home.pool_dir, json_name);
    send(&socket, address, json_name, &max_json)?;
    wait_for_sequence(&json_path, 1, Duration::from_secs(3))?;
    assert!(
        message(&json_path, 1)?.data["padding"]
            .as_str()
            .unwrap()
            .len()
            > 64_000
    );

    let lite_name = "lite-limit";
    let lite_prefix_len = lite_name.len() + 1;
    let max_lite = lite3_body_with_length(65_507 - lite_prefix_len)?;
    plasmite::api::lite3::validate_bytes(&max_lite)?;
    let lite_path = home.pool_path(&home.pool_dir, lite_name);
    send(&socket, address, lite_name, &max_lite)?;
    if let Err(error) = wait_for_sequence(&lite_path, 1, Duration::from_secs(3)) {
        let stop_result = server.stop_gracefully(Duration::from_secs(5));
        return Err(format!(
            "maximum-size Lite3 UDP append did not arrive: {error}; stop result: {stop_result:?}; logs:\n{}",
            server.combined_log
        )
        .into());
    }
    assert_eq!(raw_lite3(&lite_path, 1)?, max_lite);
    assert!(
        message(&lite_path, 1)?.data["padding"]
            .as_str()
            .unwrap()
            .len()
            > 64_000
    );

    for extra_bytes in 1..=20 {
        let mut oversized = Vec::with_capacity(65_507 + extra_bytes);
        oversized.extend_from_slice(json_name.as_bytes());
        oversized.push(0);
        oversized.extend_from_slice(&max_json);
        oversized.resize(65_507 + extra_bytes, b'x');
        assert_eq!(oversized.len(), 65_507 + extra_bytes);
        let written = socket.send_to(&oversized, address).map_err(|error| {
            format!(
                "sending IPv6 UDP payload of {} bytes failed: {error}",
                oversized.len()
            )
        })?;
        assert_eq!(written, oversized.len());
        send(
            &socket,
            address,
            "barrier",
            br#"{"data":{"after_oversize":true}}"#,
        )?;
        wait_for_sequence(
            &home.pool_path(&home.pool_dir, "barrier"),
            extra_bytes as u64,
            Duration::from_secs(3),
        )?;
    }
    assert_eq!(newest_sequence(&json_path)?, Some(1));
    assert_eq!(raw_lite3(&lite_path, 1)?, max_lite);
    assert_local_http_health(&home.pool_dir)?;

    let mut reply = [0; 1];
    assert!(matches!(
        socket.recv_from(&mut reply),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
    ));
    server.stop_gracefully(Duration::from_secs(5))?;
    #[cfg(unix)]
    {
        let summary = server
            .combined_log
            .lines()
            .find(|line| line.contains("UDP input summary"))
            .ok_or_else(|| format!("missing shutdown UDP summary:\n{}", server.combined_log))?;
        assert!(
            log_counter(summary, "oversized").unwrap_or_default() >= 20,
            "expected one oversized outcome for every IPv6 payload size 65508..65527: {summary}"
        );
        assert_eq!(log_counter(summary, "appended"), Some(22), "{summary}");
        assert_eq!(log_counter(summary, "received"), Some(42), "{summary}");
    }
    Ok(())
}

#[test]
fn default_pool_directory_and_udp_disabled_server_keep_existing_cli_behavior() -> TestResult<()> {
    let home = TestHome::new()?;
    let default_pool_dir = home.home.join(".plasmite/pools");
    create_fixture(&home, &default_pool_dir, "default-pool")?;
    let mut server = UdpServer::start(&home, false, Some("127.0.0.1:0"))?;
    let address = server.udp_addr.expect("UDP address from startup banner");
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    send(
        &socket,
        address,
        "default-pool",
        br#"{"data":{"default_directory":true}}"#,
    )?;
    let default_path = home.pool_path(&default_pool_dir, "default-pool");
    wait_for_sequence(&default_path, 1, Duration::from_secs(3))?;
    assert_eq!(message(&default_path, 1)?.data["default_directory"], true);
    assert_local_http_health(&default_pool_dir)?;
    server.stop_gracefully(Duration::from_secs(5))?;

    let disabled = TestHome::new()?;
    create_fixture(&disabled, &disabled.pool_dir, "ordinary")?;
    let mut no_udp = UdpServer::start(&disabled, true, None)?;
    assert!(!no_udp.combined_log.contains("UDP input:"));
    assert_local_http_health(&disabled.pool_dir)?;
    no_udp.assert_running()?;
    no_udp.stop_gracefully(Duration::from_secs(5))?;
    Ok(())
}

#[test]
fn udp_option_requires_equals_for_an_address_and_bare_udp_keeps_server_positional() -> TestResult<()>
{
    let home = TestHome::new()?;
    let separated_address = home
        .command()
        .args(["serve", "--udp", "127.0.0.1:0", "--bind", "not-an-address"])
        .output()?;
    assert_eq!(separated_address.status.code(), Some(2));
    let separated_error = String::from_utf8_lossy(&separated_address.stderr);
    assert!(
        separated_error.contains("SERVER must be an HTTPS origin"),
        "space-separated text should remain the SERVER positional argument: {separated_error}"
    );
    assert!(!separated_error.contains("invalid UDP bind address"));

    let bare_udp_with_server = home
        .command()
        .args([
            "serve",
            "--udp",
            "https://localhost:0",
            "--bind",
            "not-an-address",
        ])
        .output()?;
    assert_eq!(bare_udp_with_server.status.code(), Some(2));
    let bare_error = String::from_utf8_lossy(&bare_udp_with_server.stderr);
    assert!(
        bare_error.contains("invalid local bind address"),
        "bare --udp should leave the following URL for SERVER: {bare_error}"
    );
    assert!(!bare_error.contains("invalid UDP bind address"));

    let multicast = home
        .command()
        .args(["serve", "--udp=224.0.0.1:9701"])
        .output()?;
    assert_eq!(multicast.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&multicast.stderr)
            .contains("UDP input cannot bind to a multicast address")
    );
    Ok(())
}
