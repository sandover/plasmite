//! Render a received message on one terminal line.

use serde_json::Value;
use std::fmt::Write;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::output_support::human_literal;

pub(super) fn render(
    message: &Value,
    now: OffsetDateTime,
    width: Option<usize>,
    color: bool,
) -> String {
    let stamp = message["time"].as_str().unwrap_or_default();
    let stamp = local_timestamp(stamp, now);
    let seq = format!("#{}", message["seq"]);
    let meta = &message["meta"];
    let sender = meta["sender"].as_str().map(human_literal);
    let tags = meta["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(human_literal)
        .collect::<Vec<_>>()
        .join(", ");
    let data = terminal_json(&message["data"]);
    let fields = [
        (stamp.as_str(), ""),
        (seq.as_str(), "\x1b[2m"),
        (sender.as_deref().unwrap_or_default(), ""),
        (tags.as_str(), "\x1b[36m"),
        (data.as_str(), ""),
    ];
    let mut line = String::new();
    let mut remaining = width.unwrap_or(usize::MAX);
    for (field, style) in fields.into_iter().filter(|(field, _)| !field.is_empty()) {
        let separator = if line.is_empty() { "" } else { "  " };
        if remaining <= separator.len() {
            break;
        }
        remaining -= separator.len();
        line.push_str(separator);
        let styled = color && !style.is_empty();
        if styled {
            line.push_str(style);
        }
        let columns = field.width();
        if columns <= remaining {
            line.push_str(field);
        } else {
            line.push_str(clip(field, remaining - 1));
            line.push('…');
        }
        if styled {
            line.push_str("\x1b[0m");
        }
        if columns > remaining {
            break;
        }
        remaining -= columns;
    }
    line.push('\n');
    line
}

fn terminal_json(value: &Value) -> String {
    let json = value.to_string();
    if !json.chars().any(char::is_control) {
        return json;
    }
    let mut escaped = String::with_capacity(json.len());
    for character in json.chars() {
        if character.is_control() {
            write!(escaped, "\\u{:04x}", character as u32).expect("write to string");
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn clip(field: &str, columns: usize) -> &str {
    let mut used = 0;
    for (offset, character) in field.char_indices() {
        used += character.width().unwrap_or(0);
        if used > columns {
            return &field[..offset];
        }
    }
    field
}

fn local_timestamp(stamp: &str, now: OffsetDateTime) -> String {
    let Ok(stamp) = OffsetDateTime::parse(stamp, &Rfc3339) else {
        return human_literal(stamp);
    };
    let local = |date: OffsetDateTime| {
        date.to_offset(UtcOffset::local_offset_at(date).unwrap_or(UtcOffset::UTC))
    };
    let stamp = local(stamp);
    let clock = format!(
        "{:02}:{:02}:{:02}",
        stamp.hour(),
        stamp.minute(),
        stamp.second()
    );
    if stamp.date() == local(now).date() {
        clock
    } else {
        format!("{} {clock}", stamp.date())
    }
}

#[cfg(unix)]
pub(super) fn terminal_width() -> Option<usize> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
    // SAFETY: TIOCGWINSZ writes a winsize to the supplied buffer on success.
    let width = unsafe {
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) != 0 {
            return None;
        }
        size.assume_init().ws_col
    };
    (width > 0).then_some(usize::from(width))
}

#[cfg(windows)]
pub(super) fn terminal_width() -> Option<usize> {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    let mut info = std::mem::MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::uninit();
    // SAFETY: GetConsoleScreenBufferInfo writes one structure on success. The
    // process owns the standard output handle and this call only borrows it.
    let window = unsafe {
        if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), info.as_mut_ptr()) == 0 {
            return None;
        }
        info.assume_init().srWindow
    };
    usize::try_from(i32::from(window.Right) - i32::from(window.Left) + 1).ok()
}

#[cfg(not(any(unix, windows)))]
pub(super) fn terminal_width() -> Option<usize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shows_sender_when_present_and_escapes_terminal_controls() {
        let now = OffsetDateTime::parse("2026-11-20T12:00:00Z", &Rfc3339).unwrap();
        let mut message = json!({
            "time": "2026-11-19T12:00:00Z",
            "seq": 42,
            "meta": {"sender": "build\nagent", "tags": ["ship\x1b", "handoff"]},
            "data": {"state": "ready\u{009b}"}
        });
        let text = render(&message, now, None, false);
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains(['\x1b', '\u{009b}']));
        assert!(text.contains(" #42  build\\nagent  ship\\u{1b}, handoff  {\"state\":"));
        let (_, data) = text.split_once("handoff  ").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(data).unwrap(),
            message["data"]
        );

        message["meta"].as_object_mut().unwrap().remove("sender");
        let text = render(&message, now, None, true);
        assert!(text.contains("\x1b[2m#42\x1b[0m  \x1b[36mship"));
        assert!(!text.contains("build"));
    }

    #[test]
    fn clips_to_display_columns_without_splitting_utf8() {
        let now = OffsetDateTime::parse("2026-11-20T12:00:00Z", &Rfc3339).unwrap();
        let message = json!({
            "time": "2026-11-20T12:00:00Z",
            "seq": 1,
            "meta": {"tags": []},
            "data": {"city": "東京東京東京東京東京東京"}
        });
        let text = render(&message, now, Some(26), false);
        assert!(!text.contains("2026-11-20"));
        assert!(text.ends_with("…\n"));
        assert!(text.trim_end().width() <= 26);
    }
}
