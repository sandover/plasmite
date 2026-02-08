//! Purpose: Parse stdin streams into JSON values for `poke` with explicit, testable modes.
//! Exports: `IngestMode`, `ErrorPolicy`, `IngestConfig`, `IngestOutcome`, `IngestFailure`, `ingest`.
//! Role: Input ingestion engine used by the CLI; isolates streaming heuristics from main.
//! Invariants: Auto detection is deterministic, bounded, and documented by config limits.
//! Invariants: Skip mode only continues at well-defined record boundaries.
//! Invariants: No unbounded buffering; per-record buffering is capped.
use std::io::{self, BufRead, BufReader, Read};

use bstr::ByteSlice;
use plasmite::api::{Error, ErrorKind};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Parse JSON from a string slice.
fn json_from_str<T: DeserializeOwned>(s: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(s)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IngestMode {
    Auto,
    Jsonl,
    Json,
    Seq,
    Jq,
    Event,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ErrorPolicy {
    Stop,
    Skip,
}

#[derive(Copy, Clone, Debug)]
pub struct IngestConfig {
    pub mode: IngestMode,
    pub errors: ErrorPolicy,
    pub sniff_bytes: usize,
    pub sniff_lines: usize,
    pub max_record_bytes: usize,
    pub max_snippet_bytes: usize,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct IngestOutcome {
    pub records_total: u64,
    pub ok: u64,
    pub failed: u64,
}

#[derive(Clone, Debug)]
pub struct IngestFailure {
    pub index: u64,
    pub mode: IngestMode,
    pub message: String,
    pub error_kind: String,
    pub snippet: Option<String>,
    pub line: Option<u64>,
}

fn io_error(err: io::Error, message: &str) -> Error {
    Error::new(ErrorKind::Io)
        .with_message(message)
        .with_source(err)
}

pub fn ingest<R, F, N>(
    reader: R,
    config: IngestConfig,
    mut on_value: F,
    mut on_failure: N,
) -> Result<IngestOutcome, Error>
where
    R: Read,
    F: FnMut(Value) -> Result<(), Error>,
    N: FnMut(IngestFailure),
{
    let mut outcome = IngestOutcome::default();
    let mut ok = 0u64;
    let mut failed = 0u64;

    let mut handle_failure = |index: u64,
                              mode: IngestMode,
                              line: Option<u64>,
                              message: &str,
                              error_kind: &str,
                              snippet: Option<String>|
     -> Result<(), Error> {
        match config.errors {
            ErrorPolicy::Stop => {
                let mut err = Error::new(ErrorKind::Usage).with_message(message);
                if error_kind == "Parse" {
                    err =
                        err.with_hint("Use -e skip to continue or select the correct input mode.");
                }
                if mode == IngestMode::Jq {
                    err = err.with_hint("Use --in jsonl for line-delimited input.");
                }
                Err(err)
            }
            ErrorPolicy::Skip => {
                failed += 1;
                on_failure(IngestFailure {
                    index,
                    mode,
                    message: message.to_string(),
                    error_kind: error_kind.to_string(),
                    snippet,
                    line,
                });
                Ok(())
            }
        }
    };

    let mut accept_value = |value: Value, _index: u64| -> Result<(), Error> {
        on_value(value)?;
        ok += 1;
        Ok(())
    };

    match config.mode {
        IngestMode::Auto => {
            let (auto_mode, reader) = sniff_auto(reader, &config)?;
            ingest_auto(
                reader,
                auto_mode,
                config,
                &mut accept_value,
                &mut handle_failure,
            )
        }
        IngestMode::Jsonl => ingest_jsonl(
            reader,
            config,
            false,
            &mut accept_value,
            &mut handle_failure,
        ),
        IngestMode::Json => {
            ingest_single_json(reader, config, &mut accept_value, &mut handle_failure)
        }
        IngestMode::Seq => ingest_json_seq(reader, config, &mut accept_value, &mut handle_failure),
        IngestMode::Jq => ingest_jq(reader, config, &mut accept_value, &mut handle_failure),
        IngestMode::Event => {
            ingest_event_stream(reader, config, &mut accept_value, &mut handle_failure)
        }
    }?;

    outcome.ok = ok;
    outcome.failed = failed;
    outcome.records_total = ok + failed;

    Ok(outcome)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AutoMode {
    EventStream,
    JsonSeq,
    JsonlMultiline,
}

fn sniff_auto<R: Read>(
    reader: R,
    config: &IngestConfig,
) -> Result<(AutoMode, PrefixReader<R>), Error> {
    let mut buf_reader = BufReader::new(reader);
    let mut prefix = Vec::new();
    let mut lines = 0usize;
    while prefix.len() < config.sniff_bytes && lines < config.sniff_lines {
        let available = buf_reader
            .fill_buf()
            .map_err(|err| io_error(err, "failed to read stdin"))?;
        if available.is_empty() {
            break;
        }
        let take = available
            .len()
            .min(config.sniff_bytes.saturating_sub(prefix.len()));
        let newline_count = available[..take].iter().filter(|b| **b == b'\n').count();
        prefix.extend_from_slice(&available[..take]);
        buf_reader.consume(take);
        lines += newline_count;
    }

    let auto_mode = detect_auto_mode(&prefix);
    Ok((auto_mode, PrefixReader::new(prefix, buf_reader)))
}

fn detect_auto_mode(prefix: &[u8]) -> AutoMode {
    if prefix.contains(&0x1e) {
        return AutoMode::JsonSeq;
    }
    let text = prefix.to_str_lossy();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("data:")
            || line.starts_with("event:")
            || line.starts_with("id:")
            || line.starts_with(':')
        {
            return AutoMode::EventStream;
        }
        break;
    }
    AutoMode::JsonlMultiline
}

fn ingest_auto<R, F, N>(
    reader: PrefixReader<R>,
    mode: AutoMode,
    config: IngestConfig,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    R: Read,
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    match mode {
        AutoMode::EventStream => ingest_event_stream(reader, config, on_value, on_failure),
        AutoMode::JsonSeq => ingest_json_seq(reader, config, on_value, on_failure),
        AutoMode::JsonlMultiline => ingest_jsonl(reader, config, true, on_value, on_failure),
    }
}

fn ingest_jsonl<R, F, N>(
    reader: R,
    config: IngestConfig,
    allow_multiline: bool,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    R: Read,
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut index = 0u64;
    let mut line_no = 0u64;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| io_error(err, "failed to read stdin"))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.trim().is_empty() {
            continue;
        }
        index += 1;
        if trimmed.len() > config.max_record_bytes {
            on_failure(
                index,
                IngestMode::Jsonl,
                Some(line_no),
                "record exceeds size limit",
                "Oversize",
                Some(truncate_snippet(trimmed, config.max_snippet_bytes)),
            )?;
            continue;
        }
        match json_from_str::<Value>(trimmed) {
            Ok(value) => apply_value(
                value,
                index,
                IngestMode::Jsonl,
                Some(line_no),
                config.errors,
                on_value,
                on_failure,
            )?,
            Err(_) if allow_multiline && looks_like_json_start(trimmed) => {
                let mut buf = String::from(trimmed);
                let mut record_line = line_no;
                loop {
                    if buf.len() > config.max_record_bytes {
                        on_failure(
                            index,
                            IngestMode::Jsonl,
                            Some(record_line),
                            "record exceeds size limit",
                            "Oversize",
                            Some(truncate_snippet(&buf, config.max_snippet_bytes)),
                        )?;
                        break;
                    }
                    if let Ok(value) = json_from_str::<Value>(&buf) {
                        apply_value(
                            value,
                            index,
                            IngestMode::Jsonl,
                            Some(record_line),
                            config.errors,
                            on_value,
                            on_failure,
                        )?;
                        break;
                    }
                    line.clear();
                    let read = reader
                        .read_line(&mut line)
                        .map_err(|err| io_error(err, "failed to read stdin"))?;
                    if read == 0 {
                        on_failure(
                            index,
                            IngestMode::Jsonl,
                            Some(record_line),
                            "invalid json input",
                            "Parse",
                            Some(truncate_snippet(&buf, config.max_snippet_bytes)),
                        )?;
                        break;
                    }
                    line_no += 1;
                    let next_trimmed = line.trim_end_matches(['\n', '\r']);
                    if config.errors == ErrorPolicy::Skip && looks_like_json_start(next_trimmed) {
                        on_failure(
                            index,
                            IngestMode::Jsonl,
                            Some(record_line),
                            "invalid json input",
                            "Parse",
                            Some(truncate_snippet(&buf, config.max_snippet_bytes)),
                        )?;
                        index += 1;
                        record_line = line_no;
                        buf = next_trimmed.to_string();
                        continue;
                    }
                    buf.push_str(line.as_str());
                }
            }
            Err(_) => {
                on_failure(
                    index,
                    IngestMode::Jsonl,
                    Some(line_no),
                    "invalid json input",
                    "Parse",
                    Some(truncate_snippet(trimmed, config.max_snippet_bytes)),
                )?;
                continue;
            }
        }
    }
    Ok(())
}

fn ingest_single_json<R, F, N>(
    mut reader: R,
    config: IngestConfig,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    R: Read,
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    let mut buf = String::new();
    reader
        .read_to_string(&mut buf)
        .map_err(|err| io_error(err, "failed to read stdin"))?;
    if buf.trim().is_empty() {
        return Ok(());
    }
    if buf.len() > config.max_record_bytes {
        on_failure(
            1,
            IngestMode::Json,
            None,
            "record exceeds size limit",
            "Oversize",
            Some(truncate_snippet(&buf, config.max_snippet_bytes)),
        )?;
        return Ok(());
    }
    match json_from_str::<Value>(&buf) {
        Ok(value) => {
            apply_value(
                value,
                1,
                IngestMode::Json,
                None,
                config.errors,
                on_value,
                on_failure,
            )?;
        }
        Err(_) => {
            on_failure(
                1,
                IngestMode::Json,
                None,
                "invalid json input",
                "Parse",
                Some(truncate_snippet(&buf, config.max_snippet_bytes)),
            )?;
        }
    }
    Ok(())
}

fn ingest_json_seq<R, F, N>(
    reader: R,
    config: IngestConfig,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    R: Read,
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    fn handle_record<F, N>(
        record: &[u8],
        index: u64,
        config: IngestConfig,
        on_value: &mut F,
        on_failure: &mut N,
    ) -> Result<(), Error>
    where
        F: FnMut(Value, u64) -> Result<(), Error>,
        N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
    {
        if record.len() > config.max_record_bytes {
            return on_failure(
                index,
                IngestMode::Seq,
                None,
                "record exceeds size limit",
                "Oversize",
                Some(truncate_bytes(record, config.max_snippet_bytes)),
            );
        }
        let text = record.to_str_lossy();
        match json_from_str::<Value>(&text) {
            Ok(value) => apply_value(
                value,
                index,
                IngestMode::Seq,
                None,
                config.errors,
                on_value,
                on_failure,
            ),
            Err(_) => on_failure(
                index,
                IngestMode::Seq,
                None,
                "invalid json input",
                "Parse",
                Some(truncate_bytes(record, config.max_snippet_bytes)),
            ),
        }
    }

    let mut index = 0u64;
    let mut buf_reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut skipping = false;
    loop {
        let read = buf_reader
            .read(&mut tmp)
            .map_err(|err| io_error(err, "failed to read stdin"))?;
        if read == 0 {
            break;
        }
        let mut slice = &tmp[..read];
        while !slice.is_empty() {
            if skipping {
                if let Some(pos) = slice.iter().position(|b| *b == 0x1e) {
                    slice = &slice[pos + 1..];
                    skipping = false;
                } else {
                    slice = &[];
                }
                continue;
            }

            if let Some(pos) = slice.iter().position(|b| *b == 0x1e) {
                buf.extend_from_slice(&slice[..pos]);
                let record = std::mem::take(&mut buf);
                if !record.iter().all(|b| b.is_ascii_whitespace()) {
                    index += 1;
                    handle_record(&record, index, config, on_value, on_failure)?;
                }
                slice = &slice[pos + 1..];
                continue;
            }

            buf.extend_from_slice(slice);
            slice = &[];
            if buf.len() > config.max_record_bytes {
                index += 1;
                on_failure(
                    index,
                    IngestMode::Seq,
                    None,
                    "record exceeds size limit",
                    "Oversize",
                    Some(truncate_bytes(&buf, config.max_snippet_bytes)),
                )?;
                buf.clear();
                skipping = true;
            }
        }
    }
    if !skipping && !buf.is_empty() && !buf.iter().all(|b| b.is_ascii_whitespace()) {
        index += 1;
        handle_record(&buf, index, config, on_value, on_failure)?;
    }
    Ok(())
}

fn ingest_event_stream<R, F, N>(
    reader: R,
    config: IngestConfig,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    R: Read,
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut index = 0u64;
    let mut line_no = 0u64;
    let mut data_lines: Vec<String> = Vec::new();
    let mut skipping = false;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| io_error(err, "failed to read stdin"))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            if data_lines.is_empty() {
                continue;
            }
            if skipping {
                data_lines.clear();
                skipping = false;
                continue;
            }
            let payload = data_lines.join("\n");
            data_lines.clear();
            index += 1;
            if payload.len() > config.max_record_bytes {
                on_failure(
                    index,
                    IngestMode::Event,
                    Some(line_no),
                    "record exceeds size limit",
                    "Oversize",
                    Some(truncate_snippet(&payload, config.max_snippet_bytes)),
                )?;
                continue;
            }
            match json_from_str::<Value>(&payload) {
                Ok(value) => apply_value(
                    value,
                    index,
                    IngestMode::Event,
                    Some(line_no),
                    config.errors,
                    on_value,
                    on_failure,
                )?,
                Err(_) => on_failure(
                    index,
                    IngestMode::Event,
                    Some(line_no),
                    "invalid json input",
                    "Parse",
                    Some(truncate_snippet(&payload, config.max_snippet_bytes)),
                )?,
            }
            continue;
        }
        if trimmed.starts_with(':') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            if skipping {
                continue;
            }
            data_lines.push(data.to_string());
            let current_len = data_lines.iter().map(String::len).sum::<usize>()
                + data_lines.len().saturating_sub(1);
            if current_len > config.max_record_bytes {
                index += 1;
                on_failure(
                    index,
                    IngestMode::Event,
                    Some(line_no),
                    "record exceeds size limit",
                    "Oversize",
                    Some(join_snippet(&data_lines, config.max_snippet_bytes)),
                )?;
                data_lines.clear();
                skipping = true;
            }
        }
    }
    if !skipping && !data_lines.is_empty() {
        let payload = data_lines.join("\n");
        index += 1;
        if payload.len() > config.max_record_bytes {
            on_failure(
                index,
                IngestMode::Event,
                Some(line_no),
                "record exceeds size limit",
                "Oversize",
                Some(truncate_snippet(&payload, config.max_snippet_bytes)),
            )?;
        } else {
            match json_from_str::<Value>(&payload) {
                Ok(value) => apply_value(
                    value,
                    index,
                    IngestMode::Event,
                    Some(line_no),
                    config.errors,
                    on_value,
                    on_failure,
                )?,
                Err(_) => on_failure(
                    index,
                    IngestMode::Event,
                    Some(line_no),
                    "invalid json input",
                    "Parse",
                    Some(truncate_snippet(&payload, config.max_snippet_bytes)),
                )?,
            }
        }
    }
    Ok(())
}

fn ingest_jq<R, F, N>(
    reader: R,
    config: IngestConfig,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    R: Read,
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    if config.errors == ErrorPolicy::Skip {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("skip is not supported for jq-style streams")
            .with_hint("Use --in jsonl or --in auto for resyncable streams."));
    }
    let stream = serde_json::Deserializer::from_reader(reader).into_iter::<Value>();
    let mut index = 0u64;
    for item in stream {
        index += 1;
        let value = item.map_err(|_| {
            Error::new(ErrorKind::Usage)
                .with_message("invalid json input")
                .with_hint("Use --in jsonl for line-delimited input.")
        })?;
        apply_value(
            value,
            index,
            IngestMode::Jq,
            None,
            config.errors,
            on_value,
            on_failure,
        )?;
    }
    Ok(())
}

fn apply_value<F, N>(
    value: Value,
    index: u64,
    mode: IngestMode,
    line: Option<u64>,
    errors: ErrorPolicy,
    on_value: &mut F,
    on_failure: &mut N,
) -> Result<(), Error>
where
    F: FnMut(Value, u64) -> Result<(), Error>,
    N: FnMut(u64, IngestMode, Option<u64>, &str, &str, Option<String>) -> Result<(), Error>,
{
    match on_value(value, index) {
        Ok(()) => Ok(()),
        Err(err) => {
            if errors == ErrorPolicy::Skip {
                return Err(err);
            }
            let message = err.message().unwrap_or("append failed");
            let kind = format!("{:?}", err.kind());
            on_failure(index, mode, line, message, &kind, None)
        }
    }
}

fn looks_like_json_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

fn truncate_snippet(input: &str, max: usize) -> String {
    let mut snippet = String::new();
    if input.len() <= max {
        snippet.push_str(input);
        return snippet;
    }
    let suffix = "...";
    if max <= suffix.len() {
        snippet.push_str(&suffix[..max]);
        return snippet;
    }
    let take = max - suffix.len();
    snippet.push_str(&input[..take]);
    snippet.push_str(suffix);
    snippet
}

fn join_snippet(lines: &[String], max: usize) -> String {
    let mut snippet = String::new();
    let mut truncated = false;
    for (idx, line) in lines.iter().enumerate() {
        if idx > 0 {
            if snippet.len() + 1 > max {
                truncated = true;
                break;
            }
            snippet.push('\n');
        }
        let remaining = max.saturating_sub(snippet.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if line.len() <= remaining {
            snippet.push_str(line);
        } else {
            snippet.push_str(&line[..remaining]);
            truncated = true;
            break;
        }
    }
    if truncated {
        truncate_snippet(&format!("{snippet}..."), max)
    } else {
        snippet
    }
}

fn truncate_bytes(input: &[u8], max: usize) -> String {
    let text = input.to_str_lossy();
    truncate_snippet(&text, max)
}

struct PrefixReader<R: Read> {
    prefix: io::Cursor<Vec<u8>>,
    inner: BufReader<R>,
}

impl<R: Read> PrefixReader<R> {
    fn new(prefix: Vec<u8>, inner: BufReader<R>) -> Self {
        Self {
            prefix: io::Cursor::new(prefix),
            inner,
        }
    }
}

impl<R: Read> Read for PrefixReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.prefix.read(buf)?;
        if read > 0 {
            return Ok(read);
        }
        self.inner.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorPolicy, IngestConfig, IngestFailure, IngestMode, ingest, truncate_snippet};
    use plasmite::api::Error;

    fn config(mode: IngestMode, errors: ErrorPolicy) -> IngestConfig {
        IngestConfig {
            mode,
            errors,
            sniff_bytes: 128,
            sniff_lines: 4,
            max_record_bytes: 1024,
            max_snippet_bytes: 32,
        }
    }

    #[test]
    fn jsonl_skip_continues_on_parse_error() {
        let input = b"{\"a\":1}\nnot-json\n{\"b\":2}\n";
        let mut values = Vec::new();
        let mut failures = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Jsonl, ErrorPolicy::Skip),
            |value| {
                values.push(value);
                Ok(())
            },
            |failure: IngestFailure| failures.push(failure),
        )
        .expect("ingest");

        assert_eq!(values.len(), 2);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.records_total, 3);
        assert!(failures.first().unwrap().message.contains("invalid json"));
    }

    #[test]
    fn auto_handles_multiline_json() {
        let input = b"{\n  \"a\": 1,\n  \"b\": 2\n}\n";
        let mut values = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Auto, ErrorPolicy::Stop),
            |value| {
                values.push(value);
                Ok(())
            },
            |_| {},
        )
        .expect("ingest");

        assert_eq!(outcome.ok, 1);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["a"], 1);
    }

    #[test]
    fn auto_detects_event_stream() {
        let input = b"data: {\"x\":1}\n\ndata: {\"x\":2}\n\n";
        let mut values = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Auto, ErrorPolicy::Stop),
            |value| {
                values.push(value);
                Ok(())
            },
            |_| {},
        )
        .expect("ingest");

        assert_eq!(outcome.ok, 2);
        assert_eq!(values[1]["x"], 2);
    }

    #[test]
    fn json_seq_parses_rs_records() {
        let input = b"\x1e{\"x\":1}\x1e{\"x\":2}";
        let mut values = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Seq, ErrorPolicy::Stop),
            |value| {
                values.push(value);
                Ok(())
            },
            |_| {},
        )
        .expect("ingest");

        assert_eq!(outcome.ok, 2);
        assert_eq!(values[0]["x"], 1);
    }

    #[test]
    fn jq_mode_rejects_skip() {
        let input = b"{\"x\":1}\n{\"y\":2}\n";
        let err = ingest(
            &input[..],
            config(IngestMode::Jq, ErrorPolicy::Skip),
            |_| Ok(()),
            |_| {},
        )
        .unwrap_err();
        assert!(err.message().unwrap().contains("skip is not supported"));
    }

    #[test]
    fn snippet_truncates() {
        let snippet = truncate_snippet("abcdefghijklmnopqrstuvwxyz", 8);
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn json_seq_skip_resyncs_after_oversize() {
        let input = b"\x1e{\"ok\":1}\x1e{\"too\":\"large\"}\x1e{\"ok\":2}";
        let mut cfg = config(IngestMode::Seq, ErrorPolicy::Skip);
        cfg.max_record_bytes = 8;
        let mut values = Vec::new();
        let mut failures = Vec::new();
        let outcome = ingest(
            &input[..],
            cfg,
            |value| {
                values.push(value);
                Ok(())
            },
            |failure: IngestFailure| failures.push(failure),
        )
        .expect("ingest");

        assert_eq!(values.len(), 2);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.records_total, 3);
        assert_eq!(failures[0].error_kind, "Oversize");
    }

    #[test]
    fn event_stream_skips_comments_and_joins_data() {
        let input = b": keepalive\n\
data: {\"x\":\n\
data: 1}\n\
\n";
        let mut values = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Event, ErrorPolicy::Stop),
            |value| {
                values.push(value);
                Ok(())
            },
            |_| {},
        )
        .expect("ingest");

        assert_eq!(outcome.ok, 1);
        assert_eq!(values[0]["x"], 1);
    }

    #[test]
    fn auto_detects_json_seq() {
        let input = b"\x1e{\"x\":1}\x1e{\"x\":2}";
        let mut values = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Auto, ErrorPolicy::Stop),
            |value| {
                values.push(value);
                Ok(())
            },
            |_| {},
        )
        .expect("ingest");

        assert_eq!(outcome.ok, 2);
        assert_eq!(values[1]["x"], 2);
    }

    #[test]
    fn auto_multiline_skip_resyncs_on_new_record_start() {
        let input = b"{\n  \"bad\": 1,\n{\"good\":2}\n";
        let mut values = Vec::new();
        let mut failures = Vec::new();
        let outcome = ingest(
            &input[..],
            config(IngestMode::Auto, ErrorPolicy::Skip),
            |value| {
                values.push(value);
                Ok(())
            },
            |failure: IngestFailure| failures.push(failure),
        )
        .expect("ingest");

        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["good"], 2);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.records_total, 2);
        assert_eq!(failures[0].error_kind, "Parse");
    }

    fn _typecheck(_: Result<(), Error>) {}
}
