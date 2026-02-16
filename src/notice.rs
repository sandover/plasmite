//! Purpose: Define a stable, structured schema for non-fatal stderr notices.
//! Exports: `Notice`, `notice_json`.
//! Role: Shared contract helper for CLI diagnostics (non-error events).
//! Invariants: Notices are non-fatal and never alter stdout payloads.
//! Invariants: JSON schema is stable once published; fields are additive-only.
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: String,
    pub time: String,
    pub cmd: String,
    pub pool: String,
    pub message: String,
    pub details: Map<String, Value>,
}

pub fn notice_json(notice: &Notice) -> Value {
    let mut inner = Map::new();
    inner.insert("kind".to_string(), json!(notice.kind));
    inner.insert("time".to_string(), json!(notice.time));
    inner.insert("cmd".to_string(), json!(notice.cmd));
    inner.insert("pool".to_string(), json!(notice.pool));
    inner.insert("message".to_string(), json!(notice.message));
    inner.insert("details".to_string(), Value::Object(notice.details.clone()));

    let mut outer = Map::new();
    outer.insert("notice".to_string(), Value::Object(inner));
    Value::Object(outer)
}

#[cfg(test)]
mod tests {
    use super::{Notice, notice_json};
    use serde_json::{Map, Value};

    #[test]
    fn notice_json_has_required_fields() {
        let mut details = Map::new();
        details.insert("dropped_count".to_string(), Value::from(3));

        let notice = Notice {
            kind: "drop".to_string(),
            time: "2026-02-01T00:00:00Z".to_string(),
            cmd: "follow".to_string(),
            pool: "demo".to_string(),
            message: "dropped 3 messages".to_string(),
            details,
        };

        let value = notice_json(&notice);
        let obj = value
            .get("notice")
            .and_then(|v| v.as_object())
            .expect("notice object");

        assert_eq!(obj.get("kind").and_then(|v| v.as_str()), Some("drop"));
        assert_eq!(
            obj.get("time").and_then(|v| v.as_str()),
            Some("2026-02-01T00:00:00Z")
        );
        assert_eq!(obj.get("cmd").and_then(|v| v.as_str()), Some("follow"));
        assert_eq!(obj.get("pool").and_then(|v| v.as_str()), Some("demo"));
        assert_eq!(
            obj.get("message").and_then(|v| v.as_str()),
            Some("dropped 3 messages")
        );
        assert!(obj.get("details").and_then(|v| v.as_object()).is_some());
    }
}
