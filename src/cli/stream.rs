//! Purpose: Execute follow and duplex streaming commands.
//! Exports: `FollowArgs`, `DuplexArgs`, `follow`, `duplex`.
//! Role: Keep shared streaming configuration, filtering, and cancellation together.

use super::context::CliContext;
use super::feed_support::{
    FeedIngestContext, RemoteFeedIngestContext, ingest_from_stdin, ingest_from_stdin_remote,
    missing_feed_data_error,
};
use super::output_support::emit_follow_timeout_human;
use super::result::CommandResult;
use super::stream_support::{
    FollowConfig, duplex_requires_me_when_tty, follow_pool, follow_remote, follow_should_stop,
    parse_duplex_tty_line,
};
use super::support::{
    DEFAULT_POOL_SIZE, add_missing_pool_create_hint, ensure_pool_dir,
    follow_exact_create_command_hint, now_ns, parse_duration, parse_since, remote_client,
    resolve_pool_target, retry_with_config,
};
use crate::jq_filter::compile_filters;
use crate::{ErrorPolicyCli, FollowFormat, InputMode, PoolTarget};
use plasmite::api::{
    AppendOptions, Durability, Error, ErrorKind, Pool, PoolOptions, PoolRef, lite3,
};
use std::io::{self, BufRead, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

pub(super) struct FollowArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) tail: u64,
    pub(super) one: bool,
    pub(super) no_follow: bool,
    pub(super) jsonl: bool,
    pub(super) timeout: Option<String>,
    pub(super) data_only: bool,
    pub(super) format: Option<FollowFormat>,
    pub(super) since: Option<String>,
    pub(super) where_expr: Vec<String>,
    pub(super) tags: Vec<String>,
    pub(super) quiet_drops: bool,
    pub(super) no_notify: bool,
    pub(super) replay: Option<f64>,
}

pub(super) struct DuplexArgs {
    pub(super) pool: String,
    pub(super) me: Option<String>,
    pub(super) create: bool,
    pub(super) tail: u64,
    pub(super) jsonl: bool,
    pub(super) timeout: Option<String>,
    pub(super) format: Option<FollowFormat>,
    pub(super) since: Option<String>,
    pub(super) echo_self: bool,
}

pub(super) fn follow(args: FollowArgs, context: &CliContext) -> Result<CommandResult, Error> {
    if args.jsonl && args.format.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("conflicting output options")
            .with_hint("Use --format jsonl (or --jsonl), but not both."));
    }
    if context.json_output() && matches!(args.format, Some(FollowFormat::Pretty)) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--json conflicts with --format pretty")
            .with_hint("Use --json for JSONL, or omit it for human-readable output."));
    }
    let format_flag = args.format;
    let format = args
        .format
        .unwrap_or(if args.jsonl || context.json_output() {
            FollowFormat::Jsonl
        } else {
            FollowFormat::Pretty
        });
    let pretty = matches!(format, FollowFormat::Pretty);
    let since_ns = args
        .since
        .as_deref()
        .map(|value| parse_since(value, now_ns()?))
        .transpose()?;
    let timeout_input = args.timeout.as_deref();
    let timeout = timeout_input.map(parse_duration).transpose()?;
    let mut exact_follow_create_hint = follow_exact_create_command_hint(
        &args.pool,
        args.tail,
        args.one,
        args.jsonl,
        timeout_input,
        args.data_only,
        format_flag,
        args.since.as_deref(),
        &args.where_expr,
        &args.tags,
        args.quiet_drops,
        args.no_notify,
        args.replay,
    );
    if args.no_follow {
        exact_follow_create_hint.push_str(" --no-follow");
    }
    if context.json_output() && !args.jsonl && format_flag.is_none() {
        exact_follow_create_hint.push_str(" --json");
    }
    if args.no_follow && args.tail == 0 && args.since.is_none() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--no-follow requires --tail or --since")
            .with_hint("Select retained history with --tail N or --since TIME."));
    }
    let cfg = FollowConfig {
        tail: args.tail,
        pretty,
        one: args.one,
        timeout,
        data_only: args.data_only,
        since_ns,
        no_follow: args.no_follow || args.replay.is_some(),
        required_tags: args.tags,
        where_predicates: compile_filters(&args.where_expr)?,
        quiet_drops: args.quiet_drops,
        notify: !args.no_notify,
        color_mode: if pretty {
            context.color_mode()
        } else {
            crate::ColorMode::Never
        },
        replay_speed: args.replay,
        suppress_sender: None,
        stop: None,
    };
    let target = resolve_pool_target(&args.pool, context.pool_dir())?;
    match target {
        PoolTarget::LocalPath(path) => {
            if let Some(speed) = args.replay {
                if speed < 0.0 {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--replay speed must be non-negative")
                        .with_hint("Use --replay 1 for realtime, --replay 2 for 2x, --replay 0 for no delay."));
                }
                if !speed.is_finite() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--replay speed must be a finite number")
                        .with_hint("Use --replay 1 for realtime, --replay 2 for 2x, --replay 0 for no delay."));
                }
                if args.tail == 0 && args.since.is_none() {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--replay requires --tail or --since")
                        .with_hint(
                            "Replay needs historical messages. Use --tail N or --since DURATION.",
                        ));
                }
            }
            let pool_handle = match Pool::open(&path) {
                Ok(pool_handle) => pool_handle,
                Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(context.pool_dir())?;
                    Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE))?
                }
                Err(err) => {
                    return Err(add_missing_pool_create_hint(
                        err,
                        "follow",
                        &args.pool,
                        &args.pool,
                        Some(exact_follow_create_hint),
                    ));
                }
            };
            let outcome = follow_pool(&pool_handle, &args.pool, &path, cfg)?;
            if outcome.exit_code == 124 && pretty {
                if let Some(timeout_input) = timeout_input {
                    emit_follow_timeout_human(timeout_input);
                }
            }
            Ok(outcome)
        }
        PoolTarget::Remote { base_url, pool } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("remote follow does not support --create")
                    .with_hint(
                        "Create remote pools with server-side tooling, then rerun follow.",
                    ));
            }
            let client = remote_client(base_url)?;
            let outcome = follow_remote(&client, &pool, &cfg)?;
            if outcome.exit_code == 124 && pretty {
                if let Some(timeout_input) = timeout_input {
                    emit_follow_timeout_human(timeout_input);
                }
            }
            Ok(outcome)
        }
    }
}

pub(super) fn duplex(args: DuplexArgs, context: &CliContext) -> Result<CommandResult, Error> {
    if args.jsonl && args.format.is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("conflicting output options")
            .with_hint("Use --format jsonl (or --jsonl), but not both."));
    }
    let stdin_is_terminal = io::stdin().is_terminal();
    if duplex_requires_me_when_tty(stdin_is_terminal, args.me.as_deref()) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("TTY input requires --me for duplex")
            .with_hint("Provide --me NAME to send TTY line-mode messages."));
    }
    if context.json_output() && matches!(args.format, Some(FollowFormat::Pretty)) {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--json conflicts with --format pretty")
            .with_hint("Use --json for JSONL, or omit it for human-readable output."));
    }
    let format_flag = args.format;
    let format = args
        .format
        .unwrap_or(if args.jsonl || context.json_output() {
            FollowFormat::Jsonl
        } else {
            FollowFormat::Pretty
        });
    let pretty = matches!(format, FollowFormat::Pretty);
    let since_ns = args
        .since
        .as_deref()
        .map(|value| parse_since(value, now_ns()?))
        .transpose()?;
    let timeout_input = args.timeout.as_deref();
    let timeout = timeout_input.map(parse_duration).transpose()?;
    let mut exact_follow_create_hint = follow_exact_create_command_hint(
        &args.pool,
        args.tail,
        false,
        args.jsonl,
        timeout_input,
        false,
        format_flag,
        args.since.as_deref(),
        &[],
        &[],
        false,
        false,
        None,
    );
    if context.json_output() && !args.jsonl && format_flag.is_none() {
        exact_follow_create_hint.push_str(" --json");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = FollowConfig {
        tail: args.tail,
        pretty,
        one: false,
        timeout,
        data_only: false,
        since_ns,
        no_follow: false,
        required_tags: Vec::new(),
        where_predicates: compile_filters(&[])?,
        quiet_drops: false,
        notify: true,
        color_mode: if pretty {
            context.color_mode()
        } else {
            crate::ColorMode::Never
        },
        replay_speed: None,
        suppress_sender: if args.echo_self {
            None
        } else {
            args.me.clone()
        },
        stop: Some(stop.clone()),
    };

    #[derive(Clone, Copy)]
    enum DuplexSide {
        Follow,
        Send,
    }

    let (event_tx, event_rx) = mpsc::channel::<(DuplexSide, Result<CommandResult, Error>)>();
    let target = resolve_pool_target(&args.pool, context.pool_dir())?;
    match target {
        PoolTarget::LocalPath(path) => {
            let follow_pool_handle = match Pool::open(&path) {
                Ok(pool_handle) => pool_handle,
                Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(context.pool_dir())?;
                    Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE))?
                }
                Err(err) => {
                    return Err(add_missing_pool_create_hint(
                        err,
                        "duplex",
                        &args.pool,
                        &args.pool,
                        Some(exact_follow_create_hint),
                    ));
                }
            };
            let mut send_pool = Pool::open(&path)?;
            let follow_tx = event_tx.clone();
            let follow_cfg = cfg.clone();
            let stop_for_follow = stop.clone();
            let pool_name = args.pool.clone();
            let follow_path = path.clone();
            let _ = std::thread::spawn(move || {
                let outcome =
                    follow_pool(&follow_pool_handle, &pool_name, &follow_path, follow_cfg);
                if outcome.is_err() {
                    stop_for_follow.store(true, Ordering::Release);
                }
                let _ = follow_tx.send((DuplexSide::Follow, outcome));
            });

            let send_tx = event_tx;
            let stop_for_send = stop.clone();
            let me_for_send = args.me.clone();
            let pool_ref = args.pool.clone();
            let color_mode = context.color_mode();
            let context_json_output = !pretty;
            let _ = std::thread::spawn(move || {
                if stdin_is_terminal {
                    let outcome = send_tty_lines(
                        me_for_send.as_deref().expect("me required"),
                        &stop_for_send,
                        |value| {
                            let payload = match lite3::encode_message(&Vec::<String>::new(), &value)
                            {
                                Ok(payload) => payload,
                                Err(err) => return Err(err),
                            };
                            retry_with_config(None, || {
                                let timestamp_ns = now_ns()?;
                                let options = AppendOptions::new(timestamp_ns, Durability::Fast);
                                send_pool
                                    .append_with_options(payload.as_slice(), options)
                                    .map(|_| ())
                            })
                        },
                    );
                    let _ = send_tx.send((DuplexSide::Send, outcome));
                } else {
                    let pool_path_label = path.display().to_string();
                    let outcome = ingest_from_stdin(
                        io::stdin().lock(),
                        FeedIngestContext {
                            pool_ref: &pool_ref,
                            pool_path_label: &pool_path_label,
                            tags: &[],
                            durability: Durability::Fast,
                            retry_config: None,
                            pool_handle: &mut send_pool,
                            color_mode,
                            json_output: context_json_output,
                            input: InputMode::Auto,
                            errors: ErrorPolicyCli::Stop,
                        },
                        false,
                    );
                    let outcome = ingest_outcome(outcome);
                    let _ = send_tx.send((DuplexSide::Send, outcome));
                }
            });
        }
        PoolTarget::Remote {
            base_url,
            pool: name,
        } => {
            if args.create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("remote duplex does not support --create")
                    .with_hint(
                        "Create remote pools with server-side tooling, then rerun duplex.",
                    ));
            }
            let client = remote_client(base_url)?;
            let remote_pool = client.open_pool(&PoolRef::name(name.clone()))?;
            let follow_tx = event_tx.clone();
            let follow_cfg = cfg.clone();
            let stop_for_follow = stop.clone();
            let follow_client = client.clone();
            let pool_name = name.clone();
            let _ = std::thread::spawn(move || {
                let outcome = follow_remote(&follow_client, &pool_name, &follow_cfg);
                if outcome.is_err() {
                    stop_for_follow.store(true, Ordering::Release);
                }
                let _ = follow_tx.send((DuplexSide::Follow, outcome));
            });

            let send_tx = event_tx;
            let stop_for_send = stop.clone();
            let me_for_send = args.me.clone();
            let color_mode = context.color_mode();
            let context_json_output = !pretty;
            let _ = std::thread::spawn(move || {
                if stdin_is_terminal {
                    let outcome = send_tty_lines(
                        me_for_send.as_deref().expect("me required"),
                        &stop_for_send,
                        |value| {
                            remote_pool
                                .append_json_now(&value, &[], Durability::Fast)
                                .map(|_| ())
                        },
                    );
                    let _ = send_tx.send((DuplexSide::Send, outcome));
                } else {
                    let pool_path_label = format!("{}/{}", client.base_url(), name);
                    let outcome = ingest_from_stdin_remote(
                        io::stdin().lock(),
                        RemoteFeedIngestContext {
                            pool_ref: &name,
                            pool_path_label: &pool_path_label,
                            tags: &[],
                            durability: Durability::Fast,
                            retry_config: None,
                            remote_pool: &remote_pool,
                            color_mode,
                            json_output: context_json_output,
                            input: InputMode::Auto,
                            errors: ErrorPolicyCli::Stop,
                        },
                        false,
                    );
                    let outcome = ingest_outcome(outcome);
                    let _ = send_tx.send((DuplexSide::Send, outcome));
                }
            });
        }
    }

    match event_rx.recv() {
        Ok((_side, outcome)) => {
            stop.store(true, Ordering::Release);
            outcome
        }
        Err(_) => Ok(CommandResult::ok()),
    }
}

fn send_tty_lines(
    me: &str,
    stop: &Arc<AtomicBool>,
    mut append: impl FnMut(serde_json::Value) -> Result<(), Error>,
) -> Result<CommandResult, Error> {
    let mut reader = io::BufReader::new(io::stdin());
    loop {
        if follow_should_stop(Some(stop)) {
            break;
        }
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read line from stdin")
                .with_source(err)
        })?;
        if n == 0 || follow_should_stop(Some(stop)) {
            break;
        }
        if let Some(value) = parse_duplex_tty_line(me, &line) {
            append(value)?;
        }
    }
    Ok(CommandResult::ok())
}

fn ingest_outcome(
    outcome: Result<crate::ingest::IngestOutcome, Error>,
) -> Result<CommandResult, Error> {
    match outcome {
        Ok(outcome) if outcome.records_total == 0 => Err(missing_feed_data_error()),
        Ok(outcome) if outcome.failed > 0 => Ok(CommandResult::with_code(1)),
        Ok(_) => Ok(CommandResult::ok()),
        Err(err) => Err(err),
    }
}
