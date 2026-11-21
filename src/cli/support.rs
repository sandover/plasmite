//! Shared pool references, parsing, retry, and payload conversion for CLI commands.

use crate::PoolTarget;
use crate::interface_wire::MessageWire;
use crate::pool_paths::PoolNameResolveError;
use crate::pool_paths::resolve_named_pool_path;
use plasmite::api::Durability;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::FrameRef;
use plasmite::api::RemoteClient;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use url::Url;

pub(crate) fn missing_pool_dir() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("cannot determine the user home directory")
        .with_hint("Supply --dir PATH to select a pool directory.")
}

pub(crate) fn resolve_poolref(input: &str, pool_dir: Option<&Path>) -> Result<PathBuf, Error> {
    if input.starts_with("udp://") {
        return Err(udp_unsupported());
    }
    if input.chars().any(std::path::is_separator) {
        return Ok(PathBuf::from(input));
    }
    resolve_named_pool_path(input, pool_dir.ok_or_else(missing_pool_dir)?)
        .map_err(map_pool_name_resolve_error)
}

pub(crate) fn map_pool_name_resolve_error(err: PoolNameResolveError) -> Error {
    match err {
        PoolNameResolveError::InvalidName => Error::new(ErrorKind::Usage)
            .with_message("pool name must be nonempty and must not contain NUL or a path prefix"),
        PoolNameResolveError::ContainsPathSeparator => {
            Error::new(ErrorKind::Usage).with_message("pool name must not contain path separators")
        }
    }
}

pub(crate) fn resolve_pool_target(
    input: &str,
    pool_dir: Option<&Path>,
) -> Result<PoolTarget, Error> {
    if input.starts_with("http://") || input.starts_with("https://") {
        return parse_remote_pool_target(input);
    }
    if input
        .get(..6)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("udp://"))
    {
        return parse_udp_pool_target(input);
    }
    if input.contains("://") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("pool ref must use http, https, or udp scheme")
            .with_hint("Use http(s)://host:port/<pool> or udp://host:port/<pool>."));
    }
    resolve_poolref(input, pool_dir).map(PoolTarget::LocalPath)
}

pub(crate) fn udp_unsupported() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("this command does not support UDP pool refs")
        .with_hint("Use udp://HOST:PORT/POOL with feed, follow, fetch, or duplex.")
}

pub(crate) fn parse_udp_pool_target(input: &str) -> Result<PoolTarget, Error> {
    let (endpoint, pool) = plasmite::api::UdpClient::parse_pool_url(input)?;
    Ok(PoolTarget::Udp { endpoint, pool })
}

pub(crate) fn parse_remote_pool_target(input: &str) -> Result<PoolTarget, Error> {
    let mut url = Url::parse(input).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid remote pool ref")
            .with_hint("Use shorthand: http(s)://host:port/<pool>.")
            .with_source(err)
    })?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool ref must not include query or fragment")
            .with_hint("Use shorthand: http(s)://host:port/<pool>."));
    }
    let path = url.path();
    if path.contains("%2f") || path.contains("%2F") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool name must not contain path separators")
            .with_hint("Use a single pool segment: http(s)://host:port/<pool>."));
    }
    let segments: Vec<_> = url
        .path_segments()
        .map(|parts| parts.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 1
        || segments[0].is_empty()
        || segments[0] == "pool"
        || (segments.len() >= 2 && segments[0] == "pools")
        || (segments.len() >= 3 && segments[0] == "v0" && segments[1] == "pools")
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool ref must use shorthand http(s)://host:port/<pool>")
            .with_hint("API-shaped URLs are not accepted as pool refs."));
    }
    let pool = segments[0].to_string();
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(PoolTarget::Remote {
        base_url: url.to_string(),
        pool,
    })
}

pub(crate) const DEFAULT_POOL_SIZE: u64 = 1024 * 1024;
pub(crate) const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;

pub(crate) fn add_missing_pool_hint(err: Error, input: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    err.with_hint(if input.contains("://") {
        "Remote pool not found. Create it with server-side tooling first."
    } else {
        "Check the displayed pool path, or use pool create with --dir for that directory."
    })
}

pub(crate) fn add_missing_pool_create_hint(err: Error) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    err.with_hint("Add --create to your existing invocation to create this local pool, or check the displayed path.")
}

pub(crate) fn add_missing_seq_hint(err: Error, pool_ref: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.seq().is_none() || err.hint().is_some() {
        return err;
    }
    err.with_hint(format!(
        "Check available messages with plasmite pool info {pool_ref} or plasmite fetch {pool_ref} --last 10."
    ))
}

pub(crate) fn add_io_hint(err: Error) -> Error {
    if err.hint().is_some() {
        return err;
    }
    match err.kind() {
        ErrorKind::Permission => err.with_hint(
            "Permission denied. Check directory permissions or use --dir to a writable location.",
        ),
        ErrorKind::Busy => {
            err.with_hint("Pool is busy (another writer holds the lock). Retry with backoff.")
        }
        ErrorKind::Io => err.with_hint("I/O error. Check the path, filesystem, and disk space."),
        _ => err,
    }
}

pub(crate) fn add_corrupt_hint(err: Error) -> Error {
    if err.kind() != ErrorKind::Corrupt || err.hint().is_some() {
        return err;
    }
    err.with_hint("Pool appears corrupt. Recreate it or investigate with validation tooling.")
}

pub(crate) fn add_internal_hint(err: Error) -> Error {
    if err.kind() != ErrorKind::Internal || err.hint().is_some() {
        return err;
    }
    err.with_hint(
        "Unexpected internal failure. Retry with RUST_BACKTRACE=1 and share command/context if it persists.",
    )
}

pub(crate) fn ensure_pool_dir(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(dir).with_source(err))
}

pub(crate) fn remote_client(base_url: String) -> Result<RemoteClient, Error> {
    RemoteClient::new(base_url)
}

pub(crate) fn parse_size(input: &str) -> Result<u64, Error> {
    let trimmed = input.trim();
    let split = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| trimmed.len());
    let digits = trimmed[..split].trim();
    let suffix = trimmed[split..].trim();

    let value: u64 = digits.trim().parse().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid size")
            .with_hint("Use bytes or K/M/G (e.g. 64M).")
            .with_source(err)
    })?;

    let multiplier = match suffix {
        "" => 1,
        "K" | "k" => 1024,
        "M" | "m" => 1024 * 1024,
        "G" | "g" => 1024 * 1024 * 1024,
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid size suffix")
                .with_hint("Use K/M/G (e.g. 64M)."));
        }
    };

    value.checked_mul(multiplier).ok_or_else(|| {
        Error::new(ErrorKind::Usage)
            .with_message("size overflow")
            .with_hint("Use a smaller size value.")
    })
}

pub(crate) fn parse_durability(input: &str) -> Result<Durability, Error> {
    match input.trim() {
        "fast" => Ok(Durability::Fast),
        "flush" => Ok(Durability::Flush),
        _ => Err(Error::new(ErrorKind::Usage)
            .with_message("invalid durability")
            .with_hint("Use fast or flush.")),
    }
}

pub(crate) fn now_ns() -> Result<u64, Error> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("time went backwards")
                .with_source(err)
        })?;
    Ok(duration.as_nanos() as u64)
}

pub(crate) fn message_to_json(message: plasmite::api::MessageEnvelope) -> Value {
    MessageWire::new(
        message.seq,
        message.time_rfc3339(),
        message.meta.tags,
        message.data,
    )
    .with_sender(message.meta.sender)
    .into_json()
}

pub(crate) fn message_from_frame(frame: &FrameRef) -> Result<Value, Error> {
    message_from_lite3(frame.seq, frame.time, &frame.payload)
}

pub(crate) fn message_from_lite3(
    seq: u64,
    timestamp_ns: u64,
    payload: &[u8],
) -> Result<Value, Error> {
    plasmite::api::MessageEnvelope::from_lite3(seq, timestamp_ns, payload).map(message_to_json)
}
