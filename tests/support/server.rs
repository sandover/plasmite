use plasmite::api::RemoteClient;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

pub struct TestServer {
    child: Child,
    pub base_url: String,
    token: Option<String>,
    _ready_dir: tempfile::TempDir,
}

impl TestServer {
    pub fn start(pool_dir: &Path) -> Self {
        Self::start_with_args(pool_dir, &[])
    }

    pub fn start_with_args(pool_dir: &Path, extra_args: &[&str]) -> Self {
        Self::start_with_args_and_scheme(pool_dir, extra_args, "http")
    }

    pub fn start_with_args_and_scheme(pool_dir: &Path, extra_args: &[&str], scheme: &str) -> Self {
        Self::try_start_with_options(pool_dir, extra_args, scheme, None)
            .unwrap_or_else(|err| panic!("server ready: {err}"))
    }

    pub fn try_start(pool_dir: &Path) -> TestResult<Self> {
        Self::try_start_with_options(pool_dir, &[], "http", None)
    }

    pub fn try_start_with_token(pool_dir: &Path, token: Option<&str>) -> TestResult<Self> {
        let mut args = Vec::new();
        if let Some(token) = token {
            args.extend(["--token", token]);
        }
        Self::try_start_with_options(pool_dir, &args, "http", token)
    }

    pub fn try_start_with_access(pool_dir: &Path, access: &str) -> TestResult<Self> {
        Self::try_start_with_options(pool_dir, &["--access", access], "http", None)
    }

    pub fn try_start_with_cors(pool_dir: &Path, origins: &[&str]) -> TestResult<Self> {
        let mut args = Vec::with_capacity(origins.len() * 2);
        for origin in origins {
            args.extend(["--cors-origin", *origin]);
        }
        Self::try_start_with_options(pool_dir, &args, "http", None)
    }

    fn try_start_with_options(
        pool_dir: &Path,
        extra_args: &[&str],
        scheme: &str,
        token: Option<&str>,
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
                    let address = address.trim().parse::<std::net::SocketAddr>()?;
                    return Ok(Self {
                        child,
                        base_url: format!("{scheme}://{address}"),
                        token: token.map(str::to_string),
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
        Ok(RemoteClient::new(self.base_url.clone())?)
    }

    pub fn client_with_token(&self) -> TestResult<RemoteClient> {
        let mut client = RemoteClient::new(self.base_url.clone())?;
        if let Some(token) = &self.token {
            client = client.with_token(token.clone());
        }
        Ok(client)
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
