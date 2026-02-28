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
        Command::Version => {
            emit_version_output(color_mode);
            Ok(RunOutcome::ok())
        }
        Command::Doctor { pool, all, json } => {
            if all && pool.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--all cannot be combined with a pool name")
                    .with_hint("Use --all by itself, or provide a single pool."));
            }
            if !all && pool.is_none() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("doctor requires a pool name or --all")
                    .with_hint("Use `plasmite doctor <pool>` or `plasmite doctor --all`."));
            }
            let client = LocalClient::new().with_pool_dir(&pool_dir);
            let reports = if let Some(pool) = pool {
                let path = resolve_poolref(&pool, &pool_dir)?;
                let pool_ref = PoolRef::path(path.clone());
                vec![doctor_report(&client, pool_ref, pool, path)?]
            } else {
                let mut reports = Vec::new();
                for path in list_pool_paths(&pool_dir)? {
                    let label = path.to_string_lossy().to_string();
                    let pool_ref = PoolRef::path(path.clone());
                    reports.push(doctor_report(&client, pool_ref, label, path)?);
                }
                reports
            };

            if json {
                let values = reports.iter().map(report_json).collect::<Vec<_>>();
                emit_json(json!({ "reports": values }), color_mode);
            } else if all {
                emit_doctor_human_summary(&reports);
            } else {
                for report in &reports {
                    emit_doctor_human(report);
                }
            }

            let has_corrupt = reports
                .iter()
                .any(|report| report.status == ValidationStatus::Corrupt);
            let exit_code = if has_corrupt {
                to_exit_code(ErrorKind::Corrupt)
            } else {
                0
            };
            Ok(RunOutcome::with_code(exit_code))
        }
        Command::Serve { subcommand, run } => match subcommand {
            Some(ServeSubcommand::Init(args)) => {
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
        Command::Pool { command } => match command {
            PoolCommand::Create {
                names,
                size,
                index_capacity,
                json,
            } => {
                let client = LocalClient::new().with_pool_dir(&pool_dir);
                let size = size
                    .as_deref()
                    .map(parse_size)
                    .transpose()?
                    .unwrap_or(DEFAULT_POOL_SIZE);
                ensure_pool_dir(&pool_dir)?;
                let mut results = Vec::new();
                for name in names {
                    let path = resolve_poolref(&name, &pool_dir)?;
                    if path.exists() {
                        return Err(Error::new(ErrorKind::AlreadyExists)
                            .with_message("pool already exists")
                            .with_path(&path)
                            .with_hint(
                                "Choose a different name or remove the existing pool file.",
                            ));
                    }
                    let mut options = PoolOptions::new(size);
                    if let Some(index_capacity) = index_capacity {
                        let index_size_bytes = index_capacity as u64 * 16;
                        if index_size_bytes > size / 2 {
                            return Err(Error::new(ErrorKind::Usage)
                                .with_message("index capacity is too large for pool size")
                                .with_hint(
                                    "Reduce --index-capacity or increase --size (index region must be <= 50% of the pool file).",
                                ));
                        }
                        options = options.with_index_capacity(index_capacity);
                    }
                    let pool_ref = PoolRef::path(path.clone());
                    let info = client.create_pool(&pool_ref, options)?;
                    results.push(pool_info_json(&name, &info));
                }
                if json {
                    emit_json(json!({ "created": results }), color_mode);
                } else {
                    emit_pool_create_table(&results, &pool_dir);
                }
                Ok(RunOutcome::ok())
            }
            PoolCommand::Info { name, json } => {
                let client = LocalClient::new().with_pool_dir(&pool_dir);
                let path = resolve_poolref(&name, &pool_dir)?;
                let pool_ref = PoolRef::path(path);
                let info = client.pool_info(&pool_ref).map_err(|err| {
                    if err.kind() == ErrorKind::NotFound {
                        let base = Error::new(ErrorKind::NotFound).with_message("not found");
                        add_missing_pool_hint(base, &name, &name)
                    } else {
                        err
                    }
                })?;
                if json {
                    emit_json(pool_info_json(&name, &info), color_mode);
                } else {
                    emit_pool_info_pretty(&name, &info);
                }
                Ok(RunOutcome::ok())
            }
            PoolCommand::Delete { names, json } => {
                let client = LocalClient::new().with_pool_dir(&pool_dir);
                let mut deleted = Vec::new();
                let mut failed = Vec::new();
                let mut table_rows = Vec::new();
                let mut first_error_kind = None;
                enum HumanDeleteStatus {
                    Ok,
                    Err { kind: ErrorKind, detail: String },
                }
                let mut human_rows = Vec::<(String, HumanDeleteStatus)>::new();

                for name in names {
                    let result = if name.contains("://") {
                        Err(Error::new(ErrorKind::Usage)
                            .with_message("pool delete accepts local pool names or paths only")
                            .with_hint("Use pool names/paths for local delete, or call remote APIs directly."))
                    } else {
                        resolve_poolref(&name, &pool_dir).and_then(|path| {
                            let pool_ref = PoolRef::path(path.clone());
                            client.delete_pool(&pool_ref).map_err(|err| {
                                if err.kind() == ErrorKind::NotFound {
                                    Error::new(ErrorKind::NotFound)
                                        .with_message("pool not found")
                                        .with_path(&path)
                                        .with_hint("Create the pool first or check --dir.")
                                } else if err.kind() == ErrorKind::Permission {
                                    // Keep historical CLI delete semantics: permission failures
                                    // are surfaced as I/O for stable exit-code behavior.
                                    Error::new(ErrorKind::Io)
                                        .with_message("failed to delete pool")
                                        .with_path(&path)
                                } else {
                                    err
                                }
                            })?;
                            Ok(path)
                        })
                    };

                    match result {
                        Ok(path) => {
                            let display_path = short_display_path(path.as_path(), Some(&pool_dir));
                            deleted.push(json!({
                                "pool": name,
                                "path": path.display().to_string(),
                            }));
                            table_rows.push(vec![
                                name.clone(),
                                "OK".to_string(),
                                display_path,
                                String::new(),
                            ]);
                            human_rows.push((name, HumanDeleteStatus::Ok));
                        }
                        Err(err) => {
                            if first_error_kind.is_none() {
                                first_error_kind = Some(err.kind());
                            }
                            let display_path = err
                                .path()
                                .map(|path| short_display_path(path, Some(&pool_dir)))
                                .unwrap_or_else(|| "-".to_string());
                            let detail = err.message().unwrap_or("error").to_string();
                            failed.push(json!({
                                "pool": name.clone(),
                                "error": error_json(&err)["error"].clone(),
                            }));
                            table_rows.push(vec![
                                name.clone(),
                                "ERR".to_string(),
                                display_path,
                                detail.clone(),
                            ]);
                            human_rows.push((
                                name,
                                HumanDeleteStatus::Err {
                                    kind: err.kind(),
                                    detail,
                                },
                            ));
                        }
                    }
                }

                if json {
                    emit_json(
                        json!({
                            "deleted": deleted,
                            "failed": failed,
                        }),
                        color_mode,
                    );
                } else if io::stdout().is_terminal() {
                    let total = human_rows.len();
                    let deleted_count = deleted.len();
                    if total == 1 {
                        if let Some((name, status)) = human_rows.first() {
                            match status {
                                HumanDeleteStatus::Ok => {
                                    println!("Deleted pool \"{name}\".");
                                }
                                HumanDeleteStatus::Err {
                                    kind: ErrorKind::NotFound,
                                    ..
                                } => {
                                    println!("Pool \"{name}\" not found. Nothing to delete.");
                                    println!();
                                    println!(
                                        "  Pool directory: {}",
                                        display_pool_dir_for_humans(&pool_dir)
                                    );
                                    println!("  List pools:     pls pool list");
                                }
                                HumanDeleteStatus::Err { detail, .. } => {
                                    println!("Failed to delete pool \"{name}\".");
                                    println!();
                                    println!("  Reason:         {detail}");
                                    println!(
                                        "  Pool directory: {}",
                                        display_pool_dir_for_humans(&pool_dir)
                                    );
                                }
                            }
                        }
                    } else if failed.is_empty() {
                        println!("Deleted {deleted_count} pools.");
                        println!();
                        for (name, status) in &human_rows {
                            if matches!(status, HumanDeleteStatus::Ok) {
                                println!("  ✓ {name}");
                            }
                        }
                        println!();
                        println!(
                            "  Pool directory: {}",
                            display_pool_dir_for_humans(&pool_dir)
                        );
                    } else {
                        println!("Deleted {deleted_count} of {total} pools.");
                        println!();
                        for (name, status) in &human_rows {
                            match status {
                                HumanDeleteStatus::Ok => println!("  ✓ {name}"),
                                HumanDeleteStatus::Err { detail, .. } => {
                                    println!("  ✗ {name} — {detail}");
                                }
                            }
                        }
                        println!();
                        println!(
                            "  Pool directory: {}",
                            display_pool_dir_for_humans(&pool_dir)
                        );
                    }
                } else {
                    emit_table(&["NAME", "STATUS", "PATH", "DETAIL"], &table_rows);
                }
                if let Some(kind) = first_error_kind {
                    Ok(RunOutcome::with_code(to_exit_code(kind)))
                } else {
                    Ok(RunOutcome::ok())
                }
            }
            PoolCommand::List { json } => {
                let client = LocalClient::new().with_pool_dir(&pool_dir);
                let pools = list_pools(&pool_dir, &client);
                if json {
                    emit_json(json!({ "pools": pools }), color_mode);
                } else {
                    emit_pool_list_table(&pools, &pool_dir);
                }
                Ok(RunOutcome::ok())
            }
        },
        Command::Feed {
            pool,
            tag,
            data,
            file,
            durability,
            create,
            create_size,
            retry,
            retry_delay,
            input,
            errors,
            token,
            token_file,
            tls_ca,
            tls_skip_verify,
        } => {
            let target = resolve_pool_target(&pool, &pool_dir)?;
            let data_arg = data;
            let file_arg = file;
            if create_size.is_some() && !create {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--create-size requires --create")
                    .with_hint("Add --create or remove --create-size."));
            }
            if retry_delay.is_some() && retry == 0 {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("--retry-delay requires --retry")
                    .with_hint("Add --retry or remove --retry-delay."));
            }
            let durability = parse_durability(&durability)?;
            let retry_config = parse_retry_config(retry, retry_delay.as_deref())?;
            if data_arg.is_some() && file_arg.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("multiple data inputs provided")
                    .with_hint("Use only one of DATA, --file, or stdin."));
            }
            let file = file_arg.as_deref();
            let stdin_is_terminal = io::stdin().is_terminal();
            let stdin_stream = data_arg.is_none() && file.is_none() && !stdin_is_terminal;
            let single_input = data_arg.is_some() || file.is_some() || stdin_is_terminal;
            let exact_create_hint = feed_exact_create_command_hint(
                &pool,
                FeedExactCreateHint {
                    tags: &tag,
                    data: &data_arg,
                    file: &file_arg,
                    durability,
                    retry,
                    retry_delay: retry_delay.as_deref(),
                    input,
                    errors,
                    single_input,
                },
            );
            match target {
                PoolTarget::LocalPath(path) => {
                    reject_remote_only_flags_for_local_target(
                        "feed",
                        token.as_deref(),
                        token_file.as_deref(),
                        tls_ca.as_deref(),
                        tls_skip_verify,
                    )?;
                    let mut pool_handle = match Pool::open(&path) {
                        Ok(pool) => pool,
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
                            return Err(add_missing_pool_create_hint(
                                err,
                                "feed",
                                &pool,
                                &pool,
                                exact_create_hint,
                            ));
                        }
                    };
                    if let Some(data) = data_arg.as_deref() {
                        let data = parse_inline_json(data)?;
                        let payload = lite3::encode_message(&tag, &data)?;
                        let (seq, timestamp_ns) = retry_with_config(retry_config, || {
                            let timestamp_ns = now_ns()?;
                            let options = AppendOptions::new(timestamp_ns, durability);
                            let seq =
                                pool_handle.append_with_options(payload.as_slice(), options)?;
                            Ok((seq, timestamp_ns))
                        })?;
                        emit_feed_receipt(feed_receipt_json(seq, timestamp_ns, &tag)?, color_mode);
                    } else {
                        let pool_path_label = path.display().to_string();
                        let outcome = if let Some(file) = file {
                            let reader = open_feed_reader(file)?;
                            ingest_from_stdin(
                                reader,
                                FeedIngestContext {
                                    pool_ref: &pool,
                                    pool_path_label: &pool_path_label,
                                    tags: &tag,
                                    durability,
                                    retry_config,
                                    pool_handle: &mut pool_handle,
                                    color_mode,
                                    input,
                                    errors,
                                },
                                true,
                            )?
                        } else if stdin_stream {
                            ingest_from_stdin(
                                io::stdin().lock(),
                                FeedIngestContext {
                                    pool_ref: &pool,
                                    pool_path_label: &pool_path_label,
                                    tags: &tag,
                                    durability,
                                    retry_config,
                                    pool_handle: &mut pool_handle,
                                    color_mode,
                                    input,
                                    errors,
                                },
                                true,
                            )?
                        } else {
                            return Err(missing_feed_data_error());
                        };
                        if outcome.records_total == 0 {
                            return Err(missing_feed_data_error());
                        }
                        if outcome.failed > 0 {
                            return Ok(RunOutcome::with_code(1));
                        }
                    }
                }
                PoolTarget::Remote {
                    base_url,
                    pool: name,
                } => {
                    if create {
                        return Err(Error::new(ErrorKind::Usage)
                            .with_message("remote feed does not support --create")
                            .with_hint("Create remote pools with server-side tooling, not feed."));
                    }
                    let token_value = resolve_token_value(token, token_file)?;
                    let mut client = RemoteClient::new(base_url)?;
                    if let Some(token_value) = token_value {
                        client = client.with_token(token_value);
                    }
                    if let Some(path) = tls_ca {
                        client = client.with_tls_ca_file(path)?;
                    }
                    if tls_skip_verify {
                        eprintln!(
                            "warning: --tls-skip-verify disables TLS certificate verification (unsafe)"
                        );
                        client = client.with_tls_skip_verify();
                    }
                    let remote_pool = client
                        .open_pool(&PoolRef::name(name.clone()))
                        .map_err(|err| add_missing_pool_hint(err, &pool, &pool))?;
                    if let Some(data) = data_arg.as_deref() {
                        let data = parse_inline_json(data)?;
                        let message = retry_with_config(retry_config, || {
                            remote_pool.append_json_now(&data, &tag, durability)
                        })?;
                        emit_feed_receipt(feed_receipt_from_message(&message), color_mode);
                    } else {
                        let pool_path_label = format!("{}/{}", client.base_url(), name);
                        let outcome = if let Some(file) = file {
                            let reader = open_feed_reader(file)?;
                            ingest_from_stdin_remote(
                                reader,
                                RemoteFeedIngestContext {
                                    pool_ref: &pool,
                                    pool_path_label: &pool_path_label,
                                    tags: &tag,
                                    durability,
                                    retry_config,
                                    remote_pool: &remote_pool,
                                    color_mode,
                                    input,
                                    errors,
                                },
                                true,
                            )?
                        } else if stdin_stream {
                            ingest_from_stdin_remote(
                                io::stdin().lock(),
                                RemoteFeedIngestContext {
                                    pool_ref: &pool,
                                    pool_path_label: &pool_path_label,
                                    tags: &tag,
                                    durability,
                                    retry_config,
                                    remote_pool: &remote_pool,
                                    color_mode,
                                    input,
                                    errors,
                                },
                                true,
                            )?
                        } else {
                            return Err(missing_feed_data_error());
                        };
                        if outcome.records_total == 0 {
                            return Err(missing_feed_data_error());
                        }
                        if outcome.failed > 0 {
                            return Ok(RunOutcome::with_code(1));
                        }
                    }
                }
            };
            Ok(RunOutcome::ok())
        }
        Command::Fetch { pool, seq } => {
            let path = resolve_poolref(&pool, &pool_dir)?;
            let pool_handle =
                Pool::open(&path).map_err(|err| add_missing_pool_hint(err, &pool, &pool))?;
            let frame = pool_handle
                .get(seq)
                .map_err(|err| add_missing_seq_hint(err, &pool))?;
            emit_json(message_from_frame(&frame)?, color_mode);
            Ok(RunOutcome::ok())
        }
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
        Command::Duplex {
            pool,
            me,
            create,
            tail,
            jsonl,
            timeout,
            format,
            since,
            echo_self,
        } => {
            if jsonl && format.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("conflicting output options")
                    .with_hint("Use --format jsonl (or --jsonl), but not both."));
            }
            let stdin_is_terminal = io::stdin().is_terminal();
            if duplex_requires_me_when_tty(stdin_is_terminal, me.as_deref()) {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("TTY input requires --me for duplex")
                    .with_hint("Provide --me NAME to send TTY line-mode messages."));
            }
            let format_flag = format;
            let format = format.unwrap_or(if jsonl {
                FollowFormat::Jsonl
            } else {
                FollowFormat::Pretty
            });
            let pretty = matches!(format, FollowFormat::Pretty);
            let now = now_ns()?;
            let since_ns = since
                .as_deref()
                .map(|value| parse_since(value, now))
                .transpose()?;
            let timeout_input = timeout.as_deref();
            let timeout = timeout_input.map(parse_duration).transpose()?;
            let exact_follow_create_hint = follow_exact_create_command_hint(
                &pool,
                tail,
                false,
                jsonl,
                timeout_input,
                false,
                format_flag,
                since.as_deref(),
                &[],
                &[],
                false,
                false,
                None,
            );
            let stop = Arc::new(AtomicBool::new(false));
            let cfg = FollowConfig {
                tail,
                pretty,
                one: false,
                timeout,
                data_only: false,
                since_ns,
                required_tags: Vec::new(),
                where_predicates: compile_filters(&[])?,
                quiet_drops: false,
                notify: true,
                color_mode,
                replay_speed: None,
                suppress_sender: if echo_self { None } else { me.clone() },
                stop: Some(stop.clone()),
            };

            #[derive(Clone, Copy)]
            enum DuplexSide {
                Follow,
                Send,
            }

            let (event_tx, event_rx) = mpsc::channel::<(DuplexSide, Result<RunOutcome, Error>)>();
            let target = resolve_pool_target(&pool, &pool_dir)?;
            match target {
                PoolTarget::LocalPath(path) => {
                    let exact_create_hint = Some(exact_follow_create_hint.clone());
                    let follow_pool_handle = match Pool::open(&path) {
                        Ok(pool_handle) => pool_handle,
                        Err(err) if create && err.kind() == ErrorKind::NotFound => {
                            ensure_pool_dir(&pool_dir)?;
                            Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE))?
                        }
                        Err(err) => {
                            return Err(add_missing_pool_create_hint(
                                err,
                                "duplex",
                                &pool,
                                &pool,
                                exact_create_hint,
                            ));
                        }
                    };
                    if let Some(since_ns) = since_ns {
                        if since_ns > now {
                            return Ok(RunOutcome::ok());
                        }
                    }
                    let mut send_pool = Pool::open(&path)?;
                    let follow_tx = event_tx.clone();
                    let follow_cfg = cfg.clone();
                    let stop_for_follow = stop.clone();
                    let pool_name = pool.clone();
                    let follow_path = path.clone();
                    let _ = std::thread::spawn(move || {
                        let outcome = super::follow_pool(
                            &follow_pool_handle,
                            &pool_name,
                            &follow_path,
                            follow_cfg,
                        );
                        if outcome.is_err() {
                            stop_for_follow.store(true, Ordering::Release);
                        }
                        let _ = follow_tx.send((DuplexSide::Follow, outcome));
                    });

                    let send_tx = event_tx;
                    let stop_for_send = stop.clone();
                    let me_for_send = me.clone();
                    let stdin_mode_terminal = stdin_is_terminal;
                    let _ = std::thread::spawn(move || {
                        if stdin_mode_terminal {
                            let mut reader = std::io::BufReader::new(io::stdin());
                            let mut outcome = RunOutcome::ok();
                            let mut have_input = false;
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
                                if n == 0 {
                                    break;
                                }
                                if follow_should_stop(Some(&stop_for_send)) {
                                    break;
                                }
                                let Some(value) = parse_duplex_tty_line(
                                    me_for_send.as_ref().expect("me required"),
                                    &line,
                                ) else {
                                    continue;
                                };
                                have_input = true;
                                let payload = lite3::encode_message(&Vec::<String>::new(), &value);
                                if let Err(err) = payload {
                                    let _ = send_tx.send((DuplexSide::Send, Err(err)));
                                    return;
                                }
                                let payload = payload.expect("payload");
                                if let Err(err) = retry_with_config(None, || {
                                    let timestamp_ns = now_ns()?;
                                    let options =
                                        AppendOptions::new(timestamp_ns, Durability::Fast);
                                    send_pool
                                        .append_with_options(payload.as_slice(), options)
                                        .map(|_| ())
                                }) {
                                    let _ = send_tx.send((DuplexSide::Send, Err(err)));
                                    return;
                                }
                            }
                            if have_input {
                                outcome = RunOutcome::ok();
                            }
                            let _ = send_tx.send((DuplexSide::Send, Ok(outcome)));
                        } else {
                            let pool_ref = pool.to_string();
                            let pool_path_label = path.display().to_string();
                            let mut send_pool = send_pool;
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
                            let outcome = match outcome {
                                Ok(outcome) => {
                                    if outcome.records_total == 0 {
                                        Err(missing_feed_data_error())
                                    } else if outcome.failed > 0 {
                                        Ok(RunOutcome::with_code(1))
                                    } else {
                                        Ok(RunOutcome::ok())
                                    }
                                }
                                Err(err) => Err(err),
                            };
                            let _ = send_tx.send((DuplexSide::Send, outcome));
                        };
                    });
                }
                PoolTarget::Remote {
                    base_url,
                    pool: name,
                } => {
                    if create {
                        return Err(Error::new(ErrorKind::Usage)
                            .with_message("remote duplex does not support --create")
                            .with_hint(
                                "Create remote pools with server-side tooling, then rerun duplex.",
                            ));
                    }
                    if since.is_some() {
                        return Err(Error::new(ErrorKind::Usage)
                            .with_message("remote duplex does not support --since")
                            .with_hint("Use --tail N for remote refs, or run --since against a local pool path."));
                    }
                    let client = RemoteClient::new(base_url.clone())?;
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
                    let me_for_send = me.clone();
                    let stdin_mode_terminal = stdin_is_terminal;
                    let _ = std::thread::spawn(move || {
                        if stdin_mode_terminal {
                            let mut reader = std::io::BufReader::new(io::stdin());
                            let mut outcome = RunOutcome::ok();
                            let mut have_input = false;
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
                                if n == 0 {
                                    break;
                                }
                                if follow_should_stop(Some(&stop_for_send)) {
                                    break;
                                }
                                let Some(value) = parse_duplex_tty_line(
                                    me_for_send.as_ref().expect("me required"),
                                    &line,
                                ) else {
                                    continue;
                                };
                                have_input = true;
                                if let Err(err) =
                                    remote_pool.append_json_now(&value, &[], Durability::Fast)
                                {
                                    let _ = send_tx.send((DuplexSide::Send, Err(err)));
                                    return;
                                }
                            }
                            if have_input {
                                outcome = RunOutcome::ok();
                            }
                            let _ = send_tx.send((DuplexSide::Send, Ok(outcome)));
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
                            let outcome = match outcome {
                                Ok(outcome) => {
                                    if outcome.records_total == 0 {
                                        Err(missing_feed_data_error())
                                    } else if outcome.failed > 0 {
                                        Ok(RunOutcome::with_code(1))
                                    } else {
                                        Ok(RunOutcome::ok())
                                    }
                                }
                                Err(err) => Err(err),
                            };
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
                Err(_) => Ok(RunOutcome::ok()),
            }
        }
        Command::Follow {
            pool,
            create,
            jsonl,
            tail,
            one,
            timeout,
            data_only,
            quiet_drops,
            no_notify,
            format,
            since,
            where_expr,
            tags,
            replay,
            token,
            token_file,
            tls_ca,
            tls_skip_verify,
        } => {
            if jsonl && format.is_some() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("conflicting output options")
                    .with_hint("Use --format jsonl (or --jsonl), but not both."));
            }
            let format_flag = format;
            let format = format.unwrap_or(if jsonl {
                FollowFormat::Jsonl
            } else {
                FollowFormat::Pretty
            });
            let pretty = matches!(format, FollowFormat::Pretty);
            let now = now_ns()?;
            let since_ns = since
                .as_deref()
                .map(|value| parse_since(value, now))
                .transpose()?;
            let timeout_input = timeout.as_deref();
            let timeout = timeout_input.map(parse_duration).transpose()?;
            let exact_follow_create_hint = follow_exact_create_command_hint(
                &pool,
                tail,
                one,
                jsonl,
                timeout_input,
                data_only,
                format_flag,
                since.as_deref(),
                &where_expr,
                &tags,
                quiet_drops,
                no_notify,
                replay,
            );
            let cfg = FollowConfig {
                tail,
                pretty,
                one,
                timeout,
                data_only,
                since_ns,
                required_tags: tags,
                where_predicates: compile_filters(&where_expr)?,
                quiet_drops,
                notify: !no_notify,
                color_mode,
                replay_speed: replay,
                suppress_sender: None,
                stop: None,
            };
            let target = resolve_pool_target(&pool, &pool_dir)?;
            match target {
                PoolTarget::LocalPath(path) => {
                    reject_remote_only_flags_for_local_target(
                        "follow",
                        token.as_deref(),
                        token_file.as_deref(),
                        tls_ca.as_deref(),
                        tls_skip_verify,
                    )?;
                    let exact_create_hint = Some(exact_follow_create_hint.clone());
                    if let Some(speed) = replay {
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
                        if tail == 0 && since.is_none() {
                            return Err(Error::new(ErrorKind::Usage)
                                .with_message("--replay requires --tail or --since")
                                .with_hint(
                                    "Replay needs historical messages. Use --tail N or --since DURATION.",
                                ));
                        }
                    }
                    let pool_handle = match Pool::open(&path) {
                        Ok(pool_handle) => pool_handle,
                        Err(err) if create && err.kind() == ErrorKind::NotFound => {
                            ensure_pool_dir(&pool_dir)?;
                            Pool::create(&path, PoolOptions::new(DEFAULT_POOL_SIZE))?
                        }
                        Err(err) => {
                            return Err(add_missing_pool_create_hint(
                                err,
                                "follow",
                                &pool,
                                &pool,
                                exact_create_hint,
                            ));
                        }
                    };
                    if let Some(since_ns) = since_ns {
                        if since_ns > now {
                            return Ok(RunOutcome::ok());
                        }
                    }
                    let outcome = follow_pool(&pool_handle, &pool, &path, cfg)?;
                    if outcome.exit_code == 124 {
                        if let Some(timeout_input) = timeout_input {
                            emit_follow_timeout_human(timeout_input);
                        }
                    }
                    Ok(outcome)
                }
                PoolTarget::Remote { base_url, pool } => {
                    if create {
                        return Err(Error::new(ErrorKind::Usage)
                            .with_message("remote follow does not support --create")
                            .with_hint(
                                "Create remote pools with server-side tooling, then rerun follow.",
                            ));
                    }
                    let token_value = resolve_token_value(token, token_file)?;
                    let mut client = RemoteClient::new(base_url)?;
                    if let Some(token_value) = token_value {
                        client = client.with_token(token_value);
                    }
                    if let Some(path) = tls_ca {
                        client = client.with_tls_ca_file(path)?;
                    }
                    if tls_skip_verify {
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
    }
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
