//! Purpose: Shared core library crate used by the `plasmite` CLI and tests.
//! Exports: `core` (pool storage, framing, planning, validation, errors).
//! Role: Internal library backing the binaries; not yet a stable public SDK.
//! Invariants: Treat the crate API as internal until a dedicated library release.
//! Invariants: Core modules prefer explicit inputs/outputs over hidden state.
pub mod core;
