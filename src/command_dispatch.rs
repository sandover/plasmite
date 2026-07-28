//! Purpose: Hold top-level CLI command dispatch for `plasmite`.
//! Exports: `dispatch_command`.
//! Role: Keep `main.rs` focused on parse/bootstrap and delegate command execution.
//! Invariants: Command behavior, output envelopes, and exit code semantics stay unchanged.
//! Invariants: Helpers in `main.rs` remain the source of command business logic.

use super::*;

pub(super) fn dispatch_command(
    command: Command,
    pool_dir: PathBuf,
    color_mode: ColorMode,
) -> Result<RunOutcome, Error> {
    match command {
        Command::Completion { shell } => {
            let mut cmd = Cli::command();
            clap_complete::aot::generate(shell, &mut cmd, "plasmite", &mut io::stdout());
            Ok(RunOutcome::ok())
        }
        Command::Doctor { .. } => unreachable!("doctor is routed by cli::dispatch"),
        Command::Serve { subcommand, run } => match subcommand {
            Some(ServeSubcommand::Init(args)) => {
                reject_ignored_serve_init_args(&run)?;
                let bind: SocketAddr = args.bind.parse().map_err(|_| {
                    Error::new(ErrorKind::Usage)
                        .with_message("invalid bind address")
                        .with_hint("Use a host:port value like 0.0.0.0:9700.")
                })?;
                let config = serve_init::ServeInitConfig {
                    output_dir: args.output_dir,
                    token_file: args.token_file,
                    tls_cert: args.tls_cert,
                    tls_key: args.tls_key,
                    bind,
                    force: args.force,
                };
                let result = serve_init::init(config)?;
                if io::stdout().is_terminal() {
                    emit_serve_init_human(&result);
                } else {
                    emit_json(
                        json!({
                            "init": {
                                "artifact_paths": {
                                    "token_file": result.token_file,
                                    "tls_cert": result.tls_cert,
                                    "tls_key": result.tls_key,
                                },
                                "tls_fingerprint": result.tls_fingerprint,
                                "server_commands": result.server_commands,
                                "client_commands": result.client_commands,
                                "curl_client_commands": result.curl_client_commands,
                            }
                        }),
                        color_mode,
                    );
                }
                Ok(RunOutcome::ok())
            }
            Some(ServeSubcommand::Check { json }) => {
                let mut config = serve_config_from_run_args(run, &pool_dir)?;
                config.cors_allowed_origins = serve::preflight_config(&config)?;
                emit_serve_check_report(&config, color_mode, json);
                Ok(RunOutcome::ok())
            }
            None => {
                let config = serve_config_from_run_args(run, &pool_dir)?;
                emit_serve_startup_guidance(&config);
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| {
                        Error::new(ErrorKind::Internal)
                            .with_message("failed to start runtime")
                            .with_source(err)
                    })?;
                runtime.block_on(serve::serve(config))?;
                Ok(RunOutcome::ok())
            }
        },
        Command::Mcp { dir } => {
            let mcp_pool_dir = dir.unwrap_or(pool_dir);
            mcp_stdio::serve(mcp_pool_dir)?;
            Ok(RunOutcome::ok())
        }
        Command::Pool { .. } => unreachable!("pool is routed by cli::dispatch"),
        Command::Feed { .. } => unreachable!("feed is routed by cli::dispatch"),
        Command::Fetch { .. } => unreachable!("fetch is routed by cli::dispatch"),
        Command::Tap {
            pool,
            create,
            create_size,
            tag,
            quiet,
            durability,
            command,
        } => {
            if create_size.is_some() && !create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--create-size requires --create")
                    .with_hint("Add --create or remove --create-size."));
            }
            if pool.contains("://") {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("tap accepts local pool refs only")
                    .with_hint(
                        "Use a local pool name/path (for example `plasmite tap build -- ...`).",
                    ));
            }
            if command.is_empty() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("tap requires a wrapped command after `--`")
                    .with_hint("Use `plasmite tap <pool> -- <command...>`."));
            }
            let durability = parse_durability(&durability)?;
            let path = resolve_poolref(&pool, &pool_dir)?;
            let mut pool_handle = match Pool::open(&path) {
                Ok(pool_handle) => pool_handle,
                Err(err) if create && err.kind() == ErrorKind::NotFound => {
                    ensure_pool_dir(&pool_dir)?;
                    let size = create_size
                        .as_deref()
                        .map(parse_size)
                        .transpose()?
                        .unwrap_or(DEFAULT_POOL_SIZE);
                    Pool::create(&path, PoolOptions::new(size))?
                }
                Err(err) => {
                    return Err(add_missing_pool_create_hint(err, "tap", &pool, &pool, None));
                }
            };

            let mut child = std::process::Command::new(&command[0])
                .args(&command[1..])
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|err| tap_spawn_error(&command, err))?;
            // Install a dedicated relay that forwards SIGINT/SIGTERM received by tap
            // to the wrapped child process PID.
            tap_spawn_signal_forwarder(child.id() as i32);
            let status_on_tty_stderr = io::stderr().is_terminal();
            if status_on_tty_stderr {
                eprintln!(
                    "tapping {} <- {}",
                    pool,
                    render_shell_agnostic_command(&command)
                );
            }

            let child_stdout = child.stdout.take().ok_or_else(|| {
                Error::new(ErrorKind::Internal).with_message("tap child stdout pipe unavailable")
            })?;
            let child_stderr = child.stderr.take().ok_or_else(|| {
                Error::new(ErrorKind::Internal).with_message("tap child stderr pipe unavailable")
            })?;

            let lifecycle_tags = vec!["lifecycle".to_string()];
            if let Err(err) = tap_append_message(
                &mut pool_handle,
                durability,
                &lifecycle_tags,
                &json!({
                    "kind": "start",
                    "cmd": command,
                }),
            ) {
                tap_terminate_child(&mut child);
                return Err(err);
            }

            let start_time = Instant::now();
            let (event_tx, event_rx) = mpsc::channel();
            let stdout_reader =
                tap_spawn_reader(child_stdout, TapStream::Stdout, !quiet, event_tx.clone());
            let stderr_reader = tap_spawn_reader(child_stderr, TapStream::Stderr, !quiet, event_tx);

            let mut reader_error: Option<Error> = None;
            let mut child_status = None;
            let mut line_count: u64 = 0;

            while child_status.is_none() {
                match event_rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(TapEvent::Line { stream, raw_line }) => {
                        line_count = line_count.saturating_add(1);
                        if let Err(err) = tap_append_message(
                            &mut pool_handle,
                            durability,
                            &tag,
                            &json!({
                                "kind": "line",
                                "stream": stream.as_str(),
                                "line": trim_tap_line_endings(&raw_line),
                            }),
                        ) {
                            tap_terminate_child(&mut child);
                            return Err(err);
                        }
                    }
                    Ok(TapEvent::ReaderError(err)) => {
                        if reader_error.is_none() {
                            reader_error = Some(err);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {}
                }
                child_status = child.try_wait().map_err(|err| {
                    Error::new(ErrorKind::Io)
                        .with_message("failed waiting for wrapped command")
                        .with_source(err)
                })?;
            }

            let child_status = child_status.expect("status set once loop exits");
            if stdout_reader.join().is_err() && reader_error.is_none() {
                reader_error = Some(
                    Error::new(ErrorKind::Internal).with_message("tap stdout reader panicked"),
                );
            }
            if stderr_reader.join().is_err() && reader_error.is_none() {
                reader_error = Some(
                    Error::new(ErrorKind::Internal).with_message("tap stderr reader panicked"),
                );
            }

            while let Ok(event) = event_rx.try_recv() {
                match event {
                    TapEvent::Line { stream, raw_line } => {
                        line_count = line_count.saturating_add(1);
                        tap_append_message(
                            &mut pool_handle,
                            durability,
                            &tag,
                            &json!({
                                "kind": "line",
                                "stream": stream.as_str(),
                                "line": trim_tap_line_endings(&raw_line),
                            }),
                        )?;
                    }
                    TapEvent::ReaderError(err) => {
                        if reader_error.is_none() {
                            reader_error = Some(err);
                        }
                    }
                }
            }

            if let Some(err) = reader_error {
                return Err(err);
            }

            let elapsed_ms = start_time.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let exit_code = if let Some(signal) = tap_exit_signal(&child_status) {
                let signal_name = tap_signal_name(signal);
                tap_append_message(
                    &mut pool_handle,
                    durability,
                    &lifecycle_tags,
                    &json!({
                        "kind": "exit",
                        "signal": signal_name,
                        "elapsed_ms": elapsed_ms,
                    }),
                )?;
                if status_on_tty_stderr {
                    eprintln!(
                        "tapped {line_count} lines ({}) -> {} signal {}",
                        format_tap_elapsed(elapsed_ms),
                        pool,
                        signal_name
                    );
                }
                128 + signal
            } else {
                let code = child_status.code().unwrap_or(1);
                tap_append_message(
                    &mut pool_handle,
                    durability,
                    &lifecycle_tags,
                    &json!({
                        "kind": "exit",
                        "code": code,
                        "elapsed_ms": elapsed_ms,
                    }),
                )?;
                if status_on_tty_stderr {
                    eprintln!(
                        "tapped {line_count} lines ({}) -> {} exit {}",
                        format_tap_elapsed(elapsed_ms),
                        pool,
                        code
                    );
                }
                code
            };

            Ok(RunOutcome::with_code(exit_code))
        }
        Command::Duplex { .. } => unreachable!("duplex is routed by cli::dispatch"),
        Command::Follow { .. } => unreachable!("follow is routed by cli::dispatch"),
        Command::Version => unreachable!("version is routed by cli::dispatch"),
    }
}

fn reject_ignored_serve_init_args(run: &ServeRunArgs) -> Result<(), Error> {
    let ignored = if run.bind != "127.0.0.1:9700" {
        Some(("--bind", true))
    } else if !matches!(run.access, AccessModeCli::ReadWrite) {
        Some(("--access", false))
    } else if !run.cors_origin.is_empty() {
        Some(("--cors-origin", false))
    } else if run.token.is_some() {
        Some(("--token", false))
    } else if run.token_file.is_some() {
        Some(("--token-file", true))
    } else if run.tls_cert.is_some() {
        Some(("--tls-cert", true))
    } else if run.tls_key.is_some() {
        Some(("--tls-key", true))
    } else if run.tls_self_signed {
        Some(("--tls-self-signed", false))
    } else if run.allow_non_loopback {
        Some(("--allow-non-loopback", false))
    } else if run.insecure_no_tls {
        Some(("--insecure-no-tls", false))
    } else if run.max_body_bytes != DEFAULT_MAX_BODY_BYTES {
        Some(("--max-body-bytes", false))
    } else if run.max_tail_timeout_ms != DEFAULT_MAX_TAIL_TIMEOUT_MS {
        Some(("--max-tail-timeout-ms", false))
    } else if run.max_tail_concurrency != DEFAULT_MAX_TAIL_CONCURRENCY {
        Some(("--max-tail-concurrency", false))
    } else {
        None
    };

    let Some((option, has_init_equivalent)) = ignored else {
        return Ok(());
    };
    let hint = if has_init_equivalent {
        format!("Place `{option}` after `init`, or remove it from this command.")
    } else {
        format!("Remove `{option}`; it configures `serve` and `serve check`, not `serve init`.")
    };
    Err(Error::new(ErrorKind::Usage)
        .with_message(format!("serve init does not use parent option {option}"))
        .with_hint(hint))
}

#[derive(Clone, Copy)]
enum TapStream {
    Stdout,
    Stderr,
}

impl TapStream {
    fn as_str(self) -> &'static str {
        match self {
            TapStream::Stdout => "stdout",
            TapStream::Stderr => "stderr",
        }
    }
}

enum TapEvent {
    Line { stream: TapStream, raw_line: String },
    ReaderError(Error),
}

fn tap_append_message(
    pool: &mut Pool,
    durability: Durability,
    tags: &[String],
    data: &Value,
) -> Result<(), Error> {
    let payload = lite3::encode_message(tags, data)?;
    let timestamp_ns = now_ns()?;
    let options = AppendOptions::new(timestamp_ns, durability);
    pool.append_with_options(payload.as_slice(), options)?;
    Ok(())
}

fn trim_tap_line_endings(raw_line: &str) -> String {
    raw_line.trim_end_matches(['\r', '\n']).to_string()
}

fn tap_spawn_reader<R>(
    reader: R,
    stream: TapStream,
    passthrough: bool,
    tx: mpsc::Sender<TapEvent>,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        use std::io::Write as _;

        let mut reader = std::io::BufReader::new(reader);
        let mut passthrough_enabled = passthrough;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if passthrough_enabled {
                        let write_result = match stream {
                            TapStream::Stdout => {
                                let mut out = io::stdout();
                                out.write_all(line.as_bytes()).and_then(|_| out.flush())
                            }
                            TapStream::Stderr => {
                                let mut err = io::stderr();
                                err.write_all(line.as_bytes()).and_then(|_| err.flush())
                            }
                        };
                        if let Err(err) = write_result {
                            if err.kind() == std::io::ErrorKind::BrokenPipe {
                                passthrough_enabled = false;
                            } else {
                                let _ = tx.send(TapEvent::ReaderError(
                                    Error::new(ErrorKind::Io)
                                        .with_message("failed to write passthrough output")
                                        .with_source(err),
                                ));
                                return;
                            }
                        }
                    }
                    let _ = tx.send(TapEvent::Line {
                        stream,
                        raw_line: line.clone(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(TapEvent::ReaderError(
                        Error::new(ErrorKind::Io)
                            .with_message("failed to read wrapped command output")
                            .with_source(err),
                    ));
                    return;
                }
            }
        }
    })
}

fn tap_spawn_error(command: &[String], err: std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        let hint_cmd = command
            .first()
            .cloned()
            .unwrap_or_else(|| "<command>".to_string());
        return Error::new(ErrorKind::Usage)
            .with_message(format!("wrapped command not found: {hint_cmd}"))
            .with_hint("Check PATH or use an absolute executable path.")
            .with_source(err);
    }
    Error::new(ErrorKind::Io)
        .with_message("failed to spawn wrapped command")
        .with_hint("Check command arguments and executable permissions.")
        .with_source(err)
}

fn tap_terminate_child(child: &mut std::process::Child) {
    // Best-effort cleanup: command may have already exited.
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn tap_exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn tap_exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn tap_signal_name(signal: i32) -> String {
    match signal {
        2 => "SIGINT".to_string(),
        9 => "SIGKILL".to_string(),
        11 => "SIGSEGV".to_string(),
        15 => "SIGTERM".to_string(),
        _ => format!("SIG{signal}"),
    }
}

fn format_tap_elapsed(elapsed_ms: u64) -> String {
    format!("{:.1}s", (elapsed_ms as f64) / 1000.0)
}

#[cfg(unix)]
fn tap_spawn_signal_forwarder(child_pid: i32) {
    let mut signals = match signal_hook::iterator::Signals::new([libc::SIGINT, libc::SIGTERM]) {
        Ok(signals) => signals,
        Err(_) => return,
    };
    std::thread::spawn(move || {
        for signal in signals.forever() {
            tap_forward_signal(child_pid, signal);
        }
    });
}

#[cfg(not(unix))]
fn tap_spawn_signal_forwarder(_child_pid: i32) {}

#[cfg(unix)]
fn tap_forward_signal(child_pid: i32, signal: i32) {
    // If the child already exited, `kill` may return ESRCH; ignore and continue.
    let _ = unsafe { libc::kill(child_pid, signal) };
}
