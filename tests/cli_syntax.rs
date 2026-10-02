//! Purpose: Check the supported positional spellings and legacy flag aliases.

pub mod support;
use support::cli::*;

#[test]
fn serve_and_invite_accept_positional_values() {
    let cases: &[&[&str]] = &[
        &["serve", "https://pools.example.com:9743", "--help"],
        &[
            "serve",
            "install",
            "https://pools.example.com:9743",
            "--help",
        ],
        &["access", "invite", "laptop", "--help"],
    ];

    for args in cases {
        let output = cmd().args(*args).output().expect("help");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn legacy_server_and_invite_flags_remain_accepted() {
    let cases: &[&[&str]] = &[
        &[
            "serve",
            "--shared-address",
            "https://pools.example.com:9743",
            "--help",
        ],
        &["access", "invite", "--name", "laptop", "--help"],
    ];

    for args in cases {
        let output = cmd().args(*args).output().expect("help");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn positional_and_legacy_destination_inputs_cannot_be_combined() {
    let cases: &[&[&str]] = &[
        &[
            "serve",
            "https://one.example.com:9743",
            "--shared-address",
            "https://two.example.com:9743",
        ],
        &[
            "serve",
            "install",
            "https://one.example.com:9743",
            "--shared-address",
            "https://two.example.com:9743",
        ],
        &["access", "invite", "one", "--name", "two"],
    ];

    for args in cases {
        let output = cmd().args(*args).output().expect("command");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn serve_lifecycle_does_not_ignore_foreground_or_global_arguments() {
    let rejected = [
        &["serve", "--bind", "not-an-address", "start", "--json"][..],
        &[
            "serve",
            "https://pools.example.com:9743",
            "install",
            "--help",
        ][..],
    ];
    for args in rejected {
        let output = cmd().args(args).output().expect("parse command");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let accepted = [
        &["serve", "install", "--bind", "127.0.0.1:9700", "--help"][..],
        &[
            "--dir",
            "/tmp/plasmite-cli-syntax",
            "serve",
            "status",
            "--help",
        ][..],
        &[
            "serve",
            "status",
            "--help",
            "--dir",
            "/tmp/plasmite-cli-syntax",
        ][..],
    ];
    for args in accepted {
        let output = cmd().args(args).output().expect("help");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
