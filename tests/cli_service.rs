//! Purpose: Verify foreground and installed-server CLI behavior at process boundaries.

use serde_json::Value;
use std::fs;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tempfile::TempDir;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct TestHome {
    _temp: TempDir,
    home: PathBuf,
}

impl TestHome {
    fn new() -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("home");
        fs::create_dir_all(&home)?;
        Ok(Self { _temp: temp, home })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_plasmite"));
        command
            .env("HOME", &self.home)
            .env("PLASMITE_ACCESS_HOME", &self.home);
        command
    }

    fn status_all(&self, ignored_dir: &Path) -> TestResult<Vec<Value>> {
        let output = self
            .command()
            .arg("--dir")
            .arg(ignored_dir)
            .args(["serve", "status", "--all", "--json"])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "serve status --all failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}

struct ManualServer {
    child: Child,
    _ready_dir: TempDir,
    remote_addr: SocketAddr,
}

impl ManualServer {
    fn start(home: &TestHome, pool_dir: &Path, args: &[&str]) -> TestResult<Self> {
        fs::create_dir_all(pool_dir)?;
        let ready_dir = tempfile::tempdir()?;
        let ready_file = ready_dir.path().join("remote-address");
        let mut command = home.command();
        command
            .arg("--dir")
            .arg(pool_dir)
            .arg("serve")
            .args(["--bind", "127.0.0.1:0"])
            .args(args)
            .env("PLASMITE_SERVE_READY_FILE", &ready_file)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut pipe| {
                        let mut text = String::new();
                        let _ = pipe.read_to_string(&mut text);
                        text
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "foreground server exited before readiness ({status}): {}",
                    stderr.trim()
                )
                .into());
            }
            if let Ok(address) = fs::read_to_string(&ready_file) {
                return Ok(Self {
                    child,
                    _ready_dir: ready_dir,
                    remote_addr: address.trim().parse()?,
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("foreground server did not publish its bound address".into());
            }
            sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ManualServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn foreground_server_accepts_positional_url_and_remote_bind_override() -> TestResult<()> {
    let home = TestHome::new()?;
    let default_pool = home._temp.path().join("default-pool");
    let default_server = ManualServer::start(&home, &default_pool, &["https://localhost:0"])?;
    assert!(
        default_server.remote_addr.ip().is_unspecified(),
        "SERVER should keep the default wildcard listener: {}",
        default_server.remote_addr
    );
    assert_ne!(default_server.remote_addr.port(), 9743);

    drop(default_server);

    let override_pool = home._temp.path().join("override-pool");
    let override_server = ManualServer::start(
        &home,
        &override_pool,
        &["https://localhost:9444", "--remote-bind", "127.0.0.1:0"],
    )?;
    assert_eq!(
        override_server.remote_addr.ip(),
        IpAddr::from([127, 0, 0, 1]),
        "--remote-bind should override SERVER's derived listener address"
    );
    assert_ne!(override_server.remote_addr.port(), 0);
    Ok(())
}

#[test]
fn legacy_shared_address_still_advertises_the_supplied_origin() -> TestResult<()> {
    let home = TestHome::new()?;
    let pool_dir = home._temp.path().join("pools");
    let server = ManualServer::start(
        &home,
        &pool_dir,
        &[
            "--shared-address",
            "https://localhost:9444",
            "--remote-bind",
            "127.0.0.1:0",
        ],
    )?;
    let canonical_pool_dir = fs::canonicalize(&pool_dir)?;
    let rows = home.status_all(Path::new("unused"))?;
    let row = rows
        .iter()
        .find(|row| row["pool_dir"] == canonical_pool_dir.to_string_lossy().as_ref())
        .expect("manual server row");
    assert_eq!(row["remote_url"], "https://localhost:9444");
    assert_eq!(server.remote_addr.ip(), IpAddr::from([127, 0, 0, 1]));
    assert_eq!(row["pid"].as_u64(), Some(u64::from(server.child.id())));
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn invalid_install_does_not_write_a_service_setup() -> TestResult<()> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return Ok(());
    }

    let home = TestHome::new()?;
    let pool_dir = home._temp.path().join("pools");
    let output = home
        .command()
        .arg("--dir")
        .arg(&pool_dir)
        .args(["serve", "install", "http://example.com"])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("SERVER must be an HTTPS origin"),
        "{stderr}"
    );

    let services_dir = home.home.join(".plasmite/services");
    if services_dir.exists() {
        let mut entries = fs::read_dir(&services_dir)?;
        if let Some(entry) = entries.next() {
            let entry = entry?;
            assert!(
                !entry.path().join("setup.json").exists(),
                "invalid input must fail before saving a service setup"
            );
            assert!(
                !entry.path().join("plasmite").exists(),
                "invalid input must fail before copying the service executable"
            );
        }
    }
    Ok(())
}

#[test]
fn status_all_includes_a_manual_foreground_server() -> TestResult<()> {
    let home = TestHome::new()?;
    let pool_dir = home._temp.path().join("pools");
    let server = ManualServer::start(&home, &pool_dir, &["--remote-bind", "127.0.0.1:0"])?;
    let canonical_pool_dir = fs::canonicalize(&pool_dir)?;
    let rows = home.status_all(Path::new("unused"))?;
    let row = rows
        .iter()
        .find(|row| row["pool_dir"] == canonical_pool_dir.to_string_lossy().as_ref())
        .expect("status --all should include the manual server");
    assert_eq!(row["managed"], false);
    assert_eq!(row["startup"], false);
    assert_eq!(row["state"], "running");
    assert_eq!(row["pid"].as_u64(), Some(u64::from(server.child.id())));
    Ok(())
}

#[cfg(unix)]
#[test]
fn status_escapes_directory_controls_and_keeps_json_paths_lossless() -> TestResult<()> {
    let home = TestHome::new()?;
    let pool_dir = home._temp.path().join("pools\nnext\x1b[31m\r\t雪");
    let server = ManualServer::start(&home, &pool_dir, &["--remote-bind", "127.0.0.1:0"])?;
    let canonical_pool_dir = fs::canonicalize(&pool_dir)?;

    for all in [false, true] {
        let mut command = home.command();
        command
            .arg("--dir")
            .arg(home._temp.path().join("unused"))
            .args(["serve", "status"]);
        if all {
            command.arg("--all");
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "human status (all={all}) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout)?;
        assert!(
            text.contains(r"pools\nnext\u{1b}[31m\r\t雪/"),
            "directory controls should display as literals (all={all}): {text:?}"
        );
        assert_eq!(
            text.lines().count(),
            2,
            "one foreground server should occupy one row (all={all}): {text:?}"
        );
        assert!(
            text.chars().all(|ch| !ch.is_control() || ch == '\n'),
            "human status must not contain terminal controls (all={all}): {text:?}"
        );

        let output = command.arg("--json").output()?;
        assert!(output.status.success());
        let rows: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["pool_dir"],
            canonical_pool_dir.to_string_lossy().as_ref()
        );
        assert_eq!(rows[0]["pid"].as_u64(), Some(u64::from(server.child.id())));
    }
    Ok(())
}

#[test]
fn duplicate_listeners_reject_certificate_update_without_changing_identity() -> TestResult<()> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;

    let home = TestHome::new()?;
    let pool_dir = home._temp.path().join("pools");
    let original = ManualServer::start(&home, &pool_dir, &["--remote-bind", "127.0.0.1:0"])?;
    let duplicate_address = original.remote_addr.to_string();

    let invite = home
        .command()
        .arg("--dir")
        .arg(&pool_dir)
        .args(["access", "invite", "--name", "retained-client", "--json"])
        .output()?;
    assert!(
        invite.status.success(),
        "failed to create original access key: {}",
        String::from_utf8_lossy(&invite.stderr)
    );
    let invite: Value = serde_json::from_slice(&invite.stdout)?;
    let access_key = invite["access_key"].as_str().ok_or("missing access key")?;
    let fingerprint = access_key.split('.').nth(1).ok_or("missing fingerprint")?;
    drop(original);

    let state_dir = pool_dir.join(".plasmite-serve");
    let identity_bytes = fs::read(state_dir.join("identity.json"))?;
    let identity: Value = serde_json::from_slice(&identity_bytes)?;
    let cert_path = state_dir.join(identity["cert_file"].as_str().ok_or("missing cert file")?);
    let key_path = state_dir.join(identity["key_file"].as_str().ok_or("missing key file")?);
    let cert_bytes = fs::read(&cert_path)?;
    let key_bytes = fs::read(&key_path)?;
    let keys_bytes = fs::read(state_dir.join("keys.json"))?;
    let records: Vec<Value> = serde_json::from_slice(&keys_bytes)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "retained-client");
    let original_cert = CertificateDer::from_pem_slice(&cert_bytes)?;
    assert_eq!(
        plasmite::api::access::spki_fingerprint(original_cert.as_ref())?,
        fingerprint
    );

    let replacement_pool = home._temp.path().join("replacement");
    let replacement =
        ManualServer::start(&home, &replacement_pool, &["--remote-bind", "127.0.0.1:0"])?;
    drop(replacement);
    let replacement_dir = replacement_pool.join(".plasmite-serve");
    let replacement_identity: Value =
        serde_json::from_slice(&fs::read(replacement_dir.join("identity.json"))?)?;
    let replacement_cert = replacement_dir.join(
        replacement_identity["cert_file"]
            .as_str()
            .ok_or("missing replacement cert")?,
    );
    let replacement_key = replacement_dir.join(
        replacement_identity["key_file"]
            .as_str()
            .ok_or("missing replacement key")?,
    );
    let replacement_der = CertificateDer::from_pem_file(&replacement_cert)?;
    assert_ne!(
        plasmite::api::access::spki_fingerprint(replacement_der.as_ref())?,
        fingerprint
    );

    let output = home
        .command()
        .arg("--dir")
        .arg(&pool_dir)
        .args([
            "serve",
            "--bind",
            &duplicate_address,
            "--remote-bind",
            &duplicate_address,
        ])
        .arg("--tls-cert")
        .arg(&replacement_cert)
        .arg("--tls-key")
        .arg(&replacement_key)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("local and remote listeners need different addresses"),
        "{stderr}"
    );
    assert_eq!(fs::read(state_dir.join("identity.json"))?, identity_bytes);
    assert_eq!(fs::read(&cert_path)?, cert_bytes);
    assert_eq!(fs::read(&key_path)?, key_bytes);
    assert_eq!(fs::read(state_dir.join("keys.json"))?, keys_bytes);
    let retained_cert = CertificateDer::from_pem_file(&cert_path)?;
    assert_eq!(
        plasmite::api::access::spki_fingerprint(retained_cert.as_ref())?,
        fingerprint
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn status_all_keeps_healthy_rows_when_one_setup_is_malformed() -> TestResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let home = TestHome::new()?;
    let pool_dir = home._temp.path().join("pools");
    let server = ManualServer::start(&home, &pool_dir, &["--remote-bind", "127.0.0.1:0"])?;

    let private_root = home.home.join(".plasmite");
    let services_dir = private_root.join("services");
    let broken_dir = services_dir.join("broken");
    fs::create_dir_all(&broken_dir)?;
    for directory in [&private_root, &services_dir, &broken_dir] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let setup = broken_dir.join("setup.json");
    fs::write(&setup, b"{ malformed setup")?;
    fs::set_permissions(&setup, fs::Permissions::from_mode(0o600))?;

    let output = home
        .command()
        .arg("--dir")
        .arg(home._temp.path().join("unused"))
        .args(["serve", "status", "--all", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(1));

    let rows: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    let canonical_pool_dir = fs::canonicalize(&pool_dir)?;
    let row = rows
        .iter()
        .find(|row| row["pool_dir"] == canonical_pool_dir.to_string_lossy().as_ref())
        .expect("the healthy foreground server remains in the inventory");
    assert_eq!(row["managed"], false);
    assert_eq!(row["state"], "running");
    assert_eq!(row["pid"].as_u64(), Some(u64::from(server.child.id())));

    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert!(
        error["error"].is_object(),
        "stderr should contain a structured error: {error}"
    );
    assert!(
        error["error"]["message"].as_str().is_some(),
        "structured error should include a message: {error}"
    );
    Ok(())
}
