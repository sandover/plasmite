//! Purpose: `plasmite` CLI entry point and v0.0.1 command dispatch.
//! Role: Binary crate root; parses args, runs commands, emits JSON on stdout.
//! Invariants: Commands emit stable stdout formats (human or JSON by command/flags).
//! Invariants: Errors are readable text unless structured output is explicitly requested.
//! Invariants: Process exit code is derived from the shared interface error policy.
//! Invariants: All pool mutations go through `api::Pool` (locks + mmap safety).
#![allow(clippy::result_large_err)]
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, error::ErrorKind as ClapErrorKind};

mod access_store;
mod cli;
mod color_json;
mod ingest;
mod interface_wire;
mod jq_filter;
mod mcp_stdio;
mod pool_info_json;
mod pool_paths;
mod secure_serve;
mod serve;
mod serve_registry;
mod serve_service;
mod since;
#[cfg(windows)]
mod windows_private;

use cli::args::{
    AccessSubcommand, Cli, ColorMode, ErrorPolicyCli, FollowFormat, InputMode, PoolCommand,
    ServeRunArgs,
};
use cli::{CliContext, CommandResult as RunOutcome};
use interface_wire::error_policy;
use plasmite::api::{Error, ErrorKind};
use pool_paths::default_pool_dir;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PoolTarget {
    LocalPath(PathBuf),
    Remote { base_url: String, pool: String },
}

fn main() {
    let exit_code = match run() {
        Ok(outcome) => outcome.exit_code,
        Err((err, color_mode, json_output)) => {
            emit_error(&err, color_mode, json_output);
            error_policy(interface_error_kind(err.kind())).cli_exit_code
        }
    };
    std::process::exit(exit_code);
}

fn run() -> Result<RunOutcome, (Error, ColorMode, bool)> {
    let args = normalize_args(std::env::args_os());
    let explicit_json = requests_json(&args);
    let conflicting_dirs = has_conflicting_directories(&args);
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => match err.kind() {
            ClapErrorKind::DisplayHelp
            | ClapErrorKind::DisplayVersion
            | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                let written = if matches!(err.kind(), ClapErrorKind::DisplayVersion) {
                    use std::io::Write;
                    writeln!(
                        std::io::stdout(),
                        "plasmite {}",
                        env!("PLASMITE_BUILD_VERSION")
                    )
                } else {
                    err.print()
                };
                written.map_err(|io_err| {
                    (
                        Error::new(ErrorKind::Io)
                            .with_message("failed to write CLI output")
                            .with_source(io_err),
                        ColorMode::Auto,
                        explicit_json,
                    )
                })?;
                let exit_code = if matches!(
                    err.kind(),
                    ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                ) {
                    2
                } else {
                    0
                };
                return Ok(RunOutcome::with_code(exit_code));
            }
            _ => {
                let message = clap_error_summary(&err);
                let hint = clap_error_hint(&err);
                return Err((
                    Error::new(ErrorKind::Usage)
                        .with_message(message)
                        .with_hint(hint),
                    ColorMode::Auto,
                    explicit_json,
                ));
            }
        },
    };

    let color_mode = cli.color;
    let json_output = cli.command.json_output();
    if conflicting_dirs {
        return Err((
            Error::new(ErrorKind::Usage)
                .with_message("conflicting --dir values select different pool directories")
                .with_hint("Use one --dir value for the whole invocation."),
            color_mode,
            json_output,
        ));
    }
    let pool_dir = cli.dir.into_iter().next().unwrap_or_else(default_pool_dir);
    let result = cli::dispatch(
        cli.command,
        CliContext::new(pool_dir, color_mode, json_output),
    );

    result
        .map_err(add_corrupt_hint)
        .map_err(add_io_hint)
        .map_err(add_internal_hint)
        .map_err(|err| (err, color_mode, json_output))
}

// Clap propagates the last scope's global argument values. Check every
// explicit directory first so a nested --dir cannot silently replace another.
fn has_conflicting_directories(args: &[OsString]) -> bool {
    let mut first = None;
    let mut args = args.iter().skip(1).take_while(|arg| *arg != "--");
    while let Some(arg) = args.next() {
        let value = if arg == "--dir" {
            args.next().map(|value| value.as_encoded_bytes())
        } else {
            arg.as_encoded_bytes().strip_prefix(b"--dir=")
        };
        if let Some(value) = value {
            if first.is_some_and(|first| first != value) {
                return true;
            }
            first = Some(value);
        }
    }
    false
}

// The parser may fail before it can produce a command. Respect an explicit
// output request, but never interpret flags belonging to tap's child process.
fn requests_json(args: &[OsString]) -> bool {
    let mut format_value = false;
    for arg in args.iter().skip(1).take_while(|arg| *arg != "--") {
        if format_value && arg == "jsonl" {
            return true;
        }
        if arg == "--json" || arg == "--jsonl" || arg == "--format=jsonl" {
            return true;
        }
        format_value = arg == "--format";
    }
    false
}

fn normalize_args<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut child_args = false;
    args.into_iter()
        .map(|arg| {
            if arg == "--" {
                child_args = true;
            }
            if child_args {
                return arg;
            }
            let replacement = arg.to_str().and_then(|value| match value {
                "---help" => Some("--help"),
                "---version" => Some("--version"),
                _ => None,
            });
            replacement.map(OsString::from).unwrap_or_else(|| arg)
        })
        .collect()
}

use cli::output_support::{clap_error_hint, clap_error_summary, emit_error, interface_error_kind};
use cli::support::{add_corrupt_hint, add_internal_hint, add_io_hint};

#[cfg(test)]
mod tests {
    use super::{Cli, Error, ErrorKind, PoolTarget, error_policy, interface_error_kind};
    use crate::cli::output_support::{error_json, error_text, render_table, short_display_path};
    use crate::cli::output_support::{
        format_bytes, format_relative_time, format_seq_range, format_timestamp_human,
    };
    use crate::cli::stream_support::{
        duplex_requires_me_when_tty, matches_required_tags, parse_duplex_tty_line,
    };
    use crate::cli::support::{
        RetryConfig, parse_duration, parse_size, resolve_pool_target, retry_append,
    };
    use clap::CommandFactory;
    use serde_json::json;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn directory_conflicts_include_all_scopes_and_equal_spellings() {
        use std::ffi::OsString;
        let conflict = ["plasmite", "--dir", "/one", "pool", "list", "--dir=/two"];
        let equal = ["plasmite", "--dir", "/one", "pool", "list", "--dir=/one"];
        let child = [
            "plasmite",
            "--dir",
            "/one",
            "tap",
            "demo",
            "--",
            "echo",
            "--dir=/two",
        ];
        for (args, expected) in [
            (conflict.as_slice(), true),
            (equal.as_slice(), false),
            (child.as_slice(), false),
        ] {
            let args = args.iter().map(OsString::from).collect::<Vec<_>>();
            assert_eq!(super::has_conflicting_directories(&args), expected);
        }
    }

    #[test]
    fn argument_normalization_preserves_child_arguments() {
        use std::ffi::OsString;
        let args = ["plasmite", "---help", "--", "---version", "--json"];
        let args = super::normalize_args(args.into_iter().map(OsString::from));
        assert_eq!(
            args,
            ["plasmite", "--help", "--", "---version", "--json"].map(OsString::from)
        );
        assert!(!super::requests_json(&args));
    }

    #[test]
    fn cli_command_inventory() {
        Cli::command().debug_assert();
        let mut command = Cli::command();
        command.build();
        let mut inventory = Vec::new();
        collect_cli_inventory(&command, "plasmite", &mut inventory);

        let expected = vec![
            ("plasmite", vec!["color", "dir", "help", "version"]),
            ("plasmite access", vec!["help"]),
            ("plasmite access connect", vec!["help", "json"]),
            ("plasmite access disconnect", vec!["help", "json"]),
            ("plasmite access invite", vec!["help", "json", "name"]),
            ("plasmite access keys", vec!["help", "json"]),
            ("plasmite access list", vec!["help", "json"]),
            ("plasmite access revoke", vec!["help", "json"]),
            ("plasmite access status", vec!["help", "json"]),
            ("plasmite access untrust", vec!["help", "json"]),
            ("plasmite completion", vec!["help"]),
            ("plasmite doctor", vec!["all", "help", "json"]),
            (
                "plasmite duplex",
                vec![
                    "create",
                    "echo-self",
                    "format",
                    "help",
                    "json",
                    "jsonl",
                    "me",
                    "since",
                    "tail",
                    "timeout",
                ],
            ),
            (
                "plasmite feed",
                vec![
                    "create",
                    "create-size",
                    "durability",
                    "errors",
                    "file",
                    "help",
                    "in",
                    "json",
                    "retry",
                    "retry-delay",
                    "tag",
                ],
            ),
            ("plasmite fetch", vec!["help", "json"]),
            (
                "plasmite follow",
                vec![
                    "create",
                    "data-only",
                    "format",
                    "help",
                    "json",
                    "jsonl",
                    "no-follow",
                    "no-notify",
                    "one",
                    "quiet-drops",
                    "replay",
                    "since",
                    "tag",
                    "tail",
                    "timeout",
                    "where",
                ],
            ),
            ("plasmite help", vec![]),
            ("plasmite mcp", vec!["help"]),
            ("plasmite pool", vec!["help"]),
            (
                "plasmite pool create",
                vec!["help", "index-capacity", "json", "size"],
            ),
            ("plasmite pool delete", vec!["help", "json"]),
            ("plasmite pool info", vec!["help", "json"]),
            ("plasmite pool list", vec!["help", "json"]),
            (
                "plasmite serve",
                vec![
                    "bind",
                    "front-cert",
                    "help",
                    "max-body-bytes",
                    "max-tail-concurrency",
                    "max-tail-timeout-ms",
                    "remote-bind",
                    "shared-address",
                    "tls-cert",
                    "tls-key",
                ],
            ),
            (
                "plasmite serve install",
                vec![
                    "bind",
                    "front-cert",
                    "help",
                    "json",
                    "max-body-bytes",
                    "max-tail-concurrency",
                    "max-tail-timeout-ms",
                    "remote-bind",
                    "shared-address",
                    "tls-cert",
                    "tls-key",
                ],
            ),
            (
                "plasmite serve logs",
                vec!["follow", "help", "json", "tail"],
            ),
            ("plasmite serve restart", vec!["help", "json"]),
            ("plasmite serve start", vec!["help", "json"]),
            ("plasmite serve status", vec!["all", "help", "json"]),
            ("plasmite serve stop", vec!["help", "json"]),
            ("plasmite serve uninstall", vec!["help", "json"]),
            (
                "plasmite tap",
                vec![
                    "create",
                    "create-size",
                    "durability",
                    "help",
                    "quiet",
                    "tag",
                ],
            ),
            ("plasmite version", vec!["help", "json"]),
        ]
        .into_iter()
        .map(|(path, options)| {
            (
                path.to_string(),
                options.into_iter().map(str::to_string).collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
        inventory.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(inventory, expected);
    }

    #[test]
    fn cli_error_presenter_preserves_all_stable_kinds_and_exit_codes() {
        let cases = [
            (ErrorKind::Internal, "Internal", 1),
            (ErrorKind::Usage, "Usage", 2),
            (ErrorKind::NotFound, "NotFound", 3),
            (ErrorKind::AlreadyExists, "AlreadyExists", 4),
            (ErrorKind::Busy, "Busy", 5),
            (ErrorKind::Permission, "Permission", 6),
            (ErrorKind::Corrupt, "Corrupt", 7),
            (ErrorKind::Io, "Io", 8),
            (ErrorKind::RetentionGap, "RetentionGap", 9),
        ];

        for (kind, name, exit_code) in cases {
            let value = error_json(&Error::new(kind));
            assert_eq!(value["error"]["kind"], json!(name));
            assert_eq!(
                error_policy(interface_error_kind(kind)).cli_exit_code,
                exit_code
            );
        }
    }

    fn collect_cli_inventory(
        command: &clap::Command,
        path: &str,
        inventory: &mut Vec<(String, Vec<String>)>,
    ) {
        for global in ["dir", "color", "help", "version"] {
            assert!(
                command
                    .get_arguments()
                    .any(|arg| arg.get_long() == Some(global)),
                "{path} omitted shared --{global}"
            );
        }
        let mut options = command
            .get_arguments()
            .filter(|arg| !arg.is_hide_set())
            .filter(|arg| {
                path == "plasmite" || !matches!(arg.get_long(), Some("dir" | "color" | "version"))
            })
            .filter_map(|arg| arg.get_long())
            .map(str::to_string)
            .collect::<Vec<_>>();
        options.sort_unstable();
        inventory.push((path.to_string(), options));

        let mut subcommands = command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .collect::<Vec<_>>();
        subcommands.sort_by_key(|subcommand| subcommand.get_name());
        for subcommand in subcommands {
            if subcommand.get_name() == "help" {
                if path == "plasmite" {
                    inventory.push(("plasmite help".to_string(), Vec::new()));
                }
                continue;
            }
            collect_cli_inventory(
                subcommand,
                &format!("{path} {}", subcommand.get_name()),
                inventory,
            );
        }
    }

    fn read_json_stream<R, F>(reader: R, mut on_value: F) -> Result<usize, Error>
    where
        R: std::io::Read,
        F: FnMut(serde_json::Value) -> Result<(), Error>,
    {
        let stream = serde_json::Deserializer::from_reader(reader).into_iter::<serde_json::Value>();
        let mut count = 0usize;
        for item in stream {
            let value = item.map_err(|err| {
                Error::new(ErrorKind::Usage)
                    .with_message("invalid json stream")
                    .with_hint("Provide JSON values separated by whitespace or newlines.")
                    .with_source(err)
            })?;
            on_value(value)?;
            count += 1;
        }
        Ok(count)
    }

    #[test]
    fn parse_size_accepts_bytes_and_kmg() {
        assert_eq!(parse_size("42").unwrap(), 42);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("2k").unwrap(), 2048);
        assert_eq!(parse_size("3M").unwrap(), 3 * 1024 * 1024);
        assert_eq!(parse_size("4g").unwrap(), 4 * 1024 * 1024 * 1024);
    }

    #[test]
    fn format_bytes_boundaries() {
        assert_eq!(format_bytes(0), "0");
        assert_eq!(format_bytes(1023), "1023");
        assert_eq!(format_bytes(1024), "1K");
        assert_eq!(format_bytes(1536), "1.5K");
        assert_eq!(format_bytes(1024 * 1024), "1M");
    }

    #[test]
    fn format_timestamp_human_truncates_to_seconds() {
        assert_eq!(
            format_timestamp_human("2026-02-27T12:34:56.789Z"),
            "2026-02-27T12:34:56Z"
        );
        assert_eq!(format_timestamp_human(""), "-");
    }

    #[test]
    fn format_relative_time_boundaries() {
        assert_eq!(format_relative_time(Some(0)), "1s ago");
        assert_eq!(format_relative_time(Some(59_000)), "59s ago");
        assert_eq!(format_relative_time(Some(60_000)), "1m ago");
        assert_eq!(format_relative_time(Some(3_600_000)), "1h ago");
        assert_eq!(format_relative_time(Some(86_400_000)), "1d ago");
        assert_eq!(format_relative_time(Some(7 * 86_400_000)), "1w ago");
        assert_eq!(format_relative_time(None), "-");
    }

    #[test]
    fn format_seq_range_handles_empty_and_present_bounds() {
        assert_eq!(format_seq_range(None, None), "-");
        assert_eq!(format_seq_range(Some(3), Some(5)), "seq 3..5");
    }

    #[test]
    fn parse_size_rejects_iec_suffixes() {
        assert!(parse_size("1MiB").is_err());
        assert!(parse_size("2Gi").is_err());
        assert!(parse_size("3KiB").is_err());
    }

    #[test]
    fn parse_duration_accepts_ms_s_m() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn duplex_requires_me_when_tty_inputs() {
        assert!(duplex_requires_me_when_tty(true, None));
        assert!(!duplex_requires_me_when_tty(false, None));
        assert!(!duplex_requires_me_when_tty(true, Some("alice")));
        assert!(!duplex_requires_me_when_tty(false, Some("alice")));
    }

    #[test]
    fn parse_duplex_tty_line_supports_text_and_crlf() {
        let value = parse_duplex_tty_line("alice", "hello world\r\n").expect("value");
        assert_eq!(value.get("from").and_then(|v| v.as_str()), Some("alice"));
        assert_eq!(
            value.get("msg").and_then(|v| v.as_str()),
            Some("hello world")
        );

        assert!(parse_duplex_tty_line("alice", "\n").is_none());
        assert!(parse_duplex_tty_line("alice", "   \r\n").is_none());
    }

    #[test]
    fn read_json_stream_accepts_multiple_values() {
        let input = b"{\"a\":1}\n {\"b\":2} {\"c\":3}";
        let mut values = Vec::new();
        let count = read_json_stream(Cursor::new(input), |value| {
            values.push(value);
            Ok(())
        })
        .expect("stream parse");
        assert_eq!(count, 3);
        assert_eq!(values, vec![json!({"a":1}), json!({"b":2}), json!({"c":3})]);
    }

    #[test]
    fn required_tags_match_all_requested_tags() {
        let message = json!({
            "meta": {
                "tags": ["error", "billing", "prod"]
            }
        });
        assert!(matches_required_tags(
            &["error".to_string(), "billing".to_string()],
            &message
        ));
        assert!(!matches_required_tags(
            &["error".to_string(), "missing".to_string()],
            &message
        ));
    }

    #[test]
    fn required_tags_returns_false_on_missing_meta_tags() {
        let message = json!({"data": {"x": 1}});
        assert!(!matches_required_tags(&["error".to_string()], &message));
    }

    #[test]
    fn error_text_respects_color_flag() {
        let err = Error::new(ErrorKind::Usage).with_message("bad input");
        let colored = error_text(&err, true);
        let plain = error_text(&err, false);
        assert!(colored.contains("\u{1b}[31merror:\u{1b}[0m"));
        assert!(plain.contains("error:"));
        assert!(!plain.contains("\u{1b}["));
    }

    #[test]
    fn resolve_pool_target_classifies_local_name() {
        let pool_dir = Path::new("/tmp/pools");
        let target = resolve_pool_target("demo", pool_dir).expect("target");
        match target {
            PoolTarget::LocalPath(path) => assert_eq!(path, pool_dir.join("demo.plasmite")),
            _ => panic!("expected local path"),
        }
    }

    #[test]
    fn resolve_pool_target_accepts_remote_shorthand() {
        let target = resolve_pool_target("http://localhost:9170/demo", Path::new("/tmp/pools"))
            .expect("target");
        assert_eq!(
            target,
            PoolTarget::Remote {
                base_url: "http://localhost:9170/".to_string(),
                pool: "demo".to_string(),
            }
        );
    }

    #[test]
    fn resolve_pool_target_rejects_api_shaped_remote_ref() {
        let err = resolve_pool_target(
            "http://localhost:9170/v0/pools/demo/append",
            Path::new("/tmp/pools"),
        )
        .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn resolve_pool_target_rejects_trailing_slash_remote_ref() {
        let err = resolve_pool_target("http://localhost:9170/demo/", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn resolve_pool_target_rejects_unsupported_scheme() {
        let err = resolve_pool_target("tcp://localhost:9170/demo", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[test]
    fn resolve_pool_target_rejects_query_and_fragment() {
        let err = resolve_pool_target("http://localhost:9170/demo?x=1", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
        let err = resolve_pool_target("http://localhost:9170/demo#frag", Path::new("/tmp/pools"))
            .expect_err("err");
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[cfg(windows)]
    #[test]
    fn resolve_pool_target_treats_windows_backslash_path_as_local_path() {
        let input = r"C:\pools\demo.plasmite";
        let target = resolve_pool_target(input, Path::new("C:\\ignored")).expect("target");
        match target {
            PoolTarget::LocalPath(path) => assert_eq!(path, PathBuf::from(input)),
            _ => panic!("expected local path"),
        }
    }

    #[test]
    fn retry_append_retries_until_success() {
        let mut attempts = 0u32;
        let value = retry_append(
            Some(RetryConfig {
                retries: 2,
                delay: Duration::from_millis(0),
            }),
            || {
                attempts += 1;
                if attempts < 2 {
                    return Err(Error::new(ErrorKind::Busy));
                }
                Ok(21u8)
            },
        )
        .expect("retry should succeed");
        assert_eq!(attempts, 2);
        assert_eq!(value, 21u8);
    }

    #[test]
    fn retry_append_exhausts_when_still_retryable() {
        let mut attempts = 0u32;
        let result: Result<u8, Error> = retry_append(
            Some(RetryConfig {
                retries: 1,
                delay: Duration::from_millis(0),
            }),
            || {
                attempts += 1;
                Err(Error::new(ErrorKind::Busy))
            },
        );
        assert_eq!(attempts, 2);
        let err = result.expect_err("expected retry exhaustion");
        assert_eq!(err.kind(), ErrorKind::Busy);
        let hint = err.hint().unwrap_or("");
        assert!(hint.contains("Retry attempts: 2"));
    }

    #[test]
    fn render_table_aligns_and_sanitizes_cells() {
        let output = render_table(
            &["NAME", "DETAIL"],
            &[
                vec!["a".to_string(), "line1\nline2".to_string()],
                vec!["long-name".to_string(), "ok".to_string()],
            ],
        );
        let lines = output.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("NAME"));
        assert!(lines[0].contains("  DETAIL"));
        assert!(lines[1].contains("line1\\nline2"));
        assert!(lines[2].contains("long-name"));
        assert!(!lines[1].ends_with(' '));
    }

    #[test]
    fn short_display_path_prefers_relative_to_base_dir() {
        let path = PathBuf::from("/tmp/pools/demo.plasmite");
        let base = Path::new("/tmp/pools");
        assert_eq!(
            short_display_path(path.as_path(), Some(base)),
            "demo.plasmite".to_string()
        );
    }

    #[test]
    fn short_display_path_falls_back_to_basename() {
        let path = PathBuf::from("/tmp/pools/demo.plasmite");
        let other_base = Path::new("/different");
        assert_eq!(
            short_display_path(path.as_path(), Some(other_base)),
            "demo.plasmite".to_string()
        );
        assert_eq!(
            short_display_path(path.as_path(), None),
            "demo.plasmite".to_string()
        );
    }
}
