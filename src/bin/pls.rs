// Short alias binary for `plasmite`.
// Resolves the sibling `plasmite` binary first; falls back to PATH.
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let target = resolve_plasmite_binary();

    let status = Command::new(&target)
        .args(args)
        .status();

    match status {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("pls: failed to execute {}: {err}", target.display());
            std::process::exit(1);
        }
    }
}

fn resolve_plasmite_binary() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("plasmite");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("plasmite")
}
