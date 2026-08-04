//! Purpose: Generate secure bootstrap artifacts for `plasmite serve init`.
//! Exports: `ServeInitConfig`, `ServeInitResult`, `init`.
//! Role: Pure-ish orchestration for path resolution, artifact generation, and safe writes.
//! Invariants: Token values are never printed; only paths and commands are returned.
//! Invariants: Existing files are never overwritten unless `force` is set.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use getrandom::fill as fill_random;
use rcgen::{Certificate, CertificateParams, SanType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use plasmite::api::{Error, ErrorKind};

#[derive(Debug)]
pub struct ServeInitConfig {
    pub output_dir: PathBuf,
    pub token_file: PathBuf,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub bind: SocketAddr,
    pub host: Option<String>,
    pub token_only: bool,
    pub force: bool,
}

#[derive(Debug)]
pub struct ServeInitResult {
    pub client_host: String,
    pub token_file: String,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub tls_fingerprint: Option<String>,
    pub token_only: bool,
    pub server_commands: Vec<String>,
    pub client_commands: Vec<String>,
    pub curl_client_commands: Vec<String>,
    pub overwrote_existing: bool,
}

struct Artifact {
    dest: PathBuf,
    contents: Vec<u8>,
    private: bool,
}

impl Artifact {
    fn private(dest: &Path, contents: Vec<u8>) -> Self {
        Self {
            dest: dest.to_path_buf(),
            contents,
            private: true,
        }
    }

    fn public(dest: &Path, contents: Vec<u8>) -> Self {
        Self {
            dest: dest.to_path_buf(),
            contents,
            private: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TransactionEntry {
    dest: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    had_original: bool,
}

pub fn init(config: ServeInitConfig) -> Result<ServeInitResult, Error> {
    let output_dir = absolutize(&config.output_dir)?;
    let token_file = resolve_artifact_path(&output_dir, &config.token_file);
    let tls_cert = resolve_artifact_path(&output_dir, &config.tls_cert);
    let tls_key = resolve_artifact_path(&output_dir, &config.tls_key);
    let client_host = if config.token_only {
        config.host.clone().unwrap_or_else(|| {
            if config.bind.ip().is_unspecified() {
                "YOUR-HOST".to_string()
            } else {
                config.bind.ip().to_string()
            }
        })
    } else {
        resolve_client_host(config.host.as_deref(), config.bind.ip())?
    };
    let artifact_paths = if config.token_only {
        vec![&token_file]
    } else {
        ensure_distinct_paths(&[&token_file, &tls_cert, &tls_key])?;
        vec![&token_file, &tls_cert, &tls_key]
    };

    for path in &artifact_paths {
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to create artifact directory")
                    .with_path(parent)
                    .with_source(err)
            })?;
        }
    }

    let lock_path = output_dir.join(".plasmite-serve-init.lock");
    let lock = open_private_file(&lock_path, false)?;
    lock.lock_exclusive().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to lock serve init output")
            .with_path(&lock_path)
            .with_source(err)
    })?;
    let journal_path = output_dir.join(".plasmite-serve-init.transaction.json");
    recover_transaction(&journal_path)?;

    let token = generate_token()?;
    let (cert_display, key_display, tls_fingerprint, artifacts) = if config.token_only {
        (
            None,
            None,
            None,
            vec![Artifact::private(
                &token_file,
                format!("{token}\n").into_bytes(),
            )],
        )
    } else {
        let (cert_pem, key_pem, cert_der) = generate_self_signed_pem(&client_host)?;
        (
            Some(tls_cert.display().to_string()),
            Some(tls_key.display().to_string()),
            Some(format_cert_fingerprint(&cert_der)),
            vec![
                Artifact::private(&token_file, format!("{token}\n").into_bytes()),
                Artifact::public(&tls_cert, cert_pem.into_bytes()),
                Artifact::private(&tls_key, key_pem.into_bytes()),
            ],
        )
    };
    let existing_count = artifacts
        .iter()
        .filter(|artifact| artifact.dest.exists())
        .count();
    if !config.force {
        if let Some(artifact) = artifacts.iter().find(|artifact| artifact.dest.exists()) {
            return Err(Error::new(ErrorKind::AlreadyExists)
                .with_message("serve init artifact already exists")
                .with_path(&artifact.dest)
                .with_hint("Re-run with --force to overwrite or choose different paths."));
        }
    }
    replace_artifacts(&output_dir, &artifacts)?;

    let token_display = token_file.display().to_string();
    let bind = config.bind.to_string();
    let serve_cmd = if config.token_only {
        format!(
            "plasmite serve --bind {bind} --allow-non-loopback --insecure-no-tls --token-file {}",
            quote_for_shell(&token_display)
        )
    } else {
        format!(
            "plasmite serve --bind {bind} --allow-non-loopback --token-file {} --tls-cert {} --tls-key {}",
            quote_for_shell(&token_display),
            quote_for_shell(cert_display.as_deref().expect("cert")),
            quote_for_shell(key_display.as_deref().expect("key")),
        )
    };
    let scheme = if config.token_only { "http" } else { "https" };
    let base_url = format!(
        "{scheme}://{}:{}",
        url_host(&client_host),
        config.bind.port()
    );
    let pool_url = format!("{base_url}/demo");
    let append_url = format!("{base_url}/v0/pools/demo/append");
    let tail_url = format!("{base_url}/v0/pools/demo/tail?timeout_ms=5000");
    let feed_cmd = format!(
        "plasmite feed {} --token-file {} --tls-ca {} '{{\"hello\":\"world\"}}'",
        quote_for_shell(&pool_url),
        quote_for_shell(&token_display),
        quote_for_shell(cert_display.as_deref().unwrap_or("<tls-cert>")),
    );
    let follow_cmd = format!(
        "plasmite follow {} --token-file {} --tls-ca {} --tail 10",
        quote_for_shell(&pool_url),
        quote_for_shell(&token_display),
        quote_for_shell(cert_display.as_deref().unwrap_or("<tls-cert>")),
    );
    let append_cmd = format!(
        "curl --cacert {} -sS -X POST -H 'Authorization: Bearer <token>' -H 'content-type: application/json' --data '{{\"hello\":\"world\"}}' {}",
        quote_for_shell(cert_display.as_deref().unwrap_or("<tls-cert>")),
        quote_for_shell(&append_url),
    );
    let tail_cmd = format!(
        "curl --cacert {} -N -sS -H 'Authorization: Bearer <token>' {}",
        quote_for_shell(cert_display.as_deref().unwrap_or("<tls-cert>")),
        quote_for_shell(&tail_url),
    );

    let (client_commands, curl_client_commands) = if config.token_only {
        (Vec::new(), Vec::new())
    } else {
        (vec![feed_cmd, follow_cmd], vec![append_cmd, tail_cmd])
    };
    Ok(ServeInitResult {
        client_host,
        token_file: token_display,
        tls_cert: cert_display,
        tls_key: key_display,
        tls_fingerprint,
        token_only: config.token_only,
        server_commands: vec![serve_cmd],
        client_commands,
        curl_client_commands,
        overwrote_existing: config.force && existing_count > 0,
    })
}

fn replace_artifacts(output_dir: &Path, artifacts: &[Artifact]) -> Result<(), Error> {
    let nonce = generate_token()?;
    let journal_path = output_dir.join(".plasmite-serve-init.transaction.json");
    let mut entries: Vec<TransactionEntry> = Vec::with_capacity(artifacts.len());
    for (index, artifact) in artifacts.iter().enumerate() {
        let parent = artifact.dest.parent().unwrap_or(output_dir);
        let staged = parent.join(format!(".plasmite-init-{nonce}-{index}.staged"));
        let backup = parent.join(format!(".plasmite-init-{nonce}-{index}.backup"));
        let staged_result = if artifact.private {
            write_private_file(&staged, &artifact.contents)
        } else {
            write_public_file(&staged, &artifact.contents)
        };
        if let Err(error) = staged_result {
            for entry in &entries {
                let _ = std::fs::remove_file(&entry.staged);
            }
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        entries.push(TransactionEntry {
            dest: artifact.dest.clone(),
            staged,
            backup,
            had_original: artifact.dest.exists(),
        });
    }
    let journal = serde_json::to_vec(&entries).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to encode serve init transaction")
            .with_source(err)
    })?;
    let journal_staged = output_dir.join(format!(".plasmite-serve-init-{nonce}.journal-staged"));
    let journal_commit = write_private_file(&journal_staged, &journal).and_then(|()| {
        std::fs::rename(&journal_staged, &journal_path).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to commit serve init recovery journal")
                .with_path(&journal_path)
                .with_source(err)
        })
    });
    if let Err(error) = journal_commit {
        for entry in &entries {
            let _ = std::fs::remove_file(&entry.staged);
        }
        let _ = std::fs::remove_file(&journal_staged);
        return Err(error);
    }

    let replacement = (|| {
        for entry in &entries {
            if entry.had_original {
                std::fs::rename(&entry.dest, &entry.backup).map_err(|err| {
                    Error::new(ErrorKind::Io)
                        .with_message("failed to preserve existing serve artifact")
                        .with_path(&entry.dest)
                        .with_source(err)
                })?;
            }
            std::fs::rename(&entry.staged, &entry.dest).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to install serve artifact")
                    .with_path(&entry.dest)
                    .with_source(err)
            })?;
        }
        Ok(())
    })();
    if let Err(error) = replacement {
        recover_entries(&entries);
        let _ = std::fs::remove_file(&journal_path);
        return Err(error);
    }
    for entry in &entries {
        if entry.backup.exists() {
            std::fs::remove_file(&entry.backup).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to remove serve init backup")
                    .with_path(&entry.backup)
                    .with_source(err)
            })?;
        }
    }
    std::fs::remove_file(&journal_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to finish serve init transaction")
            .with_path(&journal_path)
            .with_source(err)
    })?;
    Ok(())
}

fn recover_transaction(journal_path: &Path) -> Result<(), Error> {
    if !journal_path.exists() {
        return Ok(());
    }
    let raw = std::fs::read(journal_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read interrupted serve init transaction")
            .with_path(journal_path)
            .with_source(err)
    })?;
    let entries: Vec<TransactionEntry> = serde_json::from_slice(&raw).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("serve init recovery journal is corrupt")
            .with_path(journal_path)
            .with_source(err)
    })?;
    recover_entries(&entries);
    std::fs::remove_file(journal_path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to clear recovered serve init transaction")
            .with_path(journal_path)
            .with_source(err)
    })
}

fn recover_entries(entries: &[TransactionEntry]) {
    for entry in entries.iter().rev() {
        if entry.backup.exists() {
            let _ = std::fs::remove_file(&entry.dest);
            let _ = std::fs::rename(&entry.backup, &entry.dest);
        } else if !entry.had_original {
            let _ = std::fs::remove_file(&entry.dest);
        }
        let _ = std::fs::remove_file(&entry.staged);
    }
}

fn resolve_client_host(host: Option<&str>, bind_ip: IpAddr) -> Result<String, Error> {
    if let Some(host) = host {
        let host = host.trim().trim_matches(|c| c == '[' || c == ']');
        if host.is_empty()
            || host.contains('/')
            || host.contains(':') && host.parse::<IpAddr>().is_err()
        {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid client-visible host")
                .with_hint("Use a DNS name or IP address, without a scheme or port."));
        }
        return Ok(host.to_string());
    }
    if bind_ip.is_unspecified() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("wildcard bind requires --host")
            .with_hint(
                "For example: plasmite serve init --bind 0.0.0.0:9700 --host pools.example.com",
            ));
    }
    Ok(bind_ip.to_string())
}

fn url_host(host: &str) -> String {
    if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn open_private_file(path: &Path, truncate: bool) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to create private file")
            .with_path(path)
            .with_source(err)
    })
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), Error> {
    let mut file = open_private_file(path, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|err| Error::new(ErrorKind::Io).with_path(path).with_source(err))?;
    }
    file.write_all(contents)
        .and_then(|_| file.sync_all())
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to write private artifact")
                .with_path(path)
                .with_source(err)
        })?;
    #[cfg(windows)]
    harden_windows_acl(path)?;
    Ok(())
}

#[cfg(windows)]
fn harden_windows_acl(path: &Path) -> Result<(), Error> {
    let account = std::env::var("USERDOMAIN")
        .ok()
        .zip(std::env::var("USERNAME").ok())
        .map(|(domain, user)| format!("{domain}\\{user}"))
        .or_else(|| std::env::var("USERNAME").ok())
        .ok_or_else(|| {
            Error::new(ErrorKind::Io)
                .with_message("failed to identify Windows account for private artifact")
        })?;
    let status = std::process::Command::new("icacls.exe")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{account}:(F)"))
        .status()
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to apply owner-only Windows ACL")
                .with_path(path)
                .with_source(err)
        })?;
    if !status.success() {
        return Err(Error::new(ErrorKind::Io)
            .with_message("failed to apply owner-only Windows ACL")
            .with_path(path)
            .with_hint("Run serve init from an account allowed to change file permissions."));
    }
    Ok(())
}

fn write_public_file(path: &Path, contents: &[u8]) -> Result<(), Error> {
    std::fs::write(path, contents).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write TLS certificate")
            .with_path(path)
            .with_source(err)
    })
}

fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    let existed = path.exists();
    std::fs::create_dir_all(path)?;
    if existed {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    harden_windows_acl(path).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(())
}

fn ensure_distinct_paths(paths: &[&PathBuf]) -> Result<(), Error> {
    let mut seen = HashSet::new();
    for path in paths {
        if !seen.insert(path.as_path().to_path_buf()) {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("serve init requires distinct artifact paths")
                .with_path(path)
                .with_hint("Use different values for --token-file, --tls-cert, and --tls-key."));
        }
    }
    Ok(())
}

fn resolve_artifact_path(output_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    output_dir.join(path)
}

fn absolutize(path: &Path) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read current directory")
            .with_source(err)
    })?;
    Ok(cwd.join(path))
}

fn generate_token() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message(format!("failed to generate random token: {err}"))
    })?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(nibble_hex(byte >> 4));
        out.push(nibble_hex(byte & 0x0f));
    }
    out
}

fn nibble_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        _ => char::from(b'a' + (nibble - 10)),
    }
}

fn generate_self_signed_pem(host: &str) -> Result<(String, String, Vec<u8>), Error> {
    let mut params = CertificateParams::new(vec!["localhost".to_string()]);
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    if let Ok(ip) = host.parse::<IpAddr>() {
        params.subject_alt_names.push(SanType::IpAddress(ip));
    } else {
        params
            .subject_alt_names
            .push(SanType::DnsName(host.to_string()));
    }
    let cert = Certificate::from_params(params).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to generate self-signed certificate")
            .with_source(err)
    })?;
    let cert_der = cert.serialize_der().map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to encode self-signed certificate")
            .with_source(err)
    })?;
    let cert_pem = cert.serialize_pem().map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to encode self-signed certificate")
            .with_source(err)
    })?;
    let key_pem = cert.serialize_private_key_pem();
    Ok((cert_pem, key_pem, cert_der))
}

fn format_cert_fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut output = String::from("SHA256:");
    for (idx, byte) in digest.iter().enumerate() {
        if idx > 0 {
            output.push(':');
        }
        output.push_str(&format!("{byte:02X}"));
    }
    output
}

fn quote_for_shell(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-' | '.' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_restores_complete_prior_state_at_each_replace_phase() {
        for phase in 0..=3 {
            let temp = tempfile::tempdir().unwrap();
            let mut entries = Vec::new();
            for index in 0..3 {
                let dest = temp.path().join(format!("artifact-{index}"));
                let staged = temp.path().join(format!("stage-{index}"));
                let backup = temp.path().join(format!("backup-{index}"));
                std::fs::write(&dest, format!("old-{index}")).unwrap();
                std::fs::write(&staged, format!("new-{index}")).unwrap();
                entries.push(TransactionEntry {
                    dest,
                    staged,
                    backup,
                    had_original: true,
                });
            }
            for entry in entries.iter().take(phase) {
                std::fs::rename(&entry.dest, &entry.backup).unwrap();
                std::fs::rename(&entry.staged, &entry.dest).unwrap();
            }
            recover_entries(&entries);
            for (index, entry) in entries.iter().enumerate() {
                assert_eq!(
                    std::fs::read_to_string(&entry.dest).unwrap(),
                    format!("old-{index}")
                );
                assert!(!entry.staged.exists());
                assert!(!entry.backup.exists());
            }
        }
    }

    #[test]
    fn recovery_removes_partially_installed_new_set() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("token");
        let entries = vec![TransactionEntry {
            dest: dest.clone(),
            staged: temp.path().join("token.staged"),
            backup: temp.path().join("token.backup"),
            had_original: false,
        }];
        std::fs::write(&dest, "new").unwrap();
        recover_entries(&entries);
        assert!(!dest.exists());
    }
}
