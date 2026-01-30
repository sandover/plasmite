// CLI entry point for v0.0.1 commands with JSON output.
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::json;

use plasmite::core::error::{to_exit_code, Error, ErrorKind};

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
    let cli = Cli::try_parse().map_err(|err| {
        Error::new(ErrorKind::Usage)
            .with_message(err.to_string())
    })?;

    let pool_dir = cli.dir.unwrap_or_else(default_pool_dir);

    match cli.command {
        Command::Version => {
            let output = json!({
                "name": "plasmite",
                "version": env!("CARGO_PKG_VERSION"),
            });
            emit_json(output);
            Ok(())
        }
        Command::Pool { command } => match command {
            PoolCommand::Create { name, .. } => {
                let _path = resolve_poolref(&name, &pool_dir)?;
                Err(Error::new(ErrorKind::Usage).with_message("pool create not implemented"))
            }
            PoolCommand::Info { name } => {
                let _path = resolve_poolref(&name, &pool_dir)?;
                Err(Error::new(ErrorKind::Usage).with_message("pool info not implemented"))
            }
            PoolCommand::Bounds { name } => {
                let _path = resolve_poolref(&name, &pool_dir)?;
                Err(Error::new(ErrorKind::Usage).with_message("pool bounds not implemented"))
            }
        },
        Command::Poke { pool, .. } => {
            let _path = resolve_poolref(&pool, &pool_dir)?;
            Err(Error::new(ErrorKind::Usage).with_message("poke not implemented"))
        }
        Command::Get { pool, .. } => {
            let _path = resolve_poolref(&pool, &pool_dir)?;
            Err(Error::new(ErrorKind::Usage).with_message("get not implemented"))
        }
        Command::Peek { pool, .. } => {
            let _path = resolve_poolref(&pool, &pool_dir)?;
            Err(Error::new(ErrorKind::Usage).with_message("peek not implemented"))
        }
    }
}

#[derive(Parser)]
#[command(name = "plasmite", version, about = "Plasmite CLI")]
struct Cli {
    /// Override the pool directory.
    #[arg(long)]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    Poke {
        pool: String,
        #[arg(long)]
        descrip: Vec<String>,
        #[arg(long = "data-json")]
        data_json: Option<String>,
        #[arg(long = "data")]
        data_file: Option<String>,
    },
    Get {
        pool: String,
        seq: u64,
    },
    Peek {
        pool: String,
        #[arg(long)]
        tail: Option<u64>,
        #[arg(long)]
        follow: bool,
        #[arg(long = "idle-timeout")]
        idle_timeout: Option<String>,
    },
    Version,
}

#[derive(Subcommand)]
enum PoolCommand {
    Create {
        name: String,
        #[arg(long)]
        size: Option<String>,
    },
    Info {
        name: String,
    },
    Bounds {
        name: String,
    },
}

fn default_pool_dir() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".plasmite").join("pools")
}

fn resolve_poolref(input: &str, pool_dir: &Path) -> Result<PathBuf, Error> {
    if input.contains('/') {
        return Ok(PathBuf::from(input));
    }
    if input.ends_with(".plasmite") {
        return Ok(pool_dir.join(input));
    }
    Ok(pool_dir.join(format!("{input}.plasmite")))
}

fn emit_json(value: serde_json::Value) {
    let json = if io::stdout().is_terminal() {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string());
    println!("{json}");
}

fn emit_error(err: &Error) {
    let value = json!({
        "error": {
            "kind": format!("{:?}", err.kind()),
            "message": err.to_string(),
        }
    });
    let json = if io::stderr().is_terminal() {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .unwrap_or_else(|_| "{\"error\":\"json encode failed\"}".to_string());
    eprintln!("{json}");
}
