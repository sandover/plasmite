//! Purpose: Shared pool-info JSON serializers for CLI and HTTP serving paths.
//! Exports: `pool_info_json` and `bounds_json`.
//! Role: Keep pool metadata envelope shape consistent across entry points.
//! Invariants: Stable key names/order for v0 pool info payloads.
//! Invariants: Metrics block is emitted only when source metrics exist.

use plasmite::api::{Bounds, PoolInfo, PoolMetrics};
use serde_json::{Map, Value, json};

pub(crate) fn bounds_json(bounds: Bounds) -> Value {
    let mut map = Map::new();
    if let Some(oldest) = bounds.oldest_seq {
        map.insert("oldest".to_string(), json!(oldest));
    }
    if let Some(newest) = bounds.newest_seq {
        map.insert("newest".to_string(), json!(newest));
    }
    Value::Object(map)
}

pub(crate) fn pool_info_json(pool_ref: &str, info: &PoolInfo) -> Value {
    let mut map = Map::new();
    map.insert("name".to_string(), json!(pool_ref));
    map.insert("path".to_string(), json!(info.path.display().to_string()));
    map.insert("file_size".to_string(), json!(info.file_size));
    map.insert("index_offset".to_string(), json!(info.index_offset));
    map.insert("index_capacity".to_string(), json!(info.index_capacity));
    map.insert("index_size_bytes".to_string(), json!(info.index_size_bytes));
    map.insert("ring_offset".to_string(), json!(info.ring_offset));
    map.insert("ring_size".to_string(), json!(info.ring_size));
    map.insert("bounds".to_string(), bounds_json(info.bounds));
    if let Some(metrics) = &info.metrics {
        map.insert("metrics".to_string(), pool_metrics_json(metrics));
    }
    Value::Object(map)
}

fn pool_metrics_json(metrics: &PoolMetrics) -> Value {
    json!({
        "message_count": metrics.message_count,
        "seq_span": metrics.seq_span,
        "utilization": {
            "used_bytes": metrics.utilization.used_bytes,
            "free_bytes": metrics.utilization.free_bytes,
            "used_percent": (metrics.utilization.used_percent_hundredths as f64) / 100.0,
        },
        "age": {
            "oldest_time": metrics.age.oldest_time,
            "newest_time": metrics.age.newest_time,
            "oldest_age_ms": metrics.age.oldest_age_ms,
            "newest_age_ms": metrics.age.newest_age_ms,
        },
    })
}
