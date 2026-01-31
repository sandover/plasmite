//! Purpose: Compile vendored Lite3 C sources plus the local shim for Rust FFI.
//! Role: Cargo build-script; configures `cc` inputs/includes and rebuild triggers.
//! Invariants: `cargo:rerun-if-changed` covers the shim + vendored sources we compile.
//! Invariants: Produces a `lite3` object library linked into the Rust crate.
//! Invariants: Uses only Cargo-provided env vars (e.g. `CARGO_MANIFEST_DIR`).
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let lite3_dir = manifest_dir.join("vendor").join("lite3");
    let include_dir = lite3_dir.join("include");
    let lib_dir = lite3_dir.join("lib");

    println!("cargo:rerun-if-changed=c/lite3_shim.c");
    println!("cargo:rerun-if-changed=c/lite3_shim.h");
    println!("cargo:rerun-if-changed=vendor/lite3/include/lite3.h");
    println!("cargo:rerun-if-changed=vendor/lite3/include/lite3_context_api.h");
    println!("cargo:rerun-if-changed=vendor/lite3/src/lite3.c");
    println!("cargo:rerun-if-changed=vendor/lite3/src/json_dec.c");
    println!("cargo:rerun-if-changed=vendor/lite3/src/json_enc.c");
    println!("cargo:rerun-if-changed=vendor/lite3/src/ctx_api.c");
    println!("cargo:rerun-if-changed=vendor/lite3/src/debug.c");
    println!("cargo:rerun-if-changed=vendor/lite3/lib/yyjson/yyjson.c");
    println!("cargo:rerun-if-changed=vendor/lite3/lib/nibble_base64/base64.c");

    let mut build = cc::Build::new();
    build
        .include(&include_dir)
        .include(&lib_dir)
        .file(lite3_dir.join("src").join("lite3.c"))
        .file(lite3_dir.join("src").join("json_dec.c"))
        .file(lite3_dir.join("src").join("json_enc.c"))
        .file(lite3_dir.join("src").join("ctx_api.c"))
        .file(lite3_dir.join("src").join("debug.c"))
        .file(lite3_dir.join("lib").join("yyjson").join("yyjson.c"))
        .file(lite3_dir.join("lib").join("nibble_base64").join("base64.c"))
        .file(manifest_dir.join("c").join("lite3_shim.c"))
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Wno-c23-extensions");

    build.compile("lite3");
}
