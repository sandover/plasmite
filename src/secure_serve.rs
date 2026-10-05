//! Start the trusted local listener and authenticated HTTPS listener together.

use crate::access_store::AccessStore;
use crate::cli::args::ServeRunArgs;
use crate::interface_wire::{ErrorContextWire, ErrorKindWire};
use crate::serve::{self, ServeConfig};
use plasmite::api::{Error, ErrorKind};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

pub(crate) fn run(pool_dir: &Path, run: &ServeRunArgs) -> Result<(), Error> {
    let run = crate::serve_service::effective_args(run)?;
    let local_bind: SocketAddr = run
        .bind
        .as_deref()
        .unwrap()
        .parse()
        .expect("validated bind");
    let remote_bind: SocketAddr = run
        .remote_bind
        .as_deref()
        .unwrap()
        .parse()
        .expect("validated bind");
    let shared_address = run.server.clone();
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
        max_body_bytes: run.max_body_bytes.unwrap(),
        max_tail_timeout_ms: run.max_tail_timeout_ms.unwrap(),
        max_concurrent_tails: run.max_tail_concurrency.unwrap(),
    };
    let remote = ServeConfig {
        bind: remote_bind,
        tls_cert: Some(store.cert_path()),
        tls_key: Some(store.key_path()),
        ..base.clone()
    };
    eprintln!(
        "Serving {}",
        crate::cli::output_support::human_literal(&pool_dir.display().to_string())
    );
    eprintln!("Local: http://{local_bind}/ui");
    eprintln!("Remote listener: {remote_bind}");
    if remote_bind.ip().is_unspecified() {
        eprintln!(
            "Remote listener accepts connections on all interfaces; use --remote-bind to select an interface."
        );
    }
    if let Some(address) = shared_address {
        eprintln!("Remote HTTPS: {address}");
    } else if remote_bind.ip().is_unspecified() {
        eprintln!("Remote address: pass SERVER as the HTTPS URL clients will use");
    } else {
        eprintln!("Remote HTTPS: https://{remote_bind}");
    }
    eprintln!(
        "Create access: plasmite --dir {} access invite NAME",
        crate::cli::output_support::human_literal(&pool_dir.display().to_string())
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
            local_admin_error(err, "failed to create access key through the local server")
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
            local_admin_error(err, "failed to list access keys through the local server")
        })?;
    serde_json::from_reader(response.into_reader()).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid access key list from local server")
            .with_source(err)
    })
}

#[derive(Deserialize)]
struct AdminErrorEnvelope {
    error: AdminErrorDetails,
}

#[derive(Deserialize)]
struct AdminErrorDetails {
    kind: ErrorKindWire,
    #[serde(flatten)]
    context: ErrorContextWire,
}

fn local_admin_error(err: ureq::Error, transport_message: &str) -> Error {
    let ureq::Error::Status(status, response) = err else {
        return Error::new(ErrorKind::Io)
            .with_message(transport_message)
            .with_hint("Start `plasmite serve` for this directory and retry.")
            .with_source(err);
    };
    if let Ok(body) = response.into_string()
        && let Ok(envelope) = serde_json::from_str::<AdminErrorEnvelope>(&body)
    {
        let kind = match envelope.error.kind {
            ErrorKindWire::Internal => ErrorKind::Internal,
            ErrorKindWire::Usage => ErrorKind::Usage,
            ErrorKindWire::NotFound => ErrorKind::NotFound,
            ErrorKindWire::AlreadyExists => ErrorKind::AlreadyExists,
            ErrorKindWire::Busy => ErrorKind::Busy,
            ErrorKindWire::Permission => ErrorKind::Permission,
            ErrorKindWire::Corrupt => ErrorKind::Corrupt,
            ErrorKindWire::Io => ErrorKind::Io,
            ErrorKindWire::RetentionGap => ErrorKind::RetentionGap,
        };
        let context = envelope.error.context;
        let mut error = Error::new(kind);
        if let Some(message) = context.message {
            error = error.with_message(message);
        }
        if let Some(hint) = context.hint {
            error = error.with_hint(hint);
        }
        if let Some(path) = context.path {
            error = error.with_path(path);
        }
        if let Some(seq) = context.seq {
            error = error.with_seq(seq);
        }
        if let Some(offset) = context.offset {
            error = error.with_offset(offset);
        }
        return error;
    }
    let kind = match status {
        400 | 413 => ErrorKind::Usage,
        401 | 403 => ErrorKind::Permission,
        404 => ErrorKind::NotFound,
        409 => ErrorKind::AlreadyExists,
        423 => ErrorKind::Busy,
        500..=599 => ErrorKind::Internal,
        _ => ErrorKind::Io,
    };
    Error::new(kind).with_message(format!("local server returned HTTP {status}"))
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
        Err(err @ ureq::Error::Status(_, _)) => Err(local_admin_error(
            err,
            "failed to revoke access key through the local server",
        )),
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

#[cfg(test)]
mod admin_error_tests {
    use super::*;

    #[test]
    fn preserves_domain_error_instead_of_suggesting_server_start() {
        let response = ureq::Response::new(
            400,
            "Bad Request",
            r#"{"error":{"kind":"Usage","message":"name is required","hint":"Choose a name."}}"#,
        )
        .unwrap();
        let error = local_admin_error(ureq::Error::Status(400, response), "connection failed");
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(error.message(), Some("name is required"));
        assert_eq!(error.hint(), Some("Choose a name."));
    }

    #[test]
    fn malformed_status_response_keeps_the_http_failure_kind() {
        let response = ureq::Response::new(403, "Forbidden", "bad envelope").unwrap();
        let error = local_admin_error(ureq::Error::Status(403, response), "connection failed");
        assert_eq!(error.kind(), ErrorKind::Permission);
        assert_eq!(error.hint(), None);
    }
}
