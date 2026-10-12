//! Render structured CLI output independently of terminal styling.

use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputMode {
    #[default]
    Human,
    Json,
    Yaml,
}

impl OutputMode {
    pub(crate) fn is_structured(self) -> bool {
        self != Self::Human
    }
}

pub(crate) fn render_output(value: &Value, mode: OutputMode) -> String {
    match mode {
        OutputMode::Yaml => {
            // Quote strings so older YAML readers cannot turn them into dates or booleans.
            let options = serde_saphyr::ser_options! { quote_all: true };
            let body = serde_saphyr::to_string_with_options(value, options)
                .expect("JSON values serialize as YAML");
            format!("---\n{}\n...\n", body.trim_end_matches('\n'))
        }
        OutputMode::Human | OutputMode::Json => {
            format!(
                "{}\n",
                serde_json::to_string(value).expect("JSON value serializes")
            )
        }
    }
}

pub(crate) fn emit_output(value: Value, mode: OutputMode) {
    // One write keeps documents together when duplex emits from two threads.
    print!("{}", render_output(&value, mode));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_preserves_full_width_numbers_and_scalar_types() {
        let value = serde_json::json!({
            "unsigned": u64::MAX, "signed": i64::MIN,
            "float": 1.0, "negative_zero": -0.0,
            "date": "2026-10-12", "boolean_text": "yes", "empty": "",
            "null": null, "array": [], "map": {},
        });
        let output = render_output(&value, OutputMode::Yaml);
        let decoded: Value = serde_yaml_ng::from_str(&output).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded["unsigned"].as_u64(), Some(u64::MAX));
        assert_eq!(decoded["signed"].as_i64(), Some(i64::MIN));
        assert!(decoded["float"].is_f64());
        assert!(
            decoded["negative_zero"]
                .as_f64()
                .unwrap()
                .is_sign_negative()
        );
    }
}
