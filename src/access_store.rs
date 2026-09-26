//! Persistent server identity and access records for one pool directory.

use fs2::FileExt;
use getrandom::fill;
use plasmite::api::{Error, ErrorKind, access::spki_fingerprint};
use rcgen::{Certificate, CertificateParams, IsCa, KeyPair, SanType};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use url::{Host, Url};

pub(crate) struct AccessStore {
    dir: PathBuf,
    cert_path: PathBuf,
    key_path: PathBuf,
    _lock: File,
    records: Mutex<Vec<AccessRecord>>,
    fingerprint: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct AccessRecord {
    name: String,
    verifier: String,
    revoked: bool,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
struct Identity {
    cert_file: String,
    key_file: String,
    front_cert_file: Option<String>,
    generated: bool,
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
        } else if identity.generated
            && shared_address.is_some_and(|address| {
                address_host(Some(address)) != address_host(identity.shared_address.as_deref())
            })
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
        Ok(Self {
            dir,
            cert_path,
            key_path,
            _lock: lock,
            records: Mutex::new(records),
            fingerprint,
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
        records.push(AccessRecord {
            name: name.to_owned(),
            verifier,
            revoked: false,
        });
        if let Err(err) = write_atomic_json(&self.dir.join("keys.json"), &*records) {
            // A failed directory sync may follow a successful rename. Match the
            // durable file so an undisclosed key never has different live state.
            *records = read_json(&self.dir.join("keys.json")).unwrap_or_default();
            return Err(err);
        }
        Ok(key)
    }

    pub(crate) fn accepts_secret(&self, secret: &str) -> bool {
        if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        let verifier = hex(&Sha256::digest(hex_decode(secret).unwrap_or_default()));
        self.records.lock().is_ok_and(|records| {
            records.iter().any(|record| {
                !record.revoked && constant_time_eq(record.verifier.as_bytes(), verifier.as_bytes())
            })
        })
    }
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

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
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

fn write_atomic_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), Error> {
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

fn ensure_private(path: &Path) -> Result<(), Error> {
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
    use super::AccessStore;
    use plasmite::api::ErrorKind;
    use rcgen::{Certificate, CertificateParams};

    #[test]
    fn key_survives_restart_and_second_owner_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = AccessStore::open(temp.path(), Some("https://127.0.0.1"), None, None)
            .expect("first owner");
        let key = first.issue("laptop").expect("key");
        let secret = key.rsplit('.').next().expect("secret");
        assert!(first.accepts_secret(secret));
        let second = AccessStore::open(temp.path(), Some("https://127.0.0.1"), None, None);
        assert_eq!(second.err().expect("lock failure").kind(), ErrorKind::Busy);
        drop(first);
        let restarted =
            AccessStore::open(temp.path(), Some("https://127.0.0.1"), None, None).expect("restart");
        assert!(restarted.accepts_secret(secret));
        let fingerprint = restarted.fingerprint().to_owned();
        drop(restarted);
        let moved = AccessStore::open(temp.path(), Some("https://127.0.0.2"), None, None)
            .expect("same-key certificate renewal");
        assert_eq!(moved.fingerprint(), fingerprint);
        assert!(moved.accepts_secret(secret));
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
