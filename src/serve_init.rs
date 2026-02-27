//! Purpose: Generate secure bootstrap artifacts for `plasmite serve init`.
//! Exports: `ServeInitConfig`, `ServeInitResult`, `init`.
//! Role: Pure-ish orchestration for path resolution, artifact generation, and safe writes.
//! Invariants: Token values are never printed; only paths and commands are returned.
//! Invariants: Existing files are never overwritten unless `force` is set.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use getrandom::fill as fill_random;
use rcgen::{Certificate, CertificateParams, SanType};
use sha2::{Digest, Sha256};

use plasmite::api::{Error, ErrorKind};

#[derive(Debug)]
pub struct ServeInitConfig {
    pub output_dir: PathBuf,
    pub token_file: PathBuf,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub bind: SocketAddr,
    pub force: bool,
}

#[derive(Debug)]
pub struct ServeInitResult {
    pub token_file: String,
    pub tls_cert: String,
    pub tls_key: String,
    pub tls_fingerprint: String,
    pub server_commands: Vec<String>,
    pub client_commands: Vec<String>,
    pub curl_client_commands: Vec<String>,
    pub overwrote_existing: bool,
}

pub fn init(config: ServeInitConfig) -> Result<ServeInitResult, Error> {
    let output_dir = absolutize(&config.output_dir)?;
    let token_file = resolve_artifact_path(&output_dir, &config.token_file);
    let tls_cert = resolve_artifact_path(&output_dir, &config.tls_cert);
    let tls_key = resolve_artifact_path(&output_dir, &config.tls_key);
    ensure_distinct_paths(&[&token_file, &tls_cert, &tls_key])?;

    let existing_count = [&token_file, &tls_cert, &tls_key]
        .iter()
        .filter(|path| path.exists())
        .count();

    if !config.force {
        for path in [&token_file, &tls_cert, &tls_key] {
            if path.exists() {
                return Err(Error::new(ErrorKind::AlreadyExists)
                    .with_message("serve init artifact already exists")
                    .with_path(path)
                    .with_hint("Re-run with --force to overwrite or choose different paths."));
            }
        }
    }

    for path in [&token_file, &tls_cert, &tls_key] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to create artifact directory")
                    .with_path(parent)
                    .with_source(err)
            })?;
        }
    }

    let token = generate_token()?;
    std::fs::write(&token_file, format!("{token}\n")).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write token file")
            .with_path(&token_file)
            .with_source(err)
    })?;

    let (cert_pem, key_pem, cert_der) = generate_self_signed_pem(config.bind.ip())?;
    std::fs::write(&tls_cert, cert_pem).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write TLS certificate")
            .with_path(&tls_cert)
            .with_source(err)
    })?;
    std::fs::write(&tls_key, key_pem).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write TLS key")
            .with_path(&tls_key)
            .with_source(err)
    })?;

    let token_display = token_file.display().to_string();
    let cert_display = tls_cert.display().to_string();
    let key_display = tls_key.display().to_string();
    let tls_fingerprint = format_cert_fingerprint(&cert_der);
    let bind = config.bind.to_string();
    let serve_cmd = format!(
        "plasmite serve --bind {bind} --allow-non-loopback --token-file {} --tls-cert {} --tls-key {}",
        quote_for_shell(&token_display),
        quote_for_shell(&cert_display),
        quote_for_shell(&key_display),
    );
    let base_url = format!(
        "https://{}:{}",
        display_host(config.bind.ip()),
        config.bind.port()
    );
    let pool_url = format!("{base_url}/demo");
    let append_url = format!("{base_url}/v0/pools/demo/append");
    let tail_url = format!("{base_url}/v0/pools/demo/tail?timeout_ms=5000");
    let feed_cmd = format!(
        "plasmite feed {} --token-file {} --tls-ca {} '{{\"hello\":\"world\"}}'",
        quote_for_shell(&pool_url),
        quote_for_shell(&token_display),
        quote_for_shell(&cert_display),
    );
    let follow_cmd = format!(
        "plasmite follow {} --token-file {} --tls-ca {} --tail 10",
        quote_for_shell(&pool_url),
        quote_for_shell(&token_display),
        quote_for_shell(&cert_display),
    );
    let append_cmd = format!(
        "curl -k -sS -X POST -H 'Authorization: Bearer <token>' -H 'content-type: application/json' --data '{{\"hello\":\"world\"}}' {}",
        quote_for_shell(&append_url),
    );
    let tail_cmd = format!(
        "curl -k -N -sS -H 'Authorization: Bearer <token>' {}",
        quote_for_shell(&tail_url),
    );

    Ok(ServeInitResult {
        token_file: token_display,
        tls_cert: cert_display,
        tls_key: key_display,
        tls_fingerprint,
        server_commands: vec![serve_cmd],
        client_commands: vec![feed_cmd, follow_cmd],
        curl_client_commands: vec![append_cmd, tail_cmd],
        overwrote_existing: config.force && existing_count > 0,
    })
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

fn generate_self_signed_pem(bind_ip: IpAddr) -> Result<(String, String, Vec<u8>), Error> {
    let mut params = CertificateParams::new(vec!["localhost".to_string()]);
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    if !bind_ip.is_unspecified() {
        params.subject_alt_names.push(SanType::IpAddress(bind_ip));
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

fn display_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(addr) => {
            if addr.is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                addr.to_string()
            }
        }
        IpAddr::V6(addr) => {
            let shown = if addr.is_unspecified() {
                "::1".to_string()
            } else {
                addr.to_string()
            };
            format!("[{shown}]")
        }
    }
}
