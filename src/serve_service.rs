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
    let _: SocketAddr = run
        .remote_bind
        .as_deref()
        .unwrap()
        .parse()
        .map_err(|_| usage("invalid remote bind address")
            .with_hint("Use a numeric IP:port, such as 100.101.102.103:9743 or [fd7a:115c:a1e0::abcd]:9743. Pass the public DNS name as SERVER."))?;
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

fn lock(pool_dir: &Path) -> Result<File, Error> {
    let directory = directory()?;
    fs::create_dir_all(directory.parent().unwrap())
        .map_err(|error| io_error("failed to create service directory", &directory, error))?;
    create_private_dir(&directory)?;
    ensure_private(&directory)?;
    let job_dir = directory.join(id(pool_dir));
    create_private_dir(&job_dir)?;
    ensure_private(&job_dir)?;
    let path = job_dir.join("lock");
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
    file.try_lock_exclusive()
        .map_err(|error| io_error("another command is changing this service", &path, error))?;
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

fn admin(program: &str, arguments: &[&str]) -> Result<(), Error> {
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
            Err(_) => (None, "stopped"),
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
fn loaded(setup: &Setup) -> bool {
    if cfg!(target_os = "macos") {
        capture("launchctl", &["print", &setup.target()]).is_ok()
    } else {
        capture(
            "systemctl",
            &["show", &setup.unit(), "--property=LoadState", "--value"],
        )
        .is_ok_and(|state| !state.is_empty() && state != "not-found")
    }
}

fn stop_script(setup: &Setup) -> String {
    if cfg!(target_os = "macos") {
        format!(
            "if /bin/launchctl print {target} >/dev/null 2>&1; then /bin/launchctl bootout {target}; fi\n",
            target = shell_quote(&setup.target())
        )
    } else {
        format!(
            "plasmite_load=$(/usr/bin/systemctl show {unit} --property=LoadState --value)\nif [ \"$plasmite_load\" != not-found ]; then /usr/bin/systemctl stop {unit}; fi\n",
            unit = shell_quote(&setup.unit())
        )
    }
}

fn stop_native(setup: &Setup) -> Result<(), Error> {
    admin(
        "/bin/sh",
        &["-c", &format!("set -eu\n{}", stop_script(setup))],
    )
}

fn start_native(setup: &Setup, restart: bool) -> Result<(), Error> {
    if cfg!(target_os = "macos") {
        if loaded(setup) {
            if restart {
                admin("launchctl", &["kickstart", "-k", &setup.target()])?;
            }
            Ok(())
        } else {
            admin(
                "launchctl",
                &["bootstrap", "system", &setup.job_path().to_string_lossy()],
            )
        }
    } else {
        admin(
            "systemctl",
            &[if restart { "restart" } else { "start" }, &setup.unit()],
        )
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn install_native(setup: &Setup) -> Result<(), Error> {
    // Pass the definition in memory; an unprivileged process must not replace a staging
    // file while the owner approves the administrator request.
    let job = setup.job_path();
    let template = job
        .parent()
        .unwrap()
        .join(format!(".{}.XXXXXX", id(&setup.pool_dir)));
    let mut script = format!(
        "set -eu\numask 077\nplasmite_definition=$(/usr/bin/mktemp {})\ntrap '/bin/rm -f \"$plasmite_definition\"' EXIT\n/usr/bin/printf '%s' {} > \"$plasmite_definition\"\n/bin/chmod 644 \"$plasmite_definition\"\n",
        shell_quote(&template.to_string_lossy()),
        shell_quote(&definition(setup, cfg!(target_os = "macos")))
    );
    if cfg!(target_os = "macos") {
        script.push_str(&stop_script(setup));
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
    admin("/bin/sh", &["-c", &script])
}

fn uninstall_native(setup: &Setup) -> Result<(), Error> {
    let wants = PathBuf::from("/etc/systemd/system/multi-user.target.wants").join(setup.unit());
    if !setup.job_path().exists()
        && !loaded(setup)
        && (cfg!(target_os = "macos") || wants.symlink_metadata().is_err())
    {
        return Ok(());
    }
    let mut script = String::from("set -eu\n");
    script.push_str(&stop_script(setup));
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
    admin("/bin/sh", &["-c", &script])
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
    let temporary = setup.program.with_extension("new");
    let backup = setup.program.with_extension("previous");
    if setup.program.exists() {
        fs::copy(&setup.program, &backup)
            .map_err(|error| io_error("failed to preserve installed program", &backup, error))?;
    }
    fs::copy(source, &temporary)
        .map_err(|error| io_error("failed to copy Plasmite executable", &temporary, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).map_err(|error| {
            io_error("failed to protect installed executable", &temporary, error)
        })?;
    }
    fs::rename(&temporary, &setup.program)
        .map_err(|error| io_error("failed to install executable", &setup.program, error))?;
    let result = (|| {
        // Keep a recoverable setup if the native manager fails partway through installation.
        write_atomic_json(&path, &setup)?;
        install_native(&setup)?;
        wait_ready(&setup)?;
        if !registered(&setup)? {
            return Err(usage("the service manager did not enable startup at boot"));
        }
        status(&setup)
    })();
    if result.is_err() {
        if let Some(old) = &previous {
            if backup.exists() {
                fs::rename(&backup, &setup.program).map_err(|error| {
                    io_error(
                        "could not restore the previous service executable",
                        &setup.program,
                        error,
                    )
                })?;
            }
            let restore = install_native(old)
                .and_then(|()| {
                    if was_running {
                        wait_ready(old)
                    } else {
                        stop_native(old)
                    }
                })
                .and_then(|()| write_atomic_json(&path, old));
            if let Err(restore) = restore {
                return Err(Error::new(ErrorKind::Io)
                    .with_message(
                        "service update failed and the previous service could not restart",
                    )
                    .with_hint(format!("Inspect `serve logs`; recovery failed: {restore}")));
            }
        } else {
            if let Err(cleanup) = uninstall_native(&setup) {
                return Err(Error::new(ErrorKind::Io).with_message("service installation failed and cleanup needs attention")
                    .with_path(setup.job_path()).with_hint(format!("The saved setup remains recoverable. Run `plasmite --dir '{}' serve uninstall`. Cleanup failed: {cleanup}", setup.pool_dir.display())));
            }
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&setup.program);
        }
    }
    let _ = fs::remove_file(backup);
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
            stop_native(&setup)?;
        }
        "uninstall" => {
            uninstall_native(&setup)?;
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
        use std::io::BufRead;
        let mut child = command.stdout(Stdio::piped()).spawn().map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message("failed to read service logs")
                .with_source(error)
        })?;
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line.map_err(|error| {
                Error::new(ErrorKind::Io)
                    .with_message("failed to read log line")
                    .with_source(error)
            })?;
            println!("{}", serde_json::json!({"message": line}));
        }
        child.wait()
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
