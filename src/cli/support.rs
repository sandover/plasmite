//! Purpose: Provide shared mechanics used by CLI command-family modules.
//! Exports: Parsing, rendering, retry, ingestion, and streaming helpers.
//! Role: Keep command modules small while leaving storage semantics in the public API.
//! Invariants: Helpers preserve the CLI contracts and do not own argument parsing or process exit.

use crate::cli::output::emit_json;
use crate::color_json::colorize_json;
use crate::ingest::{ErrorPolicy, IngestConfig, IngestFailure, IngestMode, IngestOutcome, ingest};
use crate::interface_wire::{ErrorKindWire, MessageWire, error_policy};
use crate::jq_filter::{JqFilter, matches_all};
use crate::pool_info_json::bounds_json;
use crate::pool_paths::{PoolNameResolveError, resolve_named_pool_path};
use crate::{
    ColorMode, ErrorPolicyCli, FollowFormat, InputMode, PoolTarget, RunOutcome, ServeRunArgs,
};
use crate::{serve, serve_init};
use plasmite::api::{
    AppendOptions, Cursor, CursorResult, Durability, Error, ErrorKind, FrameRef, Lite3DocRef,
    LocalClient, Pool, PoolRef, RemoteClient, RemotePool, TailOptions, ValidationIssue,
    ValidationReport, ValidationStatus, lite3,
    notify::{self, NotifyWait},
};
use plasmite::notice::{Notice, notice_json};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::error::Error as StdError;
use std::io::{self, IsTerminal, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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

pub(crate) fn emit_doctor_human(report: &ValidationReport) {
    if !io::stdout().is_terminal() {
        let label = report
            .pool_ref
            .clone()
            .unwrap_or_else(|| report.path.to_string_lossy().to_string());
        match report.status {
            ValidationStatus::Ok => {
                println!("OK: {label}");
            }
            ValidationStatus::Corrupt => {
                let last_good = report
                    .last_good_seq
                    .map(|seq| format!(" last_good_seq={seq}"))
                    .unwrap_or_default();
                let issue = report
                    .issues
                    .first()
                    .map(|issue| format!(" issue={}", issue.message))
                    .unwrap_or_default();
                println!("CORRUPT: {label}{last_good}{issue}");
            }
        }
        return;
    }

    let label = doctor_display_label(report);
    match report.status {
        ValidationStatus::Ok => {
            println!("{label}: healthy");
            println!("  messages:  {}", doctor_messages_summary(report));
            println!("  checked:   header, index, ring — 0 issues");
        }
        ValidationStatus::Corrupt => {
            let issue = report
                .issues
                .first()
                .map(|value| value.message.clone())
                .unwrap_or_else(|| "corruption detected".to_string());
            println!("{label}: corrupt");
            println!("  messages:  {}", doctor_messages_summary(report));
            println!(
                "  checked:   header, index, ring — {} issues",
                report.issues.len()
            );
            println!("  detail:    {issue}");
        }
    }
}

pub(crate) fn emit_doctor_human_summary(reports: &[ValidationReport]) {
    if reports.is_empty() {
        println!("No pools found.");
        return;
    }
    if !io::stdout().is_terminal() {
        for report in reports {
            emit_doctor_human(report);
        }
        return;
    }

    let corrupt = reports
        .iter()
        .filter(|report| report.status == ValidationStatus::Corrupt)
        .count();
    let labels = reports.iter().map(doctor_display_label).collect::<Vec<_>>();
    let message_labels = reports
        .iter()
        .map(doctor_messages_count_label)
        .collect::<Vec<_>>();
    let label_width = labels.iter().map(|value| value.len()).max().unwrap_or(0);
    let message_width = message_labels
        .iter()
        .map(|value| value.len())
        .max()
        .unwrap_or(0);
    if corrupt == 0 {
        println!("All {} pools healthy.", reports.len());
        println!();
        for idx in 0..reports.len() {
            println!(
                "  {:<label_width$}   {:<message_width$}   0 issues",
                labels[idx], message_labels[idx]
            );
        }
    } else {
        println!("{corrupt} of {} pools unhealthy.", reports.len());
        println!();
        for (idx, report) in reports.iter().enumerate() {
            let label = &labels[idx];
            let messages = &message_labels[idx];
            if report.status == ValidationStatus::Corrupt {
                println!(
                    "  ✗ {:<label_width$}   {:<message_width$}   {} issues (run `pls doctor {}` for detail)",
                    label,
                    messages,
                    report.issues.len(),
                    label
                );
            } else {
                println!("  ✓ {label:<label_width$}   {messages:<message_width$}   0 issues");
            }
        }
    }
}

pub(crate) fn doctor_display_label(report: &ValidationReport) -> String {
    if let Some(pool_ref) = report.pool_ref.as_deref() {
        let looks_like_path = pool_ref.contains('/') || pool_ref.contains('\\');
        if !looks_like_path {
            return pool_ref.to_string();
        }
    }
    if let Some(stem) = report.path.file_stem().and_then(|value| value.to_str()) {
        return stem.to_string();
    }
    short_display_path(&report.path, report.path.parent())
}

pub(crate) fn doctor_messages_summary(report: &ValidationReport) -> String {
    if let Some(stats) = doctor_message_stats(report) {
        let seq_range = format_seq_range(stats.oldest_seq, stats.newest_seq);
        if stats.count == 0 {
            return "empty".to_string();
        }
        if seq_range == "-" {
            return stats.count.to_string();
        }
        return format!("{} ({seq_range})", stats.count);
    }

    let seq_range = format_seq_range(report.last_good_seq, report.last_good_seq);
    if seq_range == "-" {
        "empty".to_string()
    } else {
        format!("visible count unavailable ({seq_range})")
    }
}

pub(crate) fn doctor_messages_count_label(report: &ValidationReport) -> String {
    if let Some(stats) = doctor_message_stats(report) {
        return format!("{} messages", stats.count);
    }
    if report.status == ValidationStatus::Ok {
        "messages unknown".to_string()
    } else {
        report
            .last_good_seq
            .map(|seq| format!("up to seq {seq}"))
            .unwrap_or_else(|| "messages unknown".to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DoctorMessageStats {
    count: u64,
    oldest_seq: Option<u64>,
    newest_seq: Option<u64>,
}

pub(crate) fn doctor_message_stats(report: &ValidationReport) -> Option<DoctorMessageStats> {
    let info = LocalClient::new()
        .pool_info(&PoolRef::path(report.path.clone()))
        .ok()?;
    Some(DoctorMessageStats {
        count: message_count_from_info(&info),
        oldest_seq: info.bounds.oldest_seq,
        newest_seq: info.bounds.newest_seq,
    })
}

pub(crate) fn report_json(report: &ValidationReport) -> Value {
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            json!({
                "code": issue.code,
                "message": issue.message,
                "seq": issue.seq,
                "offset": issue.offset,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "pool_ref": report.pool_ref,
        "path": report.path.to_string_lossy(),
        "status": match report.status {
            ValidationStatus::Ok => "ok",
            ValidationStatus::Corrupt => "corrupt",
        },
        "last_good_seq": report.last_good_seq,
        "issue_count": report.issue_count,
        "issues": issues,
        "remediation_hints": report.remediation_hints,
        "snapshot_path": report.snapshot_path.as_ref().map(|path| path.to_string_lossy()),
    })
}

pub(crate) fn doctor_report(
    client: &LocalClient,
    pool_ref: PoolRef,
    label: String,
    path: PathBuf,
) -> Result<ValidationReport, Error> {
    match client.validate_pool(&pool_ref) {
        Ok(report) => Ok(report.with_pool_ref(label)),
        Err(err) if err.kind() == ErrorKind::Corrupt => {
            Ok(ValidationReport::corrupt(path, error_issue(&err), None).with_pool_ref(label))
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn error_issue(err: &Error) -> ValidationIssue {
    ValidationIssue {
        code: "corrupt".to_string(),
        message: err.message().unwrap_or("corrupt").to_string(),
        seq: err.seq(),
        offset: err.offset(),
    }
}

pub(crate) fn list_pool_paths(pool_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = std::fs::read_dir(pool_dir).map_err(|err| {
        let kind = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::Permission,
            _ => ErrorKind::Io,
        };
        Error::new(kind)
            .with_message("failed to read pool directory")
            .with_path(pool_dir)
            .with_source(err)
    })?;

    let mut pools = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read pool directory entry")
                .with_path(pool_dir)
                .with_source(err)
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("plasmite") {
            pools.push(path);
        }
    }
    Ok(pools)
}

pub(crate) fn list_pools(pool_dir: &Path, client: &LocalClient) -> Vec<Value> {
    let mut pools = Vec::new();
    let entries = match std::fs::read_dir(pool_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return pools,
        Err(err) => {
            pools.push(pool_list_error(
                "pools",
                pool_dir,
                Error::new(ErrorKind::Io)
                    .with_message("failed to read pool directory")
                    .with_path(pool_dir)
                    .with_source(err),
            ));
            return pools;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("plasmite") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                pools.push(pool_list_error(
                    &name,
                    &path,
                    Error::new(ErrorKind::Io)
                        .with_message("failed to stat pool")
                        .with_path(&path)
                        .with_source(err),
                ));
                continue;
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(format_system_time)
            .map(Value::String)
            .unwrap_or(Value::Null);
        let pool_ref = PoolRef::path(path.clone());
        match client.pool_info(&pool_ref) {
            Ok(info) => {
                let mut map = Map::new();
                map.insert("name".to_string(), json!(name));
                map.insert("path".to_string(), json!(path.display().to_string()));
                map.insert("file_size".to_string(), json!(info.file_size));
                map.insert("bounds".to_string(), bounds_json(info.bounds));
                map.insert("mtime".to_string(), mtime);
                pools.push(Value::Object(map));
            }
            Err(err) => {
                pools.push(pool_list_error(
                    &name,
                    &path,
                    add_corrupt_hint(add_io_hint(err)),
                ));
            }
        }
    }

    pools.sort_by_key(pool_list_name);
    pools
}

pub(crate) fn emit_pool_list_table(pools: &[Value], pool_dir: &Path) {
    let interactive = io::stdout().is_terminal();
    if interactive && pools.is_empty() {
        println!(
            "No pools found in {}",
            display_pool_dir_for_humans(pool_dir)
        );
        println!();
        println!("  Create one: plasmite pool create <name>");
        return;
    }

    let has_errors = pools.iter().any(|pool| {
        pool.get("error")
            .and_then(|value| value.get("error"))
            .is_some()
    });
    let headers = if interactive && !has_errors {
        vec!["NAME", "SIZE", "MSGS", "MODIFIED", "PATH"]
    } else {
        vec![
            "NAME", "STATUS", "SIZE", "OLDEST", "NEWEST", "MTIME", "PATH", "DETAIL",
        ]
    };
    let rows = pools
        .iter()
        .map(|pool| {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("-")
                .to_string();
            let path_value = pool
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            let display_path = if path_value == "-" {
                "-".to_string()
            } else {
                short_display_path(Path::new(path_value), Some(pool_dir))
            };

            if let Some(error) = pool.get("error").and_then(|value| value.get("error")) {
                let detail = error
                    .get("message")
                    .and_then(|value| value.as_str())
                    .or_else(|| error.get("kind").and_then(|value| value.as_str()))
                    .unwrap_or("error")
                    .to_string();
                vec![
                    name,
                    "ERR".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    display_path,
                    detail,
                ]
            } else {
                let oldest = pool
                    .get("bounds")
                    .and_then(|value| value.get("oldest"))
                    .and_then(|value| value.as_u64());
                let newest = pool
                    .get("bounds")
                    .and_then(|value| value.get("newest"))
                    .and_then(|value| value.as_u64());
                let msg_count = match (oldest, newest) {
                    (Some(a), Some(b)) => b.saturating_sub(a).saturating_add(1),
                    _ => 0,
                };
                let size = pool
                    .get("file_size")
                    .and_then(|value| value.as_u64())
                    .map(|value| {
                        if interactive {
                            format_bytes(value)
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                let oldest_str = oldest
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let newest_str = newest
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let mtime = pool
                    .get("mtime")
                    .and_then(|value| value.as_str())
                    .map(|value| {
                        if interactive {
                            format_relative_from_timestamp(value)
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                if interactive && !has_errors {
                    vec![name, size, msg_count.to_string(), mtime, display_path]
                } else {
                    vec![
                        name,
                        "OK".to_string(),
                        size,
                        oldest_str,
                        newest_str,
                        mtime,
                        display_path,
                        String::new(),
                    ]
                }
            }
        })
        .collect::<Vec<_>>();

    emit_table(&headers, &rows);
}

pub(crate) fn emit_pool_create_table(created: &[Value], pool_dir: &Path) {
    if io::stdout().is_terminal() {
        if created.len() == 1 {
            if let Some(pool) = created.first() {
                let name = pool
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("pool");
                let size = pool
                    .get("file_size")
                    .and_then(|value| value.as_u64())
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                let index = pool
                    .get("index_capacity")
                    .and_then(|value| value.as_u64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let path = pool
                    .get("path")
                    .and_then(|value| value.as_str())
                    .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                    .unwrap_or_else(|| "-".to_string());
                println!("Created {name} ({size}, {index} index slots)");
                println!("  path: {path}");
            }
            return;
        }

        let size = created
            .first()
            .and_then(|pool| pool.get("file_size"))
            .and_then(|value| value.as_u64())
            .map(format_bytes)
            .unwrap_or_else(|| "-".to_string());
        println!("Created {} pools ({} each)", created.len(), size);
        for pool in created {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("pool");
            let path = pool
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                .unwrap_or_else(|| "-".to_string());
            println!("  - {name} ({path})");
        }
        return;
    }

    let headers = ["NAME", "SIZE", "INDEX", "PATH"];
    let rows = created
        .iter()
        .map(|pool| {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("-")
                .to_string();
            let size = pool
                .get("file_size")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let index = pool
                .get("index_capacity")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let path = pool
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                .unwrap_or_else(|| "-".to_string());
            vec![name, size, index, path]
        })
        .collect::<Vec<_>>();
    emit_table(&headers, &rows);
}

pub(crate) fn pool_list_error(name: &str, path: &Path, err: Error) -> Value {
    let mut map = Map::new();
    map.insert("name".to_string(), json!(name));
    map.insert("path".to_string(), json!(path.display().to_string()));
    map.insert("error".to_string(), error_json(&err));
    Value::Object(map)
}

pub(crate) fn pool_list_name(value: &Value) -> String {
    value
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or("")
        .to_string()
}

pub(crate) fn format_system_time(time: std::time::SystemTime) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(duration.as_nanos() as i128).ok()?;
    ts.format(&Rfc3339).ok()
}

pub(crate) fn format_bytes(value: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if value < KIB {
        return value.to_string();
    }
    let (unit, suffix) = if value >= GIB {
        (GIB, "G")
    } else if value >= MIB {
        (MIB, "M")
    } else {
        (KIB, "K")
    };
    if value.is_multiple_of(unit) {
        return format!("{}{}", value / unit, suffix);
    }
    format!("{:.1}{}", (value as f64) / (unit as f64), suffix)
}

pub(crate) fn format_timestamp_human(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "-".to_string();
    }
    let parsed =
        time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc3339);
    let Ok(parsed) = parsed else {
        return trimmed.to_string();
    };
    let parsed = parsed.to_offset(time::UtcOffset::UTC);
    let format = time::format_description::parse("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    let Ok(format) = format else {
        return trimmed.to_string();
    };
    parsed
        .format(&format)
        .unwrap_or_else(|_| trimmed.to_string())
}

pub(crate) fn format_relative_time(age_ms: Option<u64>) -> String {
    let Some(age_ms) = age_ms else {
        return "-".to_string();
    };
    let seconds = (age_ms / 1000).max(1);
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d ago");
    }
    format!("{}w ago", days / 7)
}

pub(crate) fn format_seq_range(oldest: Option<u64>, newest: Option<u64>) -> String {
    match (oldest, newest) {
        (Some(oldest), Some(newest)) => format!("seq {oldest}..{newest}"),
        _ => "-".to_string(),
    }
}

pub(crate) fn format_relative_from_timestamp(value: &str) -> String {
    let Ok(parsed) =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
    else {
        return "-".to_string();
    };
    let now_ns = match now_ns() {
        Ok(value) => value,
        Err(_) => return "-".to_string(),
    };
    let now = match time::OffsetDateTime::from_unix_timestamp_nanos(now_ns as i128) {
        Ok(value) => value,
        Err(_) => return "-".to_string(),
    };
    let delta = now
        .unix_timestamp_nanos()
        .saturating_sub(parsed.unix_timestamp_nanos());
    let age_ms = (delta / 1_000_000) as u64;
    format_relative_time(Some(age_ms))
}

pub(crate) fn ensure_pool_dir(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)
        .map_err(|err| Error::new(ErrorKind::Io).with_path(dir).with_source(err))
}

pub(crate) fn read_token_file(path: &Path) -> Result<String, Error> {
    validate_secret_file(path, "token")?;
    let raw = std::fs::read_to_string(path).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("failed to read token file")
            .with_path(path)
            .with_source(err)
    })?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("token file is empty")
            .with_path(path));
    }
    Ok(token)
}

pub(crate) fn validate_secret_file(path: &Path, kind: &str) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path).map_err(|err| {
            Error::new(ErrorKind::Usage)
                .with_message(format!("failed to inspect {kind} file"))
                .with_path(path)
                .with_source(err)
        })?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(Error::new(ErrorKind::Usage)
                .with_message(format!(
                    "{kind} file permissions are too broad ({mode:03o})"
                ))
                .with_path(path)
                .with_hint(format!(
                    "Restrict it to the owner with: chmod 600 {}",
                    display_handoff_path_from_path(path)
                )));
        }
    }
    #[cfg(windows)]
    {
        let script = r#"$acl=Get-Acl -LiteralPath $args[0]; $current=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $allowed=@($current,'S-1-5-18','S-1-5-32-544'); $allows=@($acl.Access | Where-Object { $_.AccessControlType -eq 'Allow' } | ForEach-Object { try { $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value } catch { 'unresolved' } }); $bad=@($allows | Where-Object { $_ -notin $allowed -and $_ -notmatch '^S-1-5-5-\d+-\d+$' }); if ($acl.AreAccessRulesProtected -and $bad.Count -eq 0 -and $current -in $allows) { exit 0 } else { exit 3 }"#;
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .arg(path)
            .status()
            .map_err(|err| {
                Error::new(ErrorKind::Usage)
                    .with_message(format!("failed to inspect {kind} file ACL"))
                    .with_path(path)
                    .with_source(err)
            })?;
        if !status.success() {
            return Err(Error::new(ErrorKind::Usage)
                .with_message(format!("{kind} file ACL permits an unrelated Windows account"))
                .with_path(path)
                .with_hint("Regenerate it with `plasmite serve init --force`, or remove inherited access and grant only the owning account."));
        }
    }
    Ok(())
}

pub(crate) fn resolve_token_value(
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Option<String>, Error> {
    if token.is_some() && token_file.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--token cannot be combined with --token-file")
            .with_hint("Use --token-file for safer handling, or pass --token for local/dev use."));
    }
    if let Some(path) = token_file {
        return read_token_file(&path).map(Some);
    }
    Ok(token)
}

pub(crate) fn reject_remote_only_flags_for_local_target(
    command: &str,
    token: Option<&str>,
    token_file: Option<&Path>,
    tls_ca: Option<&Path>,
    tls_skip_verify: bool,
) -> Result<(), Error> {
    if token.is_none() && token_file.is_none() && tls_ca.is_none() && !tls_skip_verify {
        return Ok(());
    }
    Err(Error::new(ErrorKind::Usage)
        .with_message(format!(
            "{command} remote auth/TLS flags require a remote http(s) pool ref"
        ))
        .with_hint("Use --token/--token-file/--tls-ca/--tls-skip-verify only with http(s)://host:port/<pool> refs."))
}

pub(crate) fn emit_serve_init_human(result: &serve_init::ServeInitResult) {
    let token_path = Path::new(&result.token_file);
    if result.token_only {
        let token_file = display_handoff_path_from_path(token_path);
        println!("Private-link token initialized.");
        println!();
        println!("  Token: {token_file}");
        println!();
        println!("  Start serving over your protected private network:");
        println!("    {}", result.server_commands[0]);
        println!();
        println!(
            "  Transport is plaintext. Use this only behind a private link such as VMware host-only networking, a VPN, or an encrypted tunnel."
        );
        println!("  The token is in the file and is not printed here.");
        return;
    }
    let cert_path = Path::new(result.tls_cert.as_deref().expect("full init certificate"));
    let key_path = Path::new(result.tls_key.as_deref().expect("full init key"));
    let (output_dir, token_label, cert_label, key_label) =
        serve_init_artifact_labels(token_path, cert_path, key_path);
    let token_file = display_handoff_path_from_path(token_path);
    let tls_cert = display_handoff_path_from_path(cert_path);
    let tls_key = display_handoff_path_from_path(key_path);
    let bind = extract_bind_from_server_commands(&result.server_commands)
        .unwrap_or_else(|| "0.0.0.0:9700".to_string());
    let remote_url_host = url_host_component(&result.client_host);
    let port = bind
        .parse::<SocketAddr>()
        .map(|addr| addr.port())
        .unwrap_or(9700);
    let (headline, files_heading) = if result.overwrote_existing {
        ("Secure serving re-initialized.", "Files overwritten:")
    } else {
        ("Secure serving initialized.", "Files created:")
    };

    println!("{headline}");
    println!("Clients on your network can now read and write your pools over HTTPS.");
    println!();
    if let Some(output_dir) = output_dir {
        println!("  Output directory: {output_dir}");
        println!();
    }
    println!("  {files_heading}");
    println!("    token   {token_label}");
    println!("    cert    {cert_label}");
    println!("    key     {key_label}");
    println!();
    println!("  Fingerprint (share this with clients to verify the cert):");
    println!(
        "    {}",
        result
            .tls_fingerprint
            .as_deref()
            .expect("full init fingerprint")
    );
    println!();
    println!("  Start serving your pools:");
    println!();
    println!("    pls serve \\");
    println!("      --bind {bind} \\");
    println!("      --allow-non-loopback \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-cert {tls_cert} \\");
    println!("      --tls-key {tls_key}");
    println!();
    println!("  From another machine, read and write pools by URL:");
    println!();
    println!("    pls feed https://{remote_url_host}:{port}/demo \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-ca {tls_cert} \\");
    println!("      '{{\"hello\":\"world\"}}'");
    println!();
    println!("    pls follow https://{remote_url_host}:{port}/demo \\");
    println!("      --token-file {token_file} \\");
    println!("      --tls-ca {tls_cert} --tail 10");
    println!();
    println!("  MCP endpoint for agent clients:");
    println!("    https://{remote_url_host}:{port}/mcp");
    println!();
    println!("  Or with curl:");
    println!("    TOKEN=$(cat {token_file})");
    println!("    curl --cacert {tls_cert} -H \"Authorization: Bearer $TOKEN\" \\");
    println!("      https://{remote_url_host}:{port}/v0/pools/demo/tail?timeout_ms=5000");
    println!();
    println!("  The token is in the file, not printed here. Share the token");
    println!("  and fingerprint with collaborators out-of-band (e.g. paste");
    println!("  in a DM). Clients use the fingerprint to verify the cert");
    println!("  on first connect.");
}

pub(crate) fn url_host_component(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        return format!("[{host}]");
    }
    host.to_string()
}

pub(crate) fn serve_init_artifact_labels(
    token_path: &Path,
    cert_path: &Path,
    key_path: &Path,
) -> (Option<String>, String, String, String) {
    let common_parent = token_path.parent().and_then(|parent| {
        if cert_path.parent() == Some(parent) && key_path.parent() == Some(parent) {
            Some(parent)
        } else {
            None
        }
    });
    if let Some(parent) = common_parent {
        return (
            Some(display_pool_dir_for_humans(parent)),
            display_artifact_name(token_path),
            display_artifact_name(cert_path),
            display_artifact_name(key_path),
        );
    }
    (
        None,
        display_handoff_path_from_path(token_path),
        display_handoff_path_from_path(cert_path),
        display_handoff_path_from_path(key_path),
    )
}

pub(crate) fn display_artifact_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| display_handoff_path_from_path(path))
}

pub(crate) fn display_handoff_path_from_path(path: &Path) -> String {
    let to_dot_relative = |value: &Path| {
        let rendered = value.display().to_string();
        if rendered.starts_with("./") || rendered.starts_with("../") {
            rendered
        } else {
            format!("./{rendered}")
        }
    };

    if path.is_relative() {
        return to_dot_relative(path);
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = path.strip_prefix(&cwd)
        && !relative.as_os_str().is_empty()
    {
        return to_dot_relative(relative);
    }
    path.display().to_string()
}

pub(crate) fn display_pool_dir_for_humans(pool_dir: &Path) -> String {
    let rendered = if let Ok(cwd) = std::env::current_dir()
        && let Ok(relative) = pool_dir.strip_prefix(&cwd)
        && !relative.as_os_str().is_empty()
    {
        format!("./{}", relative.display())
    } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(relative) = pool_dir.strip_prefix(home)
        && !relative.as_os_str().is_empty()
    {
        format!("~/{}", relative.display())
    } else {
        pool_dir.display().to_string()
    };
    if rendered.ends_with('/') {
        rendered
    } else {
        format!("{rendered}/")
    }
}

pub(crate) fn extract_bind_from_server_commands(commands: &[String]) -> Option<String> {
    let command = commands.first()?;
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    tokens
        .windows(2)
        .find(|window| window[0] == "--bind")
        .map(|window| window[1].to_string())
}

pub(crate) fn emit_serve_startup_guidance(config: &serve::ServeConfig) {
    if !io::stderr().is_terminal() {
        return;
    }
    for line in build_serve_startup_lines(config) {
        eprintln!("{line}");
    }
}

pub(crate) fn build_serve_startup_lines(config: &serve::ServeConfig) -> Vec<String> {
    let tls_enabled = serve_tls_enabled(config);
    let scheme = serve_scheme(config);
    let host = display_host(config.bind.ip());
    let base_url = format!("{scheme}://{host}:{}", config.bind.port());
    let web_ui_url = format!("{base_url}/ui");
    let mcp_url = format!("{base_url}/mcp");
    let append_url = format!("{base_url}/v0/pools/demo/append");
    let curl_tls_flag = if config.tls_self_signed {
        " --cacert <trusted-cert>"
    } else {
        ""
    };
    let scope = serve_scope(config.bind.ip());
    let auth = if config.token.is_some() {
        "bearer"
    } else {
        "none"
    };
    let tls = if config.tls_self_signed {
        "temporary self-signed"
    } else if tls_enabled {
        "on"
    } else {
        "off"
    };
    let access = match config.access_mode {
        serve::AccessMode::ReadOnly => "read-only",
        serve::AccessMode::WriteOnly => "write-only",
        serve::AccessMode::ReadWrite => "read-write",
    };
    let cors = if config.cors_allowed_origins.is_empty() {
        "same-origin"
    } else {
        "allowlist"
    };

    let mut feed_cmd = format!("pls feed {base_url}/demo");
    let mut follow_cmd = format!("pls follow {base_url}/demo");
    if config.token.is_some() {
        if config.token_file_used {
            feed_cmd.push_str(" --token-file <token-file>");
            follow_cmd.push_str(" --token-file <token-file>");
        } else {
            feed_cmd.push_str(" --token <token>");
            follow_cmd.push_str(" --token <token>");
        }
    }
    if tls_enabled {
        feed_cmd.push_str(" --tls-ca <tls-cert>");
        follow_cmd.push_str(" --tls-ca <tls-cert>");
    }
    feed_cmd.push_str(" '{\"hello\":\"world\"}'");
    follow_cmd.push_str(" --tail 10");

    let mut lines = vec![
        format!("Serving pools on {base_url} ({scope})"),
        String::new(),
        format!("  UI:   {web_ui_url}"),
        format!("  MCP:  {mcp_url}"),
        format!("  Auth: {auth}    TLS: {tls}    Access: {access}    CORS: {cors}"),
    ];

    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        lines.push(format!("  Fingerprint: {fingerprint}"));
    }

    lines.push(String::new());
    lines.push("Try it:".to_string());
    lines.push(String::new());
    lines.push(format!("  {feed_cmd}"));
    lines.push(format!("  {follow_cmd}"));
    lines.push(String::new());
    if config.token.is_some() && config.token_file_used {
        lines.push("  TOKEN=$(cat <token-file>)".to_string());
    }
    let auth_header = if config.token.is_some() {
        if config.token_file_used {
            " -H \"Authorization: Bearer $TOKEN\""
        } else {
            " -H 'Authorization: Bearer <token>'"
        }
    } else {
        ""
    };
    lines.push(format!(
        "  curl{curl_tls_flag} -sS -X POST{auth_header} -H 'content-type: application/json' \\"
    ));
    lines.push("    --data '{\"hello\":\"world\"}' \\".to_string());
    lines.push(format!("    '{append_url}'"));
    lines.push(String::new());
    lines.push("Press Ctrl-C to stop.".to_string());

    if config.token.is_some() && config.token_file_used {
        lines.push(String::new());
        lines.push(
            "The token is in the file, not printed here. Share token and fingerprint out-of-band."
                .to_string(),
        );
    }

    if config.tls_self_signed {
        lines.push("Temporary self-signed TLS: this identity changes on every restart; use it only for short-lived testing.".to_string());
    } else if tls_enabled {
        lines.push("TLS identity: persistent certificate and private key files.".to_string());
    }
    if !tls_enabled && !config.bind.ip().is_loopback() {
        lines.push("WARNING: network traffic is plaintext; protection depends on your external private network or tunnel.".to_string());
    }
    if config.bind.ip().is_unspecified() {
        lines.push(String::new());
        lines.push(
            "Replace YOUR-HOST/127.0.0.1 with your host IP or DNS name for remote clients."
                .to_string(),
        );
    }
    lines
}

pub(crate) fn emit_serve_check_report(
    config: &serve::ServeConfig,
    color_mode: ColorMode,
    json: bool,
) {
    if !json {
        for line in build_serve_check_lines(config) {
            println!("{line}");
        }
        return;
    }

    let tls_enabled = serve_tls_enabled(config);
    let base_url = format!(
        "{}://{}:{}",
        serve_scheme(config),
        display_host(config.bind.ip()),
        config.bind.port()
    );
    let auth_mode = if config.token.is_some() {
        if config.token_file_used {
            "bearer token (--token-file)"
        } else {
            "bearer token (--token)"
        }
    } else {
        "none"
    };
    let tls_mode = if config.tls_self_signed {
        "temporary-self-signed"
    } else if tls_enabled {
        "enabled"
    } else {
        "disabled"
    };
    let access_mode = match config.access_mode {
        serve::AccessMode::ReadOnly => "read-only",
        serve::AccessMode::WriteOnly => "write-only",
        serve::AccessMode::ReadWrite => "read-write",
    };
    let cors_origins = config.cors_allowed_origins.clone();

    emit_json(
        json!({
            "check": {
                "status": "valid",
                "listen": config.bind.to_string(),
                "base_url": base_url,
                "web_ui": format!("{base_url}/ui"),
                "web_ui_pool": format!("{base_url}/ui/pools/demo"),
                "mcp": format!("{base_url}/mcp"),
                "auth": auth_mode,
                "tls": tls_mode,
                "reachability": serve_scope(config.bind.ip()),
                "transport": if tls_enabled { "tls" } else { "plaintext" },
                "authentication": if config.token.is_some() { "bearer" } else { "none" },
                "endpoint": format!("{base_url}/mcp"),
                "tls_identity": if config.tls_self_signed { "ephemeral" } else if tls_enabled { "persistent-files" } else { "none" },
                "tls_fingerprint": config.tls_fingerprint,
                "access": access_mode,
                "cors_allowed_origins": cors_origins,
                "limits": {
                    "max_body_bytes": config.max_body_bytes,
                    "max_tail_timeout_ms": config.max_tail_timeout_ms,
                    "max_tail_concurrency": config.max_concurrent_tails
                }
            }
        }),
        color_mode,
    );
}

pub(crate) fn build_serve_check_lines(config: &serve::ServeConfig) -> Vec<String> {
    let tls_enabled = serve_tls_enabled(config);
    let base_url = format!(
        "{}://{}:{}",
        serve_scheme(config),
        display_host(config.bind.ip()),
        config.bind.port()
    );
    let auth = if config.token.is_some() {
        "bearer token"
    } else {
        "none"
    };
    let tls = if config.tls_self_signed {
        "temporary self-signed"
    } else if tls_enabled {
        "on"
    } else {
        "off"
    };
    let access = match config.access_mode {
        serve::AccessMode::ReadOnly => "access: read-only",
        serve::AccessMode::WriteOnly => "access: write-only",
        serve::AccessMode::ReadWrite => "access: read-write",
    };
    let access = access.strip_prefix("access: ").unwrap_or(access);
    let cors = if config.cors_allowed_origins.is_empty() {
        "same-origin"
    } else {
        "allowlist"
    };
    let mut lines = vec![
        "Configuration valid.".to_string(),
        String::new(),
        format!(
            "  Bind:   {} ({})",
            config.bind,
            serve_scope(config.bind.ip())
        ),
        format!("  MCP:    {base_url}/mcp"),
        format!("  Auth: {auth}    TLS: {tls}    Access: {access}    CORS: {cors}"),
        format!(
            "  Limits: body {}, timeout {}, concurrency {}",
            format_bytes(config.max_body_bytes),
            format_timeout_ms(config.max_tail_timeout_ms),
            config.max_concurrent_tails
        ),
    ];
    if let Some(fingerprint) = config.tls_fingerprint.as_deref() {
        lines.push(format!("  Fingerprint: {fingerprint}"));
    }
    if config.tls_self_signed {
        lines.push("  Identity: temporary; changes on every restart".to_string());
    } else if tls_enabled {
        lines.push("  Identity: persistent certificate and key files".to_string());
    }
    if !tls_enabled && !config.bind.ip().is_loopback() {
        lines.push("  WARNING: plaintext network traffic depends on external private-network or tunnel protection".to_string());
    }
    lines.push(String::new());
    lines.push("Start with: pls serve".to_string());

    lines
}

pub(crate) fn serve_scope(ip: std::net::IpAddr) -> &'static str {
    if ip.is_loopback() {
        "loopback only"
    } else if ip.is_unspecified() {
        "all interfaces"
    } else {
        "network reachable"
    }
}

pub(crate) fn serve_scheme(config: &serve::ServeConfig) -> &'static str {
    if serve_tls_enabled(config) {
        "https"
    } else {
        "http"
    }
}

pub(crate) fn serve_tls_enabled(config: &serve::ServeConfig) -> bool {
    config.tls_self_signed || (config.tls_cert.is_some() && config.tls_key.is_some())
}

pub(crate) fn serve_config_from_run_args(
    run: ServeRunArgs,
    pool_dir: &Path,
) -> Result<serve::ServeConfig, Error> {
    let bind: SocketAddr = run.bind.parse().map_err(|_| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid bind address")
            .with_hint("Use a host:port value like 127.0.0.1:9700.")
    })?;
    if run.token.is_some() && run.token_file.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--token cannot be combined with --token-file")
            .with_hint("Use --token for dev, or run `plasmite serve init` and use the generated --token-file for safer deployments."));
    }
    if run.tls_self_signed && (run.tls_cert.is_some() || run.tls_key.is_some()) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--tls-self-signed cannot be combined with --tls-cert/--tls-key")
            .with_hint("Use either --tls-self-signed or provide certificate paths; `plasmite serve init` can generate cert/key files."));
    }
    let (token, token_file_used) = if let Some(path) = run.token_file {
        (Some(read_token_file(&path)?), true)
    } else {
        (run.token, false)
    };
    if let Some(key_path) = run.tls_key.as_deref() {
        validate_secret_file(key_path, "TLS private key")?;
    }
    let tls_self_signed_material = if run.tls_self_signed {
        Some(serve::prepare_self_signed_tls(bind.ip())?)
    } else {
        None
    };
    let tls_fingerprint = if let Some(material) = &tls_self_signed_material {
        Some(material.fingerprint.clone())
    } else if let Some(cert_path) = run.tls_cert.as_ref() {
        Some(serve::tls_fingerprint_from_cert_path(cert_path)?)
    } else {
        None
    };
    Ok(serve::ServeConfig {
        bind,
        pool_dir: pool_dir.to_path_buf(),
        token,
        cors_allowed_origins: run.cors_origin,
        access_mode: run.access.into(),
        allow_non_loopback: run.allow_non_loopback,
        insecure_no_tls: run.insecure_no_tls,
        token_file_used,
        tls_cert: run.tls_cert,
        tls_key: run.tls_key,
        tls_self_signed: run.tls_self_signed,
        tls_self_signed_material,
        tls_fingerprint,
        max_body_bytes: run.max_body_bytes,
        max_tail_timeout_ms: run.max_tail_timeout_ms,
        max_concurrent_tails: run.max_tail_concurrency,
    })
}

pub(crate) fn format_timeout_ms(timeout_ms: u64) -> String {
    if timeout_ms.is_multiple_of(1000) {
        return format!("{}s", timeout_ms / 1000);
    }
    format!("{timeout_ms}ms")
}

pub(crate) fn display_host(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(addr) => {
            if addr.is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                addr.to_string()
            }
        }
        std::net::IpAddr::V6(addr) => {
            let shown = if addr.is_unspecified() {
                "::1".to_string()
            } else {
                addr.to_string()
            };
            format!("[{shown}]")
        }
    }
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
    if let Some(duration_ns) = parse_relative_since(input) {
        return Ok(now_ns.saturating_sub(duration_ns));
    }
    let trimmed = input.trim();
    let ts = time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc3339)
        .map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid --since value")
            .with_hint("Use RFC 3339 (2026-02-02T23:45:00Z) or relative like 5m.")
            .with_source(err)
    })?;
    Ok(ts.unix_timestamp_nanos() as u64)
}

pub(crate) fn parse_relative_since(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (digits, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    let seconds = match unit {
        "s" | "S" => value,
        "m" | "M" => value.saturating_mul(60),
        "h" | "H" => value.saturating_mul(60 * 60),
        "d" | "D" => value.saturating_mul(60 * 60 * 24),
        _ => return None,
    };
    Some(seconds.saturating_mul(1_000_000_000))
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

pub(crate) fn emit_pool_info_pretty(pool_ref: &str, info: &plasmite::api::PoolInfo) {
    if !io::stdout().is_terminal() {
        println!("Pool: {pool_ref}");
        println!("Path: {}", info.path.display());
        println!(
            "Size: {} bytes (index: offset={} slots={} bytes={}, ring: offset={} size={})",
            info.file_size,
            info.index_offset,
            info.index_capacity,
            info.index_size_bytes,
            info.ring_offset,
            info.ring_size
        );

        let oldest = info
            .bounds
            .oldest_seq
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let newest = info
            .bounds
            .newest_seq
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let count = info
            .metrics
            .as_ref()
            .map(|metrics| metrics.message_count)
            .unwrap_or_else(|| match (info.bounds.oldest_seq, info.bounds.newest_seq) {
                (Some(oldest), Some(newest)) => newest.saturating_sub(oldest).saturating_add(1),
                _ => 0,
            });
        println!("Bounds: oldest={oldest} newest={newest} count={count}");

        if let Some(metrics) = &info.metrics {
            let whole = metrics.utilization.used_percent_hundredths / 100;
            let frac = metrics.utilization.used_percent_hundredths % 100;
            println!(
                "Utilization: used={}B free={}B ({}.{:02}%)",
                metrics.utilization.used_bytes, metrics.utilization.free_bytes, whole, frac
            );
            println!(
                "Oldest: {} ({})",
                metrics.age.oldest_time.as_deref().unwrap_or("-"),
                human_age(metrics.age.oldest_age_ms),
            );
            println!(
                "Newest: {} ({})",
                metrics.age.newest_time.as_deref().unwrap_or("-"),
                human_age(metrics.age.newest_age_ms),
            );
        }
        return;
    }

    let count = message_count_from_info(info);
    println!("{pool_ref}");
    println!(
        "  path:      {}",
        short_display_path(&info.path, info.path.parent())
    );
    let messages_summary =
        format_pool_messages_summary(count, info.bounds.oldest_seq, info.bounds.newest_seq);
    if let Some(metrics) = &info.metrics {
        let whole = metrics.utilization.used_percent_hundredths / 100;
        let frac = metrics.utilization.used_percent_hundredths % 100;
        println!(
            "  size:      {} ({} used, {}.{:02}%)",
            format_bytes(info.file_size),
            format_bytes(metrics.utilization.used_bytes),
            whole,
            frac
        );
        println!("  messages:  {messages_summary}");
        println!(
            "  oldest:    {}",
            format_pool_time_summary(
                metrics.age.oldest_age_ms,
                metrics.age.oldest_time.as_deref()
            )
        );
        println!(
            "  newest:    {}",
            format_pool_time_summary(
                metrics.age.newest_age_ms,
                metrics.age.newest_time.as_deref()
            )
        );
    } else {
        println!("  size:      {}", format_bytes(info.file_size));
        println!("  messages:  {messages_summary}");
    }
    println!(
        "  index:     {} slots ({})",
        info.index_capacity,
        format_bytes(info.index_size_bytes)
    );
    println!("  ring:      {}", format_bytes(info.ring_size));
}

pub(crate) fn message_count_from_info(info: &plasmite::api::PoolInfo) -> u64 {
    info.metrics
        .as_ref()
        .map(|metrics| metrics.message_count)
        .unwrap_or_else(|| {
            message_count_from_bounds(info.bounds.oldest_seq, info.bounds.newest_seq)
        })
}

pub(crate) fn message_count_from_bounds(oldest: Option<u64>, newest: Option<u64>) -> u64 {
    match (oldest, newest) {
        (Some(oldest), Some(newest)) if newest >= oldest => {
            newest.saturating_sub(oldest).saturating_add(1)
        }
        _ => 0,
    }
}

pub(crate) fn format_pool_messages_summary(
    count: u64,
    oldest: Option<u64>,
    newest: Option<u64>,
) -> String {
    let seq_range = format_seq_range(oldest, newest);
    if count == 0 {
        if seq_range == "-" {
            return "empty".to_string();
        }
        return format!("0 visible ({seq_range})");
    }
    if seq_range == "-" {
        return count.to_string();
    }
    format!("{count} ({seq_range})")
}

pub(crate) fn format_pool_time_summary(age_ms: Option<u64>, timestamp: Option<&str>) -> String {
    let Some(timestamp) = timestamp else {
        return "—".to_string();
    };
    format!(
        "{} ({})",
        format_relative_time(age_ms),
        format_timestamp_human(timestamp)
    )
}

pub(crate) fn emit_feed_receipt_human(receipt: &Value) {
    let seq = receipt
        .get("seq")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let time = receipt
        .get("time")
        .and_then(|value| value.as_str())
        .map(format_timestamp_human)
        .unwrap_or_else(|| "-".to_string());
    let tags = receipt
        .get("meta")
        .and_then(|value| value.get("tags"))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    if tags.is_empty() {
        println!("fed seq={seq} at {time}");
    } else {
        println!("fed seq={seq} at {time}  tags: {tags}");
    }
}

pub(crate) fn emit_feed_receipt(value: Value, color_mode: ColorMode) {
    if io::stdout().is_terminal() {
        emit_feed_receipt_human(&value);
    } else {
        emit_json(value, color_mode);
    }
}

pub(crate) fn short_display_path(path: &Path, base_dir: Option<&Path>) -> String {
    if let Some(base) = base_dir {
        if let Ok(relative) = path.strip_prefix(base) {
            if !relative.as_os_str().is_empty() {
                return relative.display().to_string();
            }
        }
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn emit_table(headers: &[&str], rows: &[Vec<String>]) {
    println!("{}", render_table(headers, rows));
}

pub(crate) fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let column_count = headers.len();
    let mut sanitized_rows = Vec::with_capacity(rows.len());
    let mut widths = headers
        .iter()
        .map(|header| header.chars().count())
        .collect::<Vec<_>>();

    for row in rows {
        let mut sanitized = Vec::with_capacity(column_count);
        for (idx, width) in widths.iter_mut().enumerate() {
            let value = row.get(idx).map(String::as_str).unwrap_or("");
            let cleaned = sanitize_table_cell(value);
            *width = (*width).max(cleaned.chars().count());
            sanitized.push(cleaned);
        }
        sanitized_rows.push(sanitized);
    }

    let mut lines = Vec::with_capacity(sanitized_rows.len() + 1);
    lines.push(format_table_line(
        &headers
            .iter()
            .map(|header| header.to_string())
            .collect::<Vec<_>>(),
        &widths,
    ));
    for row in sanitized_rows {
        lines.push(format_table_line(&row, &widths));
    }
    lines.join("\n")
}

pub(crate) fn sanitize_table_cell(value: &str) -> String {
    value.replace('\n', "\\n").replace('\r', "\\r")
}

pub(crate) fn format_table_line(cells: &[String], widths: &[usize]) -> String {
    let mut line = String::new();
    for (idx, width) in widths.iter().enumerate() {
        if idx > 0 {
            line.push_str("  ");
        }
        let cell = cells.get(idx).map(String::as_str).unwrap_or("");
        line.push_str(cell);
        let cell_len = cell.chars().count();
        if *width > cell_len {
            line.push_str(&" ".repeat(*width - cell_len));
        }
    }
    line
}

pub(crate) fn human_age(age_ms: Option<u64>) -> String {
    format_relative_time(age_ms)
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum AnsiColor {
    Red,
    Yellow,
}

pub(crate) fn colorize_label(label: &str, enabled: bool, color: AnsiColor) -> String {
    if !enabled {
        return label.to_string();
    }
    let code = match color {
        AnsiColor::Red => "31",
        AnsiColor::Yellow => "33",
    };
    format!("\u{1b}[{code}m{label}\u{1b}[0m")
}

pub(crate) fn emit_message(value: serde_json::Value, pretty: bool, color_mode: ColorMode) {
    let is_tty = io::stdout().is_terminal();
    let use_color = color_mode.use_color(is_tty);
    let json = if pretty {
        if use_color {
            colorize_json(&value, true)
        } else {
            serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string())
        }
    } else {
        serde_json::to_string(&value)
            .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string())
    };
    println!("{json}");
}

pub(crate) fn emit_error(err: &Error, color_mode: ColorMode) {
    let is_tty = io::stderr().is_terminal();
    if is_tty {
        eprintln!("{}", error_text(err, color_mode.use_color(is_tty)));
        return;
    }

    let value = error_json(err);
    let json = serde_json::to_string(&value).unwrap_or_else(|_| {
        "{\"error\":{\"kind\":\"Internal\",\"message\":\"json encode failed\"}}".to_string()
    });
    eprintln!("{json}");
}

pub(crate) fn notice_time_now() -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(duration.as_nanos() as i128).ok()?;
    ts.format(&Rfc3339).ok()
}

pub(crate) fn emit_notice(notice: &Notice, color_mode: ColorMode) {
    let is_tty = io::stderr().is_terminal();
    if is_tty {
        let label = colorize_label("notice:", color_mode.use_color(is_tty), AnsiColor::Yellow);
        if notice.cmd == "feed" {
            eprintln!("{label} {}", notice.message);
        } else {
            eprintln!("{label} {} (pool: {})", notice.message, notice.pool);
        }
        return;
    }

    let value = notice_json(notice);
    let json = serde_json::to_string(&value).unwrap_or_else(|_| {
        "{\"notice\":{\"kind\":\"Internal\",\"message\":\"json encode failed\"}}".to_string()
    });
    eprintln!("{json}");
}

pub(crate) fn error_message(err: &Error) -> String {
    if let Some(message) = err.message() {
        return message.to_string();
    }
    error_policy(interface_error_kind(err.kind()))
        .cli_message
        .to_string()
}

pub(crate) fn interface_error_kind(kind: ErrorKind) -> ErrorKindWire {
    match kind {
        ErrorKind::Internal => ErrorKindWire::Internal,
        ErrorKind::Usage => ErrorKindWire::Usage,
        ErrorKind::NotFound => ErrorKindWire::NotFound,
        ErrorKind::AlreadyExists => ErrorKindWire::AlreadyExists,
        ErrorKind::Busy => ErrorKindWire::Busy,
        ErrorKind::Permission => ErrorKindWire::Permission,
        ErrorKind::Corrupt => ErrorKindWire::Corrupt,
        ErrorKind::Io => ErrorKindWire::Io,
        ErrorKind::RetentionGap => ErrorKindWire::RetentionGap,
    }
}

pub(crate) fn error_causes(err: &Error) -> Vec<String> {
    let mut causes = Vec::new();
    let mut cur = err.source();
    while let Some(source) = cur {
        causes.push(source.to_string());
        cur = source.source();
    }
    causes
}

pub(crate) fn error_json(err: &Error) -> Value {
    let mut inner = Map::new();
    inner.insert(
        "kind".to_string(),
        json!(error_policy(interface_error_kind(err.kind())).mcp_error_kind),
    );
    inner.insert("message".to_string(), json!(error_message(err)));
    if let Some(hint) = err.hint() {
        inner.insert("hint".to_string(), json!(hint));
    }
    if let Some(path) = err.path() {
        inner.insert("path".to_string(), json!(path.display().to_string()));
    }
    if let Some(seq) = err.seq() {
        inner.insert("seq".to_string(), json!(seq));
    }
    if let Some(offset) = err.offset() {
        inner.insert("offset".to_string(), json!(offset));
    }
    let causes = error_causes(err);
    if !causes.is_empty() {
        inner.insert("causes".to_string(), json!(causes));
    }

    let mut outer = Map::new();
    outer.insert("error".to_string(), Value::Object(inner));
    Value::Object(outer)
}

pub(crate) fn error_text(err: &Error, use_color: bool) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} {}",
        colorize_label("error:", use_color, AnsiColor::Red),
        error_message(err)
    ));

    if let Some(hint) = err.hint() {
        lines.push(format!(
            "{} {hint}",
            colorize_label("hint:", use_color, AnsiColor::Yellow)
        ));
    }
    if let Some(path) = err.path() {
        lines.push(format!(
            "{} {}",
            colorize_label("path:", use_color, AnsiColor::Yellow),
            display_handoff_path_from_path(path)
        ));
    }
    if let Some(seq) = err.seq() {
        lines.push(format!(
            "{} {seq}",
            colorize_label("seq:", use_color, AnsiColor::Yellow)
        ));
    }
    if let Some(offset) = err.offset() {
        lines.push(format!(
            "{} {offset}",
            colorize_label("offset:", use_color, AnsiColor::Yellow)
        ));
    }

    let causes = error_causes(err);
    if let Some(cause) = causes.first() {
        lines.push(format!(
            "{} {cause}",
            colorize_label("caused by:", use_color, AnsiColor::Yellow)
        ));
    }

    lines.join("\n")
}

pub(crate) fn emit_follow_timeout_human(timeout_label: &str) {
    if io::stderr().is_terminal() {
        eprintln!("No messages received (timed out after {timeout_label}).");
    }
}

pub(crate) fn clap_error_summary(err: &clap::Error) -> String {
    for line in err.to_string().lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("error:") {
            return rest.trim().to_string();
        }
        return trimmed.to_string();
    }
    "invalid arguments".to_string()
}

pub(crate) fn clap_error_hint(err: &clap::Error) -> String {
    let rendered = err.to_string();
    let missing_required = rendered.contains("required arguments were not provided")
        || rendered.contains("required argument was not provided");
    let usage = rendered
        .lines()
        .find_map(|line| line.trim().strip_prefix("Usage: "))
        .map(str::trim);

    let Some(usage) = usage else {
        return "Try `plasmite --help`.".to_string();
    };

    let tokens: Vec<&str> = usage.split_whitespace().collect();
    let Some(pos) = tokens.iter().position(|t| *t == "plasmite") else {
        return "Try `plasmite --help`.".to_string();
    };

    let mut parts = Vec::new();
    for token in tokens.iter().skip(pos + 1) {
        if token.starts_with('-') || token.starts_with('<') || token.starts_with('[') {
            break;
        }
        parts.push(*token);
    }

    if parts.is_empty() {
        return "Try `plasmite --help`.".to_string();
    }

    let required_tokens: Vec<&str> = tokens
        .iter()
        .skip(pos + 1 + parts.len())
        .copied()
        .filter(|token| token.starts_with('<') && token.ends_with('>'))
        .collect();
    if missing_required
        && parts.as_slice() == ["follow"]
        && required_tokens
            .iter()
            .any(|token| token.contains("POOL") || token.contains("pool"))
    {
        return "Provide a pool ref, for example: `plasmite follow chat -n 1`.".to_string();
    }

    format!("Try `plasmite {} --help`.", parts.join(" "))
}

pub(crate) fn parse_inline_json(data: &str) -> Result<Value, Error> {
    serde_json::from_str(data).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json")
            .with_hint("Provide a single JSON value (e.g. '{\"x\":1}').")
            .with_source(err)
    })
}

pub(crate) fn missing_feed_data_error() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("missing data input")
        .with_hint("Provide JSON via DATA, --file, or pipe JSON to stdin.")
}

pub(crate) fn open_feed_reader(path: &str) -> Result<Box<dyn Read>, Error> {
    if path == "-" {
        return Ok(Box::new(io::stdin()));
    }
    let reader = std::fs::File::open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read data file")
            .with_path(path)
            .with_source(err)
    })?;
    Ok(Box::new(reader))
}

pub(crate) fn input_mode_to_ingest(mode: InputMode) -> IngestMode {
    match mode {
        InputMode::Auto => IngestMode::Auto,
        InputMode::Jsonl => IngestMode::Jsonl,
        InputMode::Json => IngestMode::Json,
        InputMode::Seq => IngestMode::Seq,
        InputMode::Jq => IngestMode::Jq,
    }
}

pub(crate) fn error_policy_to_ingest(policy: ErrorPolicyCli) -> ErrorPolicy {
    match policy {
        ErrorPolicyCli::Stop => ErrorPolicy::Stop,
        ErrorPolicyCli::Skip => ErrorPolicy::Skip,
    }
}

pub(crate) fn ingest_failure_notice(
    failure: &IngestFailure,
    pool_ref: &str,
    pool_path_label: &str,
    color_mode: ColorMode,
) {
    let mut details = Map::new();
    details.insert("mode".to_string(), json!(mode_label(failure.mode)));
    details.insert("index".to_string(), json!(failure.index));
    details.insert("error_kind".to_string(), json!(failure.error_kind));
    details.insert("pool_path".to_string(), json!(pool_path_label));
    if let Some(line) = failure.line {
        details.insert("line".to_string(), json!(line));
    }
    if let Some(snippet) = &failure.snippet {
        details.insert("snippet".to_string(), json!(snippet));
    }
    let notice = Notice {
        kind: "ingest_skip".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "feed".to_string(),
        pool: pool_ref.to_string(),
        message: ingest_failure_message(failure),
        details,
    };
    emit_notice(&notice, color_mode);
}

pub(crate) fn ingest_failure_message(failure: &IngestFailure) -> String {
    match failure.error_kind.as_str() {
        "Parse" => "Skipped invalid JSON.".to_string(),
        "Oversize" => "Skipped oversized record.".to_string(),
        _ => format!("Skipped record: {}.", failure.message),
    }
}

pub(crate) fn ingest_summary_notice(
    outcome: &IngestOutcome,
    pool_ref: &str,
    pool_path_label: &str,
    color_mode: ColorMode,
) {
    let mut details = Map::new();
    details.insert("total".to_string(), json!(outcome.records_total));
    details.insert("ok".to_string(), json!(outcome.ok));
    details.insert("failed".to_string(), json!(outcome.failed));
    details.insert("pool_path".to_string(), json!(pool_path_label));
    let notice = Notice {
        kind: "ingest_summary".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "feed".to_string(),
        pool: pool_ref.to_string(),
        message: format!(
            "Finished with {} skipped record{}.",
            outcome.failed,
            if outcome.failed == 1 { "" } else { "s" }
        ),
        details,
    };
    emit_notice(&notice, color_mode);
}

pub(crate) fn mode_label(mode: IngestMode) -> &'static str {
    match mode {
        IngestMode::Auto => "auto",
        IngestMode::Jsonl => "jsonl",
        IngestMode::Json => "json",
        IngestMode::Seq => "seq",
        IngestMode::Jq => "jq",
        IngestMode::Event => "event",
    }
}

pub(crate) struct FeedIngestContext<'a> {
    pub(crate) pool_ref: &'a str,
    pub(crate) pool_path_label: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) durability: Durability,
    pub(crate) retry_config: Option<RetryConfig>,
    pub(crate) pool_handle: &'a mut Pool,
    pub(crate) color_mode: ColorMode,
    pub(crate) input: InputMode,
    pub(crate) errors: ErrorPolicyCli,
}

pub(crate) struct RemoteFeedIngestContext<'a> {
    pub(crate) pool_ref: &'a str,
    pub(crate) pool_path_label: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) durability: Durability,
    pub(crate) retry_config: Option<RetryConfig>,
    pub(crate) remote_pool: &'a RemotePool,
    pub(crate) color_mode: ColorMode,
    pub(crate) input: InputMode,
    pub(crate) errors: ErrorPolicyCli,
}

pub(crate) fn ingest_from_stdin<R: Read>(
    reader: R,
    ctx: FeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<IngestOutcome, Error> {
    let ingest_config = IngestConfig {
        mode: input_mode_to_ingest(ctx.input),
        errors: error_policy_to_ingest(ctx.errors),
        sniff_bytes: DEFAULT_SNIFF_BYTES,
        sniff_lines: DEFAULT_SNIFF_LINES,
        max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
        max_snippet_bytes: DEFAULT_MAX_SNIPPET_BYTES,
    };

    let outcome = ingest(
        reader,
        ingest_config,
        |data| {
            let payload = lite3::encode_message(ctx.tags, &data)?;
            let (seq, timestamp_ns) = retry_with_config(ctx.retry_config, || {
                let timestamp_ns = now_ns()?;
                let options = AppendOptions::new(timestamp_ns, ctx.durability);
                let seq = ctx
                    .pool_handle
                    .append_with_options(payload.as_slice(), options)?;
                Ok((seq, timestamp_ns))
            })?;
            if emit_receipt {
                emit_feed_receipt(
                    feed_receipt_json(seq, timestamp_ns, ctx.tags)?,
                    ctx.color_mode,
                );
            }
            Ok(())
        },
        |failure| {
            ingest_failure_notice(&failure, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode)
        },
    )?;

    if ctx.errors == ErrorPolicyCli::Skip && outcome.failed > 0 {
        ingest_summary_notice(&outcome, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode);
    }

    Ok(outcome)
}

pub(crate) fn ingest_from_stdin_remote<R: Read>(
    reader: R,
    ctx: RemoteFeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<IngestOutcome, Error> {
    let ingest_config = IngestConfig {
        mode: input_mode_to_ingest(ctx.input),
        errors: error_policy_to_ingest(ctx.errors),
        sniff_bytes: DEFAULT_SNIFF_BYTES,
        sniff_lines: DEFAULT_SNIFF_LINES,
        max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
        max_snippet_bytes: DEFAULT_MAX_SNIPPET_BYTES,
    };

    let outcome = ingest(
        reader,
        ingest_config,
        |data| {
            let message = retry_with_config(ctx.retry_config, || {
                ctx.remote_pool
                    .append_json_now(&data, ctx.tags, ctx.durability)
            })?;
            if emit_receipt {
                emit_feed_receipt(feed_receipt_from_message(&message), ctx.color_mode);
            }
            Ok(())
        },
        |failure| {
            ingest_failure_notice(&failure, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode)
        },
    )?;

    if ctx.errors == ErrorPolicyCli::Skip && outcome.failed > 0 {
        ingest_summary_notice(&outcome, ctx.pool_ref, ctx.pool_path_label, ctx.color_mode);
    }

    Ok(outcome)
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

pub(crate) fn message_from_frame(frame: &FrameRef<'_>) -> Result<Value, Error> {
    let (meta, data) = decode_payload(frame.payload)?;
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

#[derive(Debug, Clone)]
pub(crate) struct DropNotice {
    last_seen_seq: u64,
    next_seen_seq: u64,
}

impl DropNotice {
    fn dropped_count(&self) -> u64 {
        self.next_seen_seq.saturating_sub(self.last_seen_seq + 1)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FollowConfig {
    pub(crate) tail: u64,
    pub(crate) pretty: bool,
    pub(crate) one: bool,
    pub(crate) timeout: Option<Duration>,
    pub(crate) data_only: bool,
    pub(crate) since_ns: Option<u64>,
    pub(crate) required_tags: Vec<String>,
    pub(crate) where_predicates: Vec<JqFilter>,
    pub(crate) quiet_drops: bool,
    pub(crate) notify: bool,
    pub(crate) color_mode: ColorMode,
    pub(crate) replay_speed: Option<f64>,
    pub(crate) suppress_sender: Option<String>,
    pub(crate) stop: Option<Arc<AtomicBool>>,
}

pub(crate) fn matches_required_tags(required_tags: &[String], message: &Value) -> bool {
    if required_tags.is_empty() {
        return true;
    }
    let Some(tags) = message
        .get("meta")
        .and_then(|meta| meta.get("tags"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    required_tags.iter().all(|required| {
        tags.iter()
            .any(|tag| tag.as_str().is_some_and(|value| value == required))
    })
}

pub(crate) fn should_suppress_sender(message: &Value, sender: &str) -> bool {
    message
        .get("data")
        .and_then(|data| data.get("from"))
        .and_then(Value::as_str)
        .is_some_and(|value| value == sender)
}

pub(crate) fn duplex_requires_me_when_tty(stdin_is_terminal: bool, me: Option<&str>) -> bool {
    stdin_is_terminal && me.is_none()
}

pub(crate) fn parse_duplex_tty_line(me: &str, line: &str) -> Option<Value> {
    let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
    if trimmed.trim().is_empty() {
        return None;
    }
    Some(json!({
        "from": me,
        "msg": trimmed,
    }))
}

pub(crate) fn should_suppress_message(cfg: &FollowConfig, message: &Value) -> bool {
    cfg.suppress_sender
        .as_deref()
        .is_some_and(|sender| should_suppress_sender(message, sender))
}

pub(crate) fn follow_should_stop(stop: Option<&Arc<AtomicBool>>) -> bool {
    stop.is_some_and(|flag| flag.load(Ordering::Acquire))
}

pub(crate) fn follow_remote(
    client: &RemoteClient,
    pool: &str,
    cfg: &FollowConfig,
) -> Result<RunOutcome, Error> {
    if cfg.replay_speed.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --replay")
            .with_hint("Use local follow with --replay, or omit --replay for remote streams."));
    }
    if cfg.since_ns.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --since")
            .with_hint("Use --tail N for remote refs, or run --since against a local pool path."));
    }
    if !cfg.notify {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --no-notify")
            .with_hint("--no-notify only applies to local pool semaphores."));
    }
    if cfg.quiet_drops {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("remote follow does not support --quiet-drops")
            .with_hint("--quiet-drops only applies to local drop notices."));
    }

    let remote_pool = client.open_pool(&PoolRef::name(pool))?;

    let mut next_since_seq = if cfg.tail > 0 {
        let info = remote_pool.info()?;
        match (info.bounds.oldest_seq, info.bounds.newest_seq) {
            (Some(oldest), Some(newest)) => Some(
                newest
                    .saturating_sub(cfg.tail.saturating_sub(1))
                    .max(oldest),
            ),
            _ => None,
        }
    } else {
        None
    };

    let mut tail_wait_matches = VecDeque::new();
    loop {
        if follow_should_stop(cfg.stop.as_ref()) {
            return Ok(RunOutcome::ok());
        }

        let mut options = TailOptions::new();
        options.since_seq = next_since_seq;
        options.timeout = cfg.timeout;
        let mut tail = remote_pool.tail(options)?;

        let mut emitted_in_cycle = false;
        while let Some(message) = tail.next_message()? {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            next_since_seq = Some(message.seq.saturating_add(1));
            let value = message_to_json(&message);
            if should_suppress_message(cfg, &value)
                || !matches_required_tags(cfg.required_tags.as_slice(), &value)
                || !matches_all(cfg.where_predicates.as_slice(), &value)?
            {
                continue;
            }

            if cfg.one && cfg.tail > 0 {
                tail_wait_matches.push_back(value);
                while tail_wait_matches.len() > cfg.tail as usize {
                    tail_wait_matches.pop_front();
                }
                if tail_wait_matches.len() == cfg.tail as usize {
                    if let Some(latest) = tail_wait_matches.back() {
                        emit_message(
                            output_value(latest.clone(), cfg.data_only),
                            cfg.pretty,
                            cfg.color_mode,
                        );
                    }
                    return Ok(RunOutcome::ok());
                }
                emitted_in_cycle = true;
                continue;
            }

            emit_message(
                output_value(value, cfg.data_only),
                cfg.pretty,
                cfg.color_mode,
            );
            emitted_in_cycle = true;
            if cfg.one {
                return Ok(RunOutcome::ok());
            }
        }

        if cfg.timeout.is_some() && !emitted_in_cycle {
            return Ok(RunOutcome::with_code(124));
        }
    }
}

pub(crate) fn follow_pool(
    pool: &Pool,
    pool_ref: &str,
    pool_path: &Path,
    cfg: FollowConfig,
) -> Result<RunOutcome, Error> {
    if cfg.replay_speed.is_some() {
        return follow_replay(pool, &cfg);
    }

    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut emit = VecDeque::new();
    let mut last_seen_seq = None::<u64>;
    let mut pending_drop: Option<DropNotice> = None;
    let mut last_notice_at: Option<Instant> = None;
    let notice_interval = Duration::from_secs(1);
    let tail_wait = cfg.one && cfg.tail > 0;
    let mut timeout_deadline = cfg.timeout.map(|duration| Instant::now() + duration);
    let mut notify_enabled = cfg.notify;
    let mut notify_handle = if notify_enabled {
        notify::open_for_path(pool_path)
    } else {
        None
    };
    if notify_enabled && notify_handle.is_none() {
        notify_enabled = false;
    }

    let bump_timeout = |deadline: &mut Option<Instant>| {
        if let Some(duration) = cfg.timeout {
            *deadline = Some(Instant::now() + duration);
        }
    };

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if follow_should_stop(cfg.stop.as_ref()) {
                        return Ok(RunOutcome::ok());
                    }
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if !should_suppress_message(&cfg, &message)
                            && matches_required_tags(cfg.required_tags.as_slice(), &message)
                            && matches_all(cfg.where_predicates.as_slice(), &message)?
                        {
                            emit_message(
                                output_value(message, cfg.data_only),
                                cfg.pretty,
                                cfg.color_mode,
                            );
                            bump_timeout(&mut timeout_deadline);
                            if cfg.one {
                                return Ok(RunOutcome::ok());
                            }
                        }
                        last_seen_seq = Some(frame.seq);
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    } else if cfg.tail > 0 {
        cursor.seek_to(header.tail_off as usize);
        loop {
            if follow_should_stop(cfg.stop.as_ref()) {
                return Ok(RunOutcome::ok());
            }
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if follow_should_stop(cfg.stop.as_ref()) {
                        return Ok(RunOutcome::ok());
                    }
                    let message = message_from_frame(&frame)?;
                    if !should_suppress_message(&cfg, &message)
                        && matches_required_tags(cfg.required_tags.as_slice(), &message)
                        && matches_all(cfg.where_predicates.as_slice(), &message)?
                    {
                        emit.push_back(message);
                    }
                    last_seen_seq = Some(frame.seq);
                    while emit.len() > cfg.tail as usize {
                        emit.pop_front();
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
        if tail_wait {
            if emit.len() >= cfg.tail as usize {
                if let Some(value) = emit.back() {
                    emit_message(
                        output_value(value.clone(), cfg.data_only),
                        cfg.pretty,
                        cfg.color_mode,
                    );
                }
                return Ok(RunOutcome::ok());
            }
        } else {
            for value in emit.drain(..) {
                emit_message(
                    output_value(value, cfg.data_only),
                    cfg.pretty,
                    cfg.color_mode,
                );
                bump_timeout(&mut timeout_deadline);
            }
        }
    }

    if cfg.since_ns.is_none() && cfg.tail == 0 {
        cursor.seek_to(header.head_off as usize);
    }

    let mut backoff = Duration::from_millis(1);
    let max_backoff = Duration::from_millis(50);

    let pool_ref = pool_ref.to_string();
    let pool_path_label = pool_path.display().to_string();

    let maybe_emit_pending = |pending: &mut Option<DropNotice>,
                              last_notice_at: &mut Option<Instant>| {
        if cfg.quiet_drops {
            pending.take();
            return;
        }
        let Some(pending_notice) = pending.as_ref() else {
            return;
        };
        let ready = last_notice_at
            .map(|instant| instant.elapsed() >= notice_interval)
            .unwrap_or(true);
        if !ready {
            return;
        }
        let time = match notice_time_now() {
            Some(time) => time,
            None => {
                pending.take();
                return;
            }
        };
        let dropped_count = pending_notice.dropped_count();
        let mut details = Map::new();
        details.insert(
            "last_seen_seq".to_string(),
            json!(pending_notice.last_seen_seq),
        );
        details.insert(
            "next_seen_seq".to_string(),
            json!(pending_notice.next_seen_seq),
        );
        details.insert("dropped_count".to_string(), json!(dropped_count));
        details.insert("pool_path".to_string(), json!(pool_path_label.as_str()));
        let notice = Notice {
            kind: "drop".to_string(),
            time,
            cmd: "follow".to_string(),
            pool: pool_ref.clone(),
            message: format!("dropped {dropped_count} messages"),
            details,
        };
        emit_notice(&notice, cfg.color_mode);
        *last_notice_at = Some(Instant::now());
        pending.take();
    };

    let queue_drop = |last_seen_seq: u64, next_seen_seq: u64, pending: &mut Option<DropNotice>| {
        if cfg.quiet_drops {
            return;
        }
        match pending {
            Some(existing) => {
                existing.next_seen_seq = next_seen_seq;
            }
            None => {
                *pending = Some(DropNotice {
                    last_seen_seq,
                    next_seen_seq,
                });
            }
        }
    };

    loop {
        if follow_should_stop(cfg.stop.as_ref()) {
            return Ok(RunOutcome::ok());
        }
        match cursor.next(pool)? {
            CursorResult::Message(frame) => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                if let Some(last_seen_seq) = last_seen_seq {
                    if frame.seq > last_seen_seq + 1 {
                        queue_drop(last_seen_seq, frame.seq, &mut pending_drop);
                        maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                    }
                }
                let message = message_from_frame(&frame)?;
                if !should_suppress_message(&cfg, &message)
                    && matches_required_tags(cfg.required_tags.as_slice(), &message)
                    && matches_all(cfg.where_predicates.as_slice(), &message)?
                {
                    if tail_wait {
                        emit.push_back(message);
                        while emit.len() > cfg.tail as usize {
                            emit.pop_front();
                        }
                        if emit.len() == cfg.tail as usize {
                            if let Some(value) = emit.back() {
                                emit_message(
                                    output_value(value.clone(), cfg.data_only),
                                    cfg.pretty,
                                    cfg.color_mode,
                                );
                            }
                            return Ok(RunOutcome::ok());
                        }
                    } else {
                        emit_message(
                            output_value(message, cfg.data_only),
                            cfg.pretty,
                            cfg.color_mode,
                        );
                        bump_timeout(&mut timeout_deadline);
                        if cfg.one {
                            return Ok(RunOutcome::ok());
                        }
                    }
                }
                last_seen_seq = Some(frame.seq);
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                backoff = Duration::from_millis(1);
            }
            CursorResult::WouldBlock => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                maybe_emit_pending(&mut pending_drop, &mut last_notice_at);
                if let Some(deadline) = timeout_deadline {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(RunOutcome::with_code(124));
                    }
                    let remaining = deadline.duration_since(now);
                    let wait_for = std::cmp::min(backoff, remaining);
                    if notify_enabled {
                        match notify_handle
                            .as_mut()
                            .map(|handle| handle.wait(wait_for))
                            .unwrap_or(NotifyWait::Unavailable)
                        {
                            NotifyWait::Signaled | NotifyWait::TimedOut => {}
                            NotifyWait::Unavailable => {
                                notify_enabled = false;
                                notify_handle = None;
                                std::thread::sleep(wait_for);
                            }
                        }
                    } else {
                        std::thread::sleep(wait_for);
                    }
                } else if notify_enabled {
                    match notify_handle
                        .as_mut()
                        .map(|handle| handle.wait(backoff))
                        .unwrap_or(NotifyWait::Unavailable)
                    {
                        NotifyWait::Signaled | NotifyWait::TimedOut => {}
                        NotifyWait::Unavailable => {
                            notify_enabled = false;
                            notify_handle = None;
                            std::thread::sleep(backoff);
                        }
                    }
                } else {
                    std::thread::sleep(backoff);
                }
                backoff = std::cmp::min(backoff * 2, max_backoff);
            }
            CursorResult::FellBehind => {
                if follow_should_stop(cfg.stop.as_ref()) {
                    return Ok(RunOutcome::ok());
                }
                header = pool.header_from_mmap()?;
                if cfg.tail > 0 {
                    cursor.seek_to(header.tail_off as usize);
                } else {
                    cursor.seek_to(header.head_off as usize);
                }
            }
        }
    }
}

pub(crate) fn follow_replay(pool: &Pool, cfg: &FollowConfig) -> Result<RunOutcome, Error> {
    let speed = cfg.replay_speed.unwrap_or(0.0);
    let mut cursor = Cursor::new();
    let mut header = pool.header_from_mmap()?;
    let mut collected: Vec<(u64, Value)> = Vec::new();

    if let Some(since_ns) = cfg.since_ns {
        cursor.seek_to(header.tail_off as usize);
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    if frame.timestamp_ns >= since_ns {
                        let message = message_from_frame(&frame)?;
                        if matches_required_tags(cfg.required_tags.as_slice(), &message)
                            && matches_all(cfg.where_predicates.as_slice(), &message)?
                        {
                            collected.push((frame.timestamp_ns, message));
                        }
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
    } else {
        cursor.seek_to(header.tail_off as usize);
        let mut buffer: VecDeque<(u64, Value)> = VecDeque::new();
        loop {
            match cursor.next(pool)? {
                CursorResult::Message(frame) => {
                    let message = message_from_frame(&frame)?;
                    if matches_required_tags(cfg.required_tags.as_slice(), &message)
                        && matches_all(cfg.where_predicates.as_slice(), &message)?
                    {
                        if cfg.tail > 0 {
                            buffer.push_back((frame.timestamp_ns, message));
                            while buffer.len() > cfg.tail as usize {
                                buffer.pop_front();
                            }
                        } else {
                            collected.push((frame.timestamp_ns, message));
                        }
                    }
                }
                CursorResult::WouldBlock => break,
                CursorResult::FellBehind => {
                    header = pool.header_from_mmap()?;
                    cursor.seek_to(header.tail_off as usize);
                }
            }
        }
        if cfg.tail > 0 {
            collected = buffer.into_iter().collect();
        }
    }

    if collected.is_empty() {
        return Ok(RunOutcome::ok());
    }

    let mut prev_ts = collected[0].0;
    for (i, (ts, message)) in collected.into_iter().enumerate() {
        if i > 0 && speed > 0.0 {
            let delta_ns = ts.saturating_sub(prev_ts);
            let delay_ns = (delta_ns as f64 / speed) as u64;
            if delay_ns > 0 {
                std::thread::sleep(Duration::from_nanos(delay_ns));
            }
        }
        emit_message(
            output_value(message, cfg.data_only),
            cfg.pretty,
            cfg.color_mode,
        );
        prev_ts = ts;
        if cfg.one {
            return Ok(RunOutcome::ok());
        }
    }

    Ok(RunOutcome::ok())
}
