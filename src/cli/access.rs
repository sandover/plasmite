//! Purpose: Run commands for named access to a shared pool directory.
//! Exports: `run`.
//! Role: Keep access command presentation at the CLI boundary.

use super::context::CliContext;
use super::output_support::human_literal;
use super::result::CommandResult;
use crate::AccessSubcommand;
use plasmite::api::Error;
use plasmite::api::browser_trust::{self, BrowserTrustStatus};
use serde_json::json;
use std::io::{self, IsTerminal};

pub(super) fn run(command: AccessSubcommand, context: &CliContext) -> Result<CommandResult, Error> {
    let json_output = context.json_output();
    match command {
        AccessSubcommand::List { .. } => {
            let destinations = plasmite::api::access::list()?;
            if json_output {
                let rows: Vec<_> = destinations
                    .iter()
                    .map(|destination| json!({ "destination": destination }))
                    .collect();
                super::output::emit_json(json!(rows), context.color_mode());
            } else if destinations.is_empty() {
                println!("No saved connections.");
            } else {
                for destination in destinations {
                    println!("{}", human_literal(&destination));
                }
            }
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Invite {
            name, legacy_name, ..
        } => {
            let name = name.or(legacy_name).expect("clap requires one client name");
            let access_key = crate::secure_serve::invite(context.pool_dir(), &name)?;
            if !json_output {
                println!("Access key for {}: {access_key}", human_literal(&name));
                println!(
                    "Use this key to sign in through the server's web page or approve your MCP client."
                );
                println!(
                    "For Plasmite CLI access, run `plasmite access connect <server-address>` on the client machine."
                );
            } else {
                println!("{}", json!({ "name": name, "access_key": access_key }));
            }
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Connect { url, .. } => {
            let key = read_access_key()?;
            let status = plasmite::api::access::connect(&url, &key)?;
            let commands = setup_commands(&status.destination);
            let browser = browser_status(&status.destination);
            emit_status(
                &status,
                browser.as_ref().and_then(|result| result.as_ref().ok()),
                browser.as_ref().and_then(|result| result.as_ref().err()),
                Some(&commands),
                json_output,
            );
            if !json_output
                && cfg!(any(target_os = "macos", target_os = "windows"))
                && io::stdin().is_terminal()
                && io::stdout().is_terminal()
                && let Some(Ok(trust)) = browser
                && !trust.installed
            {
                offer_browser_trust(&trust);
            }
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Status { url, .. } => {
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
                json_output,
            );
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Disconnect { url, .. } => {
            plasmite::api::access::disconnect(&url)?;
            emit_disconnected(&url, json_output);
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Untrust { fingerprint, .. } => {
            browser_trust::remove(&fingerprint)?;
            if !json_output {
                println!(
                    "Removed browser trust for certificate {}.",
                    human_literal(&fingerprint.to_ascii_lowercase())
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
        AccessSubcommand::Keys { .. } => {
            let keys = crate::secure_serve::keys(context.pool_dir())?;
            emit_keys(&keys, json_output);
            Ok(CommandResult::ok())
        }
        AccessSubcommand::Revoke { id, .. } => {
            crate::secure_serve::revoke(context.pool_dir(), &id)?;
            if !json_output {
                println!("Revoked access key {}.", human_literal(&id));
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

fn emit_disconnected(destination: &str, json_output: bool) {
    if !json_output {
        println!(
            "Removed saved credentials for {}.",
            human_literal(destination)
        );
        println!("The server key remains valid until its owner revokes it.");
    } else {
        println!(
            "{}",
            json!({ "destination": destination, "credentials_saved": false })
        );
    }
}

fn emit_keys(keys: &serde_json::Value, json_output: bool) {
    if json_output {
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
    let mut table_rows = Vec::with_capacity(rows.len());
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
        table_rows.push(vec![
            id.to_owned(),
            name.to_owned(),
            state.to_owned(),
            created,
            last_used,
        ]);
    }
    super::output_support::emit_table(
        &["ID", "NAME", "STATE", "CREATED", "LAST USED"],
        &table_rows,
    );
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
    json_output: bool,
) {
    if !json_output {
        println!("Server: {}", human_literal(&status.destination));
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
            println!(
                "Certificate SHA-256: {}",
                human_literal(&browser.certificate_sha256)
            );
            println!("Certificate expires: {}", format_expiry(browser.expires_at));
        } else if let Some(error) = browser_error {
            println!(
                "Browser trust: unavailable ({})",
                human_literal(&error.to_string())
            );
        }
        if let Some(problem) = &status.problem {
            println!("What to do: {}", human_literal(problem));
        }
        if let Some(commands) = setup {
            println!("\nAdd Plasmite to your MCP client:");
            println!("Claude Code: {}", human_literal(&commands[0]));
            println!("Codex CLI:  {}", human_literal(&commands[1]));
            #[cfg(windows)]
            println!("Run these commands in PowerShell.");
            println!(
                "Then sign in from the MCP client and enter the access key in its browser approval page."
            );
            println!(
                "The saved native connection is for Plasmite commands; it does not sign in the MCP client."
            );
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
    println!(
        "\nBrowser certificate trust for {}",
        human_literal(&trust.destination)
    );
    println!(
        "Named addresses: {}",
        human_literal(&trust.names.join(", "))
    );
    println!(
        "Certificate SHA-256: {}",
        human_literal(&trust.certificate_sha256)
    );
    println!("Certificate expires: {}", format_expiry(trust.expires_at));
    #[cfg(target_os = "macos")]
    println!(
        "Scope: current user's login keychain for SSL; Chrome, Safari, and other macOS TLS apps may use this trust."
    );
    #[cfg(target_os = "windows")]
    println!(
        "Scope: current user's Windows Root store. Edge, Chrome, and other Windows apps may use this trust."
    );
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
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
                    human_literal(&trust.certificate_sha256)
                );
                #[cfg(not(target_os = "windows"))]
                println!(
                    "Browser trust installed. To remove it later: plasmite access untrust {}",
                    human_literal(&trust.certificate_sha256)
                );
                if let Err(error) = browser_trust::open(&trust.destination) {
                    eprintln!(
                        "Could not open {}: {}",
                        human_literal(&trust.destination),
                        human_literal(&error.to_string())
                    );
                }
            }
            Err(error) => {
                eprintln!(
                    "Browser trust setup failed: {}. Native access remains saved.",
                    human_literal(&error.to_string())
                )
            }
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
    let endpoint = format!("{}/mcp", destination.trim_end_matches('/'));
    let endpoint = shell_quote(&endpoint);
    [
        format!("claude mcp add --scope user --transport http plasmite {endpoint}"),
        format!(
            "codex mcp add plasmite --url {endpoint} --oauth-client-registration dcr --oauth-resource {endpoint}"
        ),
    ]
}

fn shell_quote(value: &str) -> String {
    #[cfg(windows)]
    {
        // PowerShell treats doubled apostrophes as one literal apostrophe.
        format!("'{}'", value.replace('\'', "''"))
    }
    #[cfg(not(windows))]
    {
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

#[cfg(test)]
mod tests {
    use super::{setup_commands, shell_quote};

    #[test]
    fn setup_commands_use_direct_http_mcp_with_oauth() {
        let commands = setup_commands("https://pools.example.net/");
        let endpoint = shell_quote("https://pools.example.net/mcp");
        assert_eq!(
            commands,
            [
                format!("claude mcp add --scope user --transport http plasmite {endpoint}"),
                format!(
                    "codex mcp add plasmite --url {endpoint} --oauth-client-registration dcr --oauth-resource {endpoint}"
                ),
            ]
        );
        assert!(
            commands
                .iter()
                .all(|command| !command.contains("mcp --remote"))
        );
    }

    #[test]
    fn shell_quote_handles_quotes_and_shell_metacharacters() {
        let expected = if cfg!(windows) {
            "'pool''s & address'"
        } else {
            "'pool'\\''s & address'"
        };
        assert_eq!(shell_quote("pool's & address"), expected);
    }
}
