//! JSON service logs own and reap their native reader on every exit path.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    pools: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let pools = root.join("pools");
        fs::create_dir(&pools)?;
        let label = format!(
            "net.plasmite.{:x}",
            Sha256::digest(pools.as_os_str().as_encoded_bytes())
        );
        let service = home.join(".plasmite/services").join(label);
        fs::create_dir_all(&service)?;
        fs::set_permissions(&service, fs::Permissions::from_mode(0o700))?;
        let account = Command::new("/usr/bin/id").arg("-un").output()?;
        assert!(account.status.success());
        let account = String::from_utf8(account.stdout)?;
        let setup = json!({
            "pool_dir": pools,
            "program": service.join("plasmite"),
            "account": account.trim(),
            "home": home,
            "run": {
                "bind": "127.0.0.1:49701", "remote_bind": "127.0.0.1:49702",
                "max_body_bytes": 1048576, "max_tail_timeout_ms": 30000,
                "max_tail_concurrency": 4
            }
        });
        let setup_path = service.join("setup.json");
        fs::write(&setup_path, serde_json::to_vec(&setup)?)?;
        fs::set_permissions(setup_path, fs::Permissions::from_mode(0o600))?;
        let log = service.join("serve.log");
        fs::write(&log, b"first line\n")?;
        fs::set_permissions(&log, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            _temp: temp,
            home,
            pools,
            log,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .env("HOME", &self.home)
            .env("PLASMITE_ACCESS_HOME", &self.home)
            .arg("--dir")
            .arg(&self.pools)
            .args(["serve", "logs", "--tail", "2", "--json"]);
        command
    }

    fn append(&self, bytes: &[u8]) -> TestResult<()> {
        let mut log = OpenOptions::new().append(true).open(&self.log)?;
        log.write_all(bytes)?;
        log.flush()?;
        Ok(())
    }
}

struct FollowLog {
    child: Child,
    lines: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
}

impl FollowLog {
    fn start(fixture: &Fixture, close_after_one_line: bool) -> TestResult<Self> {
        let mut child = fixture
            .command()
            .arg("--follow")
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("missing log stdout")?;
        let (send, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if send.send(line).is_err() || close_after_one_line {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            lines,
            reader: Some(reader),
        })
    }

    fn message(&self) -> TestResult<String> {
        let line = self.lines.recv_timeout(Duration::from_secs(5))??;
        let row: Value = serde_json::from_str(&line)?;
        Ok(row["message"]
            .as_str()
            .ok_or("missing JSON log message")?
            .into())
    }

    fn helper_pid(&self) -> TestResult<i32> {
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,pgid=,command="])
            .output()?;
        assert!(output.status.success(), "process inspection failed");
        let process = self.child.id().to_string();
        for row in String::from_utf8(output.stdout)?.lines() {
            let columns: Vec<_> = row.split_whitespace().collect();
            if columns.len() >= 4
                && columns[1] == process
                && columns[2] == process
                && columns[3].ends_with("/tail")
            {
                return Ok(columns[0].parse()?);
            }
        }
        Err("JSON follow did not have a native tail child".into())
    }

    fn wait(&mut self) -> TestResult<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("JSON follow did not exit after its read/write error".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_helper_reaped(&self, helper: i32) {
        assert_eq!(
            unsafe { libc::kill(helper, 0) },
            -1,
            "native tail remains after CLI exit"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    fn error(&mut self) -> TestResult<Value> {
        use std::io::Read;
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .ok_or("missing log stderr")?
            .read_to_string(&mut stderr)?;
        Ok(serde_json::from_str(&stderr)?)
    }
}

impl Drop for FollowLog {
    fn drop(&mut self) {
        // The group also contains tail if a regression leaves it orphaned.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn finite_json_logs_preserve_messages() -> TestResult<()> {
    let fixture = Fixture::new()?;
    fixture.append(b"second \"quoted\" line\n")?;
    let output = fixture.command().output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let rows: Vec<Value> = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        rows,
        vec![
            json!({"message": "first line"}),
            json!({"message": "second \"quoted\" line"})
        ]
    );
    Ok(())
}

#[test]
fn json_follow_streams_new_lines() -> TestResult<()> {
    let fixture = Fixture::new()?;
    let process = FollowLog::start(&fixture, false)?;
    assert_eq!(process.message()?, "first line");
    fixture.append(b"new follow line\n")?;
    assert_eq!(process.message()?, "new follow line");
    assert!(process.helper_pid()? > 0);
    Ok(())
}

#[test]
fn json_follow_read_error_reaps_native_reader() -> TestResult<()> {
    let fixture = Fixture::new()?;
    let mut process = FollowLog::start(&fixture, false)?;
    assert_eq!(process.message()?, "first line");
    let helper = process.helper_pid()?;
    fixture.append(b"\xff invalid UTF-8\n")?;
    assert_eq!(process.wait()?.code(), Some(8));
    process.assert_helper_reaped(helper);
    let error = process.error()?;
    assert_eq!(error["error"]["kind"], "Io");
    assert_eq!(error["error"]["message"], "failed to read log line");
    Ok(())
}

#[test]
fn json_follow_broken_pipe_reaps_native_reader_and_reports_io_error() -> TestResult<()> {
    let fixture = Fixture::new()?;
    let mut process = FollowLog::start(&fixture, true)?;
    assert_eq!(process.message()?, "first line");
    let helper = process.helper_pid()?;
    process
        .reader
        .take()
        .ok_or("missing log reader")?
        .join()
        .map_err(|_| "log reader panicked")?;
    fixture.append(b"line after consumer closes stdout\n")?;
    assert_eq!(process.wait()?.code(), Some(8));
    process.assert_helper_reaped(helper);
    let error = process.error()?;
    assert_eq!(error["error"]["kind"], "Io");
    assert_eq!(error["error"]["message"], "failed to write service logs");
    assert!(
        error["error"]["causes"]
            .as_array()
            .ok_or("missing I/O cause")?
            .iter()
            .any(|cause| cause
                .as_str()
                .is_some_and(|text| text.to_lowercase().contains("broken pipe")))
    );
    Ok(())
}
