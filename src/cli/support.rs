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
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use url::Url;

pub(crate) fn resolve_poolref(input: &str, pool_dir: &Path) -> Result<PathBuf, Error> {
    if input.starts_with("udp://") {
        return Err(udp_unsupported());
    }
    if input.chars().any(std::path::is_separator) {
        return Ok(PathBuf::from(input));
    }
    resolve_named_pool_path(input, pool_dir).map_err(map_pool_name_resolve_error)
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

pub(crate) fn resolve_pool_target(input: &str, pool_dir: &Path) -> Result<PoolTarget, Error> {
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
        .with_hint("Use udp://HOST:PORT/POOL with feed, follow, or duplex.")
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
pub(crate) const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(50);
pub(crate) const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;
pub(crate) const DEFAULT_MAX_TAIL_TIMEOUT_MS: u64 = 30_000;
pub(crate) const DEFAULT_MAX_TAIL_CONCURRENCY: usize = 64;

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

// Display argv as readable text; this does not produce portable shell quoting.
pub(crate) fn render_shell_agnostic_token(token: &str) -> String {
    if !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        token.to_string()
    } else {
        serde_json::to_string(token).unwrap_or_else(|_| format!("\"{token}\""))
    }
}

pub(crate) fn render_shell_agnostic_command(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| render_shell_agnostic_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn add_missing_seq_hint(err: Error, pool_ref: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.seq().is_none() || err.hint().is_some() {
        return err;
    }
    err.with_hint(format!(
        "Check available messages: plasmite pool info {pool_ref} (or plasmite follow {pool_ref} --tail 10)."
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

pub(crate) fn parse_since(input: &str, now_ns: u64) -> Result<u64, Error> {
    crate::since::parse_since_ns(input, now_ns).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid --since value")
            .with_hint("Use RFC 3339 (2026-02-02T23:45:00Z) or relative like 5m.")
            .with_source(err)
    })
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct RetryConfig {
    pub(crate) retries: u32,
    pub(crate) delay: Duration,
}

pub(crate) fn parse_retry_config(
    retry: u32,
    retry_delay: Option<&str>,
) -> Result<Option<RetryConfig>, Error> {
    if retry == 0 {
        return Ok(None);
    }
    let delay = match retry_delay {
        Some(value) => parse_duration(value)?,
        None => DEFAULT_RETRY_DELAY,
    };
    Ok(Some(RetryConfig {
        retries: retry,
        delay,
    }))
}

pub(crate) fn parse_duration(input: &str) -> Result<Duration, Error> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_hint("Use a number plus ms|s|m|h (e.g. 10s)."));
    }
    let split = trimmed.char_indices().find(|(_, ch)| !ch.is_ascii_digit());
    let (num_str, unit) = match split {
        Some((idx, _)) => trimmed.split_at(idx),
        None => ("", ""),
    };
    if num_str.is_empty() || unit.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_hint("Use a number plus ms|s|m|h (e.g. 10s)."));
    }
    let value: u64 = num_str.parse().map_err(|_| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid duration")
            .with_hint("Use a number plus ms|s|m|h (e.g. 10s).")
    })?;
    let millis = match unit {
        "ms" => value,
        "s" => value.saturating_mul(1_000),
        "m" => value.saturating_mul(60_000),
        "h" => value.saturating_mul(3_600_000),
        _ => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("invalid duration")
                .with_hint("Use a number plus ms|s|m|h (e.g. 10s)."));
        }
    };
    Ok(Duration::from_millis(millis))
}

pub(crate) fn add_retry_hint(err: Error, attempts: u32, waited: Duration) -> Error {
    let info = format!(
        "Retry attempts: {attempts} (waited {}ms).",
        waited.as_millis()
    );
    if let Some(hint) = err.hint().map(|hint| hint.to_string()) {
        err.with_hint(format!("{hint} {info}"))
    } else {
        err.with_hint(info)
    }
}

/// Busy occurs before an append commits. I/O failures may follow publication,
/// so repeating them could append the same message twice.
pub(crate) fn retry_append<T, F>(config: Option<RetryConfig>, mut f: F) -> Result<T, Error>
where
    F: FnMut() -> Result<T, Error>,
{
    let Some(config) = config else {
        return f();
    };
    let mut attempts = 0u32;
    let mut waited = Duration::from_millis(0);
    loop {
        attempts += 1;
        match f() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempts <= config.retries && err.kind() == ErrorKind::Busy {
                    std::thread::sleep(config.delay);
                    waited += config.delay;
                    continue;
                }
                if attempts > 1 {
                    return Err(add_retry_hint(err, attempts, waited));
                }
                return Err(err);
            }
        }
    }
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

pub(crate) fn message_to_json(message: plasmite::api::Message) -> Value {
    MessageWire::new(message.seq, message.time, message.meta.tags, message.data).into_json()
}

pub(crate) fn message_from_frame(frame: &FrameRef) -> Result<Value, Error> {
    message_from_lite3(frame.seq, frame.timestamp_ns, &frame.payload)
}

pub(crate) fn message_from_lite3(
    seq: u64,
    timestamp_ns: u64,
    payload: &[u8],
) -> Result<Value, Error> {
    plasmite::api::Message::from_lite3(seq, timestamp_ns, payload).map(message_to_json)
}

#[cfg(test)]
mod append_retry_tests {
    use super::*;
    use serde_json::json;
    use std::io;

    #[test]
    fn append_does_not_repeat_a_write_after_an_io_failure() {
        for kind in [
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::TimedOut,
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let mut pool = plasmite::api::Pool::create(
                dir.path().join("pool.plasmite"),
                plasmite::api::PoolOptions::new(1024 * 1024),
            )
            .expect("create");
            let payload =
                plasmite::api::lite3::encode_message(&[], &json!({"n": 1})).expect("encode");
            let result: Result<(), Error> = retry_append(
                Some(RetryConfig {
                    retries: 3,
                    delay: Duration::ZERO,
                }),
                || {
                    pool.append(payload.as_slice())?;
                    Err(Error::new(ErrorKind::Io).with_source(io::Error::from(kind)))
                },
            );
            assert!(result.is_err());
            assert_eq!(pool.bounds().expect("bounds").newest_seq, Some(1));
        }
    }
}
