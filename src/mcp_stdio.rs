//! Purpose: Run the local MCP server over stdin/stdout.
//! Exports: `serve`.
//! Role: Bridge newline-delimited JSON-RPC lines to the shared MCP dispatcher.
//! Invariants: stdout only emits JSON-RPC messages (one JSON value per line).
//! Invariants: stdin EOF exits cleanly without side effects.
//! Invariants: Parse/protocol errors are surfaced as JSON-RPC error responses.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use plasmite::api::{Error, ErrorKind};
use plasmite::mcp::{
    DispatchOutcome, JsonRpcError, JsonRpcId, McpDispatcher, PlasmiteMcpHandler, parse_jsonrpc_line,
};
use serde_json::{Map, Value, json};

type InFlight = Mutex<HashMap<JsonRpcId, Arc<AtomicBool>>>;

struct Job {
    payload: Result<Value, JsonRpcError>,
    id: Option<JsonRpcId>,
    cancel: Option<Arc<AtomicBool>>,
}

pub(super) fn serve(pool_dir: PathBuf) -> Result<(), Error> {
    let pending = InFlight::default();
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| dispatch_requests(receiver, &pending, pool_dir));
        let input = read_requests(sender, &pending);
        let output = worker.join().map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("MCP request worker failed")
        })?;
        input.and(output)
    })
}

fn read_requests(sender: mpsc::Sender<Job>, pending: &InFlight) -> Result<(), Error> {
    let mut reader = BufReader::new(io::stdin().lock());
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read MCP request")
                .with_source(err)
        })?;
        if bytes == 0 {
            return Ok(());
        }
        let message = line.trim_end_matches(['\n', '\r']);
        if message.is_empty() {
            continue;
        }
        let payload = parse_jsonrpc_line(message);
        if let Ok(request) = &payload
            && request.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && request.get("id").is_none()
            && request.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
        {
            if let Some(id) = request.pointer("/params/requestId").and_then(request_id)
                && let Some(flag) = pending.lock().unwrap().get(&id)
            {
                flag.store(true, Ordering::Release);
            }
            continue;
        }
        let id = payload
            .as_ref()
            .ok()
            .and_then(|request| request.get("id"))
            .and_then(request_id);
        let cancel = if let Some(id) = &id {
            let mut pending = pending.lock().unwrap();
            if pending.contains_key(id) {
                // A request ID identifies one in-flight request. Don't enqueue a duplicate.
                continue;
            }
            let flag = Arc::new(AtomicBool::new(false));
            pending.insert(id.clone(), flag.clone());
            Some(flag)
        } else {
            None
        };
        if sender
            .send(Job {
                payload,
                id,
                cancel,
            })
            .is_err()
        {
            return Ok(());
        }
    }
}

fn request_id(id: &Value) -> Option<JsonRpcId> {
    match id {
        Value::String(id) => Some(JsonRpcId::String(id.clone())),
        Value::Number(id) => id.as_i64().map(JsonRpcId::Number),
        _ => None,
    }
}

fn dispatch_requests(
    receiver: mpsc::Receiver<Job>,
    pending: &InFlight,
    pool_dir: PathBuf,
) -> Result<(), Error> {
    let mut writer = BufWriter::new(io::stdout().lock());
    let handler = PlasmiteMcpHandler::new(pool_dir);
    let mut legacy_initialized = false;
    for Job {
        payload,
        id,
        cancel,
    } in receiver
    {
        let request = match payload {
            Ok(value) => value,
            Err(error) => {
                write_parse_error(&mut writer, error)?;
                continue;
            }
        };
        let initialize = request.get("method").and_then(Value::as_str) == Some("initialize");
        let cancelled = || {
            cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        };
        let outcome = if cancelled() {
            DispatchOutcome::NoResponse
        } else {
            let mut dispatcher = McpDispatcher::new(handler.clone().with_cancel(cancel.clone()));
            dispatcher.dispatch_value_with_mode(request, !legacy_initialized && !initialize)
        };
        let suppress = if let Some(id) = id {
            // Once removed, a later cancel is for a response already committed to the wire.
            let mut pending = pending.lock().unwrap();
            let suppress = cancelled();
            pending.remove(&id);
            suppress
        } else {
            false
        };
        if !suppress && let DispatchOutcome::Response(response) = outcome {
            legacy_initialized |= initialize && response.result.is_some();
            let payload = serde_json::to_value(response).map_err(|err| {
                Error::new(ErrorKind::Internal)
                    .with_message("failed to encode MCP response")
                    .with_source(err)
            })?;
            write_json_line(&mut writer, &payload)?;
        }
    }
    Ok(())
}

fn write_json_line(
    writer: &mut BufWriter<io::StdoutLock<'_>>,
    payload: &Value,
) -> Result<(), Error> {
    serde_json::to_writer(&mut *writer, payload).map_err(|err| {
        Error::new(ErrorKind::Internal)
            .with_message("failed to encode MCP message")
            .with_source(err)
    })?;
    writer.write_all(b"\n").map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to write MCP message")
            .with_source(err)
    })?;
    writer.flush().map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to flush MCP message")
            .with_source(err)
    })
}

fn write_parse_error(
    writer: &mut BufWriter<io::StdoutLock<'_>>,
    error: plasmite::mcp::JsonRpcError,
) -> Result<(), Error> {
    let mut error_map = Map::new();
    error_map.insert("code".to_string(), json!(error.code));
    error_map.insert("message".to_string(), json!(error.message));
    if let Some(data) = error.data {
        error_map.insert("data".to_string(), data);
    }
    let payload = json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": Value::Object(error_map),
    });
    write_json_line(writer, &payload)
}
