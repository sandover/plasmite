//! Shared service settings and platform-specific server lifecycle backends.

use crate::cli::args::ServeRunArgs;
use crate::cli::support::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_TAIL_CONCURRENCY, DEFAULT_MAX_TAIL_TIMEOUT_MS,
};
use plasmite::api::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Setup {
    pub pool_dir: PathBuf,
    pub program: PathBuf,
    pub account: String,
    pub home: PathBuf,
    pub run: ServeRunArgs,
}

#[derive(Serialize)]
pub(crate) struct Status {
    pub pool_dir: PathBuf,
    pub pid: Option<u32>,
    pub local_url: String,
    pub remote_url: Option<String>,
    pub managed: bool,
    pub startup: bool,
    pub state: String,
    pub problem: Option<String>,
    pub setup: Option<Setup>,
}

pub(super) fn io_error(message: &str, path: &Path, error: std::io::Error) -> Error {
    Error::new(ErrorKind::Io)
        .with_message(message)
        .with_path(path)
        .with_source(error)
}

pub(super) fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage).with_message(message)
}

pub(super) fn id(pool_dir: &Path) -> String {
    format!(
        "net.plasmite.{:x}",
        Sha256::digest(pool_dir.as_os_str().as_encoded_bytes())
    )
}

fn absolute(path: &Path) -> Result<PathBuf, Error> {
    fs::canonicalize(path).map_err(|error| io_error("failed to resolve service path", path, error))
}

pub(crate) fn effective_args(run: &ServeRunArgs) -> Result<ServeRunArgs, Error> {
    let mut run = run.clone();
    let positional = run.server.is_some();
    let origin_name = if positional {
        "SERVER"
    } else {
        "--shared-address"
    };
    if run.server.is_some() && run.shared_address.is_some() {
        return Err(usage(
            "supply the server address once, as SERVER or --shared-address",
        ));
    }
    run.server = run.server.take().or(run.shared_address.take());
    let port = if let Some(address) = &run.server {
        let url = Url::parse(address).map_err(|error| {
            usage(&format!("{origin_name} must be an HTTPS origin")).with_source(error)
        })?;
        if url.scheme() != "https"
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(usage(&format!("{origin_name} must be an HTTPS origin"))
                .with_hint("Use https://HOST:PORT without a pool path, credentials, query, or fragment. --remote-bind controls the listening interface."));
        }
        let port = if positional {
            url.port_or_known_default()
                .expect("HTTPS has a default port")
        } else {
            9743
        };
        run.server = Some(url.origin().ascii_serialization());
        port
    } else {
        9743
    };
    run.bind.get_or_insert_with(|| "127.0.0.1:9700".into());
    run.remote_bind
        .get_or_insert_with(|| format!("0.0.0.0:{port}"));
    let bind: SocketAddr = run
        .bind
        .as_deref()
        .unwrap()
        .parse()
        .map_err(|_| usage("invalid local bind address"))?;
    if !bind.ip().is_loopback() {
        return Err(usage(
            "local administration must bind to a loopback address",
        ));
    }
    let remote_bind: SocketAddr = run
        .remote_bind
        .as_deref()
        .unwrap()
        .parse()
        .map_err(|_| usage("invalid remote bind address")
            .with_hint("Use a numeric IP:port, such as 100.101.102.103:9743 or [fd7a:115c:a1e0::abcd]:9743. Pass the public DNS name as SERVER."))?;
    if bind == remote_bind && bind.port() != 0 {
        return Err(usage("local and remote listeners need different addresses"));
    }
    run.max_body_bytes.get_or_insert(DEFAULT_MAX_BODY_BYTES);
    run.max_tail_timeout_ms
        .get_or_insert(DEFAULT_MAX_TAIL_TIMEOUT_MS);
    run.max_tail_concurrency
        .get_or_insert(DEFAULT_MAX_TAIL_CONCURRENCY);
    if run.max_body_bytes == Some(0) || run.max_body_bytes.unwrap() > usize::MAX as u64 {
        return Err(usage(
            "--max-body-bytes must fit in memory and be greater than zero",
        ));
    }
    if run.max_tail_timeout_ms == Some(0) {
        return Err(usage("--max-tail-timeout-ms must be greater than zero"));
    }
    if run.max_tail_concurrency == Some(0) {
        return Err(usage("--max-tail-concurrency must be greater than zero"));
    }
    if run.tls_cert.is_some() != run.tls_key.is_some() {
        return Err(usage("--tls-cert and --tls-key must be supplied together"));
    }
    if let (Some(cert), Some(key)) = (&run.tls_cert, &run.tls_key) {
        crate::serve::validate_tls_files(cert, key)?;
    }
    for value in [&mut run.tls_cert, &mut run.tls_key, &mut run.front_cert]
        .into_iter()
        .flatten()
    {
        *value = absolute(value)?;
    }
    if let Some(front) = &run.front_cert {
        crate::access_store::cert_fingerprint(front)?;
    }
    Ok(run)
}

pub(super) fn merge(previous: Option<&Setup>, new: &ServeRunArgs) -> Result<ServeRunArgs, Error> {
    let mut run = previous.map(|setup| setup.run.clone()).unwrap_or_default();
    if new.server.is_some() && new.shared_address.is_some() {
        return Err(usage(
            "supply the server address once, as SERVER or --shared-address",
        ));
    }
    if let Some(server) = &new.server {
        run.server = Some(server.clone());
        run.shared_address = None;
    } else if let Some(server) = &new.shared_address {
        run.server = None;
        run.shared_address = Some(server.clone());
    }
    macro_rules! update { ($($field:ident),*) => { $(if new.$field.is_some() { run.$field = new.$field.clone(); })* }; }
    update!(
        bind,
        remote_bind,
        front_cert,
        tls_cert,
        tls_key,
        max_body_bytes,
        max_tail_timeout_ms,
        max_tail_concurrency
    );
    effective_args(&run)
}

#[cfg(not(windows))]
impl Setup {
    fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            self.program.to_string_lossy().into_owned(),
            "--dir".into(),
            self.pool_dir.to_string_lossy().into_owned(),
            "serve".into(),
        ];
        if let Some(server) = &self.run.server {
            argv.push(server.clone());
        }
        macro_rules! arg {
            ($flag:expr, $value:expr) => {
                if let Some(value) = $value {
                    argv.push($flag.into());
                    argv.push(value.to_string());
                }
            };
        }
        arg!("--bind", &self.run.bind);
        arg!("--remote-bind", &self.run.remote_bind);
        arg!("--max-body-bytes", self.run.max_body_bytes);
        arg!("--max-tail-timeout-ms", self.run.max_tail_timeout_ms);
        arg!("--max-tail-concurrency", self.run.max_tail_concurrency);
        for (flag, path) in [
            ("--tls-cert", &self.run.tls_cert),
            ("--tls-key", &self.run.tls_key),
            ("--front-cert", &self.run.front_cert),
        ] {
            if let Some(path) = path {
                argv.push(flag.into());
                argv.push(path.to_string_lossy().into_owned());
            }
        }
        argv
    }
}

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub(crate) use unix::{all, control, install, logs};

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
pub(crate) use windows::{all, control, install, logs};
