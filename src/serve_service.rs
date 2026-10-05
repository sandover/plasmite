//! Install and control the ordinary foreground server through the OS service manager.

use crate::access_store::{create_private_dir, ensure_private, read_json, write_atomic_json};
use crate::cli::args::ServeRunArgs;
use crate::cli::support::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_TAIL_CONCURRENCY, DEFAULT_MAX_TAIL_TIMEOUT_MS,
};
use crate::serve_registry::{self, ServerDetails};
use fs2::FileExt;
use plasmite::api::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Setup {
    pub pool_dir: PathBuf,
    pub program: PathBuf,
    pub account: String,
    pub home: PathBuf,
    pub run: ServeRunArgs,
}

#[derive(Serialize)]
pub(crate) struct Status {
    pub pool_dir: PathBuf,
    pub pid: Option<u32>,
    pub local_url: String,
    pub remote_url: Option<String>,
    pub managed: bool,
    pub startup: bool,
    pub state: String,
    pub problem: Option<String>,
    pub setup: Option<Setup>,
}

fn io_error(message: &str, path: &Path, error: std::io::Error) -> Error {
    Error::new(ErrorKind::Io)
        .with_message(message)
        .with_path(path)
        .with_source(error)
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage).with_message(message)
}

fn home() -> Result<PathBuf, Error> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| usage("service setup requires an absolute user home directory"))
}

fn directory() -> Result<PathBuf, Error> {
    Ok(home()?.join(".plasmite/services"))
}

fn id(pool_dir: &Path) -> String {
    format!(
        "net.plasmite.{:x}",
        Sha256::digest(pool_dir.as_os_str().as_encoded_bytes())
    )
}

fn setup_path(pool_dir: &Path) -> Result<PathBuf, Error> {
    Ok(directory()?.join(id(pool_dir)).join("setup.json"))
}

fn absolute(path: &Path) -> Result<PathBuf, Error> {
    fs::canonicalize(path).map_err(|error| io_error("failed to resolve service path", path, error))
}

pub(crate) fn effective_args(run: &ServeRunArgs) -> Result<ServeRunArgs, Error> {
    let mut run = run.clone();
    let positional = run.server.is_some();
    let origin_name = if positional {
        "SERVER"
    } else {
        "--shared-address"
    };
    if run.server.is_some() && run.shared_address.is_some() {
        return Err(usage(
            "supply the server address once, as SERVER or --shared-address",
        ));
    }
    run.server = run.server.take().or(run.shared_address.take());
    let port = if let Some(address) = &run.server {
        let url = Url::parse(address).map_err(|error| {
            usage(&format!("{origin_name} must be an HTTPS origin")).with_source(error)
        })?;
        if url.scheme() != "https"
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(usage(&format!("{origin_name} must be an HTTPS origin"))
                .with_hint("Use https://HOST:PORT without a pool path, credentials, query, or fragment. --remote-bind controls the listening interface."));
        }
        let port = if positional {
            url.port_or_known_default()
                .expect("HTTPS has a default port")
        } else {
            9743
        };
        run.server = Some(url.origin().ascii_serialization());
        port
    } else {
        9743
    };
    run.bind.get_or_insert_with(|| "127.0.0.1:9700".into());
    run.remote_bind
        .get_or_insert_with(|| format!("0.0.0.0:{port}"));
    let bind: SocketAddr = run
        .bind
        .as_deref()
        .unwrap()
        .parse()
        .map_err(|_| usage("invalid local bind address"))?;
    if !bind.ip().is_loopback() {
        return Err(usage(
            "local administration must bind to a loopback address",
        ));
    }
    let remote_bind: SocketAddr = run
        .remote_bind
        .as_deref()
        .unwrap()
        .parse()
        .map_err(|_| usage("invalid remote bind address")
            .with_hint("Use a numeric IP:port, such as 100.101.102.103:9743 or [fd7a:115c:a1e0::abcd]:9743. Pass the public DNS name as SERVER."))?;
    if bind == remote_bind && bind.port() != 0 {
        return Err(usage("local and remote listeners need different addresses"));
    }
    run.max_body_bytes.get_or_insert(DEFAULT_MAX_BODY_BYTES);
    run.max_tail_timeout_ms
        .get_or_insert(DEFAULT_MAX_TAIL_TIMEOUT_MS);
    run.max_tail_concurrency
        .get_or_insert(DEFAULT_MAX_TAIL_CONCURRENCY);
    if run.max_body_bytes == Some(0) || run.max_body_bytes.unwrap() > usize::MAX as u64 {
        return Err(usage(
            "--max-body-bytes must fit in memory and be greater than zero",
        ));
    }
    if run.max_tail_timeout_ms == Some(0) {
        return Err(usage("--max-tail-timeout-ms must be greater than zero"));
    }
    if run.max_tail_concurrency == Some(0) {
        return Err(usage("--max-tail-concurrency must be greater than zero"));
    }
    if run.tls_cert.is_some() != run.tls_key.is_some() {
        return Err(usage("--tls-cert and --tls-key must be supplied together"));
    }
    if let (Some(cert), Some(key)) = (&run.tls_cert, &run.tls_key) {
        crate::serve::validate_tls_files(cert, key)?;
    }
    for value in [&mut run.tls_cert, &mut run.tls_key, &mut run.front_cert]
        .into_iter()
        .flatten()
    {
        *value = absolute(value)?;
    }
    if let Some(front) = &run.front_cert {
        crate::access_store::cert_fingerprint(front)?;
    }
    Ok(run)
}

fn merge(previous: Option<&Setup>, new: &ServeRunArgs) -> Result<ServeRunArgs, Error> {
    let mut run = previous.map(|setup| setup.run.clone()).unwrap_or_default();
    if new.server.is_some() && new.shared_address.is_some() {
        return Err(usage(
            "supply the server address once, as SERVER or --shared-address",
        ));
    }
    if let Some(server) = &new.server {
        run.server = Some(server.clone());
        run.shared_address = None;
    } else if let Some(server) = &new.shared_address {
        run.server = None;
        run.shared_address = Some(server.clone());
    }
    macro_rules! update { ($($field:ident),*) => { $(if new.$field.is_some() { run.$field = new.$field.clone(); })* }; }
    update!(
        bind,
        remote_bind,
        front_cert,
        tls_cert,
        tls_key,
        max_body_bytes,
        max_tail_timeout_ms,
        max_tail_concurrency
    );
    effective_args(&run)
}

fn supported() -> Result<(), Error> {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        Ok(())
    } else {
        Err(
            usage("automatic startup currently supports Linux and macOS")
                .with_hint("Run `plasmite serve SERVER` in the foreground on this system."),
        )
    }
}

fn load(pool_dir: &Path) -> Result<Option<Setup>, Error> {
    let path = setup_path(pool_dir)?;
    if !path.exists() {
        return Ok(None);
    }
    ensure_private(path.parent().unwrap())?;
    ensure_private(&path)?;
    let setup: Setup = read_json(&path)?;
    if setup.pool_dir != pool_dir
        || setup.home != home()?
        || setup.program != path.parent().unwrap().join("plasmite")
        || setup.account != capture("id", &["-un"])?
    {
        return Err(Error::new(ErrorKind::Corrupt)
            .with_message("service setup does not match its directory or account")
            .with_path(path));
    }
    if setup
        .run
        .bind
        .as_deref()
        .and_then(|value| value.parse::<SocketAddr>().ok())
        .is_none()
        || setup
            .run
            .remote_bind
            .as_deref()
            .and_then(|value| value.parse::<SocketAddr>().ok())
            .is_none()
        || setup.run.max_body_bytes.is_none()
        || setup.run.max_tail_timeout_ms.is_none()
        || setup.run.max_tail_concurrency.is_none()
    {
        return Err(Error::new(ErrorKind::Corrupt)
            .with_message("service setup is incomplete")
            .with_path(path));
    }
    Ok(Some(setup))
}

fn required(pool_dir: &Path) -> Result<Setup, Error> {
    load(pool_dir)?.ok_or_else(|| {
        usage("this pool directory has no installed server")
            .with_path(pool_dir)
            .with_hint("Run `plasmite --dir DIR serve install SERVER` first.")
    })
}

#[derive(Debug)]
struct ServiceLock(File);

impl Drop for ServiceLock {
    fn drop(&mut self) {
        // A concurrently forked child can inherit this descriptor until exec.
        // Unlock the shared description now rather than awaiting its last close.
        let _ = FileExt::unlock(&self.0);
    }
}

fn pool_operation_lock(pool_dir: &Path) -> Result<ServiceLock, Error> {
    // Native labels depend on the canonical pool, so their operation lock must
    // also be independent of HOME. Keep it separate from the live server lock.
    let state = pool_dir.join(".plasmite-serve");
    create_private_dir(&state)?;
    ensure_private(&state)?;
    let path = state.join("service.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|error| io_error("failed to open service lock", &path, error))?;
    ensure_private(&path)?;
    file.try_lock_exclusive().map_err(|error| {
        Error::new(ErrorKind::Busy)
            .with_message("another command is changing this service")
            .with_path(&path)
            .with_source(error)
    })?;
    Ok(ServiceLock(file))
}

fn lock(pool_dir: &Path) -> Result<ServiceLock, Error> {
    let file = pool_operation_lock(pool_dir)?;
    let directory = directory()?;
    fs::create_dir_all(directory.parent().unwrap())
        .map_err(|error| io_error("failed to create service directory", &directory, error))?;
    create_private_dir(&directory)?;
    ensure_private(&directory)?;
    let job_dir = directory.join(id(pool_dir));
    create_private_dir(&job_dir)?;
    ensure_private(&job_dir)?;
    Ok(file)
}

fn native_program(program: &str) -> &str {
    match program {
        "launchctl" => "/bin/launchctl",
        "systemctl" => "/usr/bin/systemctl",
        "id" => "/usr/bin/id",
        "tail" => "/usr/bin/tail",
        _ => program,
    }
}

fn capture(program: &str, arguments: &[&str]) -> Result<String, Error> {
    let output = Command::new(native_program(program))
        .args(arguments)
        .output()
        .map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message(format!("failed to run {program}"))
                .with_source(error)
        })?;
    if !output.status.success() {
        return Err(Error::new(ErrorKind::Io).with_message(format!("{program} {} failed: {}", arguments.join(" "), String::from_utf8_lossy(&output.stderr).trim()))
            .with_hint("Check the native service manager and use `plasmite serve logs` to inspect startup errors."));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn admin(
    program: &str,
    arguments: &[&str],
    native_may_have_started: &mut bool,
) -> Result<(), Error> {
    let root = unsafe_euid_is_root();
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let native = native_program(program);
    // Check approval separately, so a failed native operation never runs twice.
    let approved = root
        || Command::new("/usr/bin/sudo")
            .args(["-n", "/usr/bin/true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
    let mut command = if root {
        let mut command = Command::new(native);
        command.args(arguments);
        command
    } else if approved {
        let mut command = Command::new("/usr/bin/sudo");
        command.args(["-n", native]).args(arguments);
        command
    } else if interactive {
        if cfg!(target_os = "macos") {
            let shell_command = std::iter::once(native)
                .chain(arguments.iter().copied())
                .map(shell_quote)
                .collect::<Vec<_>>()
                .join(" ");
            let mut command = Command::new("/usr/bin/osascript");
            command.args(["-e", "on run argv\ndo shell script (item 1 of argv) with administrator privileges\nend run", &shell_command]);
            command
        } else {
            let mut command = Command::new("/usr/bin/sudo");
            command.arg(native).args(arguments);
            command
        }
    } else {
        return Err(Error::new(ErrorKind::Permission).with_message("administrator approval is required to manage startup")
            .with_hint("Run this Plasmite command in an interactive terminal and approve its administrator request. Run Plasmite as the account that owns the pools."));
    };
    // A failed command can leave partial native changes. Approval failures above
    // cannot, so callers may restore local files without stopping the old job.
    *native_may_have_started = true;
    let output = command.stdin(Stdio::inherit()).output().map_err(|error| {
        Error::new(ErrorKind::Io)
            .with_message("could not request the startup service change")
            .with_source(error)
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::new(ErrorKind::Io).with_message(format!("could not update the startup service: {}", String::from_utf8_lossy(&output.stderr).trim()))
        .with_hint("Check the administrator request and the native service manager. Run Plasmite as the account that owns the pools."))
}

fn unsafe_euid_is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

impl Setup {
    fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            self.program.to_string_lossy().into_owned(),
            "--dir".into(),
            self.pool_dir.to_string_lossy().into_owned(),
            "serve".into(),
        ];
        if let Some(server) = &self.run.server {
            argv.push(server.clone());
        }
        macro_rules! arg {
            ($flag:expr, $value:expr) => {
                if let Some(value) = $value {
                    argv.push($flag.into());
                    argv.push(value.to_string());
                }
            };
        }
        arg!("--bind", &self.run.bind);
        arg!("--remote-bind", &self.run.remote_bind);
        arg!("--max-body-bytes", self.run.max_body_bytes);
        arg!("--max-tail-timeout-ms", self.run.max_tail_timeout_ms);
        arg!("--max-tail-concurrency", self.run.max_tail_concurrency);
        for (flag, path) in [
            ("--tls-cert", &self.run.tls_cert),
            ("--tls-key", &self.run.tls_key),
            ("--front-cert", &self.run.front_cert),
        ] {
            if let Some(path) = path {
                argv.push(flag.into());
                argv.push(path.to_string_lossy().into_owned());
            }
        }
        argv
    }
    fn job_path(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            PathBuf::from("/Library/LaunchDaemons").join(format!("{}.plist", id(&self.pool_dir)))
        } else {
            PathBuf::from("/etc/systemd/system").join(format!("{}.service", id(&self.pool_dir)))
        }
    }
    fn log_path(&self) -> PathBuf {
        self.program.parent().unwrap().join("serve.log")
    }
    fn target(&self) -> String {
        format!("system/{}", id(&self.pool_dir))
    }
    fn unit(&self) -> String {
        format!("{}.service", id(&self.pool_dir))
    }
}

fn systemd_quote(value: &str) -> String {
    let mut result = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '%' => result.push_str("%%"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn definition(setup: &Setup, macos: bool) -> String {
    // The manager drops privileges before this shell opens the owner's log.
    // A manager-level output path could follow an owner-created symlink as root.
    let command = format!(
        "exec {} >> {} 2>&1",
        setup
            .argv()
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" "),
        shell_quote(&setup.log_path().to_string_lossy())
    );
    let arguments = ["/bin/sh", "-c", &command];
    if macos {
        let argv = arguments
            .iter()
            .map(|value| format!("<string>{}</string>", xml(value)))
            .collect::<String>();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{}</string>\n<key>UserName</key><string>{}</string>\n<key>ProgramArguments</key><array>{argv}</array>\n<key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string></dict>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>Umask</key><integer>63</integer>\n</dict></plist>\n",
            id(&setup.pool_dir),
            xml(&setup.account),
            xml(&setup.home.to_string_lossy())
        )
    } else {
        let argv = arguments
            .iter()
            .map(|value| systemd_quote(value).replace('$', "$$"))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "[Unit]\nDescription=Plasmite pool server\nAfter=network.target\nRequiresMountsFor={}\n\n[Service]\nType=simple\nUser={}\nEnvironment={}\nExecStart={argv}\nRestart=on-failure\nRestartSec=2\nUMask=0077\n\n[Install]\nWantedBy=multi-user.target\n",
            systemd_quote(&setup.pool_dir.to_string_lossy()),
            setup.account,
            systemd_quote(&format!("HOME={}", setup.home.display()))
        )
    }
}

fn native_status(setup: &Setup) -> Result<(Option<u32>, String, bool), Error> {
    if cfg!(target_os = "macos") {
        let disabled = capture("launchctl", &["print-disabled", "system"])?;
        let startup = setup.job_path().is_file()
            && !disabled.lines().any(|line| {
                line.contains(&format!("\"{}\"", id(&setup.pool_dir)))
                    && line.trim().ends_with("=> true")
            });
        let report = capture("launchctl", &["print", &setup.target()]);
        let (pid, state) = match report {
            Ok(report) => {
                let pid = report.lines().find_map(|line| {
                    line.trim()
                        .strip_prefix("pid = ")
                        .and_then(|pid| pid.parse::<u32>().ok())
                });
                let failed = report.lines().any(|line| {
                    line.trim()
                        .strip_prefix("last exit code = ")
                        .is_some_and(|code| code != "0")
                });
                (
                    pid,
                    if pid.is_some() {
                        "running"
                    } else if failed {
                        "failed"
                    } else {
                        "stopped"
                    },
                )
            }
            Err(error) => {
                if loaded(setup)? {
                    return Err(error);
                }
                (None, "stopped")
            }
        };
        Ok((pid, state.into(), startup))
    } else {
        let report = capture(
            "systemctl",
            &[
                "show",
                &setup.unit(),
                "--property=LoadState",
                "--property=ActiveState",
                "--property=MainPID",
                "--property=UnitFileState",
            ],
        )?;
        let value = |name: &str| {
            report
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap_or("")
        };
        let pid = value("MainPID=").parse::<u32>().ok().filter(|pid| *pid > 0);
        let state = match value("ActiveState=") {
            "failed" => "failed",
            "active" => "running",
            "activating" => "starting",
            _ => "stopped",
        };
        Ok((pid, state.into(), value("UnitFileState=") == "enabled"))
    }
}

fn registered(setup: &Setup) -> Result<bool, Error> {
    Ok(native_status(setup)?.2)
}
// Exit 0 means present, 1 means confirmed absent, and 2 means unknown.
// Keep the same query/classification in both user and privileged checks.
fn native_presence_script(setup: &Setup) -> String {
    if cfg!(target_os = "macos") {
        let absent = shell_quote(&format!(
            "Could not find service \"{}\" in domain for system",
            id(&setup.pool_dir)
        ));
        format!(
            "if plasmite_report=$(/bin/launchctl print {} 2>&1); then exit 0; else\nplasmite_exit=$?\ncase \"$plasmite_exit:$plasmite_report\" in 113:*{absent}*) exit 1;; *) /usr/bin/printf '%s\\n' \"$plasmite_report\" >&2; exit 2;; esac\nfi",
            shell_quote(&setup.target())
        )
    } else {
        format!(
            "if plasmite_report=$(/usr/bin/systemctl show {} --property=LoadState --value 2>&1); then\ncase \"$plasmite_report\" in not-found) exit 1;; '') exit 2;; *) exit 0;; esac\nelse\n/usr/bin/printf '%s\\n' \"$plasmite_report\" >&2; exit 2\nfi",
            shell_quote(&setup.unit())
        )
    }
}

fn loaded(setup: &Setup) -> Result<bool, Error> {
    inspect_native_presence(setup, &native_presence_script(setup))
}

fn inspect_native_presence(setup: &Setup, query: &str) -> Result<bool, Error> {
    let output = Command::new("/bin/sh")
        .args(["-c", query])
        .output()
        .map_err(|error| {
            io_error(
                "failed to inspect native service manager",
                &setup.job_path(),
                error,
            )
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::new(ErrorKind::Io)
            .with_message("could not verify native startup job ownership")
            .with_path(setup.job_path())
            .with_hint(format!(
                "Check the native service manager before retrying; its query failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
    }
}

fn startup_link(setup: &Setup) -> PathBuf {
    PathBuf::from("/etc/systemd/system/multi-user.target.wants").join(setup.unit())
}

fn ownership_error(setup: &Setup) -> Error {
    Error::new(ErrorKind::Busy)
        .with_message("the native startup job does not match this saved setup")
        .with_path(setup.job_path())
        .with_hint("Use the original HOME and its saved setup to manage this server. Do not adopt or overwrite an existing startup job.")
}

fn verify_job_definition(
    setup: &Setup,
    owners: &[&Setup],
    current: Option<&str>,
    native_loaded: bool,
    macos: bool,
) -> Result<(), Error> {
    match current {
        Some(current)
            if owners
                .iter()
                .any(|owner| current == definition(owner, macos)) =>
        {
            Ok(())
        }
        None if !native_loaded => Ok(()),
        _ => Err(ownership_error(setup)),
    }
}

fn verify_native_ownership(setup: &Setup, owners: &[&Setup]) -> Result<(), Error> {
    let path = setup.job_path();
    let current = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => Some(
            fs::read_to_string(&path)
                .map_err(|error| io_error("failed to inspect native startup job", &path, error))?,
        ),
        Ok(_) => return Err(ownership_error(setup)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(io_error(
                "failed to inspect native startup job",
                &path,
                error,
            ));
        }
    };
    if cfg!(target_os = "linux") {
        let wants = startup_link(setup);
        match fs::symlink_metadata(&wants) {
            Ok(metadata) if metadata.file_type().is_symlink() && current.is_some() => {
                let target = fs::canonicalize(&wants).map_err(|_| ownership_error(setup))?;
                if target != fs::canonicalize(&path).map_err(|_| ownership_error(setup))? {
                    return Err(ownership_error(setup));
                }
            }
            Ok(_) => return Err(ownership_error(setup)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(io_error("failed to inspect startup link", &wants, error)),
        }
    }
    verify_job_definition(
        setup,
        owners,
        current.as_deref(),
        loaded(setup)?,
        cfg!(target_os = "macos"),
    )
}

// Repeat the ownership check inside the privileged operation. Definitions come
// from validated saved settings and are passed in memory, never in owner-writable
// staging files. Refuse to mutate jobs when the definition or saved setup differs.
fn ownership_check_script(
    job: &Path,
    definitions: &[String],
    loaded_command: &str,
    wants: Option<&Path>,
) -> String {
    let reject = "{ /usr/bin/printf '%s\n' 'The native startup job does not match this saved setup. Use the original HOME and its saved setup.' >&2; exit 1; }";
    let comparison = if definitions.is_empty() {
        reject.to_owned()
    } else {
        format!(
            "plasmite_current=$(/bin/cat \"$plasmite_job\")\n{} || {reject}",
            definitions
                .iter()
                .map(|definition| format!(
                    "[ \"$plasmite_current\" = {} ]",
                    shell_quote(definition.trim_end_matches('\n'))
                ))
                .collect::<Vec<_>>()
                .join(" || ")
        )
    };
    let link_check = wants.map(|wants| format!(
        "plasmite_wants={}\nif [ -e \"$plasmite_wants\" ] || [ -L \"$plasmite_wants\" ]; then\n[ -f \"$plasmite_job\" ] && [ -L \"$plasmite_wants\" ] || {reject}\nplasmite_link=$(/usr/bin/readlink \"$plasmite_wants\")\n[ \"$plasmite_link\" = \"$plasmite_job\" ] || [ \"$plasmite_link\" = {} ] || {reject}\nfi\n",
        shell_quote(&wants.to_string_lossy()),
        shell_quote(&format!("../{}", job.file_name().unwrap().to_string_lossy()))
    )).unwrap_or_default();
    format!(
        "plasmite_job={}\nif plasmite_state=$({{ {loaded_command}; }} 2>&1); then\nplasmite_loaded=1\nelse\nplasmite_exit=$?\nif [ \"$plasmite_exit\" -ne 1 ]; then\n/usr/bin/printf '%s\\n' 'Could not verify native startup job ownership: manager query failed.' \"$plasmite_state\" >&2; exit 1\nfi\nplasmite_loaded=0\nfi\n[ ! -L \"$plasmite_job\" ] || {reject}\n{link_check}if [ -e \"$plasmite_job\" ]; then\n[ -f \"$plasmite_job\" ] || {reject}\n{comparison}\nelse\n[ \"$plasmite_loaded\" -eq 0 ] || {reject}\nfi\n",
        shell_quote(&job.to_string_lossy())
    )
}

fn ownership_script(setup: &Setup, owners: &[&Setup]) -> String {
    let definitions = owners
        .iter()
        .map(|owner| definition(owner, cfg!(target_os = "macos")))
        .collect::<Vec<_>>();
    let wants = cfg!(target_os = "linux").then(|| startup_link(setup));
    ownership_check_script(
        &setup.job_path(),
        &definitions,
        &native_presence_script(setup),
        wants.as_deref(),
    )
}

fn stop_script(setup: &Setup, macos: bool) -> String {
    // ownership_script already classified presence in this privileged shell.
    // Do not make a second fallible query that could silently skip stopping.
    if macos {
        format!(
            "if [ \"$plasmite_loaded\" -eq 1 ]; then /bin/launchctl bootout {target}; fi\n",
            target = shell_quote(&setup.target())
        )
    } else {
        format!(
            "if [ \"$plasmite_loaded\" -eq 1 ]; then /usr/bin/systemctl stop {unit}; fi\n",
            unit = shell_quote(&setup.unit())
        )
    }
}

fn stop_native(setup: &Setup, owners: &[&Setup]) -> Result<(), Error> {
    verify_native_ownership(setup, owners)?;
    let mut native_may_have_started = false;
    admin(
        "/bin/sh",
        &[
            "-c",
            &format!(
                "set -eu\n{}{}",
                ownership_script(setup, owners),
                stop_script(setup, cfg!(target_os = "macos"))
            ),
        ],
        &mut native_may_have_started,
    )
}

fn start_native(setup: &Setup, restart: bool) -> Result<(), Error> {
    verify_native_ownership(setup, &[setup])?;
    let mut script = format!("set -eu\n{}", ownership_script(setup, &[setup]));
    if cfg!(target_os = "macos") {
        let target = shell_quote(&setup.target());
        script.push_str("if [ \"$plasmite_loaded\" -eq 1 ]; then\n");
        if restart {
            script.push_str(&format!("/bin/launchctl kickstart -k {target}\n"));
        } else {
            script.push_str(":\n");
        }
        script.push_str(&format!(
            "else\n/bin/launchctl bootstrap system {}\nfi\n",
            shell_quote(&setup.job_path().to_string_lossy())
        ));
    } else {
        script.push_str(&format!(
            "/usr/bin/systemctl {} {}\n",
            if restart { "restart" } else { "start" },
            shell_quote(&setup.unit())
        ));
    }
    let mut native_may_have_started = false;
    admin("/bin/sh", &["-c", &script], &mut native_may_have_started)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn install_native(
    setup: &Setup,
    owners: &[&Setup],
    native_may_have_started: &mut bool,
) -> Result<(), Error> {
    verify_native_ownership(setup, owners)?;
    // Pass the definition in memory; an unprivileged process must not replace a staging
    // file while the owner approves the administrator request.
    let job = setup.job_path();
    let template = job
        .parent()
        .unwrap()
        .join(format!(".{}.XXXXXX", id(&setup.pool_dir)));
    let mut script = format!(
        "set -eu\n{}umask 077\nplasmite_definition=$(/usr/bin/mktemp {})\ntrap '/bin/rm -f \"$plasmite_definition\"' EXIT\n/usr/bin/printf '%s' {} > \"$plasmite_definition\"\n/bin/chmod 644 \"$plasmite_definition\"\n",
        ownership_script(setup, owners),
        shell_quote(&template.to_string_lossy()),
        shell_quote(&definition(setup, cfg!(target_os = "macos")))
    );
    if cfg!(target_os = "macos") {
        script.push_str(&stop_script(setup, true));
    }
    script.push_str(&format!(
        "/bin/mv -f \"$plasmite_definition\" {}\n",
        shell_quote(&job.to_string_lossy())
    ));
    if cfg!(target_os = "macos") {
        script.push_str(&format!(
            "/bin/launchctl enable {}\n/bin/launchctl bootstrap system {}\n",
            shell_quote(&setup.target()),
            shell_quote(&job.to_string_lossy())
        ));
    } else {
        script.push_str(&format!("/usr/bin/systemctl daemon-reload\n/usr/bin/systemctl enable {}\n/usr/bin/systemctl restart {}\n",
            shell_quote(&setup.unit()), shell_quote(&setup.unit())));
    }
    admin("/bin/sh", &["-c", &script], native_may_have_started)
}

fn uninstall_native(setup: &Setup, owners: &[&Setup]) -> Result<(), Error> {
    verify_native_ownership(setup, owners)?;
    let wants = startup_link(setup);
    if !setup.job_path().exists()
        && !loaded(setup)?
        && (cfg!(target_os = "macos") || wants.symlink_metadata().is_err())
    {
        return Ok(());
    }
    let mut script = format!("set -eu\n{}", ownership_script(setup, owners));
    script.push_str(&stop_script(setup, cfg!(target_os = "macos")));
    if !cfg!(target_os = "macos") {
        script.push_str(&format!(
            "if [ -f {job} ]; then /usr/bin/systemctl disable {unit}; fi\n",
            unit = shell_quote(&setup.unit()),
            job = shell_quote(&setup.job_path().to_string_lossy())
        ));
    }
    script.push_str(&format!(
        "/bin/rm -f {}\n",
        shell_quote(&setup.job_path().to_string_lossy())
    ));
    if !cfg!(target_os = "macos") {
        script.push_str(&format!(
            "/bin/rm -f {}\n/usr/bin/systemctl daemon-reload\n",
            shell_quote(&wants.to_string_lossy())
        ));
    }
    let mut native_may_have_started = false;
    admin("/bin/sh", &["-c", &script], &mut native_may_have_started)
}

fn live(setup: &Setup) -> Result<Option<ServerDetails>, Error> {
    let native_pid = native_status(setup)?.0;
    Ok(serve_registry::running()?
        .into_iter()
        .find(|server| server.pool_dir == setup.pool_dir && Some(server.pid) == native_pid))
}

fn wait_ready(setup: &Setup) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if live(setup)?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::new(ErrorKind::Io)
                .with_message("installed server did not become ready")
                .with_path(&setup.pool_dir)
                .with_hint("Run `plasmite --dir DIR serve logs` to inspect startup errors."));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn preflight(setup: &Setup, previous: Option<&Setup>) -> Result<(), Error> {
    let running = serve_registry::running()?
        .into_iter()
        .find(|server| server.pool_dir == setup.pool_dir);
    if running.is_some()
        && (previous.is_none()
            || native_status(setup)?.0 != running.as_ref().map(|server| server.pid))
    {
        return Err(Error::new(ErrorKind::Busy)
            .with_message("a foreground server already owns this pool directory")
            .with_path(&setup.pool_dir)
            .with_hint("Stop that server with Ctrl+C, then retry `serve install`."));
    }
    let mut listeners = Vec::new();
    for bind in [&setup.run.bind, &setup.run.remote_bind] {
        let address: SocketAddr = bind.as_deref().unwrap().parse().expect("validated bind");
        if address.port() == 0 {
            return Err(usage("installed servers require fixed listener ports"));
        }
        let unchanged = running.is_some()
            && previous.is_some_and(|old| bind == &old.run.bind || bind == &old.run.remote_bind);
        if !unchanged {
            listeners.push(TcpListener::bind(address).map_err(|error| Error::new(ErrorKind::Io)
                .with_message(format!("cannot listen on {address}"))
                .with_hint("Choose an available port with --bind or --remote-bind; low ports may require administrator setup.")
                .with_source(error))?);
        }
    }
    Ok(())
}

pub(crate) fn install(pool_dir: &Path, options: &ServeRunArgs) -> Result<Status, Error> {
    supported()?;
    if unsafe_euid_is_root() {
        return Err(usage("run service installation as the account that owns the pools")
            .with_hint("Run Plasmite without sudo. It requests administrator approval only for the native startup job."));
    }
    fs::create_dir_all(pool_dir)
        .map_err(|error| io_error("failed to create pool directory", pool_dir, error))?;
    let pool_dir = absolute(pool_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if fs::metadata(&pool_dir)
            .map_err(|error| io_error("failed to inspect pool directory", &pool_dir, error))?
            .uid()
            != unsafe { libc::geteuid() }
        {
            return Err(usage(
                "service installation requires a pool directory owned by the current account",
            ));
        }
    }
    let _lock = lock(&pool_dir)?;
    let previous = load(&pool_dir)?;
    let path = setup_path(&pool_dir)?;
    let account = capture("id", &["-un"])?;
    if account.is_empty()
        || account
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || "_-.$".contains(ch)))
    {
        return Err(usage(
            "service account name contains unsupported characters",
        ));
    }
    let owner_home = home()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&owner_home)
            .map_err(|error| io_error("failed to inspect account home", &owner_home, error))?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(usage("the service home must belong to the current account"));
        }
    }
    let setup = Setup {
        program: path.parent().unwrap().join("plasmite"),
        pool_dir,
        account,
        home: owner_home,
        run: merge(previous.as_ref(), options)?,
    };
    let owners = previous.iter().collect::<Vec<_>>();
    verify_native_ownership(&setup, &owners)?;
    let recovery_owners = previous
        .iter()
        .chain(std::iter::once(&setup))
        .collect::<Vec<_>>();
    preflight(&setup, previous.as_ref())?;
    let was_running = match &previous {
        Some(old) => live(old)?.is_some(),
        None => false,
    };
    let log = setup.log_path();
    let mut log_options = OpenOptions::new();
    log_options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    log_options
        .open(&log)
        .map_err(|error| io_error("failed to prepare private service log", &log, error))?;
    ensure_private(&log)?;
    let source = std::env::current_exe().map_err(|error| {
        io_error(
            "failed to locate Plasmite executable",
            &setup.program,
            error,
        )
    })?;
    update_program(
        &setup,
        previous.as_ref(),
        &path,
        &source,
        |candidate, restoring, native_may_have_started| {
            install_native(
                candidate,
                if restoring { &recovery_owners } else { &owners },
                native_may_have_started,
            )?;
            if restoring && !was_running {
                stop_native(candidate, &[candidate])?;
            } else {
                wait_ready(candidate)?;
            }
            if !registered(candidate)? {
                return Err(usage("the service manager did not enable startup at boot"));
            }
            status(candidate)
        },
        |candidate, uninstall| {
            if uninstall {
                uninstall_native(candidate, &recovery_owners)
            } else {
                stop_native(candidate, &recovery_owners)
            }
        },
    )
}

// Keep the filesystem transaction independent of the native manager so failure
// paths can exercise real files without installing a persistent native job.
fn update_program(
    setup: &Setup,
    previous: Option<&Setup>,
    path: &Path,
    source: &Path,
    mut activate: impl FnMut(&Setup, bool, &mut bool) -> Result<Status, Error>,
    deactivate: impl FnOnce(&Setup, bool) -> Result<(), Error>,
) -> Result<Status, Error> {
    let temporary = setup.program.with_extension("new");
    let backup = setup.program.with_extension("previous");
    let settings_backup = path.with_extension("previous.json");
    let identity = setup.pool_dir.join(".plasmite-serve/identity.json");
    let identity_backup = path.with_file_name("identity.previous.json");
    let recovery_files = [&backup, &settings_backup, &identity_backup];
    for recovery in recovery_files {
        if recovery.exists() {
            return Err(usage("a previous service update needs recovery")
                .with_path(recovery)
                .with_hint("Preserved recovery files must be recovered before another install."));
        }
    }
    let preparation = (|| {
        fs::copy(source, &temporary)
            .map_err(|error| io_error("failed to copy Plasmite executable", &temporary, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).map_err(
                |error| io_error("failed to protect installed executable", &temporary, error),
            )?;
        }
        if let Some(old) = previous {
            write_atomic_json(&settings_backup, old)?;
        }
        if identity.exists() {
            ensure_private(&identity)?;
            fs::copy(&identity, &identity_backup).map_err(|error| {
                io_error(
                    "failed to preserve server identity",
                    &identity_backup,
                    error,
                )
            })?;
        }
        if setup.program.exists() {
            fs::copy(&setup.program, &backup).map_err(|error| {
                io_error("failed to preserve installed program", &backup, error)
            })?;
        }
        fs::rename(&temporary, &setup.program)
            .map_err(|error| io_error("failed to install executable", &setup.program, error))
    })();
    if let Err(preparation_error) = preparation {
        // Until replacement succeeds the old service is untouched; discard only
        // this attempt's staging files so ordinary preparation errors are retryable.
        for staging in [&temporary, &backup, &settings_backup, &identity_backup] {
            if staging.exists() {
                fs::remove_file(staging).map_err(|error| {
                    io_error("failed to clean service staging file", staging, error)
                        .with_hint(format!("Preparation failed: {preparation_error}"))
                })?;
            }
        }
        return Err(preparation_error);
    }
    let mut native_may_have_started = false;
    let result = write_atomic_json(path, setup)
        .and_then(|()| activate(setup, false, &mut native_may_have_started));
    if let Err(update_error) = &result {
        let recovery = (|| {
            // Stop the replacement before restoring its identity or program.
            // A failed stop must leave all recovery data intact.
            if native_may_have_started {
                deactivate(setup, previous.is_none())?;
            }
            if native_may_have_started && identity_backup.exists() {
                let bytes = fs::read(&identity_backup).map_err(|error| {
                    io_error(
                        "failed to read previous server identity",
                        &identity_backup,
                        error,
                    )
                })?;
                crate::access_store::AccessStore::restore_identity(&setup.pool_dir, &bytes)?;
            }
            if let Some(old) = previous {
                if !backup.exists() {
                    return Err(usage("the previous executable is missing").with_path(&backup));
                }
                fs::copy(&backup, &temporary).map_err(|error| {
                    io_error("failed to stage previous executable", &backup, error)
                })?;
                fs::rename(&temporary, &setup.program).map_err(|error| {
                    io_error("failed to restore previous executable", &backup, error)
                })?;
                write_atomic_json(path, old)?;
                if native_may_have_started {
                    activate(old, true, &mut native_may_have_started)?;
                }
            } else {
                for failed in [path, &setup.program] {
                    match fs::remove_file(failed) {
                        Ok(()) => (),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => {
                            return Err(io_error(
                                "failed to remove failed installation",
                                failed,
                                error,
                            ));
                        }
                    }
                }
            }
            Ok::<_, Error>(())
        })();
        if let Err(recovery_error) = recovery {
            return Err(Error::new(ErrorKind::Io)
                .with_message("service update failed and recovery needs attention")
                .with_path(path)
                .with_hint(format!(
                    "Recovery files remain beside the saved setup. Update failed: {update_error}. Recovery failed: {recovery_error}"
                )));
        }
    }
    for recovery in recovery_files {
        if recovery.exists() {
            fs::remove_file(recovery).map_err(|error| {
                io_error("failed to remove completed recovery file", recovery, error)
            })?;
        }
    }
    result
}

pub(crate) fn control(pool_dir: &Path, action: &str) -> Result<Status, Error> {
    supported()?;
    let pool_dir = absolute(pool_dir)?;
    let _lock = lock(&pool_dir)?;
    let setup = required(&pool_dir)?;
    match action {
        "start" | "restart" => {
            start_native(&setup, action == "restart")?;
            wait_ready(&setup)?;
        }
        "stop" => {
            stop_native(&setup, &[&setup])?;
        }
        "uninstall" => {
            uninstall_native(&setup, &[&setup])?;
            fs::remove_file(setup_path(&pool_dir)?)
                .map_err(|error| io_error("failed to remove service setup", &pool_dir, error))?;
            let _ = fs::remove_file(&setup.program);
            return Ok(Status {
                pool_dir,
                pid: None,
                local_url: format!("http://{}", setup.run.bind.as_deref().unwrap()),
                remote_url: setup.run.server,
                managed: false,
                startup: false,
                state: "uninstalled".into(),
                problem: None,
                setup: None,
            });
        }
        _ => unreachable!("known service action"),
    }
    let status = status(&setup)?;
    if action == "stop" && matches!(status.state.as_str(), "running" | "starting") {
        return Err(Error::new(ErrorKind::Busy)
            .with_message("the installed server did not stop")
            .with_hint("Inspect the native service manager, then retry `serve stop`."));
    }
    Ok(status)
}

fn status(setup: &Setup) -> Result<Status, Error> {
    let (native_pid, native_state, startup) = native_status(setup)?;
    let live = serve_registry::running()?
        .into_iter()
        .find(|server| server.pool_dir == setup.pool_dir && Some(server.pid) == native_pid);
    let state = if live.is_some() {
        "running".to_owned()
    } else if native_state == "running" {
        "starting".to_owned()
    } else {
        native_state
    };
    let problem = (state == "failed" || state == "starting")
        .then(|| "The installed server has not responded. Inspect `serve logs`.".into());
    Ok(Status {
        pool_dir: setup.pool_dir.clone(),
        pid: live.as_ref().map(|server| server.pid),
        local_url: live
            .as_ref()
            .map(|server| server.local_url.clone())
            .unwrap_or_else(|| format!("http://{}", setup.run.bind.as_deref().unwrap())),
        remote_url: live
            .and_then(|server| server.remote_url)
            .or_else(|| setup.run.server.clone()),
        managed: true,
        startup,
        state,
        problem,
        setup: Some(setup.clone()),
    })
}

pub(crate) fn all() -> Result<(Vec<Status>, Vec<Error>), Error> {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let directory = directory()?;
    if directory.exists() {
        ensure_private(&directory)?;
        for entry in fs::read_dir(&directory)
            .map_err(|error| io_error("failed to list services", &directory, error))?
        {
            let path = entry
                .map_err(|error| io_error("failed to list service", &directory, error))?
                .path();
            if !path.is_dir() || !path.join("setup.json").exists() {
                continue;
            }
            let row = (|| {
                ensure_private(&path)?;
                ensure_private(&path.join("setup.json"))?;
                let setup: Setup = read_json(&path.join("setup.json"))?;
                let setup = required(&setup.pool_dir)?;
                status(&setup)
            })();
            match row {
                Ok(row) => rows.push(row),
                Err(error) => errors.push(error.with_path(path)),
            }
        }
    }
    for server in serve_registry::running()? {
        if !rows
            .iter()
            .any(|row| row.pool_dir == server.pool_dir && row.pid == Some(server.pid))
        {
            rows.push(Status {
                pool_dir: server.pool_dir,
                pid: Some(server.pid),
                local_url: server.local_url,
                remote_url: server.remote_url,
                managed: false,
                startup: false,
                state: "running".into(),
                problem: None,
                setup: None,
            });
        }
    }
    rows.sort_by(|left, right| left.pool_dir.cmp(&right.pool_dir));
    Ok((rows, errors))
}

struct LogChild(std::process::Child);

impl Drop for LogChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn logs(pool_dir: &Path, tail: usize, follow: bool, json: bool) -> Result<(), Error> {
    supported()?;
    let setup = required(&absolute(pool_dir)?)?;
    let mut command = Command::new(native_program("tail"));
    command.args(["-n", &tail.to_string()]);
    if follow {
        command.arg("-F");
    }
    command.arg(setup.log_path());
    let exit = if json {
        use std::io::{BufRead, Write};
        let child = command.stdout(Stdio::piped()).spawn().map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read service logs")
                .with_source(error)
        })?;
        let mut child = LogChild(child);
        let mut stdout = std::io::stdout().lock();
        for line in std::io::BufReader::new(child.0.stdout.take().unwrap()).lines() {
            let line = line.map_err(|error| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read log line")
                    .with_source(error)
            })?;
            writeln!(stdout, "{}", serde_json::json!({"message": line}))
                .and_then(|()| stdout.flush())
                .map_err(|error| {
                    Error::new(ErrorKind::Io)
                        .with_message("failed to write service logs")
                        .with_source(error)
                })?;
        }
        child.0.wait()
    } else {
        command.status()
    };
    if !exit
        .map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read service logs")
                .with_source(error)
        })?
        .success()
    {
        return Err(Error::new(ErrorKind::Io)
            .with_message("native service logs are unavailable")
            .with_hint("Check the native service manager and your permission to read its logs."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> Setup {
        Setup {
            pool_dir: "/tmp/pools with space/$name%literal".into(),
            program: "/tmp/plasmite".into(),
            account: "alice".into(),
            home: "/home/alice".into(),
            run: effective_args(&ServeRunArgs::default()).unwrap(),
        }
    }
    #[test]
    fn native_job_requires_saved_setup_matching_every_owner_field() {
        let original = setup();
        for macos in [false, true] {
            let current = definition(&original, macos);
            assert!(
                verify_job_definition(&original, &[&original], Some(&current), true, macos).is_ok()
            );
            assert!(verify_job_definition(&original, &[], Some(&current), true, macos).is_err());
            assert!(verify_job_definition(&original, &[], None, true, macos).is_err());
            assert!(verify_job_definition(&original, &[], None, false, macos).is_ok());
            for field in ["account", "home", "program", "pool"] {
                let mut other = original.clone();
                match field {
                    "account" => other.account = "other".into(),
                    "home" => other.home = "/another/home".into(),
                    "program" => other.program = "/another/program".into(),
                    "pool" => other.pool_dir = "/another/pool".into(),
                    _ => unreachable!(),
                }
                assert!(
                    verify_job_definition(&other, &[&other], Some(&current), true, macos).is_err(),
                    "{field}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn alternate_home_and_stale_privileged_checks_never_mutate_foreign_job() {
        let temp = tempfile::tempdir().unwrap();
        let mut original = setup();
        original.home = temp.path().join("home-a");
        original.program = original.home.join("plasmite");
        let mut alternate = original.clone();
        alternate.home = temp.path().join("home-b");
        alternate.program = alternate.home.join("plasmite");
        assert_eq!(original.job_path(), alternate.job_path());
        let saved = temp.path().join("original-setup.json");
        write_atomic_json(&saved, &original).unwrap();
        let saved_bytes = fs::read(&saved).unwrap();
        let job = temp.path().join("native-job");
        let events = temp.path().join("manager-events");
        for macos in [false, true] {
            let definition_a = definition(&original, macos);
            fs::write(&job, &definition_a).unwrap();
            fs::write(&events, b"").unwrap();
            let invoke = |expected: &[String], loaded: &str| {
                Command::new("/bin/sh")
                    .args([
                        "-c",
                        &format!(
                            "set -eu\n{}\n/usr/bin/printf '%s' 'stop replace remove' >> {}",
                            ownership_check_script(&job, expected, loaded, None),
                            shell_quote(&events.to_string_lossy())
                        ),
                    ])
                    .output()
                    .unwrap()
            };
            // HOME B has no matching saved setup. No manager mutation can run.
            let result = invoke(&[], "true");
            assert!(!result.status.success());
            assert!(String::from_utf8_lossy(&result.stderr).contains("original HOME"));
            assert!(fs::read(&events).unwrap().is_empty());
            assert_eq!(fs::read_to_string(&job).unwrap(), definition_a);
            assert_eq!(fs::read(&saved).unwrap(), saved_bytes);
            // Validated ownership becomes stale while approval is pending.
            verify_job_definition(&original, &[&original], Some(&definition_a), true, macos)
                .unwrap();
            let definition_b = definition(&alternate, macos);
            fs::write(&job, &definition_b).unwrap();
            let result = invoke(std::slice::from_ref(&definition_a), "true");
            assert!(!result.status.success());
            assert!(fs::read(&events).unwrap().is_empty());
            assert_eq!(fs::read_to_string(&job).unwrap(), definition_b);
            assert_eq!(fs::read(&saved).unwrap(), saved_bytes);
            // Matching definitions remain operable; an unloaded missing job can
            // be installed, but a loaded job without a definition cannot be adopted.
            fs::write(&job, &definition_a).unwrap();
            assert!(
                invoke(std::slice::from_ref(&definition_a), "true")
                    .status
                    .success()
            );
            fs::write(&events, b"").unwrap();
            fs::remove_file(&job).unwrap();
            assert!(!invoke(&[], "true").status.success());
            assert!(fs::read(&events).unwrap().is_empty());
            assert!(invoke(&[], "false").status.success());
            for matching_file in [false, true] {
                fs::write(&events, b"").unwrap();
                if matching_file {
                    fs::write(&job, &definition_a).unwrap();
                }
                let result = invoke(std::slice::from_ref(&definition_a), "exit 42");
                assert!(!result.status.success());
                assert!(String::from_utf8_lossy(&result.stderr).contains("manager query failed"));
                assert!(fs::read(&events).unwrap().is_empty());
                assert_eq!(fs::read(&saved).unwrap(), saved_bytes);
                if matching_file {
                    assert_eq!(fs::read_to_string(&job).unwrap(), definition_a);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn orphan_or_foreign_startup_links_never_allow_mutation() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let job = temp.path().join("native-job");
        let wants = temp.path().join("wants");
        let events = temp.path().join("events");
        let expected = definition(&setup(), false);
        let invoke = || {
            Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!(
                        "set -eu\n{}\n/usr/bin/printf '%s' 'stop replace remove' >> {}",
                        ownership_check_script(
                            &job,
                            std::slice::from_ref(&expected),
                            "false",
                            Some(&wants)
                        ),
                        shell_quote(&events.to_string_lossy())
                    ),
                ])
                .output()
                .unwrap()
        };
        symlink(&job, &wants).unwrap();
        fs::write(&events, b"").unwrap();
        assert!(
            !invoke().status.success(),
            "missing unit cannot establish ownership of an orphan enablement link"
        );
        assert!(fs::read(&events).unwrap().is_empty());
        assert!(wants.symlink_metadata().unwrap().file_type().is_symlink());
        fs::write(&job, &expected).unwrap();
        fs::remove_file(&wants).unwrap();
        let foreign = temp.path().join("foreign-unit");
        fs::write(&foreign, "foreign").unwrap();
        symlink(&foreign, &wants).unwrap();
        assert!(
            !invoke().status.success(),
            "wrong-target link cannot be removed"
        );
        assert!(fs::read(&events).unwrap().is_empty());
        assert_eq!(fs::read_link(&wants).unwrap(), foreign);
        fs::remove_file(&wants).unwrap();
        symlink(&job, &wants).unwrap();
        assert!(
            invoke().status.success(),
            "matching unit and link stay operable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_reuses_confirmed_presence_and_failure_preserves_recovery_files() {
        let temp = tempfile::tempdir().unwrap();
        let setup = setup();
        let job = temp.path().join("native-job");
        let queries = temp.path().join("queries");
        let stopped = temp.path().join("stopped");
        let manager = temp.path().join("manager.sh");
        let preserved = [
            "setup.json",
            "plasmite",
            "plasmite.previous",
            "identity.previous.json",
        ]
        .map(|name| temp.path().join(name));
        for path in &preserved {
            fs::write(path, b"recovery data").unwrap();
        }
        for macos in [false, true] {
            let expected = definition(&setup, macos);
            for (presence, stop_exit) in [(0, 0), (0, 42), (42, 0)] {
                fs::write(&job, &expected).unwrap();
                fs::write(&queries, b"").unwrap();
                fs::write(&stopped, b"").unwrap();
                // A second query fails, reproducing the former raw-print path.
                // The exact stop script must use the first validated result.
                fs::write(&manager, format!(
                    "case \"$1\" in inspect|print|show)\nif [ -s {} ]; then exit 42; fi\n/usr/bin/printf x >> {}\nexit {presence};;\nbootout|stop) /usr/bin/printf x >> {}; exit {stop_exit};;\n*) exit 43;; esac\n",
                    shell_quote(&queries.to_string_lossy()),
                    shell_quote(&queries.to_string_lossy()),
                    shell_quote(&stopped.to_string_lossy())
                )).unwrap();
                let fake = format!("/bin/sh {}", shell_quote(&manager.to_string_lossy()));
                let stop = stop_script(&setup, macos).replace(
                    if macos {
                        "/bin/launchctl"
                    } else {
                        "/usr/bin/systemctl"
                    },
                    &fake,
                );
                let output = Command::new("/bin/sh")
                    .args([
                        "-c",
                        &format!(
                            "set -eu\n{}{}\n/bin/rm {}",
                            ownership_check_script(
                                &job,
                                std::slice::from_ref(&expected),
                                &format!("{fake} inspect"),
                                None
                            ),
                            stop,
                            shell_quote(&job.to_string_lossy())
                        ),
                    ])
                    .output()
                    .unwrap();
                assert_eq!(fs::read(&queries).unwrap(), b"x", "query exactly once");
                if presence == 0 && stop_exit == 0 {
                    assert!(output.status.success());
                    assert_eq!(fs::read(&stopped).unwrap(), b"x", "stop before removal");
                    assert!(!job.exists());
                } else {
                    assert!(!output.status.success());
                    assert_eq!(fs::read_to_string(&job).unwrap(), expected);
                    assert_eq!(
                        fs::read(&stopped).unwrap(),
                        if presence == 0 {
                            b"x".as_slice()
                        } else {
                            b"".as_slice()
                        }
                    );
                }
                for path in &preserved {
                    assert_eq!(fs::read(path).unwrap(), b"recovery data");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn inspection_failures_are_not_confirmed_native_absence() {
        let setup = setup();
        for query in [
            "exit 42",
            "/definitely/missing/service-manager",
            "kill -TERM $$",
        ] {
            let result = inspect_native_presence(&setup, query);
            let error = result.expect_err("unknown inspection cannot report an absent job");
            assert_eq!(error.kind(), ErrorKind::Io);
            assert!(error.to_string().contains("could not verify"));
        }
        assert!(inspect_native_presence(&setup, "exit 0").unwrap());
        assert!(!inspect_native_presence(&setup, "exit 1").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_service_lock_releases_lease_with_an_inherited_descriptor() {
        let temp = tempfile::tempdir().unwrap();
        let raw_path = temp.path().join("close-only.lock");
        let raw = File::create(&raw_path).unwrap();
        raw.try_lock_exclusive().unwrap();
        let raw_inherited = raw.try_clone().unwrap();
        drop(raw);
        let contender = File::open(raw_path).unwrap();
        assert!(
            contender.try_lock_exclusive().is_err(),
            "closing only the parent descriptor leaves the duplicated lease held"
        );
        drop(raw_inherited);
        // A parallel test can fork while the raw descriptor is open. Its child
        // releases that inherited copy on exec, so allow that brief delay here.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match contender.try_lock_exclusive() {
                Ok(()) => break,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("raw lock remained held after close: {error}"),
            }
        }
        FileExt::unlock(&contender).unwrap();

        let lease = pool_operation_lock(temp.path()).unwrap();
        // dup shares the same open-file description as a descriptor inherited
        // by fork. Keep it alive after the operation guard is dropped.
        let inherited = lease.0.try_clone().unwrap();
        assert!(pool_operation_lock(temp.path()).is_err());
        drop(lease);
        let next = pool_operation_lock(temp.path()).unwrap();
        drop(inherited);
        assert!(pool_operation_lock(temp.path()).is_err());
        drop(next);
        assert!(pool_operation_lock(temp.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn cross_home_install_serializes_inspection_mutation_and_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let mut original = setup();
        original.pool_dir = temp.path().join("pools");
        fs::create_dir(&original.pool_dir).unwrap();
        original.home = temp.path().join("home-a");
        original.program = original.home.join("plasmite");
        let mut alternate = original.clone();
        alternate.home = temp.path().join("home-b");
        alternate.program = alternate.home.join("plasmite");
        assert_eq!(original.job_path(), alternate.job_path());
        let lease = pool_operation_lock(&original.pool_dir).unwrap();
        verify_job_definition(&original, &[], None, false, true).unwrap();
        // Pause A after inspection. B cannot inspect/adopt the same global label
        // while A holds the actual filesystem lock through mutation and recovery.
        let other_pool = alternate.pool_dir.clone();
        let blocked = std::thread::spawn(move || {
            pool_operation_lock(&other_pool)
                .expect_err("another HOME must not enter the same operation")
                .kind()
        })
        .join()
        .unwrap();
        assert_eq!(blocked, ErrorKind::Busy);
        let job = temp.path().join("native-job");
        let definition_a = definition(&original, true);
        fs::write(&job, &definition_a).unwrap();
        let saved = temp.path().join("a-setup.json");
        write_atomic_json(&saved, &original).unwrap();
        let saved_bytes = fs::read(&saved).unwrap();
        assert!(
            pool_operation_lock(&alternate.pool_dir).is_err(),
            "rollback still holds the shared lease"
        );
        drop(lease);
        let _other_lease = pool_operation_lock(&alternate.pool_dir).unwrap();
        assert!(
            verify_job_definition(
                &alternate,
                &[],
                Some(&fs::read_to_string(&job).unwrap()),
                true,
                true
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&job).unwrap(), definition_a);
        assert_eq!(fs::read(&saved).unwrap(), saved_bytes);
    }

    fn fixture() -> (tempfile::TempDir, Setup, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let service = temp.path().join("service");
        create_private_dir(&service).unwrap();
        let mut setup = setup();
        setup.pool_dir = temp.path().join("pools");
        fs::create_dir(&setup.pool_dir).unwrap();
        setup.program = service.join("plasmite");
        setup.home = temp.path().to_path_buf();
        let path = service.join("setup.json");
        let source = temp.path().join("new-program");
        fs::write(&source, b"new executable").unwrap();
        fs::write(&setup.program, b"old executable").unwrap();
        write_atomic_json(&path, &setup).unwrap();
        (temp, setup, path, source)
    }

    fn ready(setup: &Setup) -> Status {
        Status {
            pool_dir: setup.pool_dir.clone(),
            pid: Some(123),
            local_url: "http://127.0.0.1:9700".into(),
            remote_url: None,
            managed: true,
            startup: true,
            state: "running".into(),
            problem: None,
            setup: Some(setup.clone()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn preparation_failure_leaves_old_service_unchanged_and_retryable() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, setup, path, source) = fixture();
        let store =
            crate::access_store::AccessStore::open(&setup.pool_dir, None, None, None).unwrap();
        drop(store);
        let identity = setup.pool_dir.join(".plasmite-serve/identity.json");
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o644)).unwrap();
        let settings = fs::read(&path).unwrap();
        let result = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |_, _, _| panic!("preparation failure must not activate"),
            |_, _| panic!("preparation failure must not stop old service"),
        );
        assert!(result.is_err());
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(&setup.program).unwrap(), b"old executable");
        assert_eq!(fs::read(&path).unwrap(), settings);
        assert!(!setup.program.with_extension("previous").exists());
        assert!(!setup.program.with_extension("new").exists());
        assert!(!path.with_extension("previous.json").exists());
        update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |candidate, _, _| Ok(ready(candidate)),
            |_, _| panic!("successful retry needs no recovery"),
        )
        .unwrap();
    }

    #[test]
    fn failed_first_setup_write_removes_new_executable() {
        let (_temp, setup, _path, source) = fixture();
        fs::remove_file(&setup.program).unwrap();
        let path = setup
            .program
            .parent()
            .unwrap()
            .join("missing-directory/setup.json");
        let result = update_program(
            &setup,
            None,
            &path,
            &source,
            |_, _, _| panic!("failed setup write must not activate"),
            |_, _| panic!("failed setup write must not change the native job"),
        );
        let error = result.err().unwrap().to_string();
        assert!(!error.contains("recovery needs attention"), "{error}");
        assert!(!setup.program.exists());
        assert!(!path.exists());
    }

    #[test]
    fn failed_approval_restores_old_files_without_stopping_old_service() {
        let (_temp, old, path, source) = fixture();
        let old_settings = fs::read(&path).unwrap();
        let mut replacement = old.clone();
        replacement.run.bind = Some("127.0.0.1:12345".into());
        let result = update_program(
            &replacement,
            Some(&old),
            &path,
            &source,
            |candidate, restoring, native_may_have_started| {
                assert!(!restoring);
                assert!(!*native_may_have_started);
                assert_eq!(fs::read(&candidate.program).unwrap(), b"new executable");
                Err(usage(
                    "administrator approval was denied before native command launch",
                ))
            },
            |_, _| panic!("approval failure must not stop the old service"),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&old.program).unwrap(), b"old executable");
        assert_eq!(fs::read(&path).unwrap(), old_settings);
        assert!(!old.program.with_extension("previous").exists());
        assert!(!path.with_extension("previous.json").exists());
    }

    #[test]
    fn unchanged_reinstall_activates_stopped_service_and_refreshes_executable() {
        let (_temp, setup, path, source) = fixture();
        let running = std::cell::Cell::new(false);
        let result = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |candidate, restoring, _| {
                assert!(!restoring);
                assert_eq!(fs::read(&candidate.program).unwrap(), b"new executable");
                running.set(true);
                Ok(ready(candidate))
            },
            |_, _| panic!("successful reinstall needs no recovery"),
        )
        .unwrap();
        assert!(running.get());
        assert_eq!(result.state, "running");
        assert!(!setup.program.with_extension("previous").exists());
    }

    #[test]
    fn failed_readiness_restores_exact_identity_and_program_without_rewinding_keys() {
        use crate::access_store::AccessStore;
        let (temp, setup, path, source) = fixture();
        let old = AccessStore::open(&setup.pool_dir, None, None, None).unwrap();
        let key = old.issue("saved client").unwrap();
        let fingerprint = old.fingerprint().to_owned();
        let cert_bytes = fs::read(old.cert_path()).unwrap();
        let key_bytes = fs::read(old.key_path()).unwrap();
        let identity = setup.pool_dir.join(".plasmite-serve/identity.json");
        let identity_bytes = fs::read(&identity).unwrap();
        drop(old);
        fs::create_dir(temp.path().join("replacement")).unwrap();
        let replacement =
            AccessStore::open(&temp.path().join("replacement"), None, None, None).unwrap();
        let cert = replacement.cert_path();
        let tls_key = replacement.key_path();
        let replacement_fingerprint = replacement.fingerprint().to_owned();
        drop(replacement);
        let keys_path = setup.pool_dir.join(".plasmite-serve/keys.json");
        let updated_keys = std::cell::RefCell::new(Vec::new());
        let stopped = std::cell::Cell::new(false);
        let result = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |candidate, restoring, native_may_have_started| {
                if !restoring {
                    *native_may_have_started = true;
                    let store =
                        AccessStore::open(&candidate.pool_dir, None, Some((&cert, &tls_key)), None)
                            .unwrap();
                    assert_eq!(store.fingerprint(), replacement_fingerprint);
                    // Model an access mutation committed before readiness fails.
                    store.issue("during update").unwrap();
                    *updated_keys.borrow_mut() = fs::read(&keys_path).unwrap();
                    return Err(usage("injected readiness failure"));
                }
                assert!(stopped.get(), "stop before restoring identity");
                assert_eq!(fs::read(&candidate.program).unwrap(), b"old executable");
                assert_eq!(fs::read(&identity).unwrap(), identity_bytes);
                let store = AccessStore::open(&candidate.pool_dir, None, None, None).unwrap();
                assert_eq!(store.fingerprint(), fingerprint);
                assert_eq!(fs::read(store.cert_path()).unwrap(), cert_bytes);
                assert_eq!(fs::read(store.key_path()).unwrap(), key_bytes);
                assert!(store.authorize_key(&key).is_some());
                assert_eq!(store.list().unwrap().len(), 2);
                Ok(ready(candidate))
            },
            |_, uninstall| {
                assert!(!uninstall);
                stopped.set(true);
                Ok(())
            },
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("injected readiness failure")
        );
        assert_eq!(fs::read(keys_path).unwrap(), *updated_keys.borrow());
        assert!(!setup.program.with_extension("previous").exists());
        assert!(!path.with_file_name("identity.previous.json").exists());
    }

    #[test]
    fn failed_first_install_restores_manual_identity_and_keeps_committed_keys() {
        use crate::access_store::AccessStore;
        let (temp, setup, path, source) = fixture();
        fs::remove_file(&path).unwrap();
        fs::remove_file(&setup.program).unwrap();
        let old = AccessStore::open(&setup.pool_dir, None, None, None).unwrap();
        let key = old.issue("manual client").unwrap();
        let fingerprint = old.fingerprint().to_owned();
        let identity = setup.pool_dir.join(".plasmite-serve/identity.json");
        let identity_bytes = fs::read(&identity).unwrap();
        let old_cert = old.cert_path();
        let old_tls_key = old.key_path();
        let cert_bytes = fs::read(&old_cert).unwrap();
        let tls_key_bytes = fs::read(&old_tls_key).unwrap();
        drop(old);
        fs::create_dir(temp.path().join("replacement")).unwrap();
        let replacement =
            AccessStore::open(&temp.path().join("replacement"), None, None, None).unwrap();
        let cert = replacement.cert_path();
        let tls_key = replacement.key_path();
        let replacement_fingerprint = replacement.fingerprint().to_owned();
        drop(replacement);
        let keys_path = setup.pool_dir.join(".plasmite-serve/keys.json");
        let updated_keys = std::cell::RefCell::new(Vec::new());
        let deactivated = std::cell::Cell::new(false);
        let result = update_program(
            &setup,
            None,
            &path,
            &source,
            |candidate, restoring, native_may_have_started| {
                assert!(!restoring, "no previous managed service to restart");
                *native_may_have_started = true;
                let store =
                    AccessStore::open(&candidate.pool_dir, None, Some((&cert, &tls_key)), None)
                        .unwrap();
                assert_eq!(store.fingerprint(), replacement_fingerprint);
                store.issue("during first install").unwrap();
                *updated_keys.borrow_mut() = fs::read(&keys_path).unwrap();
                Err(usage("injected first-install readiness failure"))
            },
            |_, uninstall| {
                assert!(uninstall);
                deactivated.set(true);
                Ok(())
            },
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("first-install readiness failure")
        );
        assert!(deactivated.get());
        assert_eq!(fs::read(&identity).unwrap(), identity_bytes);
        assert_eq!(fs::read(old_cert).unwrap(), cert_bytes);
        assert_eq!(fs::read(old_tls_key).unwrap(), tls_key_bytes);
        assert_eq!(fs::read(keys_path).unwrap(), *updated_keys.borrow());
        let restarted = AccessStore::open(&setup.pool_dir, None, None, None).unwrap();
        assert_eq!(restarted.fingerprint(), fingerprint);
        assert!(restarted.authorize_key(&key).is_some());
        assert_eq!(restarted.list().unwrap().len(), 2);
        assert!(!path.exists());
        assert!(!setup.program.exists());
        assert!(!path.with_file_name("identity.previous.json").exists());
    }

    #[test]
    fn executable_restore_failure_retains_backup_and_saved_recovery_settings() {
        let (_temp, setup, path, source) = fixture();
        let result = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |_, restoring, native_may_have_started| {
                assert!(!restoring, "never restart after failed program restore");
                *native_may_have_started = true;
                Err(usage("injected readiness failure"))
            },
            |candidate, _| {
                fs::remove_file(&candidate.program).unwrap();
                fs::create_dir(&candidate.program).unwrap();
                Ok(())
            },
        );
        let error = result.err().unwrap().to_string();
        assert!(error.contains("recovery needs attention"), "{error}");
        assert_eq!(
            fs::read(setup.program.with_extension("previous")).unwrap(),
            b"old executable"
        );
        assert!(path.with_extension("previous.json").exists());
        let again = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |_, _, _| panic!("pending recovery must block activation"),
            |_, _| panic!("pending recovery must block native mutations"),
        );
        assert!(again.err().unwrap().to_string().contains("needs recovery"));
        assert_eq!(
            fs::read(setup.program.with_extension("previous")).unwrap(),
            b"old executable"
        );
    }

    #[test]
    fn concurrent_owner_blocks_identity_recovery_and_preserves_backups() {
        use crate::access_store::AccessStore;
        let (_temp, setup, path, source) = fixture();
        let store = AccessStore::open(&setup.pool_dir, None, None, None).unwrap();
        let identity = setup.pool_dir.join(".plasmite-serve/identity.json");
        let old_identity = fs::read(&identity).unwrap();
        drop(store);
        let owner = std::cell::RefCell::new(None);
        let result = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |_, restoring, native_may_have_started| {
                assert!(!restoring);
                *native_may_have_started = true;
                Err(usage("injected readiness failure"))
            },
            |candidate, _| {
                // A foreground owner acquires the actual AccessStore lock after
                // the replacement stops. Recovery must not alter its identity.
                let store = AccessStore::open(
                    &candidate.pool_dir,
                    Some("https://new-owner.local"),
                    None,
                    None,
                )
                .unwrap();
                *owner.borrow_mut() = Some(store);
                Ok(())
            },
        );
        let error = result.err().unwrap();
        assert!(error.hint().unwrap().contains("another server owns"));
        assert_ne!(fs::read(&identity).unwrap(), old_identity);
        assert_eq!(
            fs::read(path.with_file_name("identity.previous.json")).unwrap(),
            old_identity
        );
        assert_eq!(
            fs::read(setup.program.with_extension("previous")).unwrap(),
            b"old executable"
        );
        assert!(path.with_extension("previous.json").exists());
    }

    #[test]
    fn failed_stop_preserves_recovery_data_without_restoring_active_identity() {
        use crate::access_store::AccessStore;
        let (_temp, setup, path, source) = fixture();
        let store = AccessStore::open(&setup.pool_dir, None, None, None).unwrap();
        drop(store);
        let identity = setup.pool_dir.join(".plasmite-serve/identity.json");
        let bytes = fs::read(&identity).unwrap();
        let result = update_program(
            &setup,
            Some(&setup),
            &path,
            &source,
            |_, restoring, native_may_have_started| {
                assert!(!restoring);
                *native_may_have_started = true;
                Err(usage("injected startup failure"))
            },
            |_, _| Err(usage("injected stop failure")),
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("recovery needs attention")
        );
        assert_eq!(
            fs::read(path.with_file_name("identity.previous.json")).unwrap(),
            bytes
        );
        assert_eq!(fs::read(identity).unwrap(), bytes);
        assert_eq!(
            fs::read(setup.program.with_extension("previous")).unwrap(),
            b"old executable"
        );
        assert_eq!(fs::read(&setup.program).unwrap(), b"new executable");
    }

    #[test]
    fn public_url_sets_listener_port_and_explicit_bind_overrides_it() {
        let run = effective_args(&ServeRunArgs {
            server: Some("https://pi.local:8443".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(run.remote_bind.as_deref(), Some("0.0.0.0:8443"));
        let run = effective_args(&ServeRunArgs {
            server: Some("https://pools.example.net".into()),
            remote_bind: Some("127.0.0.1:9743".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(run.remote_bind.as_deref(), Some("127.0.0.1:9743"));
        assert_eq!(
            effective_args(&ServeRunArgs {
                server: Some("https://pi.local".into()),
                ..Default::default()
            })
            .unwrap()
            .remote_bind
            .as_deref(),
            Some("0.0.0.0:443")
        );
    }
    #[test]
    fn legacy_public_address_preserves_the_v1_listener_default() {
        let run = effective_args(&ServeRunArgs {
            shared_address: Some("https://proxy.example.net:8443".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(run.remote_bind.as_deref(), Some("0.0.0.0:9743"));
    }
    #[test]
    fn saved_proxy_listener_survives_public_address_updates() {
        let mut setup = setup();
        setup.run.remote_bind = Some("127.0.0.1:9743".into());
        let run = merge(
            Some(&setup),
            &ServeRunArgs {
                server: Some("https://proxy.example.net".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(run.remote_bind, setup.run.remote_bind);
    }
    #[test]
    fn saved_settings_survive_install_without_overrides() {
        let mut setup = setup();
        setup.run.bind = Some("127.0.0.1:12345".into());
        assert_eq!(
            merge(Some(&setup), &ServeRunArgs::default()).unwrap().bind,
            setup.run.bind
        );
    }
    #[test]
    fn duplicate_fixed_listeners_fail_before_identity_mutation() {
        assert!(
            effective_args(&ServeRunArgs {
                bind: Some("127.0.0.1:9700".into()),
                remote_bind: Some("127.0.0.1:9700".into()),
                ..Default::default()
            })
            .unwrap_err()
            .to_string()
            .contains("different addresses")
        );
        assert!(
            effective_args(&ServeRunArgs {
                bind: Some("127.0.0.1:0".into()),
                remote_bind: Some("127.0.0.1:0".into()),
                ..Default::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn invalid_addresses_and_duplicate_spelling_fail_before_setup() {
        for server in [
            "http://pi.local",
            "https://pi.local/pools",
            "https://user@pi.local",
            "https://pi.local?query",
        ] {
            assert!(
                effective_args(&ServeRunArgs {
                    server: Some(server.into()),
                    ..Default::default()
                })
                .is_err()
            );
        }
        assert!(
            effective_args(&ServeRunArgs {
                server: Some("https://pi.local".into()),
                shared_address: Some("https://pi.local".into()),
                ..Default::default()
            })
            .is_err()
        );
    }
    #[test]
    fn native_jobs_run_as_owner_before_login_and_escape_paths() {
        let setup = setup();
        let linux = definition(&setup, false);
        assert!(linux.contains("User=alice"));
        assert!(linux.contains("WantedBy=multi-user.target"));
        assert!(linux.contains("$$name%%literal"));
        assert!(linux.contains("Restart=on-failure"));
        let mac = definition(&setup, true);
        assert!(mac.contains("<key>UserName</key><string>alice</string>"));
        assert!(mac.contains("<key>RunAtLoad</key><true/>"));
        assert!(!mac.contains("LimitLoadToSessionType"));
        assert!(xml("<&\"").contains("&lt;&amp;&quot;"));
    }
}
