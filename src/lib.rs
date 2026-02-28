//! Purpose: Shared library crate used by the `plasmite` CLI and bindings.
//! Exports: `api` (stable public surface), `notice` (structured stderr notices).
//! Role: Public API boundary with private internal storage modules.
//! Invariants: Additive-only changes to `api`; internal modules remain private.
//! Invariants: Core modules prefer explicit inputs/outputs over hidden state.
mod abi;
pub mod api;
mod core;
pub mod mcp;
pub mod notice;
mod pool_paths;
