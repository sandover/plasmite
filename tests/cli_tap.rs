//! Check that 2.0 uses feed for process output.

pub mod support;
use support::cli::cmd;

#[test]
fn tap_is_no_longer_a_command() {
    let output = cmd()
        .args(["tap", "build", "--", "echo", "hello"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
}
