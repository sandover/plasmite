//! Purpose: Render shared CLI machine output.
//! Exports: `emit_json`.
//! Role: Keep machine-readable output independent of terminal styling.

use crate::ColorMode;
use serde_json::Value;

pub(crate) fn emit_json(value: Value, _color_mode: ColorMode) {
    let json = serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string());
    println!("{json}");
}
