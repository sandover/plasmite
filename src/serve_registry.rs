//! Discover this user's live servers through loopback identity checks.

use plasmite::api::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const STATUS_PATH: &str = "/v0/serve/status";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ServerDetails {
    pub(crate) pid: u32,
    #[serde(serialize_with = "serialize_directory")]
    pub(crate) pool_dir: PathBuf,
    pub(crate) local_url: String,
    pub(crate) remote_url: Option<String>,
}

fn serialize_directory<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Discovery only displays this path. Unix filenames need not be UTF-8, so
    // use the same readable spelling as path display instead of blocking serve.
    serializer.serialize_str(&path.to_string_lossy())
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Registration {
    instance: String,
    #[serde(flatten)]
    pub(crate) details: ServerDetails,
}

pub(crate) struct RegisteredServer {
    path: PathBuf,
    pub(crate) registration: Registration,
    #[cfg(windows)]
    _directory: crate::access_store::DirectoryGuard,
}

impl Drop for RegisteredServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn directory() -> Result<PathBuf, Error> {
    #[cfg(windows)]
    if let Some(directory) = crate::serve_service::windows::registry_directory() {
        return Ok(directory);
    }
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = home.or_else(|| std::env::var_os("USERPROFILE"));
    let home = home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            Error::new(ErrorKind::Usage)
                .with_message("server discovery requires an absolute user home directory")
        })?;
    Ok(home.join(".plasmite/servers"))
}

fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::new(ErrorKind::Io)
        .with_message("failed to access server registry")
        .with_path(path)
        .with_source(err)
}

// Compare canonical OS path bytes without using the display spelling, which
// replaces invalid UTF-8 on Unix. This value is local to this host.
pub(crate) fn directory_identity(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::canonicalize(path)
        .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
        .map_err(|err| io_error(path, err))
}

pub(crate) fn register(
    pool_dir: &Path,
    local: SocketAddr,
    remote_url: Option<String>,
) -> Result<RegisteredServer, Error> {
    let directory = directory()?;
    #[cfg(not(windows))]
    std::fs::create_dir_all(directory.parent().expect("registry has a parent"))
        .map_err(|err| io_error(&directory, err))?;
    #[cfg(windows)]
    let guard = crate::access_store::create_private_dir(&directory)?;
    #[cfg(not(windows))]
    crate::access_store::create_private_dir(&directory)?;
    crate::access_store::ensure_private(&directory)?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message(format!("failed to create server identity: {err}"))
    })?;
    let instance: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let registration = Registration {
        details: ServerDetails {
            pid: std::process::id(),
            pool_dir: std::fs::canonicalize(pool_dir).map_err(|err| io_error(pool_dir, err))?,
            local_url: format!("http://{local}"),
            remote_url,
        },
        instance,
    };
    let path = directory.join(format!("{}.json", registration.instance));
    crate::access_store::write_atomic_json(&path, &registration)?;
    Ok(RegisteredServer {
        path,
        registration,
        #[cfg(windows)]
        _directory: guard,
    })
}

pub(crate) fn running() -> Result<Vec<ServerDetails>, Error> {
    let mut servers = running_in(&directory()?)?;
    #[cfg(windows)]
    if crate::serve_service::windows::registry_directory().is_none() {
        servers.extend(crate::serve_service::windows::running()?);
    }
    servers.sort_by(|a, b| (&a.pool_dir, a.pid).cmp(&(&b.pool_dir, b.pid)));
    servers.dedup();
    Ok(servers)
}

pub(crate) fn running_in(directory: &Path) -> Result<Vec<ServerDetails>, Error> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_error(directory, err)),
    };
    crate::access_store::ensure_private(directory)?;
    #[cfg(windows)]
    let _guard =
        crate::server_private::open_directory(directory).map_err(|err| io_error(directory, err))?;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .timeout(Duration::from_millis(300))
        .build();
    let mut servers = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_error(directory, err))?;
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        if crate::access_store::ensure_private(&entry.path()).is_err() {
            continue;
        }
        #[cfg(not(windows))]
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        #[cfg(not(windows))]
        let Ok(saved) = serde_json::from_reader::<_, Registration>(file.take(16 * 1024)) else {
            continue;
        };
        #[cfg(windows)]
        let Ok(saved) = crate::access_store::read_json::<Registration>(&entry.path()) else {
            continue;
        };
        // Only contact numeric loopback addresses generated by this registry.
        let Some(address) = saved.details.local_url.strip_prefix("http://") else {
            continue;
        };
        let Ok(address) = address.parse::<SocketAddr>() else {
            continue;
        };
        if !address.ip().is_loopback() {
            continue;
        }
        let Ok(response) = agent
            .get(&format!("{}{STATUS_PATH}", saved.details.local_url))
            .call()
        else {
            continue;
        };
        let Ok(live) =
            serde_json::from_reader::<_, Registration>(response.into_reader().take(16 * 1024))
        else {
            continue;
        };
        // A new process on a reused port must not revive an old registration.
        if live == saved {
            servers.push(live.details);
        }
    }
    servers.sort_by(|a, b| (&a.pool_dir, a.pid).cmp(&(&b.pool_dir, b.pid)));
    Ok(servers)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn non_utf8_directory_serializes_for_registry_and_live_status() {
        let registration = Registration {
            instance: "test".to_owned(),
            details: ServerDetails {
                pid: 1,
                pool_dir: std::ffi::OsString::from_vec(b"/tmp/pools-\xff".to_vec()).into(),
                local_url: "http://127.0.0.1:9700".to_owned(),
                remote_url: None,
            },
        };
        let json = serde_json::to_string(&registration).expect("directory display serializes");
        let saved: Registration = serde_json::from_str(&json).expect("registry entry");
        let live: Registration = serde_json::from_str(&json).expect("live status");
        assert!(saved == live);
        assert_eq!(saved.details.pool_dir.to_str(), Some("/tmp/pools-\u{fffd}"));
    }
}
