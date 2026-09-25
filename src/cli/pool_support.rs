//! CLI helpers for pool commands.

use crate::pool_info_json::bounds_json;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::LocalClient;
use plasmite::api::PoolRef;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;

use super::output_support::display_pool_dir_for_humans;
use super::output_support::emit_table;
use super::output_support::error_json;
use super::output_support::format_bytes;
use super::output_support::format_relative_from_timestamp;
use super::output_support::format_relative_time;
use super::output_support::format_seq_range;
use super::output_support::format_system_time;
use super::output_support::format_timestamp_human;
use super::output_support::human_age;
use super::output_support::short_display_path;
use super::support::add_corrupt_hint;
use super::support::add_io_hint;

pub(crate) fn list_pool_paths(pool_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = std::fs::read_dir(pool_dir).map_err(|err| {
        let kind = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorKind::Permission,
            _ => ErrorKind::Io,
        };
        Error::new(kind)
            .with_message("failed to read pool directory")
            .with_path(pool_dir)
            .with_source(err)
    })?;

    let mut pools = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read pool directory entry")
                .with_path(pool_dir)
                .with_source(err)
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("plasmite") {
            pools.push(path);
        }
    }
    Ok(pools)
}

pub(crate) fn list_pools(pool_dir: &Path, client: &LocalClient) -> Vec<Value> {
    let mut pools = Vec::new();
    let entries = match std::fs::read_dir(pool_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return pools,
        Err(err) => {
            pools.push(pool_list_error(
                "pools",
                pool_dir,
                Error::new(ErrorKind::Io)
                    .with_message("failed to read pool directory")
                    .with_path(pool_dir)
                    .with_source(err),
            ));
            return pools;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("plasmite") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                pools.push(pool_list_error(
                    &name,
                    &path,
                    Error::new(ErrorKind::Io)
                        .with_message("failed to stat pool")
                        .with_path(&path)
                        .with_source(err),
                ));
                continue;
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(format_system_time)
            .map(Value::String)
            .unwrap_or(Value::Null);
        let pool_ref = PoolRef::path(path.clone());
        match client.pool_info(&pool_ref) {
            Ok(info) => {
                let mut map = Map::new();
                map.insert("name".to_string(), json!(name));
                map.insert("path".to_string(), json!(path.display().to_string()));
                map.insert("file_size".to_string(), json!(info.file_size));
                map.insert("bounds".to_string(), bounds_json(info.bounds));
                map.insert("mtime".to_string(), mtime);
                pools.push(Value::Object(map));
            }
            Err(err) => {
                pools.push(pool_list_error(
                    &name,
                    &path,
                    add_corrupt_hint(add_io_hint(err)),
                ));
            }
        }
    }

    pools.sort_by_key(pool_list_name);
    pools
}

pub(crate) fn emit_pool_list_table(pools: &[Value], pool_dir: &Path) {
    let interactive = io::stdout().is_terminal();
    if interactive && pools.is_empty() {
        println!(
            "No pools found in {}",
            display_pool_dir_for_humans(pool_dir)
        );
        println!();
        println!("  Create one: plasmite pool create <name>");
        return;
    }

    let has_errors = pools.iter().any(|pool| {
        pool.get("error")
            .and_then(|value| value.get("error"))
            .is_some()
    });
    let headers = if interactive && !has_errors {
        vec!["NAME", "SIZE", "MSGS", "MODIFIED", "PATH"]
    } else {
        vec![
            "NAME", "STATUS", "SIZE", "OLDEST", "NEWEST", "MTIME", "PATH", "DETAIL",
        ]
    };
    let rows = pools
        .iter()
        .map(|pool| {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("-")
                .to_string();
            let path_value = pool
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            let display_path = if path_value == "-" {
                "-".to_string()
            } else {
                short_display_path(Path::new(path_value), Some(pool_dir))
            };

            if let Some(error) = pool.get("error").and_then(|value| value.get("error")) {
                let detail = error
                    .get("message")
                    .and_then(|value| value.as_str())
                    .or_else(|| error.get("kind").and_then(|value| value.as_str()))
                    .unwrap_or("error")
                    .to_string();
                vec![
                    name,
                    "ERR".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    display_path,
                    detail,
                ]
            } else {
                let oldest = pool
                    .get("bounds")
                    .and_then(|value| value.get("oldest"))
                    .and_then(|value| value.as_u64());
                let newest = pool
                    .get("bounds")
                    .and_then(|value| value.get("newest"))
                    .and_then(|value| value.as_u64());
                let msg_count = match (oldest, newest) {
                    (Some(a), Some(b)) => b.saturating_sub(a).saturating_add(1),
                    _ => 0,
                };
                let size = pool
                    .get("file_size")
                    .and_then(|value| value.as_u64())
                    .map(|value| {
                        if interactive {
                            format_bytes(value)
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                let oldest_str = oldest
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let newest_str = newest
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let mtime = pool
                    .get("mtime")
                    .and_then(|value| value.as_str())
                    .map(|value| {
                        if interactive {
                            format_relative_from_timestamp(value)
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_else(|| "-".to_string());
                if interactive && !has_errors {
                    vec![name, size, msg_count.to_string(), mtime, display_path]
                } else {
                    vec![
                        name,
                        "OK".to_string(),
                        size,
                        oldest_str,
                        newest_str,
                        mtime,
                        display_path,
                        String::new(),
                    ]
                }
            }
        })
        .collect::<Vec<_>>();

    emit_table(&headers, &rows);
}

pub(crate) fn emit_pool_create_table(created: &[Value], pool_dir: &Path) {
    if io::stdout().is_terminal() {
        if created.len() == 1 {
            if let Some(pool) = created.first() {
                let name = pool
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("pool");
                let size = pool
                    .get("file_size")
                    .and_then(|value| value.as_u64())
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string());
                let index = pool
                    .get("index_capacity")
                    .and_then(|value| value.as_u64())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let path = pool
                    .get("path")
                    .and_then(|value| value.as_str())
                    .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                    .unwrap_or_else(|| "-".to_string());
                println!("Created {name} ({size}, {index} index slots)");
                println!("  path: {path}");
            }
            return;
        }

        let size = created
            .first()
            .and_then(|pool| pool.get("file_size"))
            .and_then(|value| value.as_u64())
            .map(format_bytes)
            .unwrap_or_else(|| "-".to_string());
        println!("Created {} pools ({} each)", created.len(), size);
        for pool in created {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("pool");
            let path = pool
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                .unwrap_or_else(|| "-".to_string());
            println!("  - {name} ({path})");
        }
        return;
    }

    let headers = ["NAME", "SIZE", "INDEX", "PATH"];
    let rows = created
        .iter()
        .map(|pool| {
            let name = pool
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("-")
                .to_string();
            let size = pool
                .get("file_size")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let index = pool
                .get("index_capacity")
                .and_then(|value| value.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let path = pool
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| short_display_path(Path::new(value), Some(pool_dir)))
                .unwrap_or_else(|| "-".to_string());
            vec![name, size, index, path]
        })
        .collect::<Vec<_>>();
    emit_table(&headers, &rows);
}

pub(crate) fn pool_list_error(name: &str, path: &Path, err: Error) -> Value {
    let mut map = Map::new();
    map.insert("name".to_string(), json!(name));
    map.insert("path".to_string(), json!(path.display().to_string()));
    map.insert("error".to_string(), error_json(&err));
    Value::Object(map)
}

pub(crate) fn pool_list_name(value: &Value) -> String {
    value
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or("")
        .to_string()
}

pub(crate) fn emit_pool_info_pretty(pool_ref: &str, info: &plasmite::api::PoolInfo) {
    if !io::stdout().is_terminal() {
        println!("Pool: {pool_ref}");
        println!("Path: {}", info.path.display());
        println!(
            "Size: {} bytes (index: offset={} slots={} bytes={}, ring: offset={} size={})",
            info.file_size,
            info.index_offset,
            info.index_capacity,
            info.index_size_bytes,
            info.ring_offset,
            info.ring_size
        );

        let oldest = info
            .bounds
            .oldest_seq
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let newest = info
            .bounds
            .newest_seq
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        let count = info
            .metrics
            .as_ref()
            .map(|metrics| metrics.message_count)
            .unwrap_or_else(|| match (info.bounds.oldest_seq, info.bounds.newest_seq) {
                (Some(oldest), Some(newest)) => newest.saturating_sub(oldest).saturating_add(1),
                _ => 0,
            });
        println!("Bounds: oldest={oldest} newest={newest} count={count}");

        if let Some(metrics) = &info.metrics {
            let whole = metrics.utilization.used_percent_hundredths / 100;
            let frac = metrics.utilization.used_percent_hundredths % 100;
            println!(
                "Utilization: used={}B free={}B ({}.{:02}%)",
                metrics.utilization.used_bytes, metrics.utilization.free_bytes, whole, frac
            );
            println!(
                "Oldest: {} ({})",
                metrics.age.oldest_time.as_deref().unwrap_or("-"),
                human_age(metrics.age.oldest_age_ms),
            );
            println!(
                "Newest: {} ({})",
                metrics.age.newest_time.as_deref().unwrap_or("-"),
                human_age(metrics.age.newest_age_ms),
            );
        }
        return;
    }

    let count = message_count_from_info(info);
    println!("{pool_ref}");
    println!(
        "  path:      {}",
        short_display_path(&info.path, info.path.parent())
    );
    let messages_summary =
        format_pool_messages_summary(count, info.bounds.oldest_seq, info.bounds.newest_seq);
    if let Some(metrics) = &info.metrics {
        let whole = metrics.utilization.used_percent_hundredths / 100;
        let frac = metrics.utilization.used_percent_hundredths % 100;
        println!(
            "  size:      {} ({} used, {}.{:02}%)",
            format_bytes(info.file_size),
            format_bytes(metrics.utilization.used_bytes),
            whole,
            frac
        );
        println!("  messages:  {messages_summary}");
        println!(
            "  oldest:    {}",
            format_pool_time_summary(
                metrics.age.oldest_age_ms,
                metrics.age.oldest_time.as_deref()
            )
        );
        println!(
            "  newest:    {}",
            format_pool_time_summary(
                metrics.age.newest_age_ms,
                metrics.age.newest_time.as_deref()
            )
        );
    } else {
        println!("  size:      {}", format_bytes(info.file_size));
        println!("  messages:  {messages_summary}");
    }
    println!(
        "  index:     {} slots ({})",
        info.index_capacity,
        format_bytes(info.index_size_bytes)
    );
    println!("  ring:      {}", format_bytes(info.ring_size));
}

pub(crate) fn message_count_from_info(info: &plasmite::api::PoolInfo) -> u64 {
    info.metrics
        .as_ref()
        .map(|metrics| metrics.message_count)
        .unwrap_or_else(|| {
            message_count_from_bounds(info.bounds.oldest_seq, info.bounds.newest_seq)
        })
}

pub(crate) fn message_count_from_bounds(oldest: Option<u64>, newest: Option<u64>) -> u64 {
    match (oldest, newest) {
        (Some(oldest), Some(newest)) if newest >= oldest => {
            newest.saturating_sub(oldest).saturating_add(1)
        }
        _ => 0,
    }
}

pub(crate) fn format_pool_messages_summary(
    count: u64,
    oldest: Option<u64>,
    newest: Option<u64>,
) -> String {
    let seq_range = format_seq_range(oldest, newest);
    if count == 0 {
        if seq_range == "-" {
            return "empty".to_string();
        }
        return format!("0 visible ({seq_range})");
    }
    if seq_range == "-" {
        return count.to_string();
    }
    format!("{count} ({seq_range})")
}

pub(crate) fn format_pool_time_summary(age_ms: Option<u64>, timestamp: Option<&str>) -> String {
    let Some(timestamp) = timestamp else {
        return "—".to_string();
    };
    format!(
        "{} ({})",
        format_relative_time(age_ms),
        format_timestamp_human(timestamp)
    )
}
