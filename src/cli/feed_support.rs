//! CLI helpers for feed commands.

use crate::ColorMode;
use crate::ErrorPolicyCli;
use crate::InputMode;
use crate::ingest::ErrorPolicy;
use crate::ingest::IngestConfig;
use crate::ingest::IngestFailure;
use crate::ingest::IngestMode;
use crate::ingest::IngestOutcome;
use crate::ingest::ingest;
use plasmite::api::AppendOptions;
use plasmite::api::Durability;
use plasmite::api::Error;
use plasmite::api::ErrorKind;
use plasmite::api::Pool;
use plasmite::api::RemotePool;
use plasmite::api::lite3;
use plasmite::notice::Notice;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::io::Read;

use super::output_support::emit_feed_receipt;
use super::output_support::emit_notice;
use super::output_support::notice_time_now;
use super::support::DEFAULT_MAX_RECORD_BYTES;
use super::support::DEFAULT_MAX_SNIPPET_BYTES;
use super::support::DEFAULT_SNIFF_BYTES;
use super::support::DEFAULT_SNIFF_LINES;
use super::support::RetryConfig;
use super::support::feed_receipt_from_message;
use super::support::feed_receipt_json;
use super::support::now_ns;
use super::support::retry_append;

pub(crate) fn parse_inline_json(data: &str) -> Result<Value, Error> {
    serde_json::from_str(data).map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid json")
            .with_hint("Provide a single JSON value (e.g. '{\"x\":1}').")
            .with_source(err)
    })
}

pub(crate) fn missing_feed_data_error() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("missing data input")
        .with_hint("Provide JSON via DATA, --file, or pipe JSON to stdin.")
}

pub(crate) fn open_feed_reader(path: &str) -> Result<Box<dyn Read>, Error> {
    if path == "-" {
        return Ok(Box::new(io::stdin()));
    }
    let reader = std::fs::File::open(path).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to read data file")
            .with_path(path)
            .with_source(err)
    })?;
    Ok(Box::new(reader))
}

pub(crate) fn input_mode_to_ingest(mode: InputMode) -> Result<IngestMode, Error> {
    Ok(match mode {
        InputMode::Auto => IngestMode::Auto,
        InputMode::Jsonl => IngestMode::Jsonl,
        InputMode::Json => IngestMode::Json,
        InputMode::Seq => IngestMode::Seq,
        InputMode::Jq => IngestMode::Jq,
        InputMode::Lite3 => {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("Lite3 input requires the single-message binary reader"));
        }
    })
}

pub(crate) fn error_policy_to_ingest(policy: ErrorPolicyCli) -> ErrorPolicy {
    match policy {
        ErrorPolicyCli::Stop => ErrorPolicy::Stop,
        ErrorPolicyCli::Skip => ErrorPolicy::Skip,
    }
}

pub(crate) fn ingest_failure_notice(
    failure: &IngestFailure,
    pool_ref: &str,
    pool_path_label: &str,
    color_mode: ColorMode,
    json_output: bool,
) {
    let mut details = Map::new();
    details.insert("mode".to_string(), json!(mode_label(failure.mode)));
    details.insert("index".to_string(), json!(failure.index));
    details.insert("error_kind".to_string(), json!(failure.error_kind));
    details.insert("pool_path".to_string(), json!(pool_path_label));
    if let Some(line) = failure.line {
        details.insert("line".to_string(), json!(line));
    }
    if let Some(snippet) = &failure.snippet {
        details.insert("snippet".to_string(), json!(snippet));
    }
    let notice = Notice {
        kind: "ingest_skip".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "feed".to_string(),
        pool: pool_ref.to_string(),
        message: ingest_failure_message(failure),
        details,
    };
    emit_notice(&notice, color_mode, json_output);
}

pub(crate) fn ingest_failure_message(failure: &IngestFailure) -> String {
    match failure.error_kind.as_str() {
        "Parse" => "Skipped invalid JSON.".to_string(),
        "Oversize" => "Skipped oversized record.".to_string(),
        _ => format!("Skipped record: {}.", failure.message),
    }
}

pub(crate) fn ingest_summary_notice(
    outcome: &IngestOutcome,
    pool_ref: &str,
    pool_path_label: &str,
    color_mode: ColorMode,
    json_output: bool,
) {
    let mut details = Map::new();
    details.insert("total".to_string(), json!(outcome.records_total));
    details.insert("ok".to_string(), json!(outcome.ok));
    details.insert("failed".to_string(), json!(outcome.failed));
    details.insert("pool_path".to_string(), json!(pool_path_label));
    let notice = Notice {
        kind: "ingest_summary".to_string(),
        time: notice_time_now().unwrap_or_else(|| "unknown".to_string()),
        cmd: "feed".to_string(),
        pool: pool_ref.to_string(),
        message: format!(
            "Finished with {} skipped record{}.",
            outcome.failed,
            if outcome.failed == 1 { "" } else { "s" }
        ),
        details,
    };
    emit_notice(&notice, color_mode, json_output);
}

pub(crate) fn mode_label(mode: IngestMode) -> &'static str {
    match mode {
        IngestMode::Auto => "auto",
        IngestMode::Jsonl => "jsonl",
        IngestMode::Json => "json",
        IngestMode::Seq => "seq",
        IngestMode::Jq => "jq",
        IngestMode::Event => "event",
    }
}

pub(crate) struct FeedIngestContext<'a> {
    pub(crate) pool_ref: &'a str,
    pub(crate) pool_path_label: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) durability: Durability,
    pub(crate) retry_config: Option<RetryConfig>,
    pub(crate) pool_handle: &'a mut Pool,
    pub(crate) color_mode: ColorMode,
    pub(crate) json_output: bool,
    pub(crate) input: InputMode,
    pub(crate) errors: ErrorPolicyCli,
}

pub(crate) struct RemoteFeedIngestContext<'a> {
    pub(crate) pool_ref: &'a str,
    pub(crate) pool_path_label: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) durability: Durability,
    pub(crate) retry_config: Option<RetryConfig>,
    pub(crate) remote_pool: &'a RemotePool,
    pub(crate) color_mode: ColorMode,
    pub(crate) json_output: bool,
    pub(crate) input: InputMode,
    pub(crate) errors: ErrorPolicyCli,
}

struct IngestPresentation<'a> {
    pool_ref: &'a str,
    pool_path_label: &'a str,
    color_mode: ColorMode,
    json_output: bool,
    input: InputMode,
    errors: ErrorPolicyCli,
}

fn ingest_with_append<R, F>(
    reader: R,
    presentation: IngestPresentation<'_>,
    emit_receipt: bool,
    mut append: F,
) -> Result<IngestOutcome, Error>
where
    R: Read,
    F: FnMut(Value, bool) -> Result<Option<Value>, Error>,
{
    let ingest_config = IngestConfig {
        mode: input_mode_to_ingest(presentation.input)?,
        errors: error_policy_to_ingest(presentation.errors),
        sniff_bytes: DEFAULT_SNIFF_BYTES,
        sniff_lines: DEFAULT_SNIFF_LINES,
        max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
        max_snippet_bytes: DEFAULT_MAX_SNIPPET_BYTES,
    };
    let outcome = ingest(
        reader,
        ingest_config,
        |data| {
            let receipt = append(data, emit_receipt)?;
            if let Some(receipt) = receipt {
                emit_feed_receipt(receipt, presentation.color_mode, presentation.json_output);
            }
            Ok(())
        },
        |failure| {
            ingest_failure_notice(
                &failure,
                presentation.pool_ref,
                presentation.pool_path_label,
                presentation.color_mode,
                presentation.json_output,
            )
        },
    )?;
    if presentation.errors == ErrorPolicyCli::Skip && outcome.failed > 0 {
        ingest_summary_notice(
            &outcome,
            presentation.pool_ref,
            presentation.pool_path_label,
            presentation.color_mode,
            presentation.json_output,
        );
    }
    Ok(outcome)
}

pub(crate) fn ingest_from_stdin<R: Read>(
    reader: R,
    ctx: FeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<IngestOutcome, Error> {
    ingest_with_append(
        reader,
        IngestPresentation {
            pool_ref: ctx.pool_ref,
            pool_path_label: ctx.pool_path_label,
            color_mode: ctx.color_mode,
            json_output: ctx.json_output,
            input: ctx.input,
            errors: ctx.errors,
        },
        emit_receipt,
        |data, emit_receipt| {
            let payload = lite3::encode_message(ctx.tags, &data)?;
            let (seq, timestamp_ns) = retry_append(ctx.retry_config, || {
                let timestamp_ns = now_ns()?;
                let options = AppendOptions::new(timestamp_ns, ctx.durability);
                let seq = ctx
                    .pool_handle
                    .append_with_options(payload.as_slice(), options)?;
                Ok((seq, timestamp_ns))
            })?;
            emit_receipt
                .then(|| feed_receipt_json(seq, timestamp_ns, ctx.tags))
                .transpose()
        },
    )
}

pub(crate) fn ingest_from_stdin_remote<R: Read>(
    reader: R,
    ctx: RemoteFeedIngestContext<'_>,
    emit_receipt: bool,
) -> Result<IngestOutcome, Error> {
    ingest_with_append(
        reader,
        IngestPresentation {
            pool_ref: ctx.pool_ref,
            pool_path_label: ctx.pool_path_label,
            color_mode: ctx.color_mode,
            json_output: ctx.json_output,
            input: ctx.input,
            errors: ctx.errors,
        },
        emit_receipt,
        |data, emit_receipt| {
            let message = retry_append(ctx.retry_config, || {
                ctx.remote_pool
                    .append_json_now(&data, ctx.tags, ctx.durability)
            })?;
            Ok(emit_receipt.then(|| feed_receipt_from_message(&message)))
        },
    )
}
