//! Shared option placement and explicit-output CLI contract.
pub mod support;
use support::cli::*;

#[test]
fn directory_works_before_after_and_inside_nested_commands() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("pools");
    let path = dir.to_str().unwrap();
    for args in [
        vec!["--dir", path, "pool", "create", "before"],
        vec!["pool", "--dir", path, "create", "inside"],
        vec!["pool", "create", "after", "--dir", path],
        vec!["--dir", path, "pool", "create", "repeat", "--dir", path],
    ] {
        let output = cmd().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for name in ["before", "inside", "after", "repeat"] {
        assert!(dir.join(format!("{name}.plasmite")).is_file());
    }
}

#[test]
fn conflicting_directories_are_rejected() {
    let output = cmd()
        .args(["--dir", "/tmp/one", "pool", "list", "--dir", "/tmp/two"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("conflicting --dir"));
}

#[test]
fn help_color_and_version_work_after_nested_commands() {
    for args in [
        vec!["pool", "list", "--color", "never", "--help"],
        vec!["pool", "list", "--color", "never", "-h"],
        vec!["pool", "list", "--version"],
    ] {
        let output = cmd().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty());
    }
}

#[test]
fn global_version_has_one_product_identity_at_every_position() {
    for args in [
        vec!["--version"],
        vec!["pool", "list", "--version"],
        vec!["access", "status", "--version"],
    ] {
        let output = cmd().args(args).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!("plasmite {}", env!("PLASMITE_BUILD_VERSION"))
        );
    }
}

#[test]
fn mcp_uses_shared_directory_and_rejects_remote_server_combination() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("pools");
    let path = dir.to_str().unwrap();
    for args in [vec!["--dir", path, "mcp"], vec!["mcp", "--dir", path]] {
        let output = cmd().args(args).stdin(Stdio::null()).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for args in [
        vec!["--dir", path, "mcp", "--remote", "https://localhost:9743"],
        vec!["mcp", "--remote", "https://localhost:9743", "--dir", path],
        vec!["--dir", path, "mcp", "https://localhost:9743"],
        vec!["mcp", "https://localhost:9743", "--dir", path],
    ] {
        let output = cmd().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("SERVER cannot be combined with --dir")
        );
    }
}

#[test]
fn redirected_version_and_errors_are_human_by_default() {
    let output = cmd().arg("version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("plasmite {}", env!("PLASMITE_BUILD_VERSION"))
    );
    let temp = tempfile::tempdir().unwrap();
    let output = cmd()
        .args([
            "fetch",
            "missing",
            "1",
            "--dir",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("error:"));
}

#[test]
fn structured_errors_disable_forced_color() {
    let temp = tempfile::tempdir().unwrap();
    let output = cmd()
        .args([
            "fetch",
            "missing",
            "1",
            "--json",
            "--color",
            "always",
            "--dir",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let _ = parse_error_json(&output.stderr);
    assert!(!output.stderr.contains(&0x1b));
}

#[cfg(unix)]
#[test]
fn tap_separator_preserves_child_global_looking_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let output = cmd()
        .args([
            "tap",
            "demo",
            "--create",
            "--dir",
            temp.path().to_str().unwrap(),
            "--",
            "printf",
            "%s\\n",
            "--dir",
            "--color",
            "--help",
            "--version",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "--dir\n--color\n--help\n--version\n--json\n"
    );
}
