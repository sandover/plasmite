//! Purpose: Execute conformance manifests against the Rust API implementation.
//! Exports: None (binary entry point).
//! Role: Reference runner for JSON conformance manifests.
//! Invariants: Manifests are JSON-only; steps execute in order; fail-fast on errors.
//! Invariants: Workdir is isolated under the manifest directory.

use plasmite::api::{Error, LocalClient, PoolApiExt, PoolOptions, PoolRef, TailOptions};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args();
    let _exe = args.next();
    let manifest_path = args
        .next()
        .ok_or_else(|| "usage: plasmite-conformance <path/to/manifest.json>".to_string())?;
    if args.next().is_some() {
        return Err("unexpected extra arguments".to_string());
    }

    let manifest_path = PathBuf::from(manifest_path);
    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let content = fs::read_to_string(&manifest_path)
        .map_err(|err| format!("failed to read manifest: {err}"))?;
    let manifest: Value = serde_json::from_str(&content)
        .map_err(|err| format!("failed to parse manifest json: {err}"))?;

    let version = manifest
        .get("conformance_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing conformance_version".to_string())?;
    if version != 0 {
        return Err(format!("unsupported conformance_version: {version}"));
    }

    let workdir = manifest
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or("work");
    let workdir_path = manifest_dir.join(workdir);
    reset_workdir(&workdir_path)?;

    let steps = manifest
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "manifest steps must be an array".to_string())?;

    let client = LocalClient::new().with_pool_dir(&workdir_path);

    for (index, step) in steps.iter().enumerate() {
        let step_id = step.get("id").and_then(Value::as_str).map(str::to_string);
        let op = step
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| step_err(index, &step_id, "missing op"))?;
        match op {
            "create_pool" => run_create_pool(&client, step, index, &step_id)?,
            "append" => run_append(&client, step, index, &step_id)?,
            "get" => run_get(&client, step, index, &step_id)?,
            "tail" => run_tail(&client, step, index, &step_id)?,
            "list_pools" => run_list_pools(&client, step, index, &step_id)?,
            "pool_info" => run_pool_info(&client, step, index, &step_id)?,
            "delete_pool" => run_delete_pool(&client, step, index, &step_id)?,
            "spawn_poke" => run_spawn_poke(&manifest_dir, &workdir_path, step, index, &step_id)?,
            "corrupt_pool_header" => run_corrupt_pool_header(&client, step, index, &step_id)?,
            "chmod_path" => run_chmod_path(step, index, &step_id)?,
            _ => return Err(step_err(index, &step_id, &format!("unknown op: {op}"))),
        }
    }

    Ok(())
}

fn reset_workdir(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path)
            .map_err(|err| format!("failed to clear workdir {}: {err}", path.display()))?;
    }
    fs::create_dir_all(path)
        .map_err(|err| format!("failed to create workdir {}: {err}", path.display()))?;
    Ok(())
}

fn pool_ref_from_value(value: &Value) -> Result<PoolRef, String> {
    let pool = value
        .as_str()
        .ok_or_else(|| "pool must be a string".to_string())?;
    if pool.contains('/') {
        Ok(PoolRef::path(pool))
    } else {
        Ok(PoolRef::name(pool))
    }
}

fn run_create_pool(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool = step
        .get("pool")
        .ok_or_else(|| step_err(index, step_id, "missing pool"))?;
    let pool_ref = pool_ref_from_value(pool).map_err(|err| step_err(index, step_id, &err))?;

    let size_bytes = step
        .get("input")
        .and_then(|input| input.get("size_bytes"))
        .and_then(Value::as_u64)
        .unwrap_or(1024 * 1024);
    let options = PoolOptions::new(size_bytes);

    match client.create_pool(&pool_ref, options) {
        Ok(_) => validate_expect_error(step.get("expect"), &Ok(()), index, step_id),
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_append(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool_ref = pool_ref_from_step(step, index, step_id)?;
    let input = step
        .get("input")
        .ok_or_else(|| step_err(index, step_id, "missing input"))?;
    let data = input
        .get("data")
        .ok_or_else(|| step_err(index, step_id, "missing input.data"))?;
    let tags = match input.get("tags") {
        Some(Value::Array(values)) => {
            parse_string_array(values.as_slice()).map_err(|err| step_err(index, step_id, &err))?
        }
        Some(_) => return Err(step_err(index, step_id, "tags must be array")),
        None => Vec::new(),
    };

    let result = client
        .open_pool(&pool_ref)
        .and_then(|mut pool| pool.append_json_now(data, &tags, plasmite::api::Durability::Fast));

    match result {
        Ok(message) => {
            if let Some(expected_seq) = step
                .get("expect")
                .and_then(|expect| expect.get("seq"))
                .and_then(Value::as_u64)
            {
                if message.seq != expected_seq {
                    return Err(step_err(
                        index,
                        step_id,
                        &format!("expected seq {expected_seq}, got {}", message.seq),
                    ));
                }
            }
            validate_expect_error(step.get("expect"), &Ok(()), index, step_id)
        }
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_get(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool_ref = pool_ref_from_step(step, index, step_id)?;
    let input = step
        .get("input")
        .ok_or_else(|| step_err(index, step_id, "missing input"))?;
    let seq = input
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| step_err(index, step_id, "missing input.seq"))?;

    let result = client
        .open_pool(&pool_ref)
        .and_then(|pool| pool.get_message(seq));

    match result {
        Ok(message) => {
            if let Some(expect) = step.get("expect") {
                expect_data(expect, &message.data, index, step_id)?;
                expect_tags(expect, &message.meta.tags, index, step_id)?;
            }
            validate_expect_error(step.get("expect"), &Ok(()), index, step_id)
        }
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_tail(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool_ref = pool_ref_from_step(step, index, step_id)?;
    let input = step.get("input").unwrap_or(&Value::Null);

    let mut options = TailOptions::default();
    if let Some(since_seq) = input.get("since_seq").and_then(Value::as_u64) {
        options.since_seq = Some(since_seq);
    }
    if let Some(max) = input.get("max").and_then(Value::as_u64) {
        options.max_messages = Some(max as usize);
    }

    let result = client.open_pool(&pool_ref).map(|pool| {
        let mut tail = pool.tail(options);
        let mut messages = Vec::new();
        while let Some(message) = tail.next_message()? {
            messages.push(message);
        }
        Ok::<_, Error>(messages)
    });

    match result {
        Ok(Ok(messages)) => {
            let expect = step
                .get("expect")
                .ok_or_else(|| step_err(index, step_id, "missing expect"))?;
            let ordered = expect.get("messages").and_then(Value::as_array);
            let unordered = expect.get("messages_unordered").and_then(Value::as_array);
            let (expected_messages, is_unordered) = match (ordered, unordered) {
                (Some(messages), None) => (messages, false),
                (None, Some(messages)) => (messages, true),
                (None, None) => {
                    return Err(step_err(
                        index,
                        step_id,
                        "expect.messages or expect.messages_unordered is required",
                    ));
                }
                (Some(_), Some(_)) => {
                    return Err(step_err(
                        index,
                        step_id,
                        "expect.messages and expect.messages_unordered are mutually exclusive",
                    ));
                }
            };

            for window in messages.windows(2) {
                if window[0].seq >= window[1].seq {
                    return Err(step_err(index, step_id, "tail messages out of order"));
                }
            }

            if messages.len() != expected_messages.len() {
                return Err(step_err(
                    index,
                    step_id,
                    &format!(
                        "expected {} messages, got {}",
                        expected_messages.len(),
                        messages.len()
                    ),
                ));
            }

            if is_unordered {
                let mut remaining: Vec<&plasmite::api::Message> = messages.iter().collect();
                for expected in expected_messages.iter() {
                    let mut pos = None;
                    for (idx, actual) in remaining.iter().enumerate() {
                        match matches_expected_message(expected, actual) {
                            Ok(true) => {
                                pos = Some(idx);
                                break;
                            }
                            Ok(false) => continue,
                            Err(err) => {
                                return Err(step_err(index, step_id, &err));
                            }
                        }
                    }
                    if let Some(pos) = pos {
                        remaining.remove(pos);
                    } else {
                        return Err(step_err(index, step_id, "message mismatch"));
                    }
                }
            } else {
                for (idx, expected) in expected_messages.iter().enumerate() {
                    let actual = &messages[idx];
                    expect_data(expected, &actual.data, index, step_id)?;
                    expect_tags(expected, &actual.meta.tags, index, step_id)?;
                }
            }

            validate_expect_error(step.get("expect"), &Ok(()), index, step_id)
        }
        Ok(Err(err)) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_list_pools(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let result = client.list_pools();
    match result {
        Ok(pools) => {
            if let Some(expect) = step.get("expect") {
                if let Some(expected) = expect.get("names") {
                    let expected = match expected.as_array() {
                        Some(values) => parse_string_array(values.as_slice())
                            .map_err(|err| step_err(index, step_id, &err))?,
                        None => return Err(step_err(index, step_id, "expect.names must be array")),
                    };
                    let mut actual = pools
                        .iter()
                        .filter_map(|pool| pool.path.file_stem().and_then(|name| name.to_str()))
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    let mut expected = expected;
                    actual.sort();
                    expected.sort();
                    if actual != expected {
                        return Err(step_err(index, step_id, "pool list mismatch"));
                    }
                }
            }
            validate_expect_error(step.get("expect"), &Ok(()), index, step_id)
        }
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_pool_info(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool_ref = pool_ref_from_step(step, index, step_id)?;
    let result = client.pool_info(&pool_ref);
    match result {
        Ok(info) => {
            if let Some(expect) = step.get("expect") {
                if let Some(size) = expect.get("file_size").and_then(Value::as_u64) {
                    if info.file_size != size {
                        return Err(step_err(index, step_id, "file_size mismatch"));
                    }
                }
                if let Some(size) = expect.get("ring_size").and_then(Value::as_u64) {
                    if info.ring_size != size {
                        return Err(step_err(index, step_id, "ring_size mismatch"));
                    }
                }
                if let Some(bounds) = expect.get("bounds") {
                    expect_bounds(bounds, &info.bounds, index, step_id)?;
                }
            }
            validate_expect_error(step.get("expect"), &Ok(()), index, step_id)
        }
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_delete_pool(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool_ref = pool_ref_from_step(step, index, step_id)?;
    let result = client.delete_pool(&pool_ref);
    match result {
        Ok(()) => validate_expect_error(step.get("expect"), &Ok(()), index, step_id),
        Err(err) => validate_expect_error(step.get("expect"), &Err(err), index, step_id),
    }
}

fn run_spawn_poke(
    manifest_dir: &Path,
    workdir_path: &Path,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool = step
        .get("pool")
        .and_then(Value::as_str)
        .ok_or_else(|| step_err(index, step_id, "missing pool"))?;
    let input = step
        .get("input")
        .ok_or_else(|| step_err(index, step_id, "missing input"))?;
    let messages = input
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| step_err(index, step_id, "input.messages must be array"))?;
    let plasmite_bin = resolve_plasmite_bin(manifest_dir)?;

    let mut children = Vec::new();
    for message in messages {
        let payload = message
            .get("data")
            .ok_or_else(|| step_err(index, step_id, "message.data is required"))?;
        let payload = serde_json::to_string(payload)
            .map_err(|err| step_err(index, step_id, &format!("encode payload failed: {err}")))?;
        let tags = message
            .get("tags")
            .and_then(Value::as_array)
            .map(|values| parse_string_array(values.as_slice()))
            .transpose()
            .map_err(|err| step_err(index, step_id, &err))?;

        let mut cmd = Command::new(&plasmite_bin);
        cmd.arg("--dir")
            .arg(workdir_path)
            .arg("feed")
            .arg(pool)
            .arg(payload)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(tags) = tags {
            for tag in tags {
                cmd.arg("--tag").arg(tag);
            }
        }
        let child = cmd
            .spawn()
            .map_err(|err| step_err(index, step_id, &format!("spawn feed failed: {err}")))?;
        children.push(child);
    }

    for mut child in children {
        let status = child
            .wait()
            .map_err(|err| step_err(index, step_id, &format!("feed wait failed: {err}")))?;
        if !status.success() {
            return Err(step_err(index, step_id, "feed process failed"));
        }
    }

    Ok(())
}

fn run_corrupt_pool_header(
    client: &LocalClient,
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let pool_ref = pool_ref_from_step(step, index, step_id)?;
    let pool = client
        .open_pool(&pool_ref)
        .map_err(|err| step_err(index, step_id, &format!("open_pool failed: {err}")))?;
    let path = pool
        .info()
        .map_err(|err| step_err(index, step_id, &format!("info failed: {err}")))?
        .path;
    std::fs::write(&path, b"NOPE").map_err(|err| {
        step_err(
            index,
            step_id,
            &format!("failed to corrupt pool header: {err}"),
        )
    })?;
    Ok(())
}

fn run_chmod_path(step: &Value, index: usize, step_id: &Option<String>) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let input = step
            .get("input")
            .ok_or_else(|| step_err(index, step_id, "missing input"))?;
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| step_err(index, step_id, "missing input.path"))?;
        let mode = input
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| step_err(index, step_id, "missing input.mode"))?;
        let mode = u32::from_str_radix(mode, 8)
            .map_err(|_| step_err(index, step_id, "invalid input.mode"))?;
        let mut perms = std::fs::metadata(path)
            .map_err(|err| step_err(index, step_id, &format!("chmod metadata failed: {err}")))?
            .permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(path, perms)
            .map_err(|err| step_err(index, step_id, &format!("chmod failed: {err}")))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (step, index, step_id);
        Err(step_err(
            index,
            step_id,
            "chmod_path is not supported on this platform",
        ))
    }
}

fn validate_expect_error(
    expect: Option<&Value>,
    result: &Result<(), Error>,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    let Some(expect) = expect else {
        return match result {
            Ok(_) => Ok(()),
            Err(err) => Err(step_err(
                index,
                step_id,
                &format!("unexpected error: {err}"),
            )),
        };
    };
    let Some(expect_error) = expect.get("error") else {
        return match result {
            Ok(_) => Ok(()),
            Err(err) => Err(step_err(
                index,
                step_id,
                &format!("unexpected error: {err}"),
            )),
        };
    };

    let err = result
        .as_ref()
        .err()
        .ok_or_else(|| step_err(index, step_id, "expected error but operation succeeded"))?;

    let kind = expect_error
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| step_err(index, step_id, "expect.error.kind is required"))?;
    if kind != error_kind_label(err.kind()) {
        return Err(step_err(
            index,
            step_id,
            &format!(
                "expected error kind {kind}, got {}",
                error_kind_label(err.kind())
            ),
        ));
    }

    if let Some(substr) = expect_error.get("message_contains").and_then(Value::as_str) {
        let message = err.message().unwrap_or("");
        if !message.contains(substr) {
            return Err(step_err(
                index,
                step_id,
                &format!("expected message to contain '{substr}', got '{message}'"),
            ));
        }
    }

    if let Some(has_path) = expect_error.get("has_path").and_then(Value::as_bool) {
        if has_path != err.path().is_some() {
            return Err(step_err(index, step_id, "path presence mismatch"));
        }
    }
    if let Some(has_seq) = expect_error.get("has_seq").and_then(Value::as_bool) {
        if has_seq != err.seq().is_some() {
            return Err(step_err(index, step_id, "seq presence mismatch"));
        }
    }
    if let Some(has_offset) = expect_error.get("has_offset").and_then(Value::as_bool) {
        if has_offset != err.offset().is_some() {
            return Err(step_err(index, step_id, "offset presence mismatch"));
        }
    }

    Ok(())
}

fn error_kind_label(kind: plasmite::api::ErrorKind) -> &'static str {
    match kind {
        plasmite::api::ErrorKind::Internal => "Internal",
        plasmite::api::ErrorKind::Usage => "Usage",
        plasmite::api::ErrorKind::NotFound => "NotFound",
        plasmite::api::ErrorKind::AlreadyExists => "AlreadyExists",
        plasmite::api::ErrorKind::Busy => "Busy",
        plasmite::api::ErrorKind::Permission => "Permission",
        plasmite::api::ErrorKind::Corrupt => "Corrupt",
        plasmite::api::ErrorKind::Io => "Io",
    }
}

fn pool_ref_from_step(
    step: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<PoolRef, String> {
    let pool = step
        .get("pool")
        .ok_or_else(|| step_err(index, step_id, "missing pool"))?;
    pool_ref_from_value(pool).map_err(|err| step_err(index, step_id, &err))
}

fn expect_data(
    expect: &Value,
    actual: &Value,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    if let Some(expected_data) = expect.get("data") {
        if expected_data != actual {
            return Err(step_err(index, step_id, "data mismatch"));
        }
    }
    Ok(())
}

fn expect_tags(
    expect: &Value,
    actual: &[String],
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    if let Some(expected_tags) = expect.get("tags") {
        let expected = match expected_tags.as_array() {
            Some(values) => parse_string_array(values.as_slice())
                .map_err(|err| step_err(index, step_id, &err))?,
            None => return Err(step_err(index, step_id, "tags must be array")),
        };
        if expected != actual {
            return Err(step_err(index, step_id, "tags mismatch"));
        }
    }
    Ok(())
}

fn expect_bounds(
    expect: &Value,
    actual: &plasmite::api::Bounds,
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    if let Some(oldest) = expect.get("oldest") {
        let expected = if oldest.is_null() {
            None
        } else {
            Some(oldest.as_u64().ok_or_else(|| {
                step_err(index, step_id, "bounds.oldest must be a number or null")
            })?)
        };
        if actual.oldest_seq != expected {
            return Err(step_err(index, step_id, "bounds.oldest mismatch"));
        }
    }
    if let Some(newest) = expect.get("newest") {
        let expected = if newest.is_null() {
            None
        } else {
            Some(newest.as_u64().ok_or_else(|| {
                step_err(index, step_id, "bounds.newest must be a number or null")
            })?)
        };
        if actual.newest_seq != expected {
            return Err(step_err(index, step_id, "bounds.newest mismatch"));
        }
    }
    Ok(())
}

fn parse_string_array(values: &[Value]) -> Result<Vec<String>, String> {
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "expected string array".to_string())
        })
        .collect()
}

fn resolve_plasmite_bin(manifest_dir: &Path) -> Result<PathBuf, String> {
    if let Ok(value) = env::var("PLASMITE_BIN") {
        return Ok(PathBuf::from(value));
    }
    let root = manifest_dir.parent().unwrap_or(manifest_dir);
    let candidate = root.join("target").join("debug").join("plasmite");
    if candidate.exists() {
        return Ok(candidate);
    }
    Err("plasmite binary not found; set PLASMITE_BIN or build target/debug/plasmite".to_string())
}

fn matches_expected_message(
    expected: &Value,
    actual: &plasmite::api::Message,
) -> Result<bool, String> {
    let expected_data = expected
        .get("data")
        .ok_or_else(|| "expected message data is required".to_string())?;
    if expected_data != &actual.data {
        return Ok(false);
    }
    if let Some(expected_tags) = expected.get("tags") {
        let expected = match expected_tags.as_array() {
            Some(values) => parse_string_array(values.as_slice())?,
            None => return Err("tags must be array".to_string()),
        };
        if expected != actual.meta.tags {
            return Ok(false);
        }
    }
    Ok(true)
}

fn step_err(index: usize, step_id: &Option<String>, message: &str) -> String {
    let mut out = format!("step {index}");
    if let Some(id) = step_id {
        out.push_str(&format!(" ({id})"));
    }
    out.push_str(": ");
    out.push_str(message);
    out
}
