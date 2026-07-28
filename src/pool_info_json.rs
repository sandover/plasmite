//! Purpose: Shared pool-info JSON serializers for CLI and HTTP serving paths.
//! Exports: `pool_info_json` and `bounds_json`.
//! Role: Keep pool metadata envelope shape consistent across entry points.
//! Invariants: Stable key names/order for v0 pool info payloads.
//! Invariants: Metrics block is emitted only when source metrics exist.

use crate::interface_wire::{
    BoundsWire, PoolAgeWire, PoolInfoWire, PoolMetricsWire, PoolUtilizationWire,
};
use plasmite::api::{Bounds, PoolInfo, PoolMetrics};
use serde_json::Value;

pub(crate) fn bounds_json(bounds: Bounds) -> Value {
    serde_json::to_value(bounds_wire(bounds)).expect("bounds wire data is serializable")
}

pub(crate) fn pool_info_json(pool_ref: &str, info: &PoolInfo) -> Value {
    serde_json::to_value(pool_info_wire(pool_ref, info))
        .expect("pool-info wire data is serializable")
}

fn pool_info_wire(pool_ref: &str, info: &PoolInfo) -> PoolInfoWire {
    PoolInfoWire {
        name: Some(pool_ref.to_string()),
        path: info.path.display().to_string(),
        file_size: info.file_size,
        index_offset: info.index_offset,
        index_capacity: info.index_capacity,
        index_size_bytes: info.index_size_bytes,
        ring_offset: info.ring_offset,
        ring_size: info.ring_size,
        bounds: bounds_wire(info.bounds),
        metrics: info.metrics.as_ref().map(pool_metrics_wire),
    }
}

fn bounds_wire(bounds: Bounds) -> BoundsWire {
    BoundsWire {
        oldest: bounds.oldest_seq,
        newest: bounds.newest_seq,
    }
}

fn pool_metrics_wire(metrics: &PoolMetrics) -> PoolMetricsWire {
    PoolMetricsWire {
        message_count: metrics.message_count,
        seq_span: metrics.seq_span,
        utilization: PoolUtilizationWire {
            used_bytes: metrics.utilization.used_bytes,
            free_bytes: metrics.utilization.free_bytes,
            used_percent: (metrics.utilization.used_percent_hundredths as f64) / 100.0,
        },
        age: PoolAgeWire {
            oldest_time: metrics.age.oldest_time.clone(),
            newest_time: metrics.age.newest_time.clone(),
            oldest_age_ms: metrics.age.oldest_age_ms,
            newest_age_ms: metrics.age.newest_age_ms,
        },
    }
}
