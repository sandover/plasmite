//! Purpose: Run the experimental MCP server over stdio transport.
//! Exports: `serve`.
//! Role: Bridge newline-delimited JSON-RPC lines to the shared MCP dispatcher.
//! Invariants: stdout only emits JSON-RPC messages (one JSON value per line).
//! Invariants: stdin EOF exits cleanly without side effects.
//! Invariants: Parse/protocol errors are surfaced as JSON-RPC error responses.

use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use plasmite::api::{Error, ErrorKind};
use plasmite::mcp::{DispatchOutcome, McpDispatcher, PlasmiteMcpHandler, parse_jsonrpc_line};
use serde_json::{Map, Value, json};

pub(super) fn serve(pool_dir: PathBuf) -> Result<(), Error> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    let mut dispatcher = McpDispatcher::new(PlasmiteMcpHandler::new(pool_dir));
    let mut line = String::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read MCP request")
                .with_source(err)
        })?;
        if read == 0 {
            writer.flush().map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to flush MCP output")
                    .with_source(err)
            })?;
            return Ok(());
        }

        let message = line.trim_end_matches(['\n', '\r']);
        if message.is_empty() {
            continue;
        }

        let request = match parse_jsonrpc_line(message) {
            Ok(value) => value,
            Err(error) => {
                write_parse_error(&mut writer, error)?;
                continue;
            }
        };

        match dispatcher.dispatch_value(request) {
            DispatchOutcome::NoResponse => {}
            DispatchOutcome::Response(response) => {
                let payload = serde_json::to_value(response).map_err(|err| {
                    Error::new(ErrorKind::Internal)
                        .with_message("failed to encode MCP response")
                        .with_source(err)
                })?;
                write_json_line(&mut writer, &payload)?;
            }
        }
    }
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
