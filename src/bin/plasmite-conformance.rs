//! Purpose: Execute conformance manifests against the Rust API implementation.
//! Exports: None (binary entry point).
//! Role: Reference runner for JSON conformance manifests.
//! Invariants: Manifests are JSON-only; steps execute in order; fail-fast on errors.
//! Invariants: Workdir is isolated under the manifest directory.

use plasmite::api::{LocalClient, PoolApiExt, PoolOptions, PoolRef, TailOptions};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

    let result = client.create_pool(&pool_ref, options);
    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(step_err(
            index,
            step_id,
            &format!("create_pool failed: {err}"),
        )),
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
    let descrips = match input.get("descrips") {
        Some(Value::Array(values)) => {
            parse_string_array(values.as_slice()).map_err(|err| step_err(index, step_id, &err))?
        }
        Some(_) => return Err(step_err(index, step_id, "descrips must be array")),
        None => Vec::new(),
    };

    let mut pool = client
        .open_pool(&pool_ref)
        .map_err(|err| step_err(index, step_id, &format!("open_pool failed: {err}")))?;
    let message = pool
        .append_json_now(data, &descrips, plasmite::api::Durability::Fast)
        .map_err(|err| step_err(index, step_id, &format!("append failed: {err}")))?;

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

    Ok(())
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

    let pool = client
        .open_pool(&pool_ref)
        .map_err(|err| step_err(index, step_id, &format!("open_pool failed: {err}")))?;
    let message = pool
        .get_message(seq)
        .map_err(|err| step_err(index, step_id, &format!("get failed: {err}")))?;

    if let Some(expect) = step.get("expect") {
        expect_data(expect, &message.data, index, step_id)?;
        expect_descrips(expect, &message.meta.descrips, index, step_id)?;
    }

    Ok(())
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

    let pool = client
        .open_pool(&pool_ref)
        .map_err(|err| step_err(index, step_id, &format!("open_pool failed: {err}")))?;
    let mut tail = pool.tail(options);

    let mut messages = Vec::new();
    while let Some(message) = tail
        .next_message()
        .map_err(|err| step_err(index, step_id, &format!("tail failed: {err}")))?
    {
        messages.push(message);
    }

    let expect = step
        .get("expect")
        .ok_or_else(|| step_err(index, step_id, "missing expect"))?;
    let expected_messages = expect
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| step_err(index, step_id, "expect.messages must be array"))?;

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

    for (idx, expected) in expected_messages.iter().enumerate() {
        let actual = &messages[idx];
        expect_data(expected, &actual.data, index, step_id)?;
        expect_descrips(expected, &actual.meta.descrips, index, step_id)?;
    }

    Ok(())
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

fn expect_descrips(
    expect: &Value,
    actual: &[String],
    index: usize,
    step_id: &Option<String>,
) -> Result<(), String> {
    if let Some(expected_descrips) = expect.get("descrips") {
        let expected = match expected_descrips.as_array() {
            Some(values) => parse_string_array(values.as_slice())
                .map_err(|err| step_err(index, step_id, &err))?,
            None => return Err(step_err(index, step_id, "descrips must be array")),
        };
        if expected != actual {
            return Err(step_err(index, step_id, "descrips mismatch"));
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

fn step_err(index: usize, step_id: &Option<String>, message: &str) -> String {
    let mut out = format!("step {index}");
    if let Some(id) = step_id {
        out.push_str(&format!(" ({id})"));
    }
    out.push_str(": ");
    out.push_str(message);
    out
}
