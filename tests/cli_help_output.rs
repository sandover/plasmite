//! Purpose: Help and output black-box CLI integration tests.

pub mod support;
use support::cli::*;

#[test]
fn top_level_help_orients_and_routes_readers() {
    let output = cmd().arg("--help").output().expect("help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("persistent, bounded stream"));
    assert!(stdout.contains("feed` appends"));
    assert!(stdout.contains("FIRST LOCAL WORKFLOW"));
    assert!(stdout.contains("plasmite pool create chat"));
    assert!(stdout.contains("Use --json on reports and message commands"));
    assert!(stdout.contains("Global options work before or after commands"));
    assert!(stdout.contains("plasmite <command> --help"));
    assert!(stdout.contains("/blob/main/docs/cli.md"));
    assert!(stdout.contains("/blob/main/docs/cookbook.md"));
    assert!(stdout.contains("pool create NAME... [--size SIZE]"));
    assert!(stdout.contains("access list"));
    assert!(stdout.contains("serve start|stop|restart|uninstall"));
    assert!(stdout.contains("serve logs [--follow]"));
    assert!(stdout.find("Send and read").unwrap() < stdout.find("Manage pools").unwrap());
    assert!(stdout.find("Manage pools").unwrap() < stdout.find("Share and connect").unwrap());
    for command in [
        "feed",
        "serve",
        "access",
        "mcp",
        "fetch",
        "follow",
        "tap",
        "duplex",
        "doctor",
        "version",
        "completion",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "root help omitted {command}"
        );
    }
}

#[test]
fn every_public_command_has_usable_help() {
    let cases: &[(&[&str], &str)] = &[
        (&["--help"], "plasmite [OPTIONS] <COMMAND>"),
        (&["pool", "--help"], "plasmite pool"),
        (&["pool", "create", "--help"], "plasmite pool create"),
        (&["pool", "info", "--help"], "plasmite pool info"),
        (&["pool", "delete", "--help"], "plasmite pool delete"),
        (&["pool", "list", "--help"], "plasmite pool list"),
        (&["feed", "--help"], "plasmite feed"),
        (&["serve", "--help"], "plasmite serve"),
        (&["serve", "install", "--help"], "plasmite serve install"),
        (&["serve", "start", "--help"], "plasmite serve start"),
        (&["serve", "stop", "--help"], "plasmite serve stop"),
        (&["serve", "restart", "--help"], "plasmite serve restart"),
        (
            &["serve", "uninstall", "--help"],
            "plasmite serve uninstall",
        ),
        (&["serve", "logs", "--help"], "plasmite serve logs"),
        (&["serve", "status", "--help"], "plasmite serve status"),
        (&["access", "--help"], "plasmite access"),
        (&["access", "invite", "--help"], "plasmite access invite"),
        (&["access", "connect", "--help"], "plasmite access connect"),
        (&["access", "list", "--help"], "plasmite access list"),
        (&["access", "status", "--help"], "plasmite access status"),
        (
            &["access", "disconnect", "--help"],
            "plasmite access disconnect",
        ),
        (&["access", "keys", "--help"], "plasmite access keys"),
        (&["access", "revoke", "--help"], "plasmite access revoke"),
        (&["mcp", "--help"], "plasmite mcp"),
        (&["fetch", "--help"], "plasmite fetch"),
        (&["follow", "--help"], "plasmite follow"),
        (&["tap", "--help"], "plasmite tap"),
        (&["duplex", "--help"], "plasmite duplex"),
        (&["doctor", "--help"], "plasmite doctor"),
        (&["version", "--help"], "plasmite version"),
        (&["completion", "--help"], "plasmite completion"),
        (&["help"], "plasmite [OPTIONS] <COMMAND>"),
    ];

    for (args, usage_path) in cases {
        let output = cmd().args(*args).output().expect("command help");
        assert!(output.status.success(), "{usage_path}");
        let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
        assert!(
            stdout.contains(usage_path),
            "help for {usage_path} did not contain its command path"
        );
    }
}

#[test]
fn short_help_exposes_material_command_constraints() {
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["pool", "create", "-h"],
            &["index slots", "at most half the pool"],
        ),
        (
            &["feed", "-h"],
            &[
                "requires --create",
                "requires --retry > 0",
                "Choose one input source",
                "exits 1",
            ],
        ),
        (
            &["follow", "-h"],
            &[
                "finite SPEED >= 0",
                "requires --tail or --since",
                "Remote refs reject --create",
                "Exit 124",
            ],
        ),
        (
            &["tap", "-h"],
            &[
                "<POOL> -- <COMMAND>...",
                "requires --create",
                "wrapped command's exit status",
            ],
        ),
        (
            &["duplex", "-h"],
            &[
                "Terminal input requires --me",
                "connect before using a remote ref",
                "Exits 124",
            ],
        ),
        (
            &["doctor", "-h"],
            &["<POOL|--all>", "Exits nonzero when corruption"],
        ),
        (
            &["serve", "-h"],
            &[
                "requires --tls-key",
                "must be positive",
                "remote clients use HTTPS",
            ],
        ),
    ];

    for (args, fragments) in cases {
        let output = cmd().args(*args).output().expect("short help");
        assert!(output.status.success(), "{args:?}");
        let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
        for fragment in *fragments {
            assert!(
                stdout.contains(fragment),
                "{args:?} help omitted {fragment:?}"
            );
        }
    }
}

#[test]
fn help_subcommand_is_enabled() {
    let output = cmd().arg("help").output().expect("help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("USAGE"));
    assert!(stdout.contains("COMMANDS"));
}

#[test]
fn help_pool_lists_pool_subcommands() {
    let output = cmd().args(["help", "pool"]).output().expect("help pool");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("plasmite pool"));
    assert!(stdout.contains("List local pools or a remote server directory"));
}

#[test]
fn version_help_describes_explicit_structured_output() {
    let output = cmd()
        .args(["version", "--help"])
        .output()
        .expect("version help");
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(stdout.contains("human-readable version information by default"));
    assert!(stdout.contains("--json"));
    assert!(stdout.contains("machine-readable JSON"));
}

#[test]
fn version_json_emits_machine_readable_json() {
    let output = cmd().args(["version", "--json"]).output().expect("version");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value = parse_json(std::str::from_utf8(&output.stdout).expect("utf8"));
    assert_eq!(value["name"], "plasmite");
    assert_eq!(value["version"], env!("PLASMITE_BUILD_VERSION"));
}

#[test]
fn not_found_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let get = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "fetch",
            "testpool",
            "999",
            "--json",
        ])
        .output()
        .expect("fetch");
    assert_eq!(get.status.code().unwrap(), 3);
    let err = parse_error_json(&get.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(
        inner.get("kind").and_then(|v| v.as_str()).unwrap(),
        "NotFound"
    );
    assert_eq!(inner.get("seq").and_then(|v| v.as_u64()).unwrap(), 999);
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("pool info") || hint.contains("follow"));
}

#[test]
fn usage_exit_code() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "testpool",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "testpool",
            "--json",
        ])
        .output()
        .expect("feed");
    assert_eq!(emit_out.status.code().unwrap(), 2);
    let err = parse_error_json(&emit_out.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--file") || hint.contains("pipe JSON"));
}

#[test]
fn color_always_does_not_color_structured_stdout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let info = cmd()
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "demo",
            "--json",
        ])
        .output()
        .expect("info");
    assert!(info.status.success());
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(!stdout.contains("\u{1b}["));
    let _ = parse_json(&stdout);
}

#[test]
fn color_never_does_not_emit_ansi() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let info = cmd()
        .args([
            "--color",
            "never",
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "info",
            "demo",
            "--json",
        ])
        .output()
        .expect("info");
    assert!(info.status.success());
    let stdout = String::from_utf8_lossy(&info.stdout);
    assert!(!stdout.contains("\u{1b}["));
}

#[test]
fn color_always_does_not_color_jsonl() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "pool",
            "create",
            "demo",
        ])
        .output()
        .expect("create");
    assert!(create.status.success());

    let emit_out = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "feed",
            "demo",
            "{\"x\":1}",
        ])
        .output()
        .expect("feed");
    assert!(emit_out.status.success());

    let mut follower = cmd()
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--tail",
            "1",
            "--format",
            "jsonl",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("follow jsonl");
    let stdout = follower.stdout.take().expect("stdout");
    let line = read_line_with_timeout(stdout, Duration::from_secs(2));
    assert!(!line.is_empty(), "expected a line from follow output");
    let line = line.trim_end();
    assert!(!line.contains("\u{1b}["));
    let _ = parse_json(line);
    let _ = follower.kill();
    let _ = follower.wait();
}

#[test]
fn errors_are_json_when_explicitly_requested() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let follower = cmd()
        .args([
            "--color",
            "always",
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "missing",
            "--json",
        ])
        .output()
        .expect("follow");
    assert_eq!(follower.status.code().unwrap(), 3);

    let err = parse_error_json(&follower.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(
        inner.get("kind").and_then(|v| v.as_str()).unwrap(),
        "NotFound"
    );
    assert!(
        !inner
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap()
            .is_empty()
    );
    assert!(
        inner
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap()
            .ends_with("missing.plasmite")
    );
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--create") || hint.contains("exact command"));
}

#[test]
fn clap_errors_are_concise_in_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let bad = cmd()
        .args([
            "--dir",
            pool_dir.to_str().unwrap(),
            "follow",
            "demo",
            "--json",
            "--definitely-not-a-flag",
        ])
        .output()
        .expect("follow");
    assert_eq!(bad.status.code().unwrap(), 2);

    let err = parse_error_json(&bad.stderr);
    let inner = err
        .get("error")
        .and_then(|v| v.as_object())
        .expect("error object");
    assert_eq!(inner.get("kind").and_then(|v| v.as_str()).unwrap(), "Usage");
    let message = inner.get("message").and_then(|v| v.as_str()).unwrap();
    assert!(!message.contains('\n'));
    assert!(!message.contains("Usage:"));
    let hint = inner.get("hint").and_then(|v| v.as_str()).unwrap_or("");
    assert!(hint.contains("--help"));
}

#[test]
fn misuse_feedback_matrix_is_actionable_across_command_families() {
    let cases: [(&[&str], &str, &str); 4] = [
        (
            &["feed", "demo", "{\"x\":1}", "--retry-delay", "1s"],
            "--retry-delay requires --retry",
            "Add --retry",
        ),
        (
            &["doctor", "demo", "--all"],
            "--all cannot be combined",
            "Use --all by itself",
        ),
        (&["completion", "nope"], "invalid value", "plasmite --help"),
        (
            &["version", "extra"],
            "unexpected argument",
            "version --help",
        ),
    ];

    for (args, expected_message_fragment, expected_hint_fragment) in cases {
        let output = cmd().args(args).output().expect("command");
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected_message_fragment), "{stderr}");
        assert!(stderr.contains(expected_hint_fragment), "{stderr}");
    }
}

#[test]
fn completion_bash_generates_valid_output() {
    let output = cmd()
        .args(["completion", "bash"])
        .output()
        .expect("completion bash");
    assert!(output.status.success(), "completion bash should succeed");
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(!stdout.is_empty(), "bash completion should produce output");
    assert!(
        stdout.contains("_plasmite"),
        "bash should define _plasmite function"
    );
    assert!(
        stdout.contains("pool"),
        "bash should include pool subcommand"
    );
    assert!(
        stdout.contains("feed"),
        "bash should include feed subcommand"
    );
    assert!(
        stdout.contains("follow"),
        "bash should include follow subcommand"
    );
}

#[test]
fn completion_zsh_generates_valid_output() {
    let output = cmd()
        .args(["completion", "zsh"])
        .output()
        .expect("completion zsh");
    assert!(output.status.success(), "completion zsh should succeed");
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(!stdout.is_empty(), "zsh completion should produce output");
    assert!(
        stdout.contains("#compdef") || stdout.contains("_plasmite"),
        "zsh should contain #compdef or _plasmite"
    );
    assert!(
        stdout.contains("pool"),
        "zsh should include pool subcommand"
    );
}

#[test]
fn completion_fish_generates_valid_output() {
    let output = cmd()
        .args(["completion", "fish"])
        .output()
        .expect("completion fish");
    assert!(output.status.success(), "completion fish should succeed");
    let stdout = std::str::from_utf8(&output.stdout).expect("utf8");
    assert!(!stdout.is_empty(), "fish completion should produce output");
    assert!(
        stdout.contains("complete"),
        "fish should use 'complete' command"
    );
    assert!(
        stdout.contains("plasmite"),
        "fish should reference plasmite"
    );
}

#[test]
fn completion_invalid_shell_fails() {
    let output = cmd()
        .args(["completion", "fake-shell"])
        .output()
        .expect("completion fake-shell");
    assert!(
        !output.status.success(),
        "completion with unsupported shell should fail"
    );
}
