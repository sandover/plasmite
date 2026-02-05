//! Purpose: Centralize pool format versioning and migration guidance.
//! Exports: `POOL_FORMAT_VERSION`, `SUPPORTED_POOL_FORMAT_VERSIONS`, `pool_version_error`.
//! Role: Shared policy for gating on-disk compatibility across open/validation paths.
//! Invariants: Version list is additive; bump only for incompatible on-disk changes.
//! Invariants: Migration guidance stays actionable and stable for users.

use crate::core::error::{Error, ErrorKind};

pub const POOL_FORMAT_VERSION: u32 = 1;
pub const SUPPORTED_POOL_FORMAT_VERSIONS: &[u32] = &[POOL_FORMAT_VERSION];

pub fn pool_version_error(detected: u32) -> Error {
    let supported = SUPPORTED_POOL_FORMAT_VERSIONS
        .iter()
        .map(|version| version.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Error::new(ErrorKind::Usage)
        .with_message(format!(
            "unsupported pool format version {detected} (supported: {supported})"
        ))
        .with_hint(
            "Upgrade plasmite or migrate the pool (export/import). Run `plasmite doctor <pool>` for guidance.",
        )
}
