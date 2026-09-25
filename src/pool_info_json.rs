//! Purpose: Shared pool-info JSON serializers for CLI and HTTP serving paths.
//! Exports: `pool_info_json` and `bounds_json`.
//! Role: Keep pool metadata envelope shape consistent across entry points.
//! Invariants: Stable key names/order for v0 pool info payloads.
//! Invariants: Metrics block is emitted only when source metrics exist.

use crate::interface_wire::BoundsWire;
use plasmite::api::{Bounds, PoolInfo};
use serde_json::Value;

pub(crate) fn bounds_json(bounds: Bounds) -> Value {
    serde_json::to_value(bounds_wire(bounds)).expect("bounds wire data is serializable")
}

pub(crate) fn pool_info_json(pool_ref: &str, info: &PoolInfo) -> Value {
    serde_json::to_value(crate::interface_wire::pool_info_wire!(pool_ref, info))
        .expect("pool-info wire data is serializable")
}

fn bounds_wire(bounds: Bounds) -> BoundsWire {
    BoundsWire {
        oldest: bounds.oldest_seq,
        newest: bounds.newest_seq,
    }
}
