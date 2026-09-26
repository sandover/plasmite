use plasmite::api::RemoteClient;
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

pub struct TestServer {
    child: Child,
    pub base_url: String,
    pub remote_url: String,
    pub local_url: String,
    access_key: String,
    _ready_dir: tempfile::TempDir,
}

impl TestServer {
    pub fn start(pool_dir: &Path) -> Self {
        Self::start_with_args(pool_dir, &[])
    }

    pub fn start_with_args(pool_dir: &Path, extra_args: &[&str]) -> Self {
        Self::start_with_args_and_scheme(pool_dir, extra_args, "https")
    }

    pub fn start_with_args_and_scheme(pool_dir: &Path, extra_args: &[&str], scheme: &str) -> Self {
        Self::try_start_with_options(pool_dir, extra_args, scheme)
            .unwrap_or_else(|err| panic!("server ready: {err}"))
    }

    pub fn try_start(pool_dir: &Path) -> TestResult<Self> {
        Self::try_start_with_options(pool_dir, &[], "https")
    }

    pub fn try_start_oauth(pool_dir: &Path) -> TestResult<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Self::try_start_oauth_at(pool_dir, port)
    }

    pub fn try_start_oauth_at(pool_dir: &Path, port: u16) -> TestResult<Self> {
        Self::try_start_with_addresses(
            pool_dir,
            &[],
            "https",
            &format!("127.0.0.1:{port}"),
            &format!("https://localhost:{port}"),
        )
    }

    fn try_start_with_options(
        pool_dir: &Path,
        extra_args: &[&str],
        scheme: &str,
    ) -> TestResult<Self> {
        Self::try_start_with_addresses(
            pool_dir,
            extra_args,
            scheme,
            "127.0.0.1:0",
            "https://localhost:9743",
        )
    }

    fn try_start_with_addresses(
        pool_dir: &Path,
        extra_args: &[&str],
        scheme: &str,
        remote_bind: &str,
        shared_address: &str,
    ) -> TestResult<Self> {
        let ready_dir = tempfile::tempdir()?;
        let ready_path = ready_dir.path().join("address");
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .arg("--dir")
            .arg(pool_dir)
            .arg("serve")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--remote-bind")
            .arg(remote_bind)
            .arg("--shared-address")
            .arg(shared_address)
            .args(extra_args)
            .env("PLASMITE_SERVE_READY_FILE", &ready_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;

        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait()? {
                let stderr = take_stderr(&mut child);
                return Err(format!(
                    "server exited before ready (status: {status}, stderr: {})",
                    display_diagnostics(&stderr)
                )
                .into());
            }
            match std::fs::read_to_string(&ready_path) {
                Ok(address) => {
                    let remote = address.trim().parse::<std::net::SocketAddr>()?;
                    let local_path = pool_dir.join(".plasmite-serve/local.json");
                    let local: String = serde_json::from_slice(&std::fs::read(local_path)?)?;
                    let local = local.parse::<std::net::SocketAddr>()?;
                    let remote_url = format!("https://localhost:{}", remote.port());
                    let local_url = format!("http://{local}");
                    let base_url = if scheme == "https" {
                        remote_url.clone()
                    } else {
                        local_url.clone()
                    };
                    let invite = Command::new(env!("CARGO_BIN_EXE_plasmite"))
                        .arg("--dir")
                        .arg(pool_dir)
                        .args(["access", "invite", "--name", "integration-test"])
                        .output()?;
                    if !invite.status.success() {
                        return Err(format!(
                            "access invite failed: {}",
                            display_diagnostics(&String::from_utf8_lossy(&invite.stderr))
                        )
                        .into());
                    }
                    let invite: Value = serde_json::from_slice(&invite.stdout)?;
                    let access_key = invite["access_key"]
                        .as_str()
                        .ok_or("access invite omitted access_key")?
                        .to_owned();
                    return Ok(Self {
                        child,
                        base_url,
                        remote_url,
                        local_url,
                        access_key,
                        _ready_dir: ready_dir,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = take_stderr(&mut child);
                return Err(format!(
                    "server did not publish its bound address (stderr: {})",
                    display_diagnostics(&stderr)
                )
                .into());
            }
            sleep(Duration::from_millis(5));
        }
    }

    pub fn client(&self) -> TestResult<RemoteClient> {
        Ok(RemoteClient::with_access_key(
            self.remote_url.clone(),
            &self.access_key,
        )?)
    }

    pub fn access_key(&self) -> &str {
        &self.access_key
    }

    #[cfg(unix)]
    pub fn terminate_and_wait(
        &mut self,
        timeout: Duration,
    ) -> TestResult<std::process::ExitStatus> {
        let rc = unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err("server did not exit after SIGTERM".into());
            }
            sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn take_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    stderr
}

fn display_diagnostics(stderr: &str) -> &str {
    let stderr = stderr.trim();
    if stderr.is_empty() { "<empty>" } else { stderr }
}
