//! CLI helpers for pool commands.

use crate::pool_info_json::bounds_json;
use plasmite::api::Bounds;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::LocalClient;
use plasmite::api::PoolRef;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;

use super::output_support::display_pool_dir_for_humans;
use super::output_support::emit_table;
use super::output_support::error_json;
use super::output_support::error_message;
use super::output_support::format_bytes;
use super::output_support::format_relative_from_timestamp;
use super::output_support::format_relative_time;
use super::output_support::format_seq_range;
use super::output_support::format_system_time;
use super::output_support::format_timestamp_human;
use super::output_support::human_age;
use super::output_support::human_literal;
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

pub(crate) enum PoolListRow {
    Pool {
        name: String,
        path: String,
        file_size: u64,
        bounds: Bounds,
        mtime: Option<String>,
    },
    Error {
        name: String,
        path: String,
        error: Error,
    },
}

impl PoolListRow {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Pool { name, .. } | Self::Error { name, .. } => name,
        }
    }

    pub(crate) fn path(&self) -> &str {
        match self {
            Self::Pool { path, .. } | Self::Error { path, .. } => path,
        }
    }

    pub(crate) fn json(&self) -> Value {
        match self {
            Self::Pool {
                name,
                path,
                file_size,
                bounds,
                mtime,
            } => json!({
                "name": name,
                "path": path,
                "file_size": file_size,
                "bounds": bounds_json(*bounds),
                "mtime": mtime,
            }),
            Self::Error { name, path, error } => json!({
                "name": name,
                "path": path,
                "error": error_json(error),
            }),
        }
    }
}

pub(crate) fn list_pools(pool_dir: &Path, client: &LocalClient) -> Result<Vec<PoolListRow>, Error> {
    let mut pools = Vec::new();
    let entries = match std::fs::read_dir(pool_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(pools),
        Err(err) => {
            return Err(Error::new(ErrorKind::Io)
                .with_message("failed to read pool directory")
                .with_path(pool_dir)
                .with_source(err));
        }
    };

    for entry in entries {
        let entry = entry.map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read pool directory entry")
                .with_path(pool_dir)
                .with_source(err)
        })?;
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
        let mtime = meta.modified().ok().and_then(format_system_time);
        let pool_ref = PoolRef::path(path.clone());
        match client.pool_info(&pool_ref) {
            Ok(info) => {
                pools.push(PoolListRow::Pool {
                    name,
                    path: path.display().to_string(),
                    file_size: info.file_size,
                    bounds: info.bounds,
                    mtime,
                });
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

    pools.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(pools)
}

pub(crate) fn emit_pool_list_table(
    pools: &[PoolListRow],
    pool_dir: &Path,
    server: Option<&str>,
) -> Result<(), Error> {
    let interactive = io::stdout().is_terminal();
    if interactive && pools.is_empty() {
        if let Some(server) = server {
            super::output::write_stdout(
                format!("No pools found at {}\n", human_literal(server)).as_bytes(),
            )?;
        } else {
            super::output::write_stdout(
                format!(
                    "No pools found in {}\n",
                    display_pool_dir_for_humans(pool_dir)
                )
                .as_bytes(),
            )?;
            super::output::write_stdout(b"\n")?;
            super::output::write_stdout(b"  Create one: plasmite pool create <name>\n")?;
        }
        return Ok(());
    }

    let has_errors = pools
        .iter()
        .any(|pool| matches!(pool, PoolListRow::Error { .. }));
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
            let name = pool.name().to_string();
            let display_path = if server.is_some() {
                pool.path().to_string()
            } else {
                short_display_path(Path::new(pool.path()), Some(pool_dir))
            };

            match pool {
                PoolListRow::Error { error, .. } => vec![
                    name,
                    "ERR".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    display_path,
                    error_message(error),
                ],
                PoolListRow::Pool {
                    file_size,
                    bounds,
                    mtime,
                    ..
                } => {
                    let oldest = bounds.oldest_seq;
                    let newest = bounds.newest_seq;
                    let msg_count = message_count_from_bounds(oldest, newest);
                    let size = if interactive {
                        format_bytes(*file_size)
                    } else {
                        file_size.to_string()
                    };
                    let oldest_str = oldest
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let newest_str = newest
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let mtime = mtime
                        .as_deref()
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
            }
        })
        .collect::<Vec<_>>();

    emit_table(&headers, &rows)?;
    Ok(())
}

pub(crate) fn emit_pool_create_table(created: &[Value], pool_dir: &Path) -> Result<(), Error> {
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
                super::output::write_stdout(
                    format!(
                        "Created {} ({size}, {index} index slots)\n",
                        human_literal(name)
                    )
                    .as_bytes(),
                )?;
                super::output::write_stdout(
                    format!("  path: {}\n", human_literal(&path)).as_bytes(),
                )?;
            }
            return Ok(());
        }

        let size = created
            .first()
            .and_then(|pool| pool.get("file_size"))
            .and_then(|value| value.as_u64())
            .map(format_bytes)
            .unwrap_or_else(|| "-".to_string());
        super::output::write_stdout(
            format!("Created {} pools ({} each)\n", created.len(), size).as_bytes(),
        )?;
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
            super::output::write_stdout(
                format!("  - {} ({})\n", human_literal(name), human_literal(&path)).as_bytes(),
            )?;
        }
        return Ok(());
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
    emit_table(&headers, &rows)?;
    Ok(())
}

fn pool_list_error(name: &str, path: &Path, error: Error) -> PoolListRow {
    PoolListRow::Error {
        name: name.to_string(),
        path: path.display().to_string(),
        error,
    }
}

pub(crate) fn emit_pool_info_pretty(
    pool_ref: &str,
    info: &plasmite::api::PoolInfo,
) -> Result<(), Error> {
    if !io::stdout().is_terminal() {
        super::output::write_stdout(format!("Pool: {}\n", human_literal(pool_ref)).as_bytes())?;
        super::output::write_stdout(
            format!(
                "Path: {}\n",
                human_literal(&info.path.display().to_string())
            )
            .as_bytes(),
        )?;
        super::output::write_stdout(
            format!(
                "Size: {} bytes (index: offset={} slots={} bytes={}, ring: offset={} size={})\n",
                info.file_size,
                info.index_offset,
                info.index_capacity,
                info.index_size_bytes,
                info.ring_offset,
                info.ring_size
            )
            .as_bytes(),
        )?;

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
        super::output::write_stdout(
            format!("Bounds: oldest={oldest} newest={newest} count={count}\n").as_bytes(),
        )?;

        if let Some(metrics) = &info.metrics {
            let whole = metrics.utilization.used_percent_hundredths / 100;
            let frac = metrics.utilization.used_percent_hundredths % 100;
            super::output::write_stdout(
                format!(
                    "Utilization: used={}B free={}B ({}.{:02}%)\n",
                    metrics.utilization.used_bytes, metrics.utilization.free_bytes, whole, frac
                )
                .as_bytes(),
            )?;
            super::output::write_stdout(
                format!(
                    "Oldest: {} ({})\n",
                    human_literal(metrics.age.oldest_time.as_deref().unwrap_or("-")),
                    human_age(metrics.age.oldest_age_ms),
                )
                .as_bytes(),
            )?;
            super::output::write_stdout(
                format!(
                    "Newest: {} ({})\n",
                    human_literal(metrics.age.newest_time.as_deref().unwrap_or("-")),
                    human_age(metrics.age.newest_age_ms),
                )
                .as_bytes(),
            )?;
        }
        return Ok(());
    }

    let count = message_count_from_info(info);
    super::output::write_stdout(format!("{}\n", human_literal(pool_ref)).as_bytes())?;
    super::output::write_stdout(
        format!(
            "  path:      {}\n",
            human_literal(&short_display_path(&info.path, info.path.parent()))
        )
        .as_bytes(),
    )?;
    let messages_summary =
        format_pool_messages_summary(count, info.bounds.oldest_seq, info.bounds.newest_seq);
    if let Some(metrics) = &info.metrics {
        let whole = metrics.utilization.used_percent_hundredths / 100;
        let frac = metrics.utilization.used_percent_hundredths % 100;
        super::output::write_stdout(
            format!(
                "  size:      {} ({} used, {}.{:02}%)\n",
                format_bytes(info.file_size),
                format_bytes(metrics.utilization.used_bytes),
                whole,
                frac
            )
            .as_bytes(),
        )?;
        super::output::write_stdout(format!("  messages:  {messages_summary}\n").as_bytes())?;
        super::output::write_stdout(
            format!(
                "  oldest:    {}\n",
                format_pool_time_summary(
                    metrics.age.oldest_age_ms,
                    metrics.age.oldest_time.as_deref()
                )
            )
            .as_bytes(),
        )?;
        super::output::write_stdout(
            format!(
                "  newest:    {}\n",
                format_pool_time_summary(
                    metrics.age.newest_age_ms,
                    metrics.age.newest_time.as_deref()
                )
            )
            .as_bytes(),
        )?;
    } else {
        super::output::write_stdout(
            format!("  size:      {}\n", format_bytes(info.file_size)).as_bytes(),
        )?;
        super::output::write_stdout(format!("  messages:  {messages_summary}\n").as_bytes())?;
    }
    super::output::write_stdout(
        format!(
            "  index:     {} slots ({})\n",
            info.index_capacity,
            format_bytes(info.index_size_bytes)
        )
        .as_bytes(),
    )?;
    super::output::write_stdout(
        format!("  ring:      {}\n", format_bytes(info.ring_size)).as_bytes(),
    )?;
    Ok(())
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
        human_literal(&format_timestamp_human(timestamp))
    )
}
