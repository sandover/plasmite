//! Purpose: Provide a short `pls` alias that execs the `plasmite` binary.
//! Role: Convenience wrapper; resolves a sibling `plasmite` first, else uses PATH.
//! Invariants: Forwards CLI args verbatim and propagates the child exit status.
//! Invariants: Prefers `./plasmite` next to the current executable when present.
//! Invariants: Emits only a plain stderr message on exec failure.
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let target = resolve_plasmite_binary();

    let status = Command::new(&target).args(args).status();

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
