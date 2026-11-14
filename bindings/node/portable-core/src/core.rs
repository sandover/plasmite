#[path = "../../../../src/core/error.rs"]
pub mod error;
#[expect(
    clippy::wrong_self_convention,
    reason = "Keep the shared DocRef API; its slice is only eight bytes on wasm32"
)]
#[path = "../../../../src/core/lite3/mod.rs"]
pub mod lite3;
