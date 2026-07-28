//! Purpose: Render shared CLI output formats.
//! Exports: `emit_json`.
//! Role: Keep terminal adaptation out of command execution.

use crate::ColorMode;
use crate::color_json::colorize_json;
use serde_json::Value;
use std::io::{self, IsTerminal};

pub(crate) fn emit_json(value: Value, color_mode: ColorMode) {
    let is_tty = io::stdout().is_terminal();
    let use_color = color_mode.use_color(is_tty);
    let pretty = is_tty || use_color;
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
