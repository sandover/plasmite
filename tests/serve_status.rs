use serde_json::Value;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct TestHome {
    _temp: TempDir,
    home: PathBuf,
    cwd: PathBuf,
}

impl TestHome {
    fn new() -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&cwd)?;
        Ok(Self {
            _temp: temp,
            home,
            cwd,
        })
    }

    fn command(&self) -> Command {
        self.command_from(&self.cwd)
    }

    fn command_from(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command.env("HOME", &self.home).current_dir(cwd);
        command
    }

    fn status_json(&self, ignored_dir: &Path) -> TestResult<Vec<Value>> {
        self.status_json_with(self.command(), ignored_dir)
    }

    fn status_json_with(&self, mut command: Command, ignored_dir: &Path) -> TestResult<Vec<Value>> {
        let output = command
            .arg("--dir")
            .arg(ignored_dir)
            .args(["serve", "status", "--json"])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "serve status --json failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn status_text(&self) -> TestResult<String> {
        let output = self.command().args(["serve", "status"]).output()?;
        if !output.status.success() {
            return Err(format!(
                "serve status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn registry_dir(&self) -> PathBuf {
        self.home.join(".plasmite/servers")
    }
}

struct RunningServer {
    child: Option<Child>,
    _ready_dir: TempDir,
}

impl RunningServer {
    fn start(home: &TestHome, pool_dir: &Path, local_bind: &str) -> TestResult<Self> {
        Self::start_with_options(home.command(), pool_dir, local_bind, "127.0.0.1:0", None)
    }

    #[cfg(windows)]
    fn start_with_command(command: Command, pool_dir: &Path, local_bind: &str) -> TestResult<Self> {
        Self::start_with_options(command, pool_dir, local_bind, "127.0.0.1:0", None)
    }

    fn start_with_options(
        mut command: Command,
        pool_dir: &Path,
        local_bind: &str,
        remote_bind: &str,
        shared_address: Option<&str>,
    ) -> TestResult<Self> {
        let ready_dir = tempfile::tempdir()?;
        let ready_file = ready_dir.path().join("remote-address");
        command.arg("--dir").arg(pool_dir).args([
            "serve",
            "--bind",
            local_bind,
            "--remote-bind",
            remote_bind,
        ]);
        if let Some(shared_address) = shared_address {
            command.args(["--shared-address", shared_address]);
        }
        let child = command
            .env("PLASMITE_SERVE_READY_FILE", &ready_file)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut server = Self {
            child: Some(child),
            _ready_dir: ready_dir,
        };
        server.wait_ready(&ready_file)?;
        Ok(server)
    }

    fn wait_ready(&mut self, ready_file: &Path) -> TestResult<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let child = self.child.as_mut().expect("server child is present");
            if let Some(status) = child.try_wait()? {
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut stderr| {
                        use std::io::Read;
                        let mut text = String::new();
                        let _ = stderr.read_to_string(&mut text);
                        text
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "server exited before readiness ({status}): {}",
                    stderr.trim()
                )
                .into());
            }
            if ready_file.exists() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("server did not publish its readiness file".into());
            }
            sleep(Duration::from_millis(10));
        }
    }

    fn kill(&mut self) -> TestResult<()> {
        if let Some(mut child) = self.child.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for_servers(
    home: &TestHome,
    ignored_dir: &Path,
    expected: usize,
) -> TestResult<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let servers = home.status_json(ignored_dir)?;
        if servers.len() == expected {
            return Ok(servers);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "expected {expected} servers, found {}: {servers:?}",
                servers.len()
            )
            .into());
        }
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn status_reports_an_empty_registry_as_text_and_json() -> TestResult<()> {
    let home = TestHome::new()?;

    assert_eq!(home.status_text()?, "No Plasmite servers running.");
    assert!(home.status_json(Path::new("ignored-pool-dir"))?.is_empty());

    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn status_discovers_a_server_with_a_non_utf8_pool_directory() -> TestResult<()> {
    use std::os::unix::ffi::OsStringExt;
    let home = TestHome::new()?;
    let pool = home
        .cwd
        .join(std::ffi::OsString::from_vec(b"pools-\xff".to_vec()));
    let server = RunningServer::start(&home, &pool, "127.0.0.1:0")?;
    let servers = wait_for_servers(&home, Path::new("unused"), 1)?;
    let canonical = std::fs::canonicalize(&pool)?;
    assert_eq!(
        servers[0]["pool_dir"].as_str(),
        Some(canonical.to_string_lossy().as_ref())
    );
    assert_eq!(
        servers[0]["pid"].as_u64(),
        Some(server.child.as_ref().unwrap().id() as u64)
    );
    drop(server);
    Ok(())
}

#[test]
fn status_finds_two_servers_and_ignores_dir_for_discovery() -> TestResult<()> {
    let home = TestHome::new()?;
    let relative_pool = home.cwd.join("relative-pool");
    let absolute_pool = home.cwd.join("absolute-pool");
    let relative = RunningServer::start(&home, Path::new("relative-pool"), "127.0.0.1:0")?;
    let absolute = RunningServer::start(&home, &absolute_pool, "127.0.0.1:0")?;

    let ignored_dir = home.cwd.join("a-dir-that-does-not-exist");
    let servers = wait_for_servers(&home, &ignored_dir, 2)?;
    let mut pool_dirs: Vec<PathBuf> = servers
        .iter()
        .map(|server| PathBuf::from(server["pool_dir"].as_str().expect("pool_dir string")))
        .collect();
    pool_dirs.sort();
    let mut expected_dirs = vec![
        std::fs::canonicalize(relative_pool)?,
        std::fs::canonicalize(absolute_pool)?,
    ];
    expected_dirs.sort();
    assert_eq!(pool_dirs, expected_dirs);

    for server in &servers {
        assert!(server["pid"].as_u64().is_some());
        assert!(
            server["local_url"]
                .as_str()
                .unwrap()
                .starts_with("http://127.0.0.1:")
        );
        assert!(
            server["remote_url"]
                .as_str()
                .unwrap()
                .starts_with("https://127.0.0.1:")
        );
    }
    assert_ne!(servers[0]["pid"], servers[1]["pid"]);
    drop((relative, absolute));
    Ok(())
}

#[test]
fn status_excludes_a_server_killed_without_registry_cleanup() -> TestResult<()> {
    let home = TestHome::new()?;
    let pool = home.cwd.join("crashed-pool");
    let mut server = RunningServer::start(&home, &pool, "127.0.0.1:0")?;
    let live = wait_for_servers(&home, Path::new("unused"), 1)?;
    assert_eq!(
        live[0]["pool_dir"].as_str(),
        Some(std::fs::canonicalize(&pool)?.to_str().unwrap())
    );

    server.kill()?;
    assert!(home.registry_dir().read_dir()?.next().is_some());
    assert!(wait_for_servers(&home, Path::new("unused"), 0)?.is_empty());

    Ok(())
}

#[test]
fn status_does_not_revive_a_stale_entry_when_another_server_reuses_its_port() -> TestResult<()> {
    let home = TestHome::new()?;
    let stale_pool = home.cwd.join("stale-pool");
    let impostor_pool = home.cwd.join("impostor-pool");
    let mut stale = RunningServer::start(&home, &stale_pool, "127.0.0.1:0")?;
    let stale_entry = wait_for_servers(&home, Path::new("unused"), 1)?
        .into_iter()
        .next()
        .expect("one server");
    let stale_port = stale_entry["local_url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap()
        .parse::<SocketAddr>()?
        .port();
    stale.kill()?;

    let impostor = RunningServer::start(&home, &impostor_pool, &format!("127.0.0.1:{stale_port}"))?;
    let servers = wait_for_servers(&home, Path::new("unused"), 1)?;
    assert_eq!(
        servers[0]["pool_dir"].as_str(),
        Some(std::fs::canonicalize(&impostor_pool)?.to_str().unwrap())
    );
    assert_ne!(servers[0]["pid"], stale_entry["pid"]);

    drop((stale, impostor));
    Ok(())
}

#[test]
fn status_serializes_remote_url_as_null_or_the_configured_shared_address() -> TestResult<()> {
    let home = TestHome::new()?;
    let private_pool = home.cwd.join("private-remote-pool");
    let shared_pool = home.cwd.join("shared-remote-pool");
    let private = RunningServer::start_with_options(
        home.command(),
        &private_pool,
        "127.0.0.1:0",
        "0.0.0.0:0",
        None,
    )?;
    let shared_address = "https://example.test:8443";
    let shared = RunningServer::start_with_options(
        home.command(),
        &shared_pool,
        "127.0.0.1:0",
        "127.0.0.1:0",
        Some(shared_address),
    )?;

    let servers = wait_for_servers(&home, Path::new("unused"), 2)?;
    let by_pool: std::collections::HashMap<_, _> = servers
        .iter()
        .map(|server| (server["pool_dir"].as_str().unwrap(), server))
        .collect();
    let private_path = std::fs::canonicalize(private_pool)?;
    let shared_path = std::fs::canonicalize(shared_pool)?;
    assert_eq!(
        by_pool[private_path.to_str().unwrap()]["remote_url"],
        Value::Null
    );
    assert_eq!(
        by_pool[shared_path.to_str().unwrap()]["remote_url"],
        shared_address
    );

    drop((private, shared));
    Ok(())
}

#[cfg(unix)]
#[test]
fn registry_directory_and_registration_file_are_private() -> TestResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let home = TestHome::new()?;
    let pool = home.cwd.join("private-registry-pool");
    let _server = RunningServer::start(&home, &pool, "127.0.0.1:0")?;
    let servers = wait_for_servers(&home, Path::new("unused"), 1)?;
    let entries: Vec<_> = home.registry_dir().read_dir()?.collect::<Result<_, _>>()?;

    assert_eq!(entries.len(), 1);
    assert_eq!(
        std::fs::metadata(home.registry_dir())?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(entries[0].path())?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        servers[0]["pool_dir"].as_str(),
        Some(std::fs::canonicalize(&pool)?.to_str().unwrap())
    );

    Ok(())
}

#[test]
fn status_endpoint_is_guarded_on_local_http_and_absent_from_remote_https() -> TestResult<()> {
    use std::sync::Arc;
    use ureq::rustls::pki_types::pem::PemObject;

    let home = TestHome::new()?;
    let pool = home.cwd.join("status-endpoint-pool");
    let _server = RunningServer::start(&home, &pool, "127.0.0.1:0")?;
    let entry = wait_for_servers(&home, Path::new("unused"), 1)?
        .into_iter()
        .next()
        .expect("one server");
    let local_url = entry["local_url"].as_str().unwrap();
    let local_status = ureq::get(&format!("{local_url}/v0/serve/status")).call()?;
    assert_eq!(local_status.status(), 200);
    let registered: Value = serde_json::from_str(&local_status.into_string()?)?;
    assert_eq!(registered["pid"], entry["pid"]);

    let rejected = ureq::get(&format!("{local_url}/v0/serve/status"))
        .set("Host", "untrusted.example")
        .call();
    assert!(matches!(rejected, Err(ureq::Error::Status(403, _))));

    let serve_dir = pool.join(".plasmite-serve");
    let identity: Value = serde_json::from_slice(&std::fs::read(serve_dir.join("identity.json"))?)?;
    let certificate = std::fs::read(serve_dir.join(identity["cert_file"].as_str().unwrap()))?;
    let certificates = ureq::rustls::pki_types::CertificateDer::pem_slice_iter(&certificate)
        .collect::<Result<Vec<_>, _>>()?;
    let mut roots = ureq::rustls::RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(certificates);
    assert_eq!(added, 1);
    let tls = ureq::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let agent = ureq::builder().tls_config(Arc::new(tls)).build();
    let remote_url = entry["remote_url"].as_str().unwrap();
    let remote = agent.get(&format!("{remote_url}/v0/serve/status")).call();
    assert!(matches!(remote, Err(ureq::Error::Status(404, _))));

    Ok(())
}

#[cfg(windows)]
#[test]
fn status_uses_userprofile_when_home_is_unset_and_cwd_differs() -> TestResult<()> {
    let home = TestHome::new()?;
    let pool = home.cwd.join("userprofile-pool");
    let mut server_command = home.command();
    server_command
        .env_remove("HOME")
        .env("USERPROFILE", &home.home);
    let _server = RunningServer::start_with_command(server_command, &pool, "127.0.0.1:0")?;

    let status_cwd = home._temp.path().join("different-status-cwd");
    std::fs::create_dir_all(&status_cwd)?;
    let mut status_command = home.command_from(&status_cwd);
    status_command
        .env_remove("HOME")
        .env("USERPROFILE", &home.home);
    let servers = home.status_json_with(status_command, Path::new("ignored"))?;
    assert_eq!(servers.len(), 1);
    assert_eq!(
        servers[0]["pool_dir"].as_str(),
        Some(std::fs::canonicalize(&pool)?.to_str().unwrap())
    );
    assert!(home.registry_dir().is_dir());

    let mut invalid_home_command = home.command();
    invalid_home_command
        .env_remove("HOME")
        .env("USERPROFILE", "relative-home");
    let invalid_home = invalid_home_command.args(["serve", "status"]).output()?;
    assert!(!invalid_home.status.success());
    assert!(String::from_utf8_lossy(&invalid_home.stderr).contains("absolute user home directory"));

    Ok(())
}
