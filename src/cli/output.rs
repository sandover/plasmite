//! Render structured CLI output independently of terminal styling.

use clap::ValueEnum;
use plasmite::api::{Error, ErrorKind};
use serde_json::Value;
use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputMode {
    #[default]
    #[value(name = "pretty")]
    Human,
    Json,
    Yaml,
    Lite3,
    Lite3Raw,
}

impl OutputMode {
    pub(crate) fn is_binary(self) -> bool {
        matches!(self, Self::Lite3 | Self::Lite3Raw)
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Human => "pretty",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Lite3 => "lite3",
            Self::Lite3Raw => "lite3-raw",
        }
    }
    pub(crate) fn is_structured(self) -> bool {
        matches!(self, Self::Json | Self::Yaml)
    }
}

pub(crate) fn render_output(value: &Value, mode: OutputMode) -> String {
    match mode {
        OutputMode::Yaml => {
            // Quote strings so older YAML readers cannot turn them into dates or booleans.
            let options = serde_saphyr::ser_options! { quote_all: true };
            let body = serde_saphyr::to_string_with_options(value, options)
                .expect("JSON values serialize as YAML");
            format!("---\n{}\n...\n", body.trim_end_matches('\n'))
        }
        OutputMode::Human | OutputMode::Json | OutputMode::Lite3 | OutputMode::Lite3Raw => {
            format!(
                "{}\n",
                serde_json::to_string(value).expect("JSON value serializes")
            )
        }
    }
}

// Only a closed stdout is a normal early exit. Input/network errors stay failures.
#[derive(Debug)]
pub(crate) struct StdoutFailure {
    pub(crate) closed: bool,
    source: Box<dyn StdError + Send + Sync>,
}
impl fmt::Display for StdoutFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}
impl StdError for StdoutFailure {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}
fn stdout_error(source: impl StdError + Send + Sync + 'static) -> Error {
    fn closed(error: &(dyn StdError + 'static)) -> bool {
        error
            .downcast_ref::<io::Error>()
            .is_some_and(|err| err.kind() == io::ErrorKind::BrokenPipe)
            || error.source().is_some_and(closed)
    }
    Error::new(ErrorKind::Io)
        .with_message("failed to write stdout")
        .with_hint("Check the destination of stdout; completed appends remain stored.")
        .with_source(StdoutFailure {
            closed: closed(&source),
            source: Box::new(source),
        })
}

// Serialize stdout writes so duplex cannot interleave records or write two prefixes.
static BINARY_STARTED: Mutex<bool> = Mutex::new(false);

pub(crate) fn write_stdout(bytes: &[u8]) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.flush())
        .map_err(stdout_error)
}

pub(crate) fn emit_output(value: Value, mode: OutputMode) -> Result<(), Error> {
    write_stdout(render_output(&value, mode).as_bytes())
}

pub(crate) fn emit_lite3(
    seq: u64,
    timestamp_ns: u64,
    payload: &[u8],
    mode: OutputMode,
) -> Result<(), Error> {
    let mut started = BINARY_STARTED
        .lock()
        .map_err(|_| Error::new(ErrorKind::Internal).with_message("stdout lock failed"))?;
    let mut stdout = io::stdout().lock();
    if mode == OutputMode::Lite3Raw {
        return super::message_stream::write_raw(&mut stdout, payload).map_err(stdout_error);
    }
    if !*started {
        super::message_stream::write_header(&mut stdout).map_err(stdout_error)?;
        *started = true;
    }
    super::message_stream::write_frame(&mut stdout, seq, timestamp_ns, payload)
        .map_err(stdout_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stdout_broken_pipe_is_a_normal_exit() {
        let output = stdout_error(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(crate::is_broken_pipe(&output));
        let input =
            Error::new(ErrorKind::Io).with_source(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(!crate::is_broken_pipe(&input));
        let failed = stdout_error(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(!crate::is_broken_pipe(&failed));
    }

    #[test]
    fn yaml_preserves_full_width_numbers_and_scalar_types() {
        let value = serde_json::json!({
            "unsigned": u64::MAX, "signed": i64::MIN,
            "float": 1.0, "negative_zero": -0.0,
            "date": "2026-10-12", "boolean_text": "yes", "empty": "",
            "null": null, "array": [], "map": {},
        });
        let output = render_output(&value, OutputMode::Yaml);
        let decoded: Value = serde_yaml_ng::from_str(&output).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded["unsigned"].as_u64(), Some(u64::MAX));
        assert_eq!(decoded["signed"].as_i64(), Some(i64::MIN));
        assert!(decoded["float"].is_f64());
        assert!(
            decoded["negative_zero"]
                .as_f64()
                .unwrap()
                .is_sign_negative()
        );
    }
}
