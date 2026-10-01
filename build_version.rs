//! Derive one product build identity from package and source provenance.
use std::process::Command;

pub fn build_identity(manifest_dir: &std::path::Path, version: &str) -> String {
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .current_dir(manifest_dir)
            .arg("--no-optional-locks")
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    // Registry packages carry their own provenance, even inside another repo.
    if manifest_dir.join(".cargo_vcs_info.json").is_file() {
        return version.to_owned();
    }
    let root = git(&["rev-parse", "--show-toplevel"])
        .and_then(|root| std::path::Path::new(&root).canonicalize().ok());
    if root.is_none() || root != manifest_dir.canonicalize().ok() {
        return format!("{version}-dev+unknown");
    }
    // Match Git's tracked-file definition of dirty. Staging new files changes
    // the index, which refreshes this list on the next build.
    if let Some(files) = git(&["ls-files", "-z"]) {
        for file in files.split('\0').filter(|file| !file.is_empty()) {
            println!(
                "cargo:rerun-if-changed={}",
                manifest_dir.join(file).display()
            );
        }
    }
    for flag in ["--absolute-git-dir", "--git-common-dir"] {
        if let Some(dir) = git(&["rev-parse", "--path-format=absolute", flag]) {
            for name in ["HEAD", "index", "refs", "packed-refs"] {
                let path = std::path::Path::new(&dir).join(name);
                if path.exists() {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }
    }
    if let Some(commit) = git(&["rev-parse", "--short=7", "HEAD"]) {
        let clean = git(&["status", "--porcelain", "--untracked-files=no"])
            .is_some_and(|status| status.is_empty());
        let tagged = git(&["tag", "--points-at", "HEAD"])
            .is_some_and(|tags| tags.lines().any(|tag| tag == format!("v{version}")));
        if clean && tagged {
            version.to_owned()
        } else {
            format!(
                "{version}-dev+g{commit}{}",
                if clean { "" } else { ".dirty" }
            )
        }
    } else {
        format!("{version}-dev+unknown")
    }
}
