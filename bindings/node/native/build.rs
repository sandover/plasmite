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
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        // The Windows extern blocks import directly from plasmite2.dll.
        return;
    }
    if target_os == "macos" {
        println!("cargo:rustc-link-arg=-Wl,-undefined,dynamic_lookup");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    } else if target_os == "linux" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }

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

    let names = if target_os == "macos" {
        ["libplasmite.2.dylib", "libplasmite.dylib"]
    } else {
        ["libplasmite.so.2", "libplasmite.so"]
    };
    let library = candidates
        .flat_map(|dir| names.map(|name| dir.join(name)))
        .find(|path| path.is_file())
        .expect("build libplasmite or set PLASMITE_LIB_DIR");

    // Cargo emits an unversioned filename. Its install name or SONAME
    // determines which versioned file the operating system loads at runtime.
    println!("cargo:rerun-if-changed={}", library.display());
    println!("cargo:rustc-link-arg={}", library.display());
}
