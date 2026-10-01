//! Shared pool references, parsing, retry, and payload conversion for CLI commands.

use crate::ErrorPolicyCli;
use crate::FollowFormat;
use crate::InputMode;
use crate::PoolTarget;
use crate::interface_wire::MessageWire;
use crate::pool_paths::PoolNameResolveError;
use crate::pool_paths::resolve_named_pool_path;
use plasmite::api::Durability;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::FrameRef;
use plasmite::api::Lite3DocRef;
use plasmite::api::RemoteClient;
use plasmite::api::lite3;
use serde_json::Value;
use serde_json::json;
use std::error::Error as StdError;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use url::Url;

pub(crate) fn resolve_poolref(input: &str, pool_dir: &Path) -> Result<PathBuf, Error> {
    if input.chars().any(std::path::is_separator) {
        return Ok(PathBuf::from(input));
    }
    resolve_named_pool_path(input, pool_dir).map_err(map_pool_name_resolve_error)
}

pub(crate) fn map_pool_name_resolve_error(err: PoolNameResolveError) -> Error {
    match err {
        PoolNameResolveError::ContainsPathSeparator => {
            Error::new(ErrorKind::Usage).with_message("pool name must not contain path separators")
        }
    }
}

pub(crate) fn resolve_pool_target(input: &str, pool_dir: &Path) -> Result<PoolTarget, Error> {
    if input.starts_with("http://") || input.starts_with("https://") {
        return parse_remote_pool_target(input);
    }
    if input.contains("://") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote pool ref must use http or https scheme")
            .with_hint("Use shorthand: http(s)://host:port/<pool>."));
    }
    resolve_poolref(input, pool_dir).map(PoolTarget::LocalPath)
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
pub(crate) const DEFAULT_SNIFF_BYTES: usize = 8 * 1024;
pub(crate) const DEFAULT_SNIFF_LINES: usize = 8;
pub(crate) const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_MAX_SNIPPET_BYTES: usize = 200;
pub(crate) const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;
pub(crate) const DEFAULT_MAX_TAIL_TIMEOUT_MS: u64 = 30_000;
pub(crate) const DEFAULT_MAX_TAIL_CONCURRENCY: usize = 64;

// ── Missing-pool remediation hint policy ──────────────────────────────────
//
// When a pool is not found, the CLI tries to suggest a retry command with
// `--create`.  The rendering strategy is *shell-agnostic argv echo*:
//
//   • Render an exact command only when the CLI has a stable, unambiguous argv
//     token sequence available at error time (inline JSON, --file, repeated
//     flags, etc.).
//   • When the data source is stdin/pipe, exact reconstruction is unsafe —
//     fall back to generic wording ("add --create to your invocation").
//   • Never infer data not present in argv context.
//   • Tokens that contain special characters are JSON-escaped rather than
//     shell-quoted, keeping the hint correct across bash/zsh/fish/PowerShell.
//
// Coverage checklist (each shape should have a matching integration test):
//   1. Inline JSON payload         → exact command emitted
//   2. Paths with spaces (--file)  → exact command with quoted path args
//   3. Repeated flags (--tag …)    → exact command preserves repeated flags
//   4. Stdin/pipe usage            → fallback wording (no exact command)
//
// See also: `render_shell_agnostic_token`, `render_shell_agnostic_command`,
//           `feed_exact_create_command_hint`, `follow_exact_create_command_hint`.
// ──────────────────────────────────────────────────────────────────────────

pub(crate) fn add_missing_pool_hint(err: Error, pool_ref: &str, input: &str) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    if input.chars().any(std::path::is_separator) {
        return err.with_hint(
            "Pool path not found. Check the path or pass --dir for a different pool directory.",
        );
    }
    err.with_hint(format!(
        "Create it first: plasmite pool create {pool_ref} (or pass --dir for a different pool directory)."
    ))
}

pub(crate) fn add_missing_pool_create_hint(
    err: Error,
    command: &str,
    pool_ref: &str,
    input: &str,
    exact_command: Option<String>,
) -> Error {
    if err.kind() != ErrorKind::NotFound || err.hint().is_some() {
        return err;
    }
    if input.contains("://") {
        return err.with_hint("Remote pool not found. Create it with server-side tooling first.");
    }
    if input.chars().any(std::path::is_separator) {
        return err.with_hint(
            "Pool path not found. Check the path or pass --dir for a different pool directory.",
        );
    }
    if let Some(exact_command) = exact_command {
        return err.with_hint(format!(
            "Pool is missing. Retry with exact command: {exact_command}"
        ));
    }
    err.with_hint(format!(
        "Pool is missing. Re-run with --create (local refs only), e.g. plasmite {command} {pool_ref} --create."
    ))
}

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

pub(crate) struct FeedExactCreateHint<'a> {
    pub(crate) tags: &'a [String],
    pub(crate) data: &'a Option<String>,
    pub(crate) file: &'a Option<String>,
    pub(crate) durability: Durability,
    pub(crate) retry: u32,
    pub(crate) retry_delay: Option<&'a str>,
    pub(crate) input: InputMode,
    pub(crate) errors: ErrorPolicyCli,
    pub(crate) single_input: bool,
}

pub(crate) fn feed_exact_create_command_hint(
    pool: &str,
    options: FeedExactCreateHint<'_>,
) -> Option<String> {
    if !options.single_input {
        return None;
    }
    let mut tokens = vec![
        "plasmite".to_string(),
        "feed".to_string(),
        pool.to_string(),
        "--create".to_string(),
    ];
    for tag in options.tags {
        tokens.push("--tag".to_string());
        tokens.push(tag.clone());
    }
    if let Some(data) = options.data {
        tokens.push(data.clone());
    }
    if let Some(file) = options.file {
        tokens.push("--file".to_string());
        tokens.push(file.clone());
    }
    if options.durability != Durability::Fast {
        tokens.push("--durability".to_string());
        tokens.push(
            match options.durability {
                Durability::Fast => "fast",
                Durability::Flush => "flush",
            }
            .to_string(),
        );
    }
    if options.retry > 0 {
        tokens.push("--retry".to_string());
        tokens.push(options.retry.to_string());
    }
    if let Some(delay) = options.retry_delay {
        tokens.push("--retry-delay".to_string());
        tokens.push(delay.to_string());
    }
    if options.input != InputMode::Auto {
        tokens.push("--in".to_string());
        tokens.push(
            match options.input {
                InputMode::Auto => "auto",
                InputMode::Jsonl => "jsonl",
                InputMode::Json => "json",
                InputMode::Seq => "seq",
                InputMode::Jq => "jq",
            }
            .to_string(),
        );
    }
    if options.errors != ErrorPolicyCli::Stop {
        tokens.push("--errors".to_string());
        tokens.push("skip".to_string());
    }
    Some(render_shell_agnostic_command(&tokens))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn follow_exact_create_command_hint(
    pool: &str,
    tail: u64,
    one: bool,
    jsonl: bool,
    timeout: Option<&str>,
    data_only: bool,
    format: Option<FollowFormat>,
    since: Option<&str>,
    where_expr: &[String],
    tags: &[String],
    quiet_drops: bool,
    no_notify: bool,
    replay: Option<f64>,
) -> String {
    let mut tokens = vec![
        "plasmite".to_string(),
        "follow".to_string(),
        pool.to_string(),
        "--create".to_string(),
    ];
    if tail > 0 {
        tokens.push("--tail".to_string());
        tokens.push(tail.to_string());
    }
    if one {
        tokens.push("--one".to_string());
    }
    if jsonl {
        tokens.push("--jsonl".to_string());
    }
    if let Some(timeout) = timeout {
        tokens.push("--timeout".to_string());
        tokens.push(timeout.to_string());
    }
    if data_only {
        tokens.push("--data-only".to_string());
    }
    if let Some(format) = format {
        tokens.push("--format".to_string());
        tokens.push(
            match format {
                FollowFormat::Pretty => "pretty",
                FollowFormat::Jsonl => "jsonl",
            }
            .to_string(),
        );
    }
    if let Some(since) = since {
        tokens.push("--since".to_string());
        tokens.push(since.to_string());
    }
    for expr in where_expr {
        tokens.push("--where".to_string());
        tokens.push(expr.clone());
    }
    for tag in tags {
        tokens.push("--tag".to_string());
        tokens.push(tag.clone());
    }
    if quiet_drops {
        tokens.push("--quiet-drops".to_string());
    }
    if no_notify {
        tokens.push("--no-notify".to_string());
    }
    if let Some(replay) = replay {
        tokens.push("--replay".to_string());
        tokens.push(replay.to_string());
    }
    render_shell_agnostic_command(&tokens)
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

pub(crate) fn is_retryable(err: &Error) -> bool {
    match err.kind() {
        ErrorKind::Busy => true,
        ErrorKind::Io => err
            .source()
            .and_then(|source| source.downcast_ref::<io::Error>())
            .is_some_and(|io_err| {
                matches!(
                    io_err.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                )
            }),
        _ => false,
    }
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

pub(crate) fn retry_with_config<T, F>(config: Option<RetryConfig>, mut f: F) -> Result<T, Error>
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
                if attempts <= config.retries && is_retryable(&err) {
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

pub(crate) fn format_ts(timestamp_ns: u64) -> Result<String, Error> {
    use time::format_description::well_known::Rfc3339;
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(timestamp_ns as i128).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("invalid timestamp")
                .with_source(err)
        })?;
    ts.format(&Rfc3339).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("timestamp format failed")
            .with_source(err)
    })
}

pub(crate) fn feed_receipt_json(
    seq: u64,
    timestamp_ns: u64,
    tags: &[String],
) -> Result<Value, Error> {
    Ok(json!({
        "seq": seq,
        "time": format_ts(timestamp_ns)?,
        "meta": {
            "tags": tags,
        },
    }))
}

pub(crate) fn feed_receipt_from_message(message: &plasmite::api::Message) -> Value {
    json!({
        "seq": message.seq,
        "time": message.time,
        "meta": {
            "tags": message.meta.tags,
        },
    })
}

pub(crate) fn message_to_json(message: &plasmite::api::Message) -> Value {
    serde_json::to_value(MessageWire::new(
        message.seq,
        message.time.clone(),
        message.meta.tags.clone(),
        message.data.clone(),
    ))
    .expect("message wire data is serializable")
}

pub(crate) fn message_from_frame(frame: &FrameRef) -> Result<Value, Error> {
    let (meta, data) = decode_payload(&frame.payload)?;
    let tags = serde_json::from_value(
        meta.get("tags")
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Corrupt).with_message("missing meta.tags"))?,
    )
    .map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("meta.tags is not an array of strings")
            .with_source(err)
    })?;
    Ok(serde_json::to_value(MessageWire::new(
        frame.seq,
        format_ts(frame.timestamp_ns)?,
        tags,
        data,
    ))
    .expect("message wire data is serializable"))
}

pub(crate) fn output_value(message: Value, data_only: bool) -> Value {
    if data_only {
        message.get("data").cloned().unwrap_or(Value::Null)
    } else {
        message
    }
}

pub(crate) fn decode_payload(payload: &[u8]) -> Result<(Value, Value), Error> {
    let doc = Lite3DocRef::new(payload);
    let meta_type = doc
        .type_at_key(0, "meta")
        .map_err(|err| err.with_message("missing meta"))?;
    if meta_type != lite3::sys::LITE3_TYPE_OBJECT {
        return Err(Error::new(ErrorKind::Corrupt).with_message("meta is not object"));
    }

    let meta_ofs = doc
        .key_offset("meta")
        .map_err(|err| err.with_message("missing meta"))?;
    let tags_ofs = doc
        .key_offset_at(meta_ofs, "tags")
        .map_err(|err| err.with_message("missing meta.tags"))?;
    let tags_count = doc
        .count_at(tags_ofs)
        .map_err(|_| Error::new(ErrorKind::Corrupt).with_message("meta.tags must be array"))?;
    let mut tags = Vec::with_capacity(tags_count as usize);
    for index in 0..tags_count {
        let item_type = doc.array_item_type(tags_ofs, index).map_err(|_| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;
        if item_type != lite3::sys::LITE3_TYPE_STRING {
            return Err(
                Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
            );
        }
        let tag = doc.array_string_at(tags_ofs, index).map_err(|_| {
            Error::new(ErrorKind::Corrupt).with_message("meta.tags must be string array")
        })?;
        tags.push(tag);
    }
    let meta = json!({ "tags": tags });

    let data_ofs = doc
        .key_offset("data")
        .map_err(|err| err.with_message("missing data"))?;
    let data_json = doc.to_json_at(data_ofs, false)?;
    let data: Value = serde_json::from_str(&data_json).map_err(|err| {
        Error::new(ErrorKind::Corrupt)
            .with_message("invalid payload json")
            .with_source(err)
    })?;
    Ok((meta, data))
}
