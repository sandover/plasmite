//! Purpose: Run commands for named access to a shared pool directory.
//! Exports: `run`.
//! Role: Keep access command presentation at the CLI boundary.

use super::context::CliContext;
use super::result::CommandResult;
use crate::AccessSubcommand;
use plasmite::api::Error;
use serde_json::json;
use std::io::{self, IsTerminal};

pub(super) fn run(command: AccessSubcommand, context: &CliContext) -> Result<CommandResult, Error> {
    match command {
        AccessSubcommand::Invite { name } => {
            let access_key = crate::secure_serve::invite(context.pool_dir(), &name)?;
            if io::stdout().is_terminal() {
                println!("Access key for {name}: {access_key}");
                println!("On the client machine, run `plasmite access connect <server-address>`.");
            } else {
                println!("{}", json!({ "name": name, "access_key": access_key }));
            }
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Connect { url } => {
            let key = read_access_key()?;
            let status = plasmite::api::access::connect(&url, &key)?;
            emit_status(&status);
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Status { url } => {
            let status = plasmite::api::access::status(&url)?;
            emit_status(&status);
            Ok(CommandResult::ok())
        }
    }
}

fn emit_status(status: &plasmite::api::access::ConnectionStatus) {
    if io::stdout().is_terminal() {
        println!("Server: {}", status.destination);
        println!("Credentials saved: {}", yes_no(status.credentials_saved));
        println!("Reachable: {}", optional_yes_no(status.reachable));
        println!("Access accepted: {}", optional_yes_no(status.accepted));
        if let Some(problem) = &status.problem {
            println!("What to do: {problem}");
        }
    } else {
        println!(
            "{}",
            json!({
                "destination": status.destination,
                "credentials_saved": status.credentials_saved,
                "reachable": status.reachable,
                "accepted": status.accepted,
                "problem": status.problem,
            })
        );
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn optional_yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn read_access_key() -> Result<String, Error> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        eprint!("Access key: ");
        use std::io::Write;
        io::stderr().flush().map_err(|err| {
            Error::new(plasmite::api::ErrorKind::Io)
                .with_message("failed to prompt for access key")
                .with_source(err)
        })?;
        let _echo = TerminalEcho::disable()?;
        let mut key = String::new();
        stdin.read_line(&mut key).map_err(|err| {
            Error::new(plasmite::api::ErrorKind::Io)
                .with_message("failed to read access key")
                .with_source(err)
        })?;
        eprintln!();
        while key.ends_with(['\n', '\r']) {
            key.pop();
        }
        Ok(key)
    } else {
        let mut key = String::new();
        stdin.read_line(&mut key).map_err(|err| {
            Error::new(plasmite::api::ErrorKind::Io)
                .with_message("failed to read access key from stdin")
                .with_source(err)
        })?;
        while key.ends_with(['\n', '\r']) {
            key.pop();
        }
        Ok(key)
    }
}

#[cfg(unix)]
struct TerminalEcho(libc::termios);

#[cfg(unix)]
impl TerminalEcho {
    fn disable() -> Result<Self, Error> {
        let fd = libc::STDIN_FILENO;
        let mut original = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(Error::new(plasmite::api::ErrorKind::Io)
                .with_message("failed to read terminal settings"));
        }
        let mut hidden = original;
        hidden.c_lflag &= !libc::ECHO;
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
            return Err(Error::new(plasmite::api::ErrorKind::Io)
                .with_message("failed to hide access key input"));
        }
        Ok(Self(original))
    }
}

#[cfg(unix)]
impl Drop for TerminalEcho {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.0) };
    }
}

#[cfg(windows)]
struct TerminalEcho(*mut std::ffi::c_void, u32);

#[cfg(windows)]
impl TerminalEcho {
    fn disable() -> Result<Self, Error> {
        use std::ffi::c_void;
        unsafe extern "system" {
            fn GetStdHandle(which: i32) -> *mut c_void;
            fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
        }
        let handle = unsafe { GetStdHandle(-10) };
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0
            || unsafe { SetConsoleMode(handle, mode & !0x0004) } == 0
        {
            return Err(Error::new(plasmite::api::ErrorKind::Io)
                .with_message("failed to hide access key input"));
        }
        Ok(Self(handle, mode))
    }
}

#[cfg(windows)]
impl Drop for TerminalEcho {
    fn drop(&mut self) {
        unsafe extern "system" {
            fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
        }
        unsafe { SetConsoleMode(self.0, self.1) };
    }
}

#[cfg(not(any(unix, windows)))]
struct TerminalEcho;

#[cfg(not(any(unix, windows)))]
impl TerminalEcho {
    fn disable() -> Result<Self, Error> {
        Err(Error::new(plasmite::api::ErrorKind::Io)
            .with_message("hidden access key input is unsupported on this platform"))
    }
}
