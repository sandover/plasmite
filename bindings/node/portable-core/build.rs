use std::{env, path::PathBuf};
fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../..");
    let sdk =
        PathBuf::from(env::var("WASI_SDK_PATH").expect("set WASI_SDK_PATH to the pinned WASI SDK"));
    let mut cc = cc::Build::new();
    cc.compiler(sdk.join("bin/clang"))
        .archiver(sdk.join("bin/llvm-ar"))
        .flag("--target=wasm32-unknown-unknown")
        .flag(format!(
            "--sysroot={}",
            sdk.join("share/wasi-sysroot").display()
        ))
        .flag("-isystem")
        .flag(
            sdk.join("share/wasi-sysroot/include/wasm32-wasip1")
                .to_str()
                .unwrap(),
        )
        .define("__wasi__", "1")
        .define("NDEBUG", "1")
        .flag("-std=gnu11")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .include(root.join("vendor/lite3/include"))
        .include(root.join("vendor/lite3/lib"));
    for name in [
        "vendor/lite3/src/lite3.c",
        "vendor/lite3/src/json_dec.c",
        "vendor/lite3/src/json_enc.c",
        "vendor/lite3/src/ctx_api.c",
        "vendor/lite3/src/debug.c",
        "vendor/lite3/lib/yyjson/yyjson.c",
        "vendor/lite3/lib/nibble_base64/base64.c",
        "c/lite3_shim.c",
        "c/lite3_builder.c",
    ] {
        let path = root.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        cc.file(path);
    }
    println!("cargo:rerun-if-env-changed=WASI_SDK_PATH");
    // One ordinary libc primitive is needed. Extract that object rather than
    // link a second allocator or WASI initialization/runtime into Rust's heap.
    let object = std::process::Command::new(sdk.join("bin/llvm-ar"))
        .arg("p")
        .arg(sdk.join("share/wasi-sysroot/lib/wasm32-wasip1/libc.a"))
        .arg("memchr.c.obj")
        .output()
        .expect("read SDK memchr object");
    assert!(object.status.success() && !object.stdout.is_empty());
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("memchr.o");
    std::fs::write(&path, object.stdout).unwrap();
    cc.object(path).compile("lite3-portable");
    println!("cargo:rustc-link-arg=--max-memory=1073741824");
}
