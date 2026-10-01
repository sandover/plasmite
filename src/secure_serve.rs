//! Start the trusted local listener and authenticated HTTPS listener together.

use crate::access_store::AccessStore;
use crate::cli::args::ServeRunArgs;
use crate::serve::{self, ServeConfig};
use plasmite::api::{Error, ErrorKind};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use url::Url;

pub(crate) fn run(pool_dir: &Path, run: &ServeRunArgs) -> Result<(), Error> {
    if run.tls_cert.is_some() != run.tls_key.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--tls-cert and --tls-key must be supplied together"));
    }
    if let (Some(cert), Some(key)) = (&run.tls_cert, &run.tls_key) {
        serve::validate_tls_files(cert, key)?;
    }
    let local_bind: SocketAddr = run
        .bind
        .parse()
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("invalid local bind address"))?;
    let remote_bind: SocketAddr = run
        .remote_bind
        .parse()
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("invalid remote bind address"))?;
    if !local_bind.ip().is_loopback() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("local administration must bind to a loopback address"));
    }
    let shared_address = run
        .shared_address
        .as_deref()
        .map(|value| {
            let url = Url::parse(value).map_err(|err| {
                Error::new(ErrorKind::Usage)
                    .with_message("invalid shared HTTPS address")
                    .with_source(err)
            })?;
            if url.scheme() != "https"
                || url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--shared-address must be an HTTPS origin"));
            }
            Ok(url.origin().ascii_serialization())
        })
        .transpose()?;
    std::fs::create_dir_all(pool_dir).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to create pool directory")
            .with_path(pool_dir)
            .with_source(err)
    })?;
    let store = Arc::new(AccessStore::open(
        pool_dir,
        shared_address.as_deref(),
        run.tls_cert.as_deref().zip(run.tls_key.as_deref()),
        run.front_cert.as_deref(),
    )?);
    let base = ServeConfig {
        bind: local_bind,
        pool_dir: pool_dir.to_path_buf(),
        tls_cert: None,
        tls_key: None,
        max_body_bytes: run.max_body_bytes,
        max_tail_timeout_ms: run.max_tail_timeout_ms,
        max_concurrent_tails: run.max_tail_concurrency,
    };
    let remote = ServeConfig {
        bind: remote_bind,
        tls_cert: Some(store.cert_path()),
        tls_key: Some(store.key_path()),
        ..base.clone()
    };
    eprintln!("Serving {}", pool_dir.display());
    eprintln!("Local: http://{local_bind}/ui");
    if let Some(address) = shared_address {
        eprintln!("Remote HTTPS: {address}");
    } else if remote_bind.ip().is_unspecified() {
        eprintln!("Remote address: set --shared-address to the HTTPS URL clients will use");
    } else {
        eprintln!("Remote HTTPS: https://{remote_bind}");
    }
    eprintln!(
        "Create access: plasmite --dir {} access invite --name NAME",
        pool_dir.display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("failed to start server runtime")
                .with_source(err)
        })?;
    runtime.block_on(serve::serve_secure_pair(base, remote, store))
}

#[derive(Deserialize)]
struct InviteReply {
    access_key: String,
}

pub(crate) fn invite(pool_dir: &Path, name: &str) -> Result<String, Error> {
    let (url, fingerprint) = local_admin_endpoint(pool_dir, "invite")?;
    let body = serde_json::json!({"name": name, "server_fingerprint": fingerprint}).to_string();
    let response = local_agent()
        .post(&url)
        .set("content-type", "application/json")
        .send_string(&body)
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to create access key through the local server")
                .with_hint("Start `plasmite serve` for this directory and retry.")
                .with_source(err)
        })?;
    serde_json::from_reader::<_, InviteReply>(response.into_reader())
        .map(|reply| reply.access_key)
        .map_err(|err| {
            Error::new(ErrorKind::Corrupt)
                .with_message("invalid access key response from local server")
                .with_source(err)
        })
}

pub(crate) fn keys(pool_dir: &Path) -> Result<serde_json::Value, Error> {
    let (url, fingerprint) = local_admin_endpoint(pool_dir, "keys")?;
    let response = local_agent()
        .get(&url)
        .set("x-plasmite-server-fingerprint", &fingerprint)
        .call()
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to list access keys through the local server")
                .with_hint("Start `plasmite serve` for this directory and retry.")
                .with_source(err)
        })?;
    serde_json::from_reader(response.into_reader()).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid access key list from local server")
            .with_source(err)
    })
}

pub(crate) fn revoke(pool_dir: &Path, id: &str) -> Result<(), Error> {
    let (url, fingerprint) = local_admin_endpoint(pool_dir, "revoke")?;
    let body = serde_json::json!({"id": id, "server_fingerprint": fingerprint}).to_string();
    let result = local_agent()
        .post(&url)
        .set("content-type", "application/json")
        .send_string(&body);
    match result {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => {
            Err(Error::new(ErrorKind::NotFound).with_message("access key not found"))
        }
        Err(err) => Err(Error::new(ErrorKind::Io)
            .with_message("failed to revoke access key through the local server")
            .with_hint(
                "Check `plasmite access keys` before retrying; the result may have been committed.",
            )
            .with_source(err)),
    }
}

fn local_admin_endpoint(pool_dir: &Path, operation: &str) -> Result<(String, String), Error> {
    let bind = AccessStore::local_bind(pool_dir)?;
    let fingerprint = AccessStore::saved_fingerprint(pool_dir)?;
    if !bind.ip().is_loopback() {
        return Err(
            Error::new(ErrorKind::Corrupt).with_message("saved local listener is not loopback")
        );
    }
    Ok((format!("http://{bind}/v0/access/{operation}"), fingerprint))
}

fn local_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(std::time::Duration::from_secs(30))
        .build()
}
