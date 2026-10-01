//! Purpose: Execute child-process capture into a local pool.
//! Exports: `TapArgs`, `run`.
//! Role: Own process lifecycle, stream draining, signal forwarding, and capture messages.

use super::context::CliContext;
use super::result::CommandResult;
use super::support::{
    DEFAULT_POOL_SIZE, add_missing_pool_create_hint, ensure_pool_dir, now_ns, parse_durability,
    parse_size, render_shell_agnostic_command, resolve_poolref,
};
use plasmite::api::{AppendOptions, Durability, Error, ErrorKind, Pool, PoolOptions, lite3};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Read};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TAP_EVENT_QUEUE_CAPACITY: usize = 64;

pub(super) struct TapArgs {
    pub(super) pool: String,
    pub(super) create: bool,
    pub(super) create_size: Option<String>,
    pub(super) tags: Vec<String>,
    pub(super) quiet: bool,
    pub(super) durability: String,
    pub(super) command: Vec<String>,
}

pub(super) fn run(args: TapArgs, context: &CliContext) -> Result<CommandResult, Error> {
    if args.create_size.is_some() && !args.create {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("--create-size requires --create")
            .with_hint("Add --create or remove --create-size."));
    }
    if args.pool.contains("://") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("tap accepts local pool refs only")
            .with_hint("Use a local pool name/path (for example `plasmite tap build -- ...`)."));
    }
    if args.command.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("tap requires a wrapped command after `--`")
            .with_hint("Use `plasmite tap <pool> -- <command...>`."));
    }
    let durability = parse_durability(&args.durability)?;
    let path = resolve_poolref(&args.pool, context.pool_dir())?;
    let mut pool_handle = match Pool::open(&path) {
        Ok(pool_handle) => pool_handle,
        Err(err) if args.create && err.kind() == ErrorKind::NotFound => {
            ensure_pool_dir(context.pool_dir())?;
            let size = args
                .create_size
                .as_deref()
                .map(parse_size)
                .transpose()?
                .unwrap_or(DEFAULT_POOL_SIZE);
            Pool::create(&path, PoolOptions::new(size))?
        }
        Err(err) => {
            return Err(add_missing_pool_create_hint(
                err, "tap", &args.pool, &args.pool, None,
            ));
        }
    };

    let max_line_bytes = pool_handle.info()?.ring_size;

    let mut child = std::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| tap_spawn_error(&args.command, err))?;
    tap_spawn_signal_forwarder(child.id() as i32);
    let status_on_tty_stderr = io::stderr().is_terminal();
    if status_on_tty_stderr {
        eprintln!(
            "tapping {} <- {}",
            super::output_support::human_literal(&args.pool),
            super::output_support::human_literal(&render_shell_agnostic_command(&args.command))
        );
    }

    let child_stdout = child.stdout.take().ok_or_else(|| {
        Error::new(ErrorKind::Internal).with_message("tap child stdout pipe unavailable")
    })?;
    let child_stderr = child.stderr.take().ok_or_else(|| {
        Error::new(ErrorKind::Internal).with_message("tap child stderr pipe unavailable")
    })?;

    let lifecycle_tags = vec!["lifecycle".to_string()];
    if let Err(err) = tap_append_message(
        &mut pool_handle,
        durability,
        &lifecycle_tags,
        &json!({
            "kind": "start",
            "cmd": args.command,
        }),
    ) {
        tap_terminate_child(&mut child);
        return Err(err);
    }

    let start_time = Instant::now();
    let (event_tx, event_rx) = mpsc::sync_channel(TAP_EVENT_QUEUE_CAPACITY);
    let stdout_reader = tap_spawn_reader(
        child_stdout,
        TapStream::Stdout,
        !args.quiet,
        max_line_bytes,
        event_tx.clone(),
    );
    let stderr_reader = tap_spawn_reader(
        child_stderr,
        TapStream::Stderr,
        !args.quiet,
        max_line_bytes,
        event_tx,
    );

    let mut child_status = None;
    let mut event_channel_closed = false;
    let mut line_count: u64 = 0;

    while child_status.is_none() || !event_channel_closed {
        let event = if event_channel_closed {
            std::thread::sleep(Duration::from_millis(25));
            Err(mpsc::RecvTimeoutError::Timeout)
        } else {
            event_rx.recv_timeout(Duration::from_millis(25))
        };
        match event {
            Ok(TapEvent::Line { stream, raw_line }) => {
                line_count = line_count.saturating_add(1);
                if let Err(err) = tap_append_message(
                    &mut pool_handle,
                    durability,
                    &args.tags,
                    &json!({
                        "kind": "line",
                        "stream": stream.as_str(),
                        "line": trim_tap_line_endings(&raw_line),
                    }),
                ) {
                    drop(event_rx);
                    tap_terminate_child(&mut child);
                    return Err(err);
                }
            }
            Ok(TapEvent::ReaderError(err)) => {
                drop(event_rx);
                tap_terminate_child(&mut child);
                return Err(err);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => event_channel_closed = true,
        }
        if child_status.is_none() {
            child_status = child.try_wait().map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed waiting for wrapped command")
                    .with_source(err)
            })?;
        }
    }

    let child_status = child_status.expect("status set once loop exits");
    let mut reader_error: Option<Error> = None;
    if stdout_reader.join().is_err() && reader_error.is_none() {
        reader_error =
            Some(Error::new(ErrorKind::Internal).with_message("tap stdout reader panicked"));
    }
    if stderr_reader.join().is_err() && reader_error.is_none() {
        reader_error =
            Some(Error::new(ErrorKind::Internal).with_message("tap stderr reader panicked"));
    }

    if let Some(err) = reader_error {
        return Err(err);
    }

    let elapsed_ms = start_time.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let exit_code = if let Some(signal) = tap_exit_signal(&child_status) {
        let signal_name = tap_signal_name(signal);
        tap_append_message(
            &mut pool_handle,
            durability,
            &lifecycle_tags,
            &json!({
                "kind": "exit",
                "signal": signal_name,
                "elapsed_ms": elapsed_ms,
            }),
        )?;
        if status_on_tty_stderr {
            eprintln!(
                "tapped {line_count} lines ({}) -> {} signal {}",
                format_tap_elapsed(elapsed_ms),
                super::output_support::human_literal(&args.pool),
                signal_name
            );
        }
        128 + signal
    } else {
        let code = child_status.code().unwrap_or(1);
        tap_append_message(
            &mut pool_handle,
            durability,
            &lifecycle_tags,
            &json!({
                "kind": "exit",
                "code": code,
                "elapsed_ms": elapsed_ms,
            }),
        )?;
        if status_on_tty_stderr {
            eprintln!(
                "tapped {line_count} lines ({}) -> {} exit {}",
                format_tap_elapsed(elapsed_ms),
                super::output_support::human_literal(&args.pool),
                code
            );
        }
        code
    };

    Ok(CommandResult::with_code(exit_code))
}

#[derive(Clone, Copy)]
enum TapStream {
    Stdout,
    Stderr,
}

impl TapStream {
    fn as_str(self) -> &'static str {
        match self {
            TapStream::Stdout => "stdout",
            TapStream::Stderr => "stderr",
        }
    }
}

enum TapEvent {
    Line { stream: TapStream, raw_line: String },
    ReaderError(Error),
}

fn tap_append_message(
    pool: &mut Pool,
    durability: Durability,
    tags: &[String],
    data: &Value,
) -> Result<(), Error> {
    let payload = lite3::encode_message(tags, data)?;
    let timestamp_ns = now_ns()?;
    let options = AppendOptions::new(timestamp_ns, durability);
    pool.append_with_options(payload.as_slice(), options)?;
    Ok(())
}

fn trim_tap_line_endings(raw_line: &str) -> String {
    raw_line.trim_end_matches(['\r', '\n']).to_string()
}

fn tap_spawn_reader<R>(
    reader: R,
    stream: TapStream,
    passthrough: bool,
    max_line_bytes: u64,
    tx: mpsc::SyncSender<TapEvent>,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        use std::io::Write as _;

        let mut reader = std::io::BufReader::new(reader);
        let mut passthrough_enabled = passthrough;
        let mut buffer = Vec::new();
        loop {
            match tap_read_line(&mut reader, &mut buffer, max_line_bytes) {
                Ok(None) => break,
                Ok(Some(line)) => {
                    if passthrough_enabled {
                        let write_result = match stream {
                            TapStream::Stdout => {
                                let mut out = io::stdout();
                                out.write_all(line.as_bytes()).and_then(|_| out.flush())
                            }
                            TapStream::Stderr => {
                                let mut err = io::stderr();
                                err.write_all(line.as_bytes()).and_then(|_| err.flush())
                            }
                        };
                        if let Err(err) = write_result {
                            if err.kind() == std::io::ErrorKind::BrokenPipe {
                                passthrough_enabled = false;
                            } else {
                                let _ = tx.send(TapEvent::ReaderError(
                                    Error::new(ErrorKind::Io)
                                        .with_message("failed to write passthrough output")
                                        .with_source(err),
                                ));
                                return;
                            }
                        }
                    }
                    if tx
                        .send(TapEvent::Line {
                            stream,
                            raw_line: line,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(TapEvent::ReaderError(err));
                    return;
                }
            }
        }
    })
}

fn tap_read_line<R: std::io::BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max_line_bytes: u64,
) -> Result<Option<String>, Error> {
    use std::io::BufRead as _;
    buffer.clear();
    // Read one overflow byte first, then at most two terminator bytes. A
    // newline-free writer cannot force us to wait beyond an oversized payload.
    let mut read_limit = max_line_bytes.saturating_add(1);
    loop {
        let read = reader
            .take(read_limit)
            .read_until(b'\n', buffer)
            .map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read wrapped command output")
                    .with_source(err)
            })?;
        if read == 0 {
            if buffer.is_empty() {
                return Ok(None);
            }
            break;
        }
        let line_bytes = buffer
            .iter()
            .rposition(|byte| !matches!(byte, b'\r' | b'\n'))
            .map_or(0, |index| index + 1);
        if buffer.len() as u64 > max_line_bytes.saturating_add(2)
            || line_bytes as u64 > max_line_bytes
        {
            return Err(Error::new(ErrorKind::Usage)
                .with_message(format!("captured line exceeds pool ring capacity ({max_line_bytes} bytes)"))
                .with_hint("Write shorter lines or capture into a larger pool; use --create-size when creating a new pool."));
        }
        if buffer.last() == Some(&b'\n') || (read as u64) < read_limit {
            break;
        }
        read_limit = 1;
    }
    String::from_utf8(buffer.clone()).map(Some).map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("wrapped command output is not valid UTF-8")
            .with_source(err)
    })
}

fn tap_spawn_error(command: &[String], err: std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        let hint_cmd = command
            .first()
            .cloned()
            .unwrap_or_else(|| "<command>".to_string());
        return Error::new(ErrorKind::Usage)
            .with_message(format!("wrapped command not found: {hint_cmd}"))
            .with_hint("Check PATH or use an absolute executable path.")
            .with_source(err);
    }
    Error::new(ErrorKind::Io)
        .with_message("failed to spawn wrapped command")
        .with_hint("Check command arguments and executable permissions.")
        .with_source(err)
}

fn tap_terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn tap_exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn tap_exit_signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn tap_signal_name(signal: i32) -> String {
    match signal {
        2 => "SIGINT".to_string(),
        9 => "SIGKILL".to_string(),
        11 => "SIGSEGV".to_string(),
        15 => "SIGTERM".to_string(),
        _ => format!("SIG{signal}"),
    }
}

fn format_tap_elapsed(elapsed_ms: u64) -> String {
    format!("{:.1}s", (elapsed_ms as f64) / 1000.0)
}

#[cfg(unix)]
fn tap_spawn_signal_forwarder(child_pid: i32) {
    let mut signals = match signal_hook::iterator::Signals::new([libc::SIGINT, libc::SIGTERM]) {
        Ok(signals) => signals,
        Err(_) => return,
    };
    std::thread::spawn(move || {
        for signal in signals.forever() {
            tap_forward_signal(child_pid, signal);
        }
    });
}

#[cfg(not(unix))]
fn tap_spawn_signal_forwarder(_child_pid: i32) {}

#[cfg(unix)]
fn tap_forward_signal(child_pid: i32, signal: i32) {
    let _ = unsafe { libc::kill(child_pid, signal) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_reader_preserves_crlf_and_unterminated_lines() {
        let mut reader = Cursor::new(b"12345678\r\nlast\r\nend");
        let mut buffer = Vec::new();
        for expected in ["12345678\r\n", "last\r\n", "end"] {
            assert_eq!(
                tap_read_line(&mut reader, &mut buffer, 8)
                    .unwrap()
                    .as_deref(),
                Some(expected)
            );
        }
        assert!(
            tap_read_line(&mut reader, &mut buffer, 8)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn bounded_reader_rejects_one_overflow_byte_without_reading_more() {
        let mut reader = Cursor::new(b"123456789still-running");
        let mut buffer = Vec::new();
        let error = tap_read_line(&mut reader, &mut buffer, 8).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(reader.position(), 9);
        assert_eq!(buffer.len(), 9);
        assert!(error.hint().unwrap().contains("larger pool"));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_unterminated_output_stops_and_reaps_owned_child() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("capture.plasmite");
        let pool = Pool::create(&path, PoolOptions::new(8192).with_index_capacity(0)).unwrap();
        let capacity = pool.info().unwrap().ring_size;
        drop(pool);
        let pid_path = temporary.path().join("writer.pid");
        let args = TapArgs {
            pool: "capture".into(), create: false, create_size: None,
            tags: vec![], quiet: true, durability: "fast".into(),
            command: vec!["python3".into(), "-c".into(),
                "import os,sys,time; open(sys.argv[1], 'w').write(str(os.getpid())); sys.stdout.buffer.write(b'x' * int(sys.argv[2])); sys.stdout.buffer.flush(); time.sleep(60)".into(),
                pid_path.to_str().unwrap().into(), (capacity + 1).to_string()],
        };
        let context = CliContext::new(
            temporary.path().to_path_buf(),
            crate::ColorMode::Never,
            false,
        );
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = tx.send(run(args, &context));
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
        // Clean up the test's child even if this regression ever hangs again.
        if result.is_err() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        worker.join().unwrap();
        let error = result
            .expect("tap must fail before the writer exits")
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(
            error
                .message()
                .unwrap()
                .contains("captured line exceeds pool ring capacity")
        );
        assert!(error.hint().unwrap().contains("--create-size"));
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "wrapped child remains alive"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
