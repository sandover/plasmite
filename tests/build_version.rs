#[path = "../build_version.rs"]
mod build_version;

use std::process::Command;

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_identity_tracks_tags_commits_and_local_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Version test"]);
    std::fs::write(path.join("source"), "one").unwrap();
    git(path, &["add", "source"]);
    git(path, &["commit", "-qm", "Initial source"]);
    let dev = build_version::build_identity(path, "0.8.0");
    assert!(dev.starts_with("0.8.0-dev+g"));
    assert!(!dev.ends_with(".dirty"));
    git(path, &["tag", "v0.8.0"]);
    assert_eq!(build_version::build_identity(path, "0.8.0"), "0.8.0");
    std::fs::write(path.join("source"), "two").unwrap();
    assert_eq!(
        build_version::build_identity(path, "0.8.0"),
        format!("{dev}.dirty")
    );
    git(path, &["add", "source"]);
    assert!(build_version::build_identity(path, "0.8.0").ends_with(".dirty"));
    git(path, &["commit", "-qm", "Change source"]);
    let next = build_version::build_identity(path, "0.8.0");
    assert!(next.starts_with("0.8.0-dev+g"));
    assert_ne!(next, dev);
    std::fs::write(path.join("new-file"), "new").unwrap();
    assert_eq!(build_version::build_identity(path, "0.8.0"), next);
    git(path, &["add", "new-file"]);
    assert_eq!(
        build_version::build_identity(path, "0.8.0"),
        format!("{next}.dirty")
    );

    // An extracted package must not borrow its containing repository's identity.
    let package = path.join("package");
    std::fs::create_dir(&package).unwrap();
    assert_eq!(
        build_version::build_identity(&package, "0.8.0"),
        "0.8.0-dev+unknown"
    );
    std::fs::write(package.join(".cargo_vcs_info.json"), "{}").unwrap();
    assert_eq!(build_version::build_identity(&package, "0.8.0"), "0.8.0");
}

#[test]
fn cli_version_surfaces_agree() {
    let binary = env!("CARGO_BIN_EXE_plasmite");
    let flag = Command::new(binary).arg("--version").output().unwrap();
    assert!(flag.status.success());
    assert_eq!(
        String::from_utf8(flag.stdout).unwrap().trim(),
        format!("plasmite {}", env!("PLASMITE_BUILD_VERSION"))
    );
    let command = Command::new(binary).arg("version").output().unwrap();
    assert!(command.status.success());
    let value: serde_json::Value = serde_json::from_slice(&command.stdout).unwrap();
    assert_eq!(value["version"], env!("PLASMITE_BUILD_VERSION"));
    assert_eq!(
        plasmite::mcp::ServerMetadata::default().version,
        env!("PLASMITE_BUILD_VERSION")
    );
}
