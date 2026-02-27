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
                let info = client
                    .pool_info(&pool_ref)
                    .map_err(|err| {
                        if err.kind() == ErrorKind::NotFound {
                            let base =
                                Error::new(ErrorKind::NotFound).with_message("not found");
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
                                name,
                                "OK".to_string(),
                                display_path,
                                String::new(),
                            ]);
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
                                "pool": name,
                                "error": error_json(&err)["error"].clone(),
                            }));
                            table_rows.push(vec![name, "ERR".to_string(), display_path, detail]);
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
                    let total = table_rows.len();
                    let deleted_count = deleted.len();
                    if total == 1 && deleted_count == 1 {
                        if let Some(row) = table_rows.first() {
                            println!("Deleted {}", row.first().cloned().unwrap_or_default());
                            println!("  path: {}", row.get(2).cloned().unwrap_or_default());
                        }
                    } else if failed.is_empty() {
                        println!("Deleted {deleted_count} pools");
                        for row in table_rows
                            .iter()
                            .filter(|row| row.get(1).is_some_and(|value| value == "OK"))
                        {
                            println!(
                                "  - {} ({})",
                                row.first().cloned().unwrap_or_default(),
                                row.get(2).cloned().unwrap_or_default()
                            );
                        }
                    } else {
                        println!("Deleted {deleted_count} of {total} pools");
                        emit_table(&["NAME", "STATUS", "PATH", "DETAIL"], &table_rows);
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
                    Ok(outcome)
                }
            }
        }
    }
}
