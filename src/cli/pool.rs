//! Purpose: Execute local pool lifecycle and inspection commands.
//! Exports: `run`.
//! Role: Own pool-command orchestration while core APIs own storage semantics.

use super::context::CliContext;
use super::output::emit_json;
use super::result::CommandResult;
use crate::pool_info_json::pool_info_json;
use crate::{
    DEFAULT_POOL_SIZE, PoolCommand, add_missing_pool_hint, display_pool_dir_for_humans,
    emit_pool_create_table, emit_pool_info_pretty, emit_pool_list_table, emit_table,
    ensure_pool_dir, error_json, list_pools, parse_size, resolve_poolref, short_display_path,
};
use plasmite::api::{Error, ErrorKind, LocalClient, PoolOptions, PoolRef, to_exit_code};
use serde_json::json;
use std::io::{self, IsTerminal};

pub(super) fn run(command: PoolCommand, context: &CliContext) -> Result<CommandResult, Error> {
    let pool_dir = context.pool_dir();
    match command {
        PoolCommand::Create {
            names,
            size,
            index_capacity,
            json: json_output,
        } => {
            let client = LocalClient::new().with_pool_dir(pool_dir);
            let size = size
                .as_deref()
                .map(parse_size)
                .transpose()?
                .unwrap_or(DEFAULT_POOL_SIZE);
            ensure_pool_dir(pool_dir)?;
            let mut results = Vec::new();
            for name in names {
                let path = resolve_poolref(&name, pool_dir)?;
                if path.exists() {
                    return Err(Error::new(ErrorKind::AlreadyExists)
                        .with_message("pool already exists")
                        .with_path(&path)
                        .with_hint("Choose a different name or remove the existing pool file."));
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
                let pool_ref = PoolRef::path(path);
                let info = client.create_pool(&pool_ref, options)?;
                results.push(pool_info_json(&name, &info));
            }
            if json_output {
                emit_json(json!({ "created": results }), context.color_mode());
            } else {
                emit_pool_create_table(&results, pool_dir);
            }
            Ok(CommandResult::ok())
        }
        PoolCommand::Info {
            name,
            json: json_output,
        } => {
            let client = LocalClient::new().with_pool_dir(pool_dir);
            let path = resolve_poolref(&name, pool_dir)?;
            let pool_ref = PoolRef::path(path);
            let info = client.pool_info(&pool_ref).map_err(|err| {
                if err.kind() == ErrorKind::NotFound {
                    let base = Error::new(ErrorKind::NotFound).with_message("not found");
                    add_missing_pool_hint(base, &name, &name)
                } else {
                    err
                }
            })?;
            if json_output {
                emit_json(pool_info_json(&name, &info), context.color_mode());
            } else {
                emit_pool_info_pretty(&name, &info);
            }
            Ok(CommandResult::ok())
        }
        PoolCommand::Delete {
            names,
            json: json_output,
        } => {
            let client = LocalClient::new().with_pool_dir(pool_dir);
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
                        .with_hint(
                            "Use pool names/paths for local delete, or call remote APIs directly.",
                        ))
                } else {
                    resolve_poolref(&name, pool_dir).and_then(|path| {
                        let pool_ref = PoolRef::path(path.clone());
                        client.delete_pool(&pool_ref).map_err(|err| {
                            if err.kind() == ErrorKind::NotFound {
                                Error::new(ErrorKind::NotFound)
                                    .with_message("pool not found")
                                    .with_path(&path)
                                    .with_hint("Create the pool first or check --dir.")
                            } else if err.kind() == ErrorKind::Permission {
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
                        let display_path = short_display_path(path.as_path(), Some(pool_dir));
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
                            .map(|path| short_display_path(path, Some(pool_dir)))
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

            if json_output {
                emit_json(
                    json!({
                        "deleted": deleted,
                        "failed": failed,
                    }),
                    context.color_mode(),
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
                                    display_pool_dir_for_humans(pool_dir)
                                );
                                println!("  List pools:     pls pool list");
                            }
                            HumanDeleteStatus::Err { detail, .. } => {
                                println!("Failed to delete pool \"{name}\".");
                                println!();
                                println!("  Reason:         {detail}");
                                println!(
                                    "  Pool directory: {}",
                                    display_pool_dir_for_humans(pool_dir)
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
                        display_pool_dir_for_humans(pool_dir)
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
                        display_pool_dir_for_humans(pool_dir)
                    );
                }
            } else {
                emit_table(&["NAME", "STATUS", "PATH", "DETAIL"], &table_rows);
            }
            if let Some(kind) = first_error_kind {
                Ok(CommandResult::with_code(to_exit_code(kind)))
            } else {
                Ok(CommandResult::ok())
            }
        }
        PoolCommand::List { json: json_output } => {
            let client = LocalClient::new().with_pool_dir(pool_dir);
            let pools = list_pools(pool_dir, &client);
            if json_output {
                emit_json(json!({ "pools": pools }), context.color_mode());
            } else {
                emit_pool_list_table(&pools, pool_dir);
            }
            Ok(CommandResult::ok())
        }
    }
}
