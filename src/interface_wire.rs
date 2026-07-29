//! Purpose: Define the canonical serialized data shared by interface adapters.
//! Exports: Internal message, pool-info, bounds, metrics, and error wire types.
//! Role: Single source of truth for stable v0 field names and omission rules.
//! Invariants: This module contains representation only, not transport policy.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MessageWire {
    pub(crate) seq: u64,
    pub(crate) time: String,
    pub(crate) meta: MessageMetaWire,
    pub(crate) data: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct MessageMetaWire {
    pub(crate) tags: Vec<String>,
}

impl MessageWire {
    pub(crate) fn new(seq: u64, time: String, tags: Vec<String>, data: Value) -> Self {
        Self {
            seq,
            time,
            meta: MessageMetaWire { tags },
            data,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct BoundsWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) oldest: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) newest: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PoolInfoWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) path: String,
    pub(crate) file_size: u64,
    #[serde(default)]
    pub(crate) index_offset: u64,
    #[serde(default)]
    pub(crate) index_capacity: u32,
    #[serde(default)]
    pub(crate) index_size_bytes: u64,
    pub(crate) ring_offset: u64,
    pub(crate) ring_size: u64,
    #[serde(default)]
    pub(crate) bounds: BoundsWire,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) metrics: Option<PoolMetricsWire>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PoolMetricsWire {
    pub(crate) message_count: u64,
    pub(crate) seq_span: u64,
    pub(crate) utilization: PoolUtilizationWire,
    pub(crate) age: PoolAgeWire,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PoolUtilizationWire {
    pub(crate) used_bytes: u64,
    pub(crate) free_bytes: u64,
    pub(crate) used_percent: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PoolAgeWire {
    pub(crate) oldest_time: Option<String>,
    pub(crate) newest_time: Option<String>,
    pub(crate) oldest_age_ms: Option<u64>,
    pub(crate) newest_age_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ErrorKindWire {
    Internal,
    Usage,
    NotFound,
    AlreadyExists,
    Busy,
    Permission,
    Corrupt,
    Io,
    RetentionGap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ErrorPolicy {
    pub(crate) cli_message: &'static str,
    pub(crate) cli_exit_code: i32,
    pub(crate) http_status: u16,
    pub(crate) mcp_error_kind: &'static str,
}

pub(crate) const fn error_policy(kind: ErrorKindWire) -> ErrorPolicy {
    match kind {
        ErrorKindWire::Internal => ErrorPolicy {
            cli_message: "internal error",
            cli_exit_code: 1,
            http_status: 500,
            mcp_error_kind: "Internal",
        },
        ErrorKindWire::Usage => ErrorPolicy {
            cli_message: "usage error",
            cli_exit_code: 2,
            http_status: 400,
            mcp_error_kind: "Usage",
        },
        ErrorKindWire::NotFound => ErrorPolicy {
            cli_message: "not found",
            cli_exit_code: 3,
            http_status: 404,
            mcp_error_kind: "NotFound",
        },
        ErrorKindWire::AlreadyExists => ErrorPolicy {
            cli_message: "already exists",
            cli_exit_code: 4,
            http_status: 409,
            mcp_error_kind: "AlreadyExists",
        },
        ErrorKindWire::Busy => ErrorPolicy {
            cli_message: "resource is busy",
            cli_exit_code: 5,
            http_status: 423,
            mcp_error_kind: "Busy",
        },
        ErrorKindWire::Permission => ErrorPolicy {
            cli_message: "permission denied",
            cli_exit_code: 6,
            http_status: 401,
            mcp_error_kind: "Permission",
        },
        ErrorKindWire::Corrupt => ErrorPolicy {
            cli_message: "corrupt data",
            cli_exit_code: 7,
            http_status: 500,
            mcp_error_kind: "Corrupt",
        },
        ErrorKindWire::Io => ErrorPolicy {
            cli_message: "i/o error",
            cli_exit_code: 8,
            http_status: 500,
            mcp_error_kind: "Io",
        },
        ErrorKindWire::RetentionGap => ErrorPolicy {
            cli_message: "retention gap",
            cli_exit_code: 9,
            http_status: 410,
            mcp_error_kind: "RetentionGap",
        },
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ErrorContextWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) causes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        BoundsWire, ErrorContextWire, ErrorKindWire, MessageMetaWire, MessageWire, PoolAgeWire,
        PoolInfoWire, PoolMetricsWire, PoolUtilizationWire, error_policy,
    };
    use serde_json::json;

    #[test]
    fn message_contract_preserves_tags_and_envelope_fields() {
        let wire = MessageWire {
            seq: 7,
            time: "2026-07-28T12:00:00Z".to_string(),
            meta: MessageMetaWire {
                tags: vec!["build".to_string(), "green".to_string()],
            },
            data: json!({"ok": true}),
        };

        assert_eq!(
            serde_json::to_value(wire).expect("serialize message"),
            json!({
                "seq": 7,
                "time": "2026-07-28T12:00:00Z",
                "meta": {"tags": ["build", "green"]},
                "data": {"ok": true}
            })
        );
    }

    #[test]
    fn bounds_omit_absent_endpoints() {
        assert_eq!(
            serde_json::to_value(BoundsWire::default()).expect("serialize empty bounds"),
            json!({})
        );
        assert_eq!(
            serde_json::to_value(BoundsWire {
                oldest: Some(4),
                newest: None,
            })
            .expect("serialize partial bounds"),
            json!({"oldest": 4})
        );
    }

    #[test]
    fn pool_info_omits_optional_name_and_metrics() {
        let wire = PoolInfoWire {
            name: None,
            path: "/tmp/events.pool".to_string(),
            file_size: 4096,
            index_offset: 128,
            index_capacity: 16,
            index_size_bytes: 256,
            ring_offset: 384,
            ring_size: 3712,
            bounds: BoundsWire::default(),
            metrics: None,
        };

        let value = serde_json::to_value(wire).expect("serialize pool info");
        assert_eq!(value["bounds"], json!({}));
        assert!(value.get("name").is_none());
        assert!(value.get("metrics").is_none());
    }

    #[test]
    fn pool_metrics_preserve_fractional_percent_and_optional_age() {
        let wire = PoolMetricsWire {
            message_count: 3,
            seq_span: 5,
            utilization: PoolUtilizationWire {
                used_bytes: 100,
                free_bytes: 300,
                used_percent: 25.25,
            },
            age: PoolAgeWire {
                oldest_time: Some("2026-07-28T12:00:00Z".to_string()),
                newest_time: None,
                oldest_age_ms: Some(400),
                newest_age_ms: None,
            },
        };

        assert_eq!(
            serde_json::to_value(wire).expect("serialize metrics"),
            json!({
                "message_count": 3,
                "seq_span": 5,
                "utilization": {
                    "used_bytes": 100,
                    "free_bytes": 300,
                    "used_percent": 25.25
                },
                "age": {
                    "oldest_time": "2026-07-28T12:00:00Z",
                    "newest_time": null,
                    "oldest_age_ms": 400,
                    "newest_age_ms": null
                }
            })
        );
    }

    #[test]
    fn every_error_kind_has_its_stable_name() {
        let cases = [
            (ErrorKindWire::Internal, "Internal"),
            (ErrorKindWire::Usage, "Usage"),
            (ErrorKindWire::NotFound, "NotFound"),
            (ErrorKindWire::AlreadyExists, "AlreadyExists"),
            (ErrorKindWire::Busy, "Busy"),
            (ErrorKindWire::Permission, "Permission"),
            (ErrorKindWire::Corrupt, "Corrupt"),
            (ErrorKindWire::Io, "Io"),
            (ErrorKindWire::RetentionGap, "RetentionGap"),
        ];
        for (kind, name) in cases {
            assert_eq!(
                serde_json::to_value(kind).expect("serialize error kind"),
                json!(name)
            );
        }
    }

    #[test]
    fn every_error_kind_has_stable_interface_defaults() {
        let cases = [
            (
                ErrorKindWire::Internal,
                "internal error",
                1,
                500,
                "Internal",
            ),
            (ErrorKindWire::Usage, "usage error", 2, 400, "Usage"),
            (ErrorKindWire::NotFound, "not found", 3, 404, "NotFound"),
            (
                ErrorKindWire::AlreadyExists,
                "already exists",
                4,
                409,
                "AlreadyExists",
            ),
            (ErrorKindWire::Busy, "resource is busy", 5, 423, "Busy"),
            (
                ErrorKindWire::Permission,
                "permission denied",
                6,
                401,
                "Permission",
            ),
            (ErrorKindWire::Corrupt, "corrupt data", 7, 500, "Corrupt"),
            (ErrorKindWire::Io, "i/o error", 8, 500, "Io"),
            (
                ErrorKindWire::RetentionGap,
                "retention gap",
                9,
                410,
                "RetentionGap",
            ),
        ];

        for (kind, message, exit_code, http_status, mcp_error_kind) in cases {
            let policy = error_policy(kind);
            assert_eq!(policy.cli_message, message);
            assert_eq!(policy.cli_exit_code, exit_code);
            assert_eq!(policy.http_status, http_status);
            assert_eq!(policy.mcp_error_kind, mcp_error_kind);
        }
    }

    #[test]
    fn error_context_omits_absent_facts() {
        let context = ErrorContextWire {
            seq: Some(9),
            ..ErrorContextWire::default()
        };

        assert_eq!(
            serde_json::to_value(context).expect("serialize error context"),
            json!({"seq": 9})
        );
    }
}
