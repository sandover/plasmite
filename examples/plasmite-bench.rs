//! Purpose: Developer-only benchmark runner for plasmite pools.
//! Exports: None (example binary entry point only).
//! Role: Internal CLI that invokes the bench harness and spawns workers.
//! Invariants: Not part of the shipped user CLI; built via `cargo run --example`.
//! Invariants: Bench workers are spawned via a hidden subcommand.
#![allow(clippy::result_large_err)]
use std::error::Error as StdError;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value, json};

#[path = "../src/bench.rs"]
mod bench;

use bench::{BenchArgs, BenchFormat, WorkerArgs, WorkerRole};
use plasmite::api::{Durability, Error, ErrorKind, to_exit_code};

fn main() {
    let exit_code = match run() {
        Ok(()) => 0,
        Err(err) => {
            emit_error(&err);
            to_exit_code(err.kind())
        }
    };
    std::process::exit(exit_code);
}

fn run() -> Result<(), Error> {
    let cli = BenchCli::parse();
    match cli.command {
        Some(BenchCommand::BenchWorker {
            role,
            pool,
            messages,
            payload_bytes,
            durability,
            out_json,
        }) => bench::run_worker(WorkerArgs {
            pool_path: pool,
            role: WorkerRole::parse(&role)?,
            messages,
            payload_bytes: payload_bytes as usize,
            out_json,
            durability: parse_durability(&durability)?,
        }),
        None => {
            let pool_sizes = if cli.options.pool_size.is_empty() {
                vec![parse_size("1M")?, parse_size("64M")?]
            } else {
                cli.options
                    .pool_size
                    .iter()
                    .map(|value| parse_size(value))
                    .collect::<Result<Vec<_>, _>>()?
            };

            let payload_sizes = if cli.options.payload_bytes.is_empty() {
                vec![128usize, 1024usize, 16 * 1024usize]
            } else {
                cli.options
                    .payload_bytes
                    .iter()
                    .map(|value| parse_usize(value, "payload-bytes"))
                    .collect::<Result<Vec<_>, _>>()?
            };

            let writer_counts = if cli.options.writers.is_empty() {
                vec![1usize, 2usize, 4usize, 8usize]
            } else {
                cli.options
                    .writers
                    .iter()
                    .map(|value| parse_usize(value, "writers"))
                    .collect::<Result<Vec<_>, _>>()?
            };

            let durabilities = parse_bench_durabilities(&cli.options.durability)?;
            let format = BenchFormat::parse(&cli.options.format)?;
            bench::run_bench(
                BenchArgs {
                    work_dir: cli.options.work_dir,
                    pool_sizes,
                    payload_sizes,
                    messages: cli.options.messages,
                    writers: writer_counts,
                    format,
                    durabilities,
                },
                env!("CARGO_PKG_VERSION"),
            )
        }
    }
}

#[derive(Parser)]
#[command(
    name = "plasmite-bench",
    version,
    about = "Developer-only benchmark runner",
    long_about = None,
    disable_help_subcommand = true
)]
struct BenchCli {
    #[command(flatten)]
    options: BenchOptions,
    #[command(subcommand)]
    command: Option<BenchCommand>,
}

#[derive(Args)]
struct BenchOptions {
    #[arg(
        long,
        help = "Directory for temporary pools/artifacts (default: .scratch/plasmite-bench-<pid>-<ts>)"
    )]
    work_dir: Option<PathBuf>,
    #[arg(long = "pool-size", help = "Repeatable pool size (bytes or K/M/G)")]
    pool_size: Vec<String>,
    #[arg(
        long = "payload-bytes",
        help = "Repeatable payload target size (bytes)"
    )]
    payload_bytes: Vec<String>,
    #[arg(long, default_value_t = 20_000, help = "Messages per scenario")]
    messages: u64,
    #[arg(
        long,
        help = "Repeatable writer counts for contention scenarios (default: 1,2,4,8)"
    )]
    writers: Vec<String>,
    #[arg(
        long,
        help = "Durability mode(s): fast|flush|both (repeatable; default: fast)"
    )]
    durability: Vec<String>,
    #[arg(long, default_value = "both", help = "Output format: json|table|both")]
    format: String,
}

#[derive(Subcommand)]
enum BenchCommand {
    #[command(hide = true)]
    BenchWorker {
        #[arg(help = "worker role: writer|follower")]
        role: String,
        #[arg(long = "pool", help = "Pool file path")]
        pool: PathBuf,
        #[arg(long, help = "Message count (writer) or upper bound (follower)")]
        messages: u64,
        #[arg(long = "payload-bytes", help = "Approximate payload size in bytes")]
        payload_bytes: u64,
        #[arg(long, default_value = "fast", help = "Durability mode: fast|flush")]
        durability: String,
        #[arg(long = "out-json", help = "Write worker result JSON to this path")]
        out_json: PathBuf,
    },
}

fn parse_size(input: &str) -> Result<u64, Error> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("invalid size")
            .with_hint("Use bytes or a K/M/G suffix (example: 64M)."));
    }
    let (value_str, multiplier) = match trimmed.chars().last() {
        Some('k') | Some('K') => (&trimmed[..trimmed.len() - 1], 1024u64),
        Some('m') | Some('M') => (&trimmed[..trimmed.len() - 1], 1024u64 * 1024),
        Some('g') | Some('G') => (&trimmed[..trimmed.len() - 1], 1024u64 * 1024 * 1024),
        _ => (trimmed, 1),
    };

    let base = value_str.trim().parse::<u64>().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid size")
            .with_hint("Use bytes or a K/M/G suffix (example: 64M).")
            .with_source(err)
    })?;
    base.checked_mul(multiplier).ok_or_else(|| {
        Error::new(ErrorKind::Usage)
            .with_message("size overflow")
            .with_hint("Use a smaller size value.")
    })
}

fn parse_usize(input: &str, label: &str) -> Result<usize, Error> {
    input.trim().parse::<usize>().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message(format!("invalid {label}"))
            .with_hint(format!("Use a numeric value for {label}."))
            .with_source(err)
    })
}

fn parse_bench_durabilities(values: &[String]) -> Result<Vec<Durability>, Error> {
    if values.is_empty() {
        return Ok(vec![Durability::Fast]);
    }

    let mut out = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("both") {
            push_durability(&mut out, Durability::Fast);
            push_durability(&mut out, Durability::Flush);
            continue;
        }
        let durability = parse_durability(trimmed)?;
        push_durability(&mut out, durability);
    }

    if out.is_empty() {
        out.push(Durability::Fast);
    }
    Ok(out)
}

fn push_durability(out: &mut Vec<Durability>, durability: Durability) {
    if !out.contains(&durability) {
        out.push(durability);
    }
}

fn parse_durability(input: &str) -> Result<Durability, Error> {
    match input.trim() {
        "fast" => Ok(Durability::Fast),
        "flush" => Ok(Durability::Flush),
        _ => Err(Error::new(ErrorKind::Usage)
            .with_message("invalid durability")
            .with_hint("Use fast or flush.")),
    }
}

fn emit_error(err: &Error) {
    if io::stderr().is_terminal() {
        eprintln!("{}", error_text(err));
        return;
    }

    let value = error_json(err);
    let json = serde_json::to_string(&value).unwrap_or_else(|_| {
        "{\"error\":{\"kind\":\"Internal\",\"message\":\"json encode failed\"}}".to_string()
    });
    eprintln!("{json}");
}

fn error_message(err: &Error) -> String {
    if let Some(message) = err.message() {
        return message.to_string();
    }
    match err.kind() {
        ErrorKind::Internal => "internal error".to_string(),
        ErrorKind::Usage => "usage error".to_string(),
        ErrorKind::NotFound => "not found".to_string(),
        ErrorKind::AlreadyExists => "already exists".to_string(),
        ErrorKind::Busy => "resource is busy".to_string(),
        ErrorKind::Permission => "permission denied".to_string(),
        ErrorKind::Corrupt => "corrupt data".to_string(),
        ErrorKind::Io => "i/o error".to_string(),
    }
}

fn error_causes(err: &Error) -> Vec<String> {
    let mut causes = Vec::new();
    let mut cur = err.source();
    while let Some(source) = cur {
        causes.push(source.to_string());
        cur = source.source();
    }
    causes
}

fn error_json(err: &Error) -> Value {
    let mut inner = Map::new();
    inner.insert("kind".to_string(), json!(format!("{:?}", err.kind())));
    inner.insert("message".to_string(), json!(error_message(err)));
    if let Some(hint) = err.hint() {
        inner.insert("hint".to_string(), json!(hint));
    }
    if let Some(path) = err.path() {
        inner.insert("path".to_string(), json!(path.display().to_string()));
    }
    if let Some(seq) = err.seq() {
        inner.insert("seq".to_string(), json!(seq));
    }
    if let Some(offset) = err.offset() {
        inner.insert("offset".to_string(), json!(offset));
    }
    let causes = error_causes(err);
    if !causes.is_empty() {
        inner.insert("causes".to_string(), json!(causes));
    }

    let mut outer = Map::new();
    outer.insert("error".to_string(), Value::Object(inner));
    Value::Object(outer)
}

fn error_text(err: &Error) -> String {
    let mut lines = Vec::new();
    lines.push(format!("error: {}", error_message(err)));

    if let Some(hint) = err.hint() {
        lines.push(format!("hint: {hint}"));
    }
    if let Some(path) = err.path() {
        lines.push(format!("path: {}", path.display()));
    }
    if let Some(seq) = err.seq() {
        lines.push(format!("seq: {seq}"));
    }
    if let Some(offset) = err.offset() {
        lines.push(format!("offset: {offset}"));
    }
    let causes = error_causes(err);
    if !causes.is_empty() {
        for cause in causes {
            lines.push(format!("cause: {cause}"));
        }
    }
    lines.join("\n")
}
