//! Purpose: Execute follow and duplex streaming commands.
//! Exports: `FollowArgs`, `DuplexArgs`, `follow`, `duplex`.
//! Role: Keep shared streaming configuration, filtering, and cancellation together.

use super::context::CliContext;
use super::result::CommandResult;
use crate::jq_filter::compile_filters;
use crate::{
    DEFAULT_POOL_SIZE, ErrorPolicyCli, FeedIngestContext, FollowConfig, FollowFormat, InputMode,
    PoolTarget, RemoteFeedIngestContext, add_missing_pool_create_hint, duplex_requires_me_when_tty,
    emit_follow_timeout_human, ensure_pool_dir, follow_exact_create_command_hint, follow_pool,
    follow_remote, follow_should_stop, ingest_from_stdin, ingest_from_stdin_remote,
    missing_feed_data_error, now_ns, parse_duplex_tty_line, parse_duration, parse_since,
    reject_remote_only_flags_for_local_target, resolve_pool_target, resolve_token_value,
    retry_with_config,
};
use plasmite::api::{
    AppendOptions, Durability, Error, ErrorKind, Pool, PoolOptions, PoolRef, RemoteClient, lite3,
};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

pub(super) struct FollowArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) tail: u64,
    pub(super) one: bool,
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
    pub(super) token: Option<String>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) tls_ca: Option<PathBuf>,
    pub(super) tls_skip_verify: bool,
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
    let format_flag = args.format;
    let format = args.format.unwrap_or(if args.jsonl {
        FollowFormat::Jsonl
    } else {
        FollowFormat::Pretty
    });
    let pretty = matches!(format, FollowFormat::Pretty);
    let now = now_ns()?;
    let since_ns = args
        .since
        .as_deref()
        .map(|value| parse_since(value, now))
        .transpose()?;
    let timeout_input = args.timeout.as_deref();
    let timeout = timeout_input.map(parse_duration).transpose()?;
    let exact_follow_create_hint = follow_exact_create_command_hint(
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
    let cfg = FollowConfig {
        tail: args.tail,
        pretty,
        one: args.one,
        timeout,
        data_only: args.data_only,
        since_ns,
        required_tags: args.tags,
        where_predicates: compile_filters(&args.where_expr)?,
        quiet_drops: args.quiet_drops,
        notify: !args.no_notify,
        color_mode: context.color_mode(),
        replay_speed: args.replay,
        suppress_sender: None,
        stop: None,
    };
    let target = resolve_pool_target(&args.pool, context.pool_dir())?;
    match target {
        PoolTarget::LocalPath(path) => {
            reject_remote_only_flags_for_local_target(
                "follow",
                args.token.as_deref(),
                args.token_file.as_deref(),
                args.tls_ca.as_deref(),
                args.tls_skip_verify,
            )?;
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
            if since_ns.is_some_and(|since_ns| since_ns > now) {
                return Ok(CommandResult::ok());
            }
            let outcome = follow_pool(&pool_handle, &args.pool, &path, cfg)?;
            if outcome.exit_code == 124 {
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
            let token_value = resolve_token_value(args.token, args.token_file)?;
            let mut client = RemoteClient::new(base_url)?;
            if let Some(token_value) = token_value {
                client = client.with_token(token_value);
            }
            if let Some(path) = args.tls_ca {
                client = client.with_tls_ca_file(path)?;
            }
            if args.tls_skip_verify {
                eprintln!(
                    "warning: --tls-skip-verify disables TLS certificate verification (unsafe)"
                );
                client = client.with_tls_skip_verify();
            }
            let outcome = follow_remote(&client, &pool, &cfg)?;
            if outcome.exit_code == 124 {
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
    let format_flag = args.format;
    let format = args.format.unwrap_or(if args.jsonl {
        FollowFormat::Jsonl
    } else {
        FollowFormat::Pretty
    });
    let pretty = matches!(format, FollowFormat::Pretty);
    let now = now_ns()?;
    let since_ns = args
        .since
        .as_deref()
        .map(|value| parse_since(value, now))
        .transpose()?;
    let timeout_input = args.timeout.as_deref();
    let timeout = timeout_input.map(parse_duration).transpose()?;
    let exact_follow_create_hint = follow_exact_create_command_hint(
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
    let stop = Arc::new(AtomicBool::new(false));
    let cfg = FollowConfig {
        tail: args.tail,
        pretty,
        one: false,
        timeout,
        data_only: false,
        since_ns,
        required_tags: Vec::new(),
        where_predicates: compile_filters(&[])?,
        quiet_drops: false,
        notify: true,
        color_mode: context.color_mode(),
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
            if since_ns.is_some_and(|since_ns| since_ns > now) {
                return Ok(CommandResult::ok());
            }
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
            let _ = std::thread::spawn(move || {
                if stdin_is_terminal {
                    let mut reader = std::io::BufReader::new(io::stdin());
                    loop {
                        if follow_should_stop(Some(&stop_for_send)) {
                            break;
                        }
                        let mut line = String::new();
                        let n = match std::io::BufRead::read_line(&mut reader, &mut line) {
                            Ok(n) => n,
                            Err(err) => {
                                let err = Error::new(ErrorKind::Io)
                                    .with_message("failed to read line from stdin")
                                    .with_source(err);
                                let _ = send_tx.send((DuplexSide::Send, Err(err)));
                                return;
                            }
                        };
                        if n == 0 || follow_should_stop(Some(&stop_for_send)) {
                            break;
                        }
                        let Some(value) = parse_duplex_tty_line(
                            me_for_send.as_ref().expect("me required"),
                            &line,
                        ) else {
                            continue;
                        };
                        let payload = match lite3::encode_message(&Vec::<String>::new(), &value) {
                            Ok(payload) => payload,
                            Err(err) => {
                                let _ = send_tx.send((DuplexSide::Send, Err(err)));
                                return;
                            }
                        };
                        if let Err(err) = retry_with_config(None, || {
                            let timestamp_ns = now_ns()?;
                            let options = AppendOptions::new(timestamp_ns, Durability::Fast);
                            send_pool
                                .append_with_options(payload.as_slice(), options)
                                .map(|_| ())
                        }) {
                            let _ = send_tx.send((DuplexSide::Send, Err(err)));
                            return;
                        }
                    }
                    let _ = send_tx.send((DuplexSide::Send, Ok(CommandResult::ok())));
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
            if args.since.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("remote duplex does not support --since")
                    .with_hint(
                        "Use --tail N for remote refs, or run --since against a local pool path.",
                    ));
            }
            let client = RemoteClient::new(base_url)?;
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
            let _ = std::thread::spawn(move || {
                if stdin_is_terminal {
                    let mut reader = std::io::BufReader::new(io::stdin());
                    loop {
                        if follow_should_stop(Some(&stop_for_send)) {
                            break;
                        }
                        let mut line = String::new();
                        let n = match std::io::BufRead::read_line(&mut reader, &mut line) {
                            Ok(n) => n,
                            Err(err) => {
                                let err = Error::new(ErrorKind::Io)
                                    .with_message("failed to read line from stdin")
                                    .with_source(err);
                                let _ = send_tx.send((DuplexSide::Send, Err(err)));
                                return;
                            }
                        };
                        if n == 0 || follow_should_stop(Some(&stop_for_send)) {
                            break;
                        }
                        let Some(value) = parse_duplex_tty_line(
                            me_for_send.as_ref().expect("me required"),
                            &line,
                        ) else {
                            continue;
                        };
                        if let Err(err) = remote_pool.append_json_now(&value, &[], Durability::Fast)
                        {
                            let _ = send_tx.send((DuplexSide::Send, Err(err)));
                            return;
                        }
                    }
                    let _ = send_tx.send((DuplexSide::Send, Ok(CommandResult::ok())));
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
