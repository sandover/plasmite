//! Persistent server identity and access records for one pool directory.

use fs2::FileExt;
use getrandom::fill;
use plasmite::api::{Error, ErrorKind, access::spki_fingerprint};
use rcgen::{
    Certificate, CertificateParams, CustomExtension, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use url::{Host, Url};

pub(crate) struct AccessStore {
    dir: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
    _lock: File,
    records: Mutex<AccessState>,
    fingerprint: String,
    shared_address: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct AccessRecord {
    name: String,
    verifier: String,
    revoked: bool,
    #[serde(default)]
    created_at: Option<u64>,
}

struct AccessState {
    records: Vec<AccessRecord>,
    runtime: Vec<AccessRuntime>,
}

struct AccessRuntime {
    revoked: Arc<AtomicBool>,
    last_used_at: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessGrant {
    revoked: Arc<AtomicBool>,
}

impl AccessGrant {
    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    pub(crate) fn cancellation_flag(&self) -> Arc<AtomicBool> {
        self.revoked.clone()
    }
}

#[derive(Serialize)]
pub(crate) struct AccessKeySummary {
    id: String,
    name: String,
    revoked: bool,
    created_at: Option<u64>,
    last_used_at: Option<u64>,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
struct Identity {
    cert_file: String,
    key_file: String,
    front_cert_file: Option<String>,
    generated: bool,
    #[serde(default)]
    browser_leaf_v1: bool,
    shared_address: Option<String>,
}

impl AccessStore {
    pub(crate) fn open(
        pool_dir: &Path,
        shared_address: Option<&str>,
        owner_cert: Option<(&Path, &Path)>,
        front_cert: Option<&Path>,
    ) -> Result<Self, Error> {
        let dir = pool_dir.join(".plasmite-serve");
        create_private_dir(&dir)?;
        ensure_private(&dir)?;
        let lock = open_private(&dir.join("lock"))?;
        lock.try_lock_exclusive().map_err(|err| {
            Error::new(ErrorKind::Busy)
                .with_message("another server owns this pool directory")
                .with_path(&dir)
                .with_source(err)
        })?;
        let identity_path = dir.join("identity.json");
        let previous = if identity_path.exists() {
            ensure_private(&identity_path)?;
            Some(read_json::<Identity>(&identity_path)?)
        } else {
            None
        };
        let mut identity = previous.clone().unwrap_or(Identity {
            cert_file: String::new(),
            key_file: String::new(),
            front_cert_file: None,
            generated: owner_cert.is_none(),
            browser_leaf_v1: false,
            shared_address: None,
        });
        if previous.is_some() {
            for name in [&identity.cert_file, &identity.key_file] {
                ensure_private(&state_file(&dir, name)?)?;
            }
            if let Some(name) = &identity.front_cert_file {
                ensure_private(&state_file(&dir, name)?)?;
            }
        }
        // Validate all owner inputs before publishing any part of a new identity.
        let owner_material = owner_cert
            .map(|(cert, key)| {
                crate::serve::validate_tls_files(cert, key)?;
                let cert_bytes = read_owner_file(cert, "certificate")?;
                let key_bytes = read_owner_file(key, "TLS key")?;
                Ok::<_, Error>((cert_bytes, key_bytes))
            })
            .transpose()?;
        let front_material = front_cert
            .map(|front| {
                cert_fingerprint(front)?;
                read_owner_file(front, "front certificate")
            })
            .transpose()?;
        let old_fingerprint = if previous.is_some() {
            Some(cert_fingerprint(&state_file(&dir, &identity.cert_file)?)?)
        } else {
            None
        };
        if let Some((cert_bytes, key_bytes)) = owner_material {
            let unchanged = previous.is_some()
                && std::fs::read(state_file(&dir, &identity.cert_file)?)
                    .ok()
                    .as_deref()
                    == Some(cert_bytes.as_slice())
                && std::fs::read(state_file(&dir, &identity.key_file)?)
                    .ok()
                    .as_deref()
                    == Some(key_bytes.as_slice());
            if !unchanged {
                identity.cert_file = write_generation(&dir, "cert", &cert_bytes)?;
                identity.key_file = write_generation(&dir, "key", &key_bytes)?;
            }
            identity.generated = false;
            identity.browser_leaf_v1 = false;
        } else if previous.is_none() {
            let cert =
                Certificate::from_params(certificate_params(shared_address)?).map_err(|err| {
                    Error::new(ErrorKind::Internal)
                        .with_message("failed to create server certificate")
                        .with_source(err)
                })?;
            let cert_pem = cert.serialize_pem().map_err(|err| {
                Error::new(ErrorKind::Internal)
                    .with_message("failed to serialize server certificate")
                    .with_source(err)
            })?;
            identity.cert_file = write_generation(&dir, "cert", cert_pem.as_bytes())?;
            identity.key_file =
                write_generation(&dir, "key", cert.serialize_private_key_pem().as_bytes())?;
            identity.generated = true;
            identity.browser_leaf_v1 = true;
        } else if identity.generated
            && (!identity.browser_leaf_v1
                || shared_address.is_some_and(|address| {
                    address_host(Some(address)) != address_host(identity.shared_address.as_deref())
                }))
        {
            let key_pem =
                std::fs::read_to_string(state_file(&dir, &identity.key_file)?).map_err(|err| {
                    Error::new(ErrorKind::Io)
                        .with_message("failed to read retained server key")
                        .with_source(err)
                })?;
            let mut params = certificate_params(shared_address)?;
            params.key_pair = Some(KeyPair::from_pem(&key_pem).map_err(|err| {
                Error::new(ErrorKind::Corrupt)
                    .with_message("invalid retained server key")
                    .with_source(err)
            })?);
            let cert = Certificate::from_params(params).map_err(|err| {
                Error::new(ErrorKind::Internal)
                    .with_message("failed to renew server certificate")
                    .with_source(err)
            })?;
            let pem = cert.serialize_pem().map_err(|err| {
                Error::new(ErrorKind::Internal)
                    .with_message("failed to encode renewed certificate")
                    .with_source(err)
            })?;
            identity.cert_file = write_generation(&dir, "cert", pem.as_bytes())?;
            identity.browser_leaf_v1 = true;
        }
        if let Some(front_bytes) = front_material {
            let unchanged = identity.front_cert_file.as_deref().is_some_and(|name| {
                state_file(&dir, name)
                    .ok()
                    .and_then(|path| std::fs::read(path).ok())
                    .as_deref()
                    == Some(front_bytes.as_slice())
            });
            if !unchanged {
                identity.front_cert_file = Some(write_generation(&dir, "front", &front_bytes)?);
            }
        }
        if let Some(address) = shared_address {
            identity.shared_address = Some(address.to_owned());
        }
        let cert_path = state_file(&dir, &identity.cert_file)?;
        let key_path = state_file(&dir, &identity.key_file)?;
        crate::serve::validate_tls_files(&cert_path, &key_path)?;
        let server_fingerprint = cert_fingerprint(&cert_path)?;
        if identity.generated
            && old_fingerprint
                .as_ref()
                .is_some_and(|old| old != &server_fingerprint)
        {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("renewed certificate changed the server public key"));
        }
        let fingerprint = if let Some(name) = &identity.front_cert_file {
            cert_fingerprint(&state_file(&dir, name)?)?
        } else {
            server_fingerprint
        };
        if previous.as_ref() != Some(&identity) {
            sync_dir(&dir)?;
            write_atomic_json(&identity_path, &identity)?;
        }
        let records_path = dir.join("keys.json");
        let records = if records_path.exists() {
            ensure_private(&records_path)?;
            read_json::<Vec<AccessRecord>>(&records_path)?
        } else {
            Vec::new()
        };
        let runtime = records
            .iter()
            .map(|record| AccessRuntime {
                revoked: Arc::new(AtomicBool::new(record.revoked)),
                last_used_at: None,
            })
            .collect();
        Ok(Self {
            dir,
            cert_path,
            key_path,
            _lock: lock,
            records: Mutex::new(AccessState { records, runtime }),
            fingerprint,
            shared_address: identity.shared_address,
        })
    }

    pub(crate) fn cert_path(&self) -> PathBuf {
        self.cert_path.clone()
    }
    pub(crate) fn key_path(&self) -> PathBuf {
        self.key_path.clone()
    }
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub(crate) fn shared_address(&self) -> Option<&str> {
        self.shared_address.as_deref()
    }
    pub(crate) fn state_dir(&self) -> &Path {
        &self.dir
    }
    pub(crate) fn write_local_bind(&self, bind: std::net::SocketAddr) -> Result<(), Error> {
        write_atomic_json(&self.dir.join("local.json"), &bind.to_string())
    }

    pub(crate) fn local_bind(pool_dir: &Path) -> Result<std::net::SocketAddr, Error> {
        let path = pool_dir.join(".plasmite-serve/local.json");
        let value: String = read_json(&path)?;
        value.parse().map_err(|_| {
            Error::new(ErrorKind::Corrupt)
                .with_message("invalid saved local listener address")
                .with_path(path)
        })
    }

    pub(crate) fn saved_fingerprint(pool_dir: &Path) -> Result<String, Error> {
        let dir = pool_dir.join(".plasmite-serve");
        let identity: Identity = read_json(&dir.join("identity.json"))?;
        let name = identity
            .front_cert_file
            .as_ref()
            .unwrap_or(&identity.cert_file);
        cert_fingerprint(&state_file(&dir, name)?)
    }

    pub(crate) fn issue(&self, name: &str) -> Result<String, Error> {
        let name = name.trim();
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("access key name must contain 1 to 128 characters"));
        }
        let mut secret = [0u8; 32];
        fill(&mut secret).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message(format!("failed to generate access key: {err}"))
        })?;
        let verifier = hex(&Sha256::digest(secret));
        let key = format!("pk1.{}.{}", self.fingerprint, hex(&secret));
        let mut records = self.records.lock().map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("access key state is unavailable")
        })?;
        records.records.push(AccessRecord {
            name: name.to_owned(),
            verifier,
            revoked: false,
            created_at: unix_seconds(),
        });
        if let Err(err) = write_atomic_json(&self.dir.join("keys.json"), &records.records) {
            // A failed directory sync may follow a successful rename. Match the
            // durable file so an undisclosed key never has different live state.
            records.records = read_json(&self.dir.join("keys.json")).unwrap_or_default();
            records.sync_runtime();
            return Err(err);
        }
        records.runtime.push(AccessRuntime {
            revoked: Arc::new(AtomicBool::new(false)),
            last_used_at: None,
        });
        Ok(key)
    }

    pub(crate) fn authorize_secret(&self, secret: &str) -> Option<AccessGrant> {
        if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let verifier = hex(&Sha256::digest(hex_decode(secret).unwrap_or_default()));
        let mut state = self.records.lock().ok()?;
        let index = state
            .records
            .iter()
            .position(|record| constant_time_eq(record.verifier.as_bytes(), verifier.as_bytes()))?;
        if state.records[index].revoked {
            return None;
        }
        state.runtime[index].last_used_at = unix_seconds();
        Some(AccessGrant {
            revoked: state.runtime[index].revoked.clone(),
        })
    }

    pub(crate) fn authorize_id(&self, id: &str) -> Option<AccessGrant> {
        let mut state = self.records.lock().ok()?;
        let index = state
            .records
            .iter()
            .position(|record| key_id(&record.verifier) == id)?;
        if state.records[index].revoked {
            return None;
        }
        state.runtime[index].last_used_at = unix_seconds();
        Some(AccessGrant {
            revoked: state.runtime[index].revoked.clone(),
        })
    }

    pub(crate) fn authorize_key(&self, key: &str) -> Option<(String, AccessGrant)> {
        let mut parts = key.split('.');
        if parts.next()? != "pk1" || parts.next()? != self.fingerprint {
            return None;
        }
        let secret = parts.next()?;
        if parts.next().is_some()
            || secret.len() != 64
            || !secret.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        let verifier = hex(&Sha256::digest(hex_decode(secret)?));
        let id = key_id(&verifier);
        self.authorize_id(&id).map(|grant| (id, grant))
    }

    pub(crate) fn list(&self) -> Result<Vec<AccessKeySummary>, Error> {
        let state = self.records.lock().map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("access key state is unavailable")
        })?;
        Ok(state
            .records
            .iter()
            .zip(&state.runtime)
            .map(|(record, runtime)| AccessKeySummary {
                id: key_id(&record.verifier),
                name: record.name.clone(),
                revoked: record.revoked,
                created_at: record.created_at,
                last_used_at: runtime.last_used_at,
            })
            .collect())
    }

    pub(crate) fn revoke(&self, id: &str) -> Result<(), Error> {
        let mut state = self.records.lock().map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("access key state is unavailable")
        })?;
        let index = state
            .records
            .iter()
            .position(|record| key_id(&record.verifier) == id)
            .ok_or_else(|| Error::new(ErrorKind::NotFound).with_message("access key not found"))?;
        if state.records[index].revoked {
            return Ok(());
        }
        let mut updated = state.records.clone();
        updated[index].revoked = true;
        if let Err(err) = write_atomic_json(&self.dir.join("keys.json"), &updated) {
            state.records = read_json(&self.dir.join("keys.json")).unwrap_or_default();
            state.sync_runtime();
            return Err(err);
        }
        state.records = updated;
        state.runtime[index].revoked.store(true, Ordering::Release);
        Ok(())
    }
}

impl AccessState {
    fn sync_runtime(&mut self) {
        for runtime in self.runtime.iter().skip(self.records.len()) {
            runtime.revoked.store(true, Ordering::Release);
        }
        self.runtime.truncate(self.records.len());
        for (index, record) in self.records.iter().enumerate() {
            if let Some(runtime) = self.runtime.get(index) {
                runtime.revoked.store(record.revoked, Ordering::Release);
            } else {
                self.runtime.push(AccessRuntime {
                    revoked: Arc::new(AtomicBool::new(record.revoked)),
                    last_used_at: None,
                });
            }
        }
    }
}

fn key_id(verifier: &str) -> String {
    hex(&Sha256::digest(verifier.as_bytes()))
}

fn unix_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs())
}

fn state_file(dir: &Path, name: &str) -> Result<PathBuf, Error> {
    if name.is_empty()
        || name.contains("..")
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
    {
        return Err(
            Error::new(ErrorKind::Corrupt).with_message("invalid server identity file name")
        );
    }
    Ok(dir.join(name))
}

fn read_owner_file(path: &Path, what: &str) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message(format!("failed to read owner {what}"))
            .with_path(path)
            .with_source(err)
    })
}

fn write_generation(dir: &Path, kind: &str, bytes: &[u8]) -> Result<String, Error> {
    let mut nonce = [0u8; 8];
    fill(&mut nonce).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message(format!("failed to prepare server identity: {err}"))
    })?;
    let name = format!("{kind}-{}.pem", hex(&nonce));
    write_new_private(&dir.join(&name), bytes)?;
    Ok(name)
}

fn address_host(address: Option<&str>) -> Option<String> {
    address
        .and_then(|address| Url::parse(address).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<(), Error> {
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to sync server identity directory")
                .with_path(path)
                .with_source(err)
        })
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<(), Error> {
    Ok(())
}

fn certificate_params(shared_address: Option<&str>) -> Result<CertificateParams, Error> {
    let mut params = CertificateParams::new(vec!["localhost".to_string()]);
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // rcgen's NoCa omits Basic Constraints. An explicit critical cA:false
    // extension lets browser trust setup reject any certificate with issuer
    // authority before it reaches an operating-system trust store.
    let mut basic_constraints =
        CustomExtension::from_oid_content(&[2, 5, 29, 19], vec![0x30, 0x00]);
    basic_constraints.set_criticality(true);
    params.custom_extensions.push(basic_constraints);
    params.subject_alt_names.push(SanType::IpAddress(IpAddr::V4(
        std::net::Ipv4Addr::LOCALHOST,
    )));
    params.subject_alt_names.push(SanType::IpAddress(IpAddr::V6(
        std::net::Ipv6Addr::LOCALHOST,
    )));
    if let Some(address) = shared_address {
        let url = Url::parse(address).map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid shared address")
                .with_source(err)
        })?;
        match url.host() {
            Some(Host::Ipv4(ip)) => params
                .subject_alt_names
                .push(SanType::IpAddress(IpAddr::V4(ip))),
            Some(Host::Ipv6(ip)) => params
                .subject_alt_names
                .push(SanType::IpAddress(IpAddr::V6(ip))),
            Some(Host::Domain(host)) if host != "localhost" => params
                .subject_alt_names
                .push(SanType::DnsName(host.to_owned())),
            Some(Host::Domain(_)) => {}
            None => {
                return Err(
                    Error::new(ErrorKind::Usage).with_message("shared address requires a host")
                );
            }
        }
    }
    Ok(params)
}

fn cert_fingerprint(path: &Path) -> Result<String, Error> {
    let pem = std::fs::read(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read certificate")
            .with_path(path)
            .with_source(err)
    })?;
    let cert = CertificateDer::pem_slice_iter(&pem)
        .next()
        .ok_or_else(|| {
            Error::new(ErrorKind::Usage)
                .with_message("certificate file contains no certificate")
                .with_path(path)
        })?
        .map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid certificate PEM")
                .with_path(path)
                .with_source(err)
        })?;
    spki_fingerprint(cert.as_ref())
}

fn hex(input: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(input.len() * 2);
    for byte in input {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

fn hex_decode(input: &str) -> Option<Vec<u8>> {
    if input.len() % 2 != 0 {
        return None;
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some(((hi << 4) | lo) as u8)
        })
        .collect()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&a, &b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let bytes = std::fs::read(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read server state")
            .with_path(path)
            .with_source(err)
    })?;
    serde_json::from_slice(&bytes).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid server state")
            .with_path(path)
            .with_source(err)
    })
}

pub(crate) fn write_atomic_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to encode server state")
            .with_source(err)
    })?;
    write_atomic_bytes(path, &bytes)
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut nonce = [0u8; 8];
    fill(&mut nonce).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message(format!("failed to prepare server state write: {err}"))
    })?;
    let temp = path.with_extension(format!("tmp-{}", hex(&nonce)));
    let mut file = create_private(&temp)?;
    if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::new(ErrorKind::Io)
            .with_message("failed to persist server state")
            .with_path(&temp)
            .with_source(err));
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::new(ErrorKind::Io)
            .with_message("failed to commit server state")
            .with_path(path)
            .with_source(err));
    }
    #[cfg(unix)]
    File::open(path.parent().unwrap())
        .and_then(|dir| dir.sync_all())
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to sync server state directory")
                .with_path(path)
                .with_source(err)
        })?;
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let mut file = create_private(path)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to write server identity")
                .with_path(path)
                .with_source(err)
        })
}

fn open_private(path: &Path) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to open server state")
            .with_path(path)
            .with_source(err)
    })?;
    ensure_private(path)?;
    Ok(file)
}

fn create_private(path: &Path) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to create server state")
            .with_path(path)
            .with_source(err)
    })
}

fn create_private_dir(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        if path.exists() {
            return Ok(());
        }
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to create server state directory")
                .with_path(path)
                .with_source(err)
        })
    }
    #[cfg(windows)]
    {
        let _ = path;
        Err(Error::new(ErrorKind::Usage)
            .with_message("secure serving on Windows requires private server-state ACL support"))
    }
    #[cfg(not(any(unix, windows)))]
    std::fs::create_dir(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to create server state directory")
            .with_path(path)
            .with_source(err)
    })
}

pub(crate) fn ensure_private(path: &Path) -> Result<(), Error> {
    let metadata = std::fs::symlink_metadata(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to inspect server state permissions")
            .with_path(path)
            .with_source(err)
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::new(ErrorKind::Permission)
            .with_message("server state must not be a symbolic link")
            .with_path(path));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(ErrorKind::Permission)
                .with_message("server state must be private to its owner")
                .with_path(path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AccessStore, certificate_params};
    use plasmite::api::ErrorKind;
    use rcgen::{Certificate, CertificateParams};

    #[test]
    fn generated_certificate_marks_ca_false_explicitly() {
        let params = certificate_params(Some("https://localhost:9743/")).expect("params");
        let cert = Certificate::from_params(params).expect("certificate");
        let der = cert.serialize_der().expect("DER");
        let extension = [
            0x06, 0x03, 0x55, 0x1d, 0x13, // Basic Constraints
            0x01, 0x01, 0xff, // critical
            0x04, 0x02, 0x30, 0x00, // cA:false (DER default omitted)
        ];
        assert!(der.windows(extension.len()).any(|part| part == extension));
    }

    #[test]
    fn older_generated_identity_renews_with_same_public_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = AccessStore::open(temp.path(), Some("https://localhost:9743"), None, None)
            .expect("first identity");
        let fingerprint = first.fingerprint().to_owned();
        let old_cert = std::fs::read(first.cert_path()).expect("certificate");
        drop(first);

        let identity_path = temp.path().join(".plasmite-serve/identity.json");
        let mut identity: serde_json::Value = super::read_json(&identity_path).expect("identity");
        identity.as_object_mut().unwrap().remove("browser_leaf_v1");
        super::write_atomic_json(&identity_path, &identity).expect("old identity");

        let upgraded = AccessStore::open(temp.path(), Some("https://localhost:9743"), None, None)
            .expect("upgraded identity");
        assert_eq!(upgraded.fingerprint(), fingerprint);
        assert_ne!(std::fs::read(upgraded.cert_path()).unwrap(), old_cert);
    }

    #[test]
    fn key_survives_restart_and_second_owner_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = AccessStore::open(temp.path(), Some("https://127.0.0.1"), None, None)
            .expect("first owner");
        let key = first.issue("laptop").expect("key");
        let secret = key.rsplit('.').next().expect("secret");
        assert!(first.authorize_secret(secret).is_some());
        let second = AccessStore::open(temp.path(), Some("https://127.0.0.1"), None, None);
        assert_eq!(second.err().expect("lock failure").kind(), ErrorKind::Busy);
        drop(first);
        let restarted =
            AccessStore::open(temp.path(), Some("https://127.0.0.1"), None, None).expect("restart");
        assert!(restarted.authorize_secret(secret).is_some());
        let fingerprint = restarted.fingerprint().to_owned();
        drop(restarted);
        let moved = AccessStore::open(temp.path(), Some("https://127.0.0.2"), None, None)
            .expect("same-key certificate renewal");
        assert_eq!(moved.fingerprint(), fingerprint);
        assert!(moved.authorize_secret(secret).is_some());
    }

    #[test]
    fn invalid_front_certificate_leaves_previous_identity_usable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = AccessStore::open(temp.path(), None, None, None).expect("first identity");
        let original_cert = std::fs::read(first.cert_path()).expect("certificate");
        let original_fingerprint = first.fingerprint().to_owned();
        drop(first);

        let replacement =
            Certificate::from_params(CertificateParams::new(vec!["localhost".into()]))
                .expect("replacement");
        let cert_path = temp.path().join("replacement-cert.pem");
        let key_path = temp.path().join("replacement-key.pem");
        let bad_front = temp.path().join("bad-front.pem");
        std::fs::write(&cert_path, replacement.serialize_pem().expect("pem")).expect("cert file");
        std::fs::write(&key_path, replacement.serialize_private_key_pem()).expect("key file");
        std::fs::write(&bad_front, b"not a certificate").expect("front file");
        assert!(
            AccessStore::open(
                temp.path(),
                None,
                Some((&cert_path, &key_path)),
                Some(&bad_front)
            )
            .is_err()
        );

        let restarted = AccessStore::open(temp.path(), None, None, None).expect("restart");
        assert_eq!(
            std::fs::read(restarted.cert_path()).expect("retained cert"),
            original_cert
        );
        assert_eq!(restarted.fingerprint(), original_fingerprint);
    }
}
