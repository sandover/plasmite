/*
Purpose: Link the Node binding against libplasmite.
Exports: None (build script only).
Role: Resolve libplasmite search path for the N-API addon.
Invariants: Uses PLASMITE_LIB_DIR or repo-local target/ outputs.
Notes: Fails fast when libplasmite cannot be located.
Notes: Reruns when PLASMITE_LIB_DIR changes.
*/

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PLASMITE_LIB_DIR");

    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = crate_dir
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .expect("bindings/node/native should be three levels below repo root")
        .to_path_buf();

    let candidates = env::var("PLASMITE_LIB_DIR")
        .ok()
        .map(PathBuf::from)
        .into_iter()
        .chain([
            repo_root.join("target").join("debug"),
            repo_root.join("target").join("release"),
        ]);

    let mut found = None;
    for candidate in candidates {
        if candidate.exists() {
            found = Some(candidate);
            break;
        }
    }

    let lib_dir = found.unwrap_or_else(|| {
        panic!(
            "libplasmite not found; set PLASMITE_LIB_DIR or build target/debug/libplasmite.*"
        )
    });

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=plasmite");
}
