//! Purpose: Render pretty JSON with optional ANSI colorization for CLI output.
//! Exports: colorize_json.
//! Role: Small, pure formatter used by CLI emission paths.
//! Invariants: When color is disabled, output equals serde_json::to_string_pretty.
//! Invariants: ANSI escapes appear only when explicitly enabled.
use serde_json::Value;

const INDENT: &str = "  ";

// Conservative 8/16-color palette for broad terminal compatibility.
// Avoid bright variants that can lose contrast on themes like Solarized.
const COLOR_KEY: &str = "36";
const COLOR_STRING: &str = "32";
const COLOR_NUMBER: &str = "33";
const COLOR_BOOL: &str = "35";
const COLOR_NULL: &str = "39";
const COLOR_PUNCT: &str = "39";

pub fn colorize_json(value: &Value, use_color: bool) -> String {
    let mut out = String::new();
    write_value(value, 0, use_color, &mut out);
    out
}

fn write_value(value: &Value, indent: usize, use_color: bool, out: &mut String) {
    match value {
        Value::Null => push_colored("null", COLOR_NULL, use_color, out),
        Value::Bool(val) => {
            let text = if *val { "true" } else { "false" };
            push_colored(text, COLOR_BOOL, use_color, out);
        }
        Value::Number(num) => push_colored(&num.to_string(), COLOR_NUMBER, use_color, out),
        Value::String(text) => {
            let encoded = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
            push_colored(&encoded, COLOR_STRING, use_color, out);
        }
        Value::Array(items) => write_array(items, indent, use_color, out),
        Value::Object(map) => write_object(map, indent, use_color, out),
    }
}

fn write_array(items: &[Value], indent: usize, use_color: bool, out: &mut String) {
    if items.is_empty() {
        push_colored("[]", COLOR_PUNCT, use_color, out);
        return;
    }
    push_colored("[", COLOR_PUNCT, use_color, out);
    out.push('\n');
    for (idx, item) in items.iter().enumerate() {
        push_indent(indent + 1, out);
        write_value(item, indent + 1, use_color, out);
        if idx + 1 < items.len() {
            push_colored(",", COLOR_PUNCT, use_color, out);
        }
        out.push('\n');
    }
    push_indent(indent, out);
    push_colored("]", COLOR_PUNCT, use_color, out);
}

fn write_object(
    map: &serde_json::Map<String, Value>,
    indent: usize,
    use_color: bool,
    out: &mut String,
) {
    if map.is_empty() {
        push_colored("{}", COLOR_PUNCT, use_color, out);
        return;
    }
    push_colored("{", COLOR_PUNCT, use_color, out);
    out.push('\n');
    let len = map.len();
    for (idx, (key, value)) in map.iter().enumerate() {
        push_indent(indent + 1, out);
        let encoded = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
        push_colored(&encoded, COLOR_KEY, use_color, out);
        push_colored(":", COLOR_PUNCT, use_color, out);
        out.push(' ');
        write_value(value, indent + 1, use_color, out);
        if idx + 1 < len {
            push_colored(",", COLOR_PUNCT, use_color, out);
        }
        out.push('\n');
    }
    push_indent(indent, out);
    push_colored("}", COLOR_PUNCT, use_color, out);
}

fn push_indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str(INDENT);
    }
}

fn push_colored(text: &str, color: &str, use_color: bool, out: &mut String) {
    if !use_color {
        out.push_str(text);
        return;
    }
    out.push_str("\u{1b}[");
    out.push_str(color);
    out.push('m');
    out.push_str(text);
    out.push_str("\u{1b}[0m");
}

#[cfg(test)]
mod tests {
    use super::colorize_json;
    use serde_json::json;

    #[test]
    fn colorize_json_matches_pretty_when_disabled() {
        let value = json!({
            "arr": [1, true, null],
            "nested": { "x": "y" }
        });
        let plain = colorize_json(&value, false);
        let pretty = serde_json::to_string_pretty(&value).expect("pretty");
        assert_eq!(plain, pretty);
    }

    #[test]
    fn colorize_json_emits_ansi_when_enabled() {
        let value = json!({"k":"v","n":1,"b":true,"z":null});
        let colored = colorize_json(&value, true);
        assert!(colored.contains("\u{1b}["));
        assert!(colored.contains("\u{1b}[36m\"k\"\u{1b}[0m"));
        assert!(colored.contains("\u{1b}[32m\"v\"\u{1b}[0m"));
        assert!(colored.contains("\u{1b}[33m1\u{1b}[0m"));
        assert!(colored.contains("\u{1b}[35mtrue\u{1b}[0m"));
        assert!(colored.contains("\u{1b}[39mnull\u{1b}[0m"));
    }
}
