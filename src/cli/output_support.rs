//! CLI helpers for output commands.

use crate::ColorMode;
use crate::cli::output::emit_json;
use crate::color_json::colorize_json;
use crate::interface_wire::ErrorKindWire;
use crate::interface_wire::error_policy;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::notice::Notice;
use plasmite::notice::notice_json;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::error::Error as StdError;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use super::support::now_ns;

pub(crate) fn emit_feed_receipt_human(receipt: &Value) {
    let seq = receipt
        .get("seq")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let time = receipt
        .get("time")
        .and_then(|value| value.as_str())
        .map(|value| human_literal(&format_timestamp_human(value)))
        .unwrap_or_else(|| "-".to_string());
    let tags = receipt
        .get("meta")
        .and_then(|value| value.get("tags"))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .map(human_literal)
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

pub(crate) fn emit_feed_receipt(value: Value, color_mode: ColorMode, json_output: bool) {
    if json_output {
        emit_json(value, color_mode);
    } else {
        emit_feed_receipt_human(&value);
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
    let headers = headers
        .iter()
        .map(|header| human_literal(header))
        .collect::<Vec<_>>();
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
            let cleaned = human_literal(value);
            *width = (*width).max(cleaned.chars().count());
            sanitized.push(cleaned);
        }
        sanitized_rows.push(sanitized);
    }

    let mut lines = Vec::with_capacity(sanitized_rows.len() + 1);
    lines.push(format_table_line(&headers, &widths));
    for row in sanitized_rows {
        lines.push(format_table_line(&row, &widths));
    }
    lines.join("\n")
}

/// Render untrusted text literally before adding trusted terminal styling.
pub(crate) fn human_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            literal.extend(character.escape_debug());
        } else {
            literal.push(character);
        }
    }
    literal
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

pub(crate) fn emit_error(err: &Error, color_mode: ColorMode, json_output: bool) {
    let is_tty = io::stderr().is_terminal();
    if !json_output {
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

pub(crate) fn emit_notice(notice: &Notice, color_mode: ColorMode, json_output: bool) {
    let is_tty = io::stderr().is_terminal();
    if !json_output {
        let label = colorize_label("notice:", color_mode.use_color(is_tty), AnsiColor::Yellow);
        let message = human_literal(&notice.message);
        if notice.cmd == "feed" {
            eprintln!("{label} {message}");
        } else {
            eprintln!("{label} {message} (pool: {})", human_literal(&notice.pool));
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
        human_literal(&error_message(err))
    ));

    if let Some(hint) = err.hint() {
        let hint = human_literal(hint);
        lines.push(format!(
            "{} {hint}",
            colorize_label("hint:", use_color, AnsiColor::Yellow)
        ));
    }
    if let Some(path) = err.path() {
        lines.push(format!(
            "{} {}",
            colorize_label("path:", use_color, AnsiColor::Yellow),
            human_literal(&display_handoff_path_from_path(path))
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
        let cause = human_literal(cause);
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
    let rendered = human_literal(&rendered);
    if rendered.ends_with('/') {
        rendered
    } else {
        format!("{rendered}/")
    }
}

#[cfg(test)]
mod literal_tests {
    use super::{error_text, human_literal, render_table};
    use plasmite::api::{Error, ErrorKind};

    #[test]
    fn human_literals_escape_terminal_controls_and_preserve_unicode() {
        assert_eq!(
            human_literal("雪\x1b]52;c;SGVsbG8=\x07\trow\nnext\r\0\x7f\u{85}"),
            "雪\\u{1b}]52;c;SGVsbG8=\\u{7}\\trow\\nnext\\r\\0\\u{7f}\\u{85}"
        );
    }

    #[test]
    fn table_widths_use_literal_headers_and_cells() {
        let table = render_table(
            &["NAME", "DETAIL"],
            &[
                vec!["x\x1b\t".into(), "safe".into()],
                vec!["雪".into(), "ok".into()],
            ],
        );
        assert_eq!(
            table,
            "NAME       DETAIL\nx\\u{1b}\\t  safe  \n雪          ok    "
        );
        assert!(
            !table
                .chars()
                .any(|character| character.is_control() && character != '\n')
        );
    }

    #[test]
    fn human_errors_escape_fields_before_adding_label_color() {
        let error = Error::new(ErrorKind::Io)
            .with_message("bad\x1b]52;c;value\x07")
            .with_hint("try\tpath\nnext")
            .with_path("evil\x1b\t.plasmite")
            .with_source(std::io::Error::other("cause\x1b\r"));
        let plain = error_text(&error, false);
        assert!(plain.contains("bad\\u{1b}]52;c;value\\u{7}"));
        assert!(plain.contains("try\\tpath\\nnext"));
        assert!(plain.contains("evil\\u{1b}\\t.plasmite"));
        assert!(plain.contains("cause\\u{1b}\\r"));
        assert!(!plain.contains('\x1b'));
        let styled = error_text(&error, true);
        assert!(styled.contains("\x1b[31merror:\x1b[0m"));
        assert!(!styled.contains("\x1b]52"));
    }
}
