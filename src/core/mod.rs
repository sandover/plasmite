//! Purpose: Core storage, encoding, planning, validation, and error modeling.
//! Exports: `pool`, `cursor`, `plan`, `frame`, `validate`, `error`, `lite3`.
//! Role: Internal core layer shared by CLI and tests; does not perform CLI I/O.
//! Invariants: Public functions take explicit inputs and return explicit results/errors.
//! Invariants: Full scans/expensive validation are opt-in and not on hot paths.
pub mod cursor;
pub mod error;
pub mod frame;
pub mod lite3;
pub mod plan;
pub mod pool;
pub mod validate;
