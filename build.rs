//! Purpose: Compile vendored Lite3 C sources plus the local shim for Rust FFI.
//! Role: Cargo build-script; configures `cc` inputs/includes and rebuild triggers.
//! Invariants: `cargo:rerun-if-changed` covers C sources plus embedded UI assets used by the server.
//! Invariants: Produces a `lite3` object library linked into the Rust crate.
//! Invariants: Requests C23-compatible mode for vendored Lite3 sources that declare variables after labels.
//! Invariants: Uses only Cargo-provided env vars (e.g. `CARGO_MANIFEST_DIR`).
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let lite3_dir = manifest_dir.join("vendor").join("lite3");
    let include_dir = lite3_dir.join("include");
    let lib_dir = lite3_dir.join("lib");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

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
    println!("cargo:rerun-if-changed=ui/index.html");

    ensure_c23_label_decl_support(&target, &out_dir);

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
        .file(manifest_dir.join("c").join("lite3_shim.c"));

    configure_lite3_compiler(&mut build, &target);

    build.compile("lite3");
}

fn ensure_c23_label_decl_support(target: &str, out_dir: &Path) {
    let probe_source = out_dir.join("lite3_c23_probe.c");
    fs::write(
        &probe_source,
        r#"
int lite3_c23_probe(int x) {
    switch (x) {
    case 5:
        int n = 1;
        return n;
    default:
        return 0;
    }
}
"#,
    )
    .expect("failed to write lite3 C23 probe source");

    let mut probe = cc::Build::new();
    probe.warnings(false).file(&probe_source);
    configure_lite3_compiler(&mut probe, target);

    if let Err(err) = probe.try_compile("lite3_c23_probe") {
        panic!(
            "C compiler for target `{target}` does not support required C23 syntax (declarations \
             immediately after labels) used by vendored Lite3.\n\
             Fix: install a C23-capable compiler and retry (set `CC` to override), then run cargo \
             build again.\n\
             Underlying compiler error: {err}"
        );
    }
}

fn configure_lite3_compiler(build: &mut cc::Build, target: &str) {
    if target.contains("windows-msvc") {
        if !has_user_cc_override(target) {
            build.compiler("clang-cl");
        }
    } else {
        build
            .flag_if_supported("-std=gnu2x")
            .flag_if_supported("-std=c2x");
    }
}

fn has_user_cc_override(target: &str) -> bool {
    let target_cc = format!("CC_{target}");
    let target_cc_underscored = format!("CC_{}", target.replace('-', "_"));
    env::var_os("CC").is_some()
        || env::var_os(target_cc).is_some()
        || env::var_os(target_cc_underscored).is_some()
}
