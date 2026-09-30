//! Purpose: Run commands for named access to a shared pool directory.
//! Exports: `run`.
//! Role: Keep access command presentation at the CLI boundary.

use super::context::CliContext;
use super::result::CommandResult;
use crate::AccessSubcommand;
use plasmite::api::Error;
use plasmite::api::browser_trust::{self, BrowserTrustStatus};
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
            let commands = setup_commands(&status.destination);
            let browser = browser_status(&status.destination);
            emit_status(
                &status,
                browser.as_ref().and_then(|result| result.as_ref().ok()),
                browser.as_ref().and_then(|result| result.as_ref().err()),
                Some(&commands),
            );
            if cfg!(target_os = "macos")
                && io::stdin().is_terminal()
                && io::stdout().is_terminal()
                && let Some(Ok(trust)) = browser
                && !trust.installed
            {
                offer_browser_trust(&trust);
            }
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Status { url } => {
            let status = plasmite::api::access::status(&url)?;
            let browser = status
                .credentials_saved
                .then(|| browser_status(&status.destination))
                .flatten();
            emit_status(
                &status,
                browser.as_ref().and_then(|result| result.as_ref().ok()),
                browser.as_ref().and_then(|result| result.as_ref().err()),
                None,
            );
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Disconnect { url } => {
            plasmite::api::access::disconnect(&url)?;
            emit_disconnected(&url);
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Untrust { fingerprint } => {
            browser_trust::remove(&fingerprint)?;
            if io::stdout().is_terminal() {
                println!(
                    "Removed browser trust for certificate {}.",
                    fingerprint.to_ascii_lowercase()
                );
                println!("Saved native credentials remain available.");
            } else {
                println!(
                    "{}",
                    json!({ "certificate_sha256": fingerprint.to_ascii_lowercase(), "browser_trust_installed": false })
                );
            }
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Keys => {
            let keys = crate::secure_serve::keys(context.pool_dir())?;
            emit_keys(&keys);
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Revoke { id } => {
            crate::secure_serve::revoke(context.pool_dir(), &id)?;
            if io::stdout().is_terminal() {
                println!("Revoked access key {id}.");
            } else {
                println!("{}", json!({ "id": id, "revoked": true }));
            }
            Ok(CommandResult::ok())
        }
    }
}

fn browser_status(destination: &str) -> Option<Result<BrowserTrustStatus, Error>> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Some(browser_trust::status(destination))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = destination;
        None
    }
}

fn emit_disconnected(destination: &str) {
    if io::stdout().is_terminal() {
        println!("Removed saved credentials for {destination}.");
        println!("The server key remains valid until its owner revokes it.");
    } else {
        println!(
            "{}",
            json!({ "destination": destination, "credentials_saved": false })
        );
    }
}

fn emit_keys(keys: &serde_json::Value) {
    if !io::stdout().is_terminal() {
        println!("{keys}");
        return;
    }

    let rows = keys["keys"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if rows.is_empty() {
        println!("No access keys.");
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    println!("ID  NAME  STATE  CREATED  LAST USED");
    for row in rows {
        let id = row["id"].as_str().unwrap_or("?");
        let name = row["name"].as_str().unwrap_or("?");
        let state = if row["revoked"].as_bool().unwrap_or(false) {
            "revoked"
        } else {
            "active"
        };
        let created = row["created_at"].as_u64().map_or_else(
            || "unknown".to_string(),
            |timestamp| relative_time(now, timestamp),
        );
        let last_used = row["last_used_at"].as_u64().map_or_else(
            || "never".to_string(),
            |timestamp| relative_time(now, timestamp),
        );
        println!("{id}  {name}  {state}  {created}  {last_used}");
    }
}

fn relative_time(now: u64, timestamp: u64) -> String {
    super::output_support::format_relative_time(Some(
        now.saturating_sub(timestamp).saturating_mul(1000),
    ))
}

fn emit_status(
    status: &plasmite::api::access::ConnectionStatus,
    browser: Option<&BrowserTrustStatus>,
    browser_error: Option<&Error>,
    setup: Option<&[String; 2]>,
) {
    if io::stdout().is_terminal() {
        println!("Server: {}", status.destination);
        println!("Credentials saved: {}", yes_no(status.credentials_saved));
        println!("Reachable: {}", optional_yes_no(status.reachable));
        println!("Access accepted: {}", optional_yes_no(status.accepted));
        if let Some(browser) = browser {
            #[cfg(target_os = "windows")]
            println!(
                "Certificate installed in Windows Root: {}",
                yes_no(browser.installed)
            );
            #[cfg(not(target_os = "windows"))]
            println!("Browser trust installed: {}", yes_no(browser.installed));
            println!("Certificate SHA-256: {}", browser.certificate_sha256);
            println!("Certificate expires: {}", format_expiry(browser.expires_at));
        } else if let Some(error) = browser_error {
            println!("Browser trust: unavailable ({error})");
        }
        if let Some(problem) = &status.problem {
            println!("What to do: {problem}");
        }
        if let Some(commands) = setup {
            println!("\nAdd Plasmite to your MCP client:");
            println!("Claude Code: {}", commands[0]);
            println!("Codex CLI:  {}", commands[1]);
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
                "browser_trust": browser.map(|trust| json!({
                    "installed": trust.installed,
                    "certificate_sha256": trust.certificate_sha256,
                    "expires_at": trust.expires_at,
                    "names": trust.names,
                })),
                "browser_trust_problem": browser_error.map(ToString::to_string),
                "mcp_setup_commands": setup,
            })
        );
    }
}

fn offer_browser_trust(trust: &BrowserTrustStatus) {
    println!("\nBrowser certificate trust for {}", trust.destination);
    println!("Named addresses: {}", trust.names.join(", "));
    println!("Certificate SHA-256: {}", trust.certificate_sha256);
    println!("Certificate expires: {}", format_expiry(trust.expires_at));
    #[cfg(target_os = "macos")]
    println!(
        "Scope: current user's login keychain for SSL; Chrome, Safari, and other macOS TLS apps may use this trust."
    );
    #[cfg(target_os = "windows")]
    println!(
        "Scope: current user's Windows Root store. Edge, Chrome, and other Windows apps may use this trust."
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return;
    }

    eprint!("Trust this certificate for browser access? [y/N] ");
    use std::io::Write;
    if io::stderr().flush().is_err() {
        return;
    }
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() || !answer.trim().eq_ignore_ascii_case("y") {
        return;
    }
    match browser_trust::install(&trust.destination, &trust.certificate_sha256) {
        Ok(_) => {
            #[cfg(target_os = "windows")]
            println!(
                "Certificate installed in Windows Root. To remove it later: plasmite access untrust {}",
                trust.certificate_sha256
            );
            #[cfg(not(target_os = "windows"))]
            println!(
                "Browser trust installed. To remove it later: plasmite access untrust {}",
                trust.certificate_sha256
            );
            if let Err(error) = browser_trust::open(&trust.destination) {
                eprintln!("Could not open {}: {error}", trust.destination);
            }
        }
        Err(error) => {
            eprintln!("Browser trust setup failed: {error}. Native access remains saved.")
        }
    }
}

fn format_expiry(timestamp: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|value| {
            value
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| timestamp.to_string())
}

fn setup_commands(destination: &str) -> [String; 2] {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_else(|| "plasmite".to_string());
    let executable = shell_quote(&executable);
    let destination = shell_quote(destination);
    [
        format!(
            "claude mcp add --scope user --transport stdio plasmite -- {executable} mcp --remote {destination}"
        ),
        format!("codex mcp add plasmite -- {executable} mcp --remote {destination}"),
    ]
}

fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        format!("\"{value}\"")
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
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
