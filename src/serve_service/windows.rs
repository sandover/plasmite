//! Native Windows services. The manager owns the process lifecycle; a protected
//! installation binds one pool owner to one virtual service account.
use super::{Setup, Status, id, io_error, merge, usage};
use crate::cli::args::ServeRunArgs;
use crate::{access_store, serve_registry, windows_private};
use fs2::FileExt;
use plasmite::api::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, BufRead, Seek, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE,
    LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_SERVICE,
    SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, LookupAccountNameW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Services::*;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramFiles, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHGetKnownFolderPath,
    ShellExecuteExW,
};

#[derive(Clone, Serialize, Deserialize)]
struct Installed {
    setup: Setup,
    owner: String,
    service_sid: String,
}

#[derive(Serialize, Deserialize)]
struct Request {
    action: String,
    pool_dir: PathBuf,
    owner: String,
    home: PathBuf,
    source: PathBuf,
    options: ServeRunArgs,
    result: String,
}

static HOST: OnceLock<Installed> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
static SHUTDOWN: OnceLock<tokio::sync::Notify> = OnceLock::new();

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn path_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn win_error(message: &str) -> Error {
    Error::new(ErrorKind::Io)
        .with_message(message)
        .with_source(io::Error::last_os_error())
}
fn io_result<T>(result: io::Result<T>, path: &Path) -> Result<T, Error> {
    result.map_err(|err| io_error("failed to access Windows service state", path, err))
}
fn current_sid() -> Result<String, Error> {
    io_result(windows_private::current_sid(), Path::new("Windows account"))
}
fn home() -> Result<PathBuf, Error> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| usage("service installation needs an absolute user profile"))
}
fn root() -> Result<PathBuf, Error> {
    let mut value = std::ptr::null_mut();
    let status = unsafe {
        SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut value)
    };
    if status < 0 {
        return Err(usage("Windows Program Files directory is unavailable"));
    }
    let text = unsafe { from_wide(value) };
    unsafe {
        CoTaskMemFree(value.cast());
    }
    Ok(PathBuf::from(text).join("Plasmite").join("Services"))
}
fn installation(pool: &Path) -> Result<PathBuf, Error> {
    Ok(root()?.join(id(pool)))
}
fn manifest(pool: &Path) -> Result<PathBuf, Error> {
    Ok(installation(pool)?.join("setup.json"))
}
fn runtime_dir(pool: &Path) -> PathBuf {
    pool.join(".plasmite-serve").join("service")
}
fn service_name(pool: &Path) -> String {
    id(pool)
}
fn account(pool: &Path) -> String {
    format!("NT SERVICE\\{}", service_name(pool))
}

// Always quote service arguments with Windows' argv rules, including trailing
// backslashes. No shell interprets the resulting command line.
fn quote(text: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in text.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            result.extend(std::iter::repeat_n('\\', slashes));
        }
        result.push(ch);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}
fn command(setup: &Setup) -> Result<String, Error> {
    Ok(format!(
        "{} __plasmite_service {}",
        quote(&setup.program.to_string_lossy()),
        quote(&manifest(&setup.pool_dir)?.to_string_lossy())
    ))
}
unsafe fn from_wide(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

struct Service(SC_HANDLE);
impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

// Returning service-created files to the pool owner needs this administrator
// privilege. Keep it enabled only inside the elevated installation operation.
struct RestorePrivilege {
    token: OwnedHandle,
    previous: windows_sys::Win32::Security::TOKEN_PRIVILEGES,
}
impl RestorePrivilege {
    fn enable() -> Result<Self, Error> {
        use windows_sys::Win32::Security::{
            AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
            TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token = std::ptr::null_mut();
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
                &mut token,
            )
        } == 0
        {
            return Err(win_error(
                "service installation requires administrator approval",
            ));
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            ..Default::default()
        };
        if unsafe {
            LookupPrivilegeValueW(
                std::ptr::null(),
                wide("SeRestorePrivilege").as_ptr(),
                &mut privileges.Privileges[0].Luid,
            )
        } == 0
        {
            return Err(win_error("failed to find file ownership privilege"));
        }
        privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
        let mut previous = TOKEN_PRIVILEGES::default();
        let mut needed = 0;
        unsafe {
            windows_sys::Win32::Foundation::SetLastError(0);
        }
        if unsafe {
            AdjustTokenPrivileges(
                token.as_raw_handle(),
                0,
                &privileges,
                std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
                &mut previous,
                &mut needed,
            )
        } == 0
            || io::Error::last_os_error().raw_os_error() != Some(0)
        {
            return Err(usage(
                "approve service installation as the pool-owning administrator",
            ));
        }
        Ok(Self { token, previous })
    }
}
impl Drop for RestorePrivilege {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Security::AdjustTokenPrivileges(
                self.token.as_raw_handle(),
                0,
                &self.previous,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
    }
}
fn manager(access: u32) -> Result<Service, Error> {
    let handle = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
    if handle.is_null() {
        Err(win_error("failed to open Windows Service Control Manager"))
    } else {
        Ok(Service(handle))
    }
}
fn open(pool: &Path, access: u32, allow_deleted: bool) -> Result<Option<Service>, Error> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let handle = unsafe { OpenServiceW(manager.0, wide(&service_name(pool)).as_ptr(), access) };
    if !handle.is_null() {
        return Ok(Some(Service(handle)));
    }
    let code = io::Error::last_os_error().raw_os_error();
    if code == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32)
        || (allow_deleted
            && code == Some(windows_sys::Win32::Foundation::ERROR_SERVICE_MARKED_FOR_DELETE as i32))
    {
        // Uninstall can finish file cleanup while SCM waits for other handles.
        // Installation keeps this state as an error until the name is free.
        Ok(None)
    } else {
        Err(win_error("failed to open installed Windows service"))
    }
}
fn query(service: &Service) -> Result<SERVICE_STATUS_PROCESS, Error> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(win_error("failed to query Windows service"));
    }
    Ok(status)
}
fn verify_service(record: &Installed, access: u32) -> Result<Service, Error> {
    let service = open(
        &record.setup.pool_dir,
        access | SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
        false,
    )?
    .ok_or_else(|| usage("the installed Windows service is missing"))?;
    let mut needed = 0;
    unsafe {
        QueryServiceConfigW(service.0, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 64 * 1024 {
        return Err(win_error("failed to inspect Windows service configuration"));
    }
    let mut data = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    let config = data.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    if unsafe { QueryServiceConfigW(service.0, config, needed, &mut needed) } == 0 {
        return Err(win_error("failed to inspect Windows service configuration"));
    }
    let config = unsafe { &*config };
    if config.dwServiceType != SERVICE_WIN32_OWN_PROCESS
        || unsafe { from_wide(config.lpBinaryPathName) } != command(&record.setup)?
        || unsafe { from_wide(config.lpServiceStartName) }.to_lowercase()
            != account(&record.setup.pool_dir).to_lowercase()
    {
        return Err(usage(
            "Windows service configuration does not match this installation",
        ));
    }
    Ok(service)
}
fn start_type(service: &Service) -> Result<u32, Error> {
    let mut needed = 0;
    unsafe {
        QueryServiceConfigW(service.0, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 64 * 1024 {
        return Err(win_error("failed to inspect Windows service startup"));
    }
    let mut data = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    let config = data.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    if unsafe { QueryServiceConfigW(service.0, config, needed, &mut needed) } == 0 {
        return Err(win_error("failed to inspect Windows service startup"));
    }
    Ok(unsafe { (*config).dwStartType })
}
fn set_start_type(service: &Service, start_type: u32) -> Result<(), Error> {
    if unsafe {
        ChangeServiceConfigW(
            service.0,
            SERVICE_NO_CHANGE,
            start_type,
            SERVICE_NO_CHANGE,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(win_error("failed to enable Windows boot startup"));
    }
    Ok(())
}
fn load(pool: &Path) -> Result<Option<Installed>, Error> {
    let path = manifest(pool)?;
    let bytes = match windows_private::read_installed(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error("unsafe Windows service setup", &path, err)),
    };
    let record: Installed = serde_json::from_slice(&bytes)
        .map_err(|err| usage("invalid Windows service setup").with_source(err))?;
    if record.setup.pool_dir != pool
        || record.setup.program != installation(pool)?.join("plasmite.exe")
        || record.setup.account != account(pool)
        || !record.service_sid.starts_with("S-1-5-80-")
    {
        return Err(usage(
            "Windows service setup does not match its directory or account",
        ));
    }
    let sid = current_sid()?;
    if sid != record.owner && sid != record.service_sid {
        return Err(usage("this Windows service belongs to another account"));
    }
    shared(&record)?;
    if open(pool, SERVICE_QUERY_CONFIG, true)?.is_some() {
        if record.service_sid != lookup_sid(&account(pool))? {
            return Err(usage("Windows service identity does not match its setup"));
        }
        verify_service(&record, 0)?;
    }
    Ok(Some(record))
}
fn required(pool: &Path) -> Result<Installed, Error> {
    load(pool)?.ok_or_else(|| {
        usage("this pool directory has no installed server")
            .with_hint("Run `plasmite --dir DIR serve install SERVER` first.")
    })
}
fn shared(record: &Installed) -> Result<windows_private::Shared, Error> {
    io_result(
        windows_private::Shared::new(&record.owner, &record.service_sid),
        &record.setup.pool_dir,
    )
}

/// Authenticate shared server state through its protected installation. Paths
/// outside this pool's private server state keep the original owner-only policy.
pub(crate) fn shared_policy(path: &Path) -> Result<Option<windows_private::Shared>, Error> {
    let Some(state) = path
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == ".plasmite-serve"))
    else {
        return Ok(None);
    };
    let Some(pool) = state.parent() else {
        return Ok(None);
    };
    let pool = fs::canonicalize(pool)
        .map_err(|err| io_error("failed to resolve server state", pool, err))?;
    load(&pool)?.as_ref().map(shared).transpose()
}

fn lookup_sid(name: &str) -> Result<String, Error> {
    let mut size = 0;
    let mut domain_size = 0;
    let mut kind = 0;
    let name = wide(name);
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            &mut domain_size,
            &mut kind,
        );
    }
    if size == 0 {
        return Err(win_error("failed to resolve service account"));
    }
    let mut sid = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    let mut domain = vec![0u16; domain_size as usize];
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut size,
            domain.as_mut_ptr(),
            &mut domain_size,
            &mut kind,
        )
    } == 0
    {
        return Err(win_error("failed to resolve service account"));
    }
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut text) } == 0 {
        return Err(win_error("failed to encode service account"));
    }
    let result = unsafe { from_wide(text) };
    unsafe {
        LocalFree(text.cast());
    }
    Ok(result)
}
fn start(service: &Service) -> Result<(), Error> {
    if unsafe { StartServiceW(service.0, 0, std::ptr::null()) } == 0
        && io::Error::last_os_error().raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32)
    {
        return Err(win_error("failed to start Windows service"));
    }
    Ok(())
}
fn stop(service: &Service) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut process = None;
    let mut observed_pid = 0;
    let mut requested = false;
    loop {
        let native = query(service)?;
        if native.dwProcessId != 0 && native.dwProcessId != observed_pid {
            observed_pid = native.dwProcessId;
            let handle = unsafe {
                windows_sys::Win32::System::Threading::OpenProcess(
                    windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
                    0,
                    observed_pid,
                )
            };
            // START_PENDING can initially report PID 0. Capture the process as
            // soon as it appears, before trusting STOPPED for file replacement.
            // A normal owner may lack process rights; elevated updates can wait.
            process = (!handle.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(handle) });
        }
        match native.dwCurrentState {
            SERVICE_STOPPED => {
                if let Some(process) = &process {
                    let timeout = deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u32;
                    if unsafe { WaitForSingleObject(process.as_raw_handle(), timeout) } != 0 {
                        return Err(usage(
                            "Windows service process has not exited; update files remain intact",
                        ));
                    }
                }
                return Ok(());
            }
            SERVICE_START_PENDING | SERVICE_STOP_PENDING => (),
            _ if !requested => {
                let mut status = SERVICE_STATUS::default();
                if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } == 0 {
                    let code = io::Error::last_os_error().raw_os_error();
                    if code != Some(ERROR_SERVICE_NOT_ACTIVE as i32)
                        && code
                            != Some(
                                windows_sys::Win32::Foundation::ERROR_SERVICE_CANNOT_ACCEPT_CTRL
                                    as i32,
                            )
                    {
                        return Err(win_error("failed to request service shutdown"));
                    }
                } else {
                    requested = true;
                }
            }
            _ => (),
        }
        if Instant::now() >= deadline {
            return Err(usage(
                "Windows service did not stop; update files remain intact",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn status(record: &Installed) -> Result<Status, Error> {
    if open(&record.setup.pool_dir, SERVICE_QUERY_CONFIG, true)?.is_none() {
        return Ok(Status {
            pool_dir: record.setup.pool_dir.clone(), pid: None,
            local_url: format!("http://{}", record.setup.run.bind.as_deref().unwrap()),
            remote_url: record.setup.run.server.clone(), managed: true, startup: false,
            state: "failed".into(), problem: Some("The Windows service is missing. Run `serve uninstall` to finish cleanup before reinstalling.".into()),
            setup: Some(record.setup.clone()),
        });
    }
    let service = verify_service(record, 0)?;
    let native = query(&service)?;
    let server = managed_running(record)?
        .into_iter()
        .find(|s| s.pid == native.dwProcessId);
    let state = match native.dwCurrentState {
        SERVICE_RUNNING if server.is_some() => "running",
        SERVICE_RUNNING | SERVICE_START_PENDING => "starting",
        SERVICE_STOP_PENDING => "stopping",
        SERVICE_STOPPED if native.dwWin32ExitCode != 0 => "failed",
        _ => "stopped",
    };
    Ok(Status {
        pool_dir: record.setup.pool_dir.clone(),
        pid: server.as_ref().map(|s| s.pid),
        local_url: server
            .as_ref()
            .map(|s| s.local_url.clone())
            .unwrap_or_else(|| format!("http://{}", record.setup.run.bind.as_deref().unwrap())),
        remote_url: record.setup.run.server.clone(),
        managed: true,
        startup: start_type(&service)? == SERVICE_AUTO_START,
        state: state.into(),
        problem: (state == "failed" || state == "starting")
            .then(|| "The installed server has not responded. Inspect `serve logs`.".into()),
        setup: Some(record.setup.clone()),
    })
}
fn wait_ready(record: &Installed) -> Result<(), Error> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if status(record)?.state == "running" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(usage("installed server did not become ready")
                .with_hint("Run `plasmite --dir DIR serve logs` to inspect startup errors."));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn result_path(request: &Request) -> Result<PathBuf, Error> {
    if request.home != home()?
        || request.result.len() != 32
        || !request.result.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(usage("invalid service operation result path"));
    }
    Ok(request
        .home
        .join(".plasmite/service-results")
        .join(format!("{}.json", request.result)))
}
fn invoke(request: &Request) -> Result<(), Error> {
    if unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() } != 0 {
        return admin(request);
    }
    // The complete request travels in argv, not a mutable staging script/file.
    let bytes = serde_json::to_vec(request)
        .map_err(|err| usage("failed to encode service request").with_source(err))?;
    use base64::Engine;
    let parameters = wide(&format!(
        "__plasmite_admin {}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ));
    let program = path_wide(
        &std::env::current_exe()
            .map_err(|err| io_error("failed to find Plasmite", &request.source, err))?,
    );
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: program.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: 0,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(win_error("Windows administrator approval did not complete"));
    }
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    unsafe {
        WaitForSingleObject(process.as_raw_handle(), u32::MAX);
    }
    let mut code = 1;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(win_error("failed to read installation result"));
    }
    let path = result_path(request)?;
    let message = access_store::read_json::<Option<String>>(&path)
        .ok()
        .flatten();
    if let Err(error) = fs::remove_file(&path) {
        if error.kind() != io::ErrorKind::NotFound {
            eprintln!(
                "could not remove service operation details at {}: {error}",
                path.display()
            );
        }
    }
    if code != 0 {
        return Err(usage(
            message
                .as_deref()
                .unwrap_or("Windows service operation failed"),
        ));
    }
    Ok(())
}
fn request(pool: &Path, action: &str, options: ServeRunArgs) -> Result<Request, Error> {
    let original = pool;
    let pool = fs::canonicalize(pool)
        .map_err(|err| io_error("failed to resolve pool directory", pool, err))?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|err| usage(&format!("failed to identify service operation: {err}")))?;
    Ok(Request {
        result: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        action: action.into(),
        pool_dir: pool,
        owner: current_sid()?,
        home: home()?,
        source: std::env::current_exe()
            .map_err(|err| io_error("failed to find Plasmite", original, err))?,
        options,
    })
}
pub(crate) fn install(pool: &Path, options: &ServeRunArgs) -> Result<Status, Error> {
    if !pool.exists() {
        io_result(windows_private::create_dir_all(pool), pool)?;
    }
    io_result(windows_private::ensure_owner(pool), pool)?;
    let request = request(pool, "install", options.clone())?;
    let previous = load(&request.pool_dir)?;
    let run = merge(previous.as_ref().map(|p| &p.setup), options)?;
    if run
        .bind
        .as_deref()
        .unwrap()
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .port()
        == 0
        || run
            .remote_bind
            .as_deref()
            .unwrap()
            .parse::<std::net::SocketAddr>()
            .unwrap()
            .port()
            == 0
    {
        return Err(usage("installed servers require fixed listener ports"));
    }
    invoke(&request)?;
    let record = required(&request.pool_dir)?;
    wait_ready(&record)?;
    status(&record)
}
pub(crate) fn control(pool: &Path, action: &str) -> Result<Status, Error> {
    let request = request(pool, action, ServeRunArgs::default())?;
    let record = required(&request.pool_dir)?;
    let lock_path = request.pool_dir.join(".plasmite-serve/service.lock");
    let lock = io_result(shared(&record)?.open_lock(&lock_path), &lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|err| io_error("another service operation is in progress", &lock_path, err))?;
    if action == "start" || action == "stop" || action == "restart" {
        let service = verify_service(&record, SERVICE_START | SERVICE_STOP)?;
        if action != "start" {
            stop(&service)?;
        }
        if action != "stop" {
            start(&service)?;
            wait_ready(&record)?;
        }
        return status(&record);
    }
    drop(lock);
    invoke(&request)?;
    Ok(Status {
        pool_dir: request.pool_dir,
        pid: None,
        local_url: format!("http://{}", record.setup.run.bind.as_deref().unwrap()),
        remote_url: record.setup.run.server,
        managed: false,
        startup: false,
        state: "uninstalled".into(),
        problem: None,
        setup: None,
    })
}
pub(crate) fn all() -> Result<(Vec<Status>, Vec<Error>), Error> {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for record in installations()? {
        match record.and_then(|record| status(&record)) {
            Ok(row) => rows.push(row),
            Err(error) => errors.push(error),
        }
    }
    for server in serve_registry::running()? {
        if !rows.iter().any(|r| r.pool_dir == server.pool_dir) {
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
    rows.sort_by(|a, b| a.pool_dir.cmp(&b.pool_dir));
    Ok((rows, errors))
}
fn installations() -> Result<Vec<Result<Installed, Error>>, Error> {
    let root = root()?;
    let mut records = Vec::new();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(records),
        Err(err) => return Err(io_error("failed to list Windows services", &root, err)),
    };
    let owner = current_sid()?;
    for entry in entries {
        let path = entry
            .map_err(|err| io_error("failed to inspect Windows service", &root, err))?
            .path()
            .join("setup.json");
        let Ok(bytes) = windows_private::read_installed(&path) else {
            continue;
        };
        let record = serde_json::from_slice::<Installed>(&bytes)
            .map_err(|err| usage("invalid installed Windows service").with_source(err));
        match record {
            Ok(record) if record.owner == owner || record.service_sid == owner => {
                records.push(required(&record.setup.pool_dir))
            }
            Ok(_) => (),
            Err(error) => records.push(Err(error.with_path(path))),
        }
    }
    Ok(records)
}
fn managed_running(record: &Installed) -> Result<Vec<serve_registry::ServerDetails>, Error> {
    serve_registry::running_in(&runtime_dir(&record.setup.pool_dir).join("registry"))
}
pub(crate) fn running() -> Result<Vec<serve_registry::ServerDetails>, Error> {
    let mut servers = Vec::new();
    for record in installations()?.into_iter().flatten() {
        servers.extend(managed_running(&record)?);
    }
    Ok(servers)
}
pub(crate) fn registry_directory() -> Option<PathBuf> {
    HOST.get()
        .map(|record| runtime_dir(&record.setup.pool_dir).join("registry"))
}
pub(crate) fn logs(pool: &Path, tail: usize, follow: bool, json: bool) -> Result<(), Error> {
    let record = required(
        &fs::canonicalize(pool)
            .map_err(|err| io_error("failed to resolve pool directory", pool, err))?,
    )?;
    let path = runtime_dir(&record.setup.pool_dir).join("server.log");
    let policy = shared(&record)?;
    let bytes = io_result(policy.read(&path), &path)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    for line in &lines[lines.len().saturating_sub(tail)..] {
        log_line(&mut stdout, line, json)?;
    }
    if follow {
        let mut offset = bytes.len() as u64;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let file = io_result(policy.open_lock(&path), &path)?;
            let mut reader = io::BufReader::new(file);
            if reader
                .get_ref()
                .metadata()
                .map_err(|err| io_error("failed to inspect service log", &path, err))?
                .len()
                < offset
            {
                offset = 0;
            }
            reader
                .seek(io::SeekFrom::Start(offset))
                .map_err(|err| io_error("failed to follow service log", &path, err))?;
            for line in BufRead::lines(&mut reader) {
                log_line(
                    &mut stdout,
                    &line.map_err(|err| io_error("failed to read service log", &path, err))?,
                    json,
                )?;
            }
            offset = reader
                .stream_position()
                .map_err(|err| io_error("failed to follow service log", &path, err))?;
        }
    }
    Ok(())
}
fn log_line(stdout: &mut impl Write, line: &str, json: bool) -> Result<(), Error> {
    let text = if json {
        serde_json::json!({"message":line}).to_string()
    } else {
        line.into()
    };
    writeln!(stdout, "{text}")
        .and_then(|()| stdout.flush())
        .map_err(|err| win_error("failed to write service log").with_source(err))
}

pub(crate) async fn shutdown_requested() {
    let notify = SHUTDOWN.get_or_init(tokio::sync::Notify::new);
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if STOP.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}
fn startup_error(error: &Error, log: Option<&mut File>) {
    let mut details = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(error) = source {
        details.push_str(": ");
        details.push_str(&error.to_string());
        source = error.source();
    }
    // Before log access, sources only describe protected setup and OS access.
    // Runtime errors may contain private details; keep their chain in the
    // private log and emit only the outer error if writing that log fails.
    let public_details = if log.is_none() {
        details.clone()
    } else {
        error.to_string()
    };
    if log.is_some_and(|log| {
        writeln!(log, "{details}")
            .and_then(|()| log.flush())
            .is_ok()
    }) {
        return;
    }
    // A log permission failure must remain visible without weakening its ACL.
    // Windows accepts unregistered sources in the Application event log.
    use windows_sys::Win32::System::EventLog::{
        DeregisterEventSource, EVENTLOG_ERROR_TYPE, RegisterEventSourceW, ReportEventW,
    };
    let name = wide("Plasmite");
    let handle = unsafe { RegisterEventSourceW(std::ptr::null(), name.as_ptr()) };
    if !handle.is_null() {
        let details = wide(&public_details);
        unsafe {
            ReportEventW(
                handle,
                EVENTLOG_ERROR_TYPE,
                0,
                1,
                std::ptr::null_mut(),
                1,
                0,
                &details.as_ptr(),
                std::ptr::null(),
            );
            DeregisterEventSource(handle);
        }
    }
}
fn report(state: u32, error: u32) {
    let handle = STATUS_HANDLE.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: if error == 0 {
            0
        } else {
            windows_sys::Win32::Foundation::ERROR_SERVICE_SPECIFIC_ERROR
        },
        dwServiceSpecificExitCode: error,
        dwWaitHint: if state == SERVICE_STOP_PENDING {
            30_000
        } else {
            0
        },
        ..Default::default()
    };
    unsafe {
        SetServiceStatus(handle, &status);
    }
}
unsafe extern "system" fn handler(control: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        report(SERVICE_STOP_PENDING, 0);
        STOP.store(true, Ordering::Release);
        SHUTDOWN
            .get_or_init(tokio::sync::Notify::new)
            .notify_waiters();
    }
    0
}
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Some(record) = HOST.get() else {
        return;
    };
    let name = wide(&service_name(&record.setup.pool_dir));
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), std::ptr::null()) };
    if handle.is_null() {
        startup_error(
            &win_error("failed to register Windows service controls"),
            None,
        );
        return;
    }
    STATUS_HANDLE.store(handle as usize, Ordering::Release);
    let mut retained_log = None;
    let result = (|| {
        let path = runtime_dir(&record.setup.pool_dir).join("server.log");
        let log = io_result(shared(record)?.open_lock(&path), &path)?;
        use windows_sys::Win32::System::Console::{
            STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        };
        unsafe {
            SetStdHandle(STD_ERROR_HANDLE, log.as_raw_handle());
            SetStdHandle(STD_OUTPUT_HANDLE, log.as_raw_handle());
        }
        // Append, rather than overwriting the previous process's useful evidence.
        let mut log = log;
        log.seek(io::SeekFrom::End(0))
            .map_err(|err| io_error("failed to append service log", &path, err))?;
        retained_log = Some(log);
        report(SERVICE_RUNNING, 0);
        crate::secure_serve::run(&record.setup.pool_dir, &record.setup.run)
    })();
    if let Err(error) = &result {
        startup_error(error, retained_log.as_mut());
    }
    report(
        SERVICE_STOPPED,
        if result.is_ok() || STOP.load(Ordering::Acquire) {
            0
        } else {
            1
        },
    );
}

pub(crate) fn entry() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next()?;
    if mode == "__plasmite_admin" {
        let result = (|| {
            use base64::Engine;
            let encoded = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| usage("missing service request"))?;
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|err| usage("invalid service request").with_source(err))?;
            let request: Request = serde_json::from_slice(&bytes)
                .map_err(|err| usage("invalid service request").with_source(err))?;
            let path = result_path(&request)?;
            access_store::create_private_dir(path.parent().unwrap())?;
            let outcome = admin(&request);
            if let Err(error) = access_store::write_atomic_json(
                &path,
                &outcome.as_ref().err().map(|error| error.to_string()),
            ) {
                eprintln!("could not write service operation details: {error}");
            }
            outcome
        })();
        if let Err(error) = &result {
            eprintln!("{error}");
        }
        return Some(if result.is_ok() { 0 } else { 1 });
    }
    if mode != "__plasmite_service" {
        return None;
    }
    let result = (|| {
        let path = args
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| usage("missing service setup"))?;
        let bytes = io_result(windows_private::read_installed(&path), &path)?;
        let record: Installed = serde_json::from_slice(&bytes)
            .map_err(|err| usage("invalid service setup").with_source(err))?;
        if current_sid()? != record.service_sid || path != manifest(&record.setup.pool_dir)? {
            return Err(usage("service process does not match installed account"));
        }
        let record = required(&record.setup.pool_dir)?;
        HOST.set(record.clone())
            .map_err(|_| usage("service host already initialized"))?;
        let mut name = wide(&service_name(&record.setup.pool_dir));
        let table = [
            SERVICE_TABLE_ENTRYW {
                lpServiceName: name.as_mut_ptr(),
                lpServiceProc: Some(service_main),
            },
            SERVICE_TABLE_ENTRYW::default(),
        ];
        if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
            return Err(win_error("failed to connect service host to Windows"));
        }
        Ok(())
    })();
    if let Err(error) = &result {
        startup_error(error, None);
    }
    Some(if result.is_ok() { 0 } else { 1 })
}

fn protected(path: &Path, owner: Option<&str>, service: Option<&str>) -> Result<(), Error> {
    let readers = owner
        .into_iter()
        .chain(service)
        .map(|sid| format!("(A;OICI;FRFX;;;{sid})"))
        .collect::<String>();
    let root_read = if owner.is_none() {
        "(A;OICI;FRFX;;;BU)"
    } else {
        ""
    };
    io_result(
        windows_private::set_descriptor(
            path,
            &format!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){readers}{root_read}"),
            true,
        ),
        path,
    )
}
fn create_install_directory(
    path: &Path,
    owner: Option<&str>,
    service: Option<&str>,
) -> Result<(), Error> {
    if path.exists() {
        io_result(windows_private::read_installed(path), path)?;
    } else {
        fs::create_dir(path)
            .map_err(|err| io_error("failed to create installation directory", path, err))?;
    }
    protected(path, owner, service)
}
fn save(record: &Installed) -> Result<(), Error> {
    let path = manifest(&record.setup.pool_dir)?;
    let temporary = path.with_extension("new.json");
    let bytes = serde_json::to_vec(record)
        .map_err(|err| usage("failed to encode service setup").with_source(err))?;
    let mut file = File::create(&temporary)
        .map_err(|err| io_error("failed to stage service setup", &temporary, err))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| io_error("failed to save service setup", &temporary, err))?;
    drop(file);
    protected(&temporary, Some(&record.owner), Some(&record.service_sid))?;
    replace_installed(&temporary, &path)
}
fn replace_installed(source: &Path, destination: &Path) -> Result<(), Error> {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    if unsafe {
        MoveFileExW(
            path_wide(source).as_ptr(),
            path_wide(destination).as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(win_error("failed to replace installed service file"))
    } else {
        Ok(())
    }
}
fn service_permissions(service: &Service, owner: &str, sid: &str) -> Result<(), Error> {
    let text = wide(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;CCLCRPWPLOCRRC;;;{owner})(A;;CCLCRC;;;{sid})"
    ));
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(win_error("failed to prepare service permissions"));
    }
    let mut acl = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    let result =
        if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            == 0
        {
            Err(win_error("failed to read service permissions"))
        } else {
            let status = unsafe {
                SetSecurityInfo(
                    service.0,
                    SE_SERVICE,
                    DACL_SECURITY_INFORMATION
                        | windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    acl,
                    std::ptr::null(),
                )
            };
            if status == 0 {
                Ok(())
            } else {
                Err(usage("failed to protect Windows service")
                    .with_source(io::Error::from_raw_os_error(status as i32)))
            }
        };
    unsafe {
        LocalFree(descriptor);
    }
    result
}
fn set_recovery(service: &Service, enabled: bool) -> Result<(), Error> {
    let mut actions = [
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 2_000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5_000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 10_000,
        },
    ];
    let config = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: if enabled { 60 } else { 0 },
        cActions: if enabled { actions.len() as u32 } else { 0 },
        // A null pointer leaves existing actions unchanged; zero actions with
        // this non-null pointer removes them during candidate validation.
        lpsaActions: actions.as_mut_ptr(),
        ..Default::default()
    };
    let failures = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: 1,
    };
    if enabled
        && unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
                (&failures as *const SERVICE_FAILURE_ACTIONS_FLAG).cast(),
            )
        } == 0
    {
        return Err(win_error("failed to configure service crash recovery"));
    }
    if unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            (&config as *const SERVICE_FAILURE_ACTIONSW).cast(),
        )
    } == 0
    {
        return Err(win_error("failed to configure service crash recovery"));
    }
    Ok(())
}
fn create_service(setup: &Setup) -> Result<Service, Error> {
    let manager = manager(SC_MANAGER_CREATE_SERVICE)?;
    let name = wide(&service_name(&setup.pool_dir));
    let program = wide(&command(setup)?);
    let user = wide(&setup.account);
    let handle = unsafe {
        CreateServiceW(
            manager.0,
            name.as_ptr(),
            name.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            // Startup begins only after the protected image and setup exist.
            SERVICE_DISABLED,
            SERVICE_ERROR_NORMAL,
            program.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            user.as_ptr(),
            std::ptr::null(),
        )
    };
    if handle.is_null() {
        Err(win_error("failed to register Windows service"))
    } else {
        Ok(Service(handle))
    }
}
fn walk(path: &Path, visit: &mut impl FnMut(&Path) -> Result<(), Error>) -> Result<(), Error> {
    use std::os::windows::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)
        .map_err(|err| io_error("failed to inspect service permissions", path, err))?;
    if metadata.file_attributes()
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
        != 0
    {
        return Err(usage("service paths cannot contain reparse points").with_path(path));
    }
    visit(path)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|err| io_error("failed to inspect service directory", path, err))?
        {
            walk(
                &entry
                    .map_err(|err| io_error("failed to inspect service file", path, err))?
                    .path(),
                visit,
            )?;
        }
    }
    Ok(())
}
fn grant_state(record: &Installed) -> Result<(), Error> {
    let policy = shared(record)?;
    walk(
        &record.setup.pool_dir.join(".plasmite-serve"),
        &mut |path| io_result(policy.grant(path), path),
    )?;
    let runtime = runtime_dir(&record.setup.pool_dir);
    io_result(policy.create_dir_all(&runtime.join("registry")), &runtime)?;
    io_result(policy.open_lock(&runtime.join("server.log")), &runtime)?;
    pool_access(record, true)
}
fn ancestor_access(record: &Installed, grant: bool) -> Result<(), Error> {
    let policy = shared(record)?;
    for path in record
        .setup
        .pool_dir
        .ancestors()
        .skip(1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        io_result(policy.ancestor_access(path, grant), path)?;
    }
    Ok(())
}
fn pool_access(record: &Installed, grant: bool) -> Result<(), Error> {
    let policy = shared(record)?;
    io_result(
        policy.pool_access(&record.setup.pool_dir, grant),
        &record.setup.pool_dir,
    )?;
    for entry in fs::read_dir(&record.setup.pool_dir)
        .map_err(|err| io_error("failed to inspect pools", &record.setup.pool_dir, err))?
    {
        let path = entry
            .map_err(|err| io_error("failed to inspect pool", &record.setup.pool_dir, err))?
            .path();
        if path.extension().is_some_and(|ext| ext == "plasmite") {
            io_result(policy.pool_access(&path, grant), &path)?;
        }
    }
    Ok(())
}
fn import_tls(
    record: &mut Installed,
    previous: Option<&Installed>,
    generation: &str,
    retained: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    let policy = shared(record)?;
    let runtime = runtime_dir(&record.setup.pool_dir);
    for (field, previous_path, name) in [
        (
            &mut record.setup.run.tls_cert,
            previous.and_then(|p| p.setup.run.tls_cert.as_ref()),
            "cert.pem",
        ),
        (
            &mut record.setup.run.tls_key,
            previous.and_then(|p| p.setup.run.tls_key.as_ref()),
            "key.pem",
        ),
        (
            &mut record.setup.run.front_cert,
            previous.and_then(|p| p.setup.run.front_cert.as_ref()),
            "front.pem",
        ),
    ] {
        if let Some(original) = field {
            if previous_path == Some(&*original) {
                continue;
            }
            let path = runtime.join(format!("tls-{generation}-{name}"));
            let bytes = fs::read(&*original)
                .map_err(|err| io_error("failed to read supplied TLS material", original, err))?;
            let mut file = io_result(policy.create_file(&path), &path)?;
            retained.push(path.clone());
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|err| io_error("failed to retain TLS material", &path, err))?;
            *original = path;
        }
    }
    Ok(())
}
fn restore_identity(record: &Installed, bytes: &[u8]) -> Result<(), Error> {
    let policy = shared(record)?;
    let state = record.setup.pool_dir.join(".plasmite-serve");
    let path = state.join("identity.json");
    let lock_path = state.join("lock");
    let lock = io_result(policy.open_lock(&lock_path), &lock_path)?;
    lock.try_lock_exclusive()
        .map_err(|err| io_error("another server owns this pool directory", &lock_path, err))?;
    let temporary = state.join("identity.restore.json");
    let mut file = io_result(policy.create_file(&temporary), &temporary)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| io_error("failed to restore server identity", &temporary, err))?;
    drop(file);
    io_result(policy.replace(&temporary, &path), &path)
}
fn admin(request: &Request) -> Result<(), Error> {
    if current_sid()? != request.owner {
        return Err(usage(
            "approve service installation as the pool-owning account",
        ));
    }
    let pool = fs::canonicalize(&request.pool_dir)
        .map_err(|err| io_error("failed to resolve pool directory", &request.pool_dir, err))?;
    if pool != request.pool_dir {
        return Err(usage("service request must use a canonical pool directory"));
    }
    io_result(windows_private::ensure_owner(&pool), &pool)?;
    let _restore = RestorePrivilege::enable()?;
    let previous = load(&pool)?;
    if request.action == "install" && previous.is_none() {
        if open(&pool, SERVICE_QUERY_CONFIG, false)?.is_some() {
            return Err(usage(
                "a foreign Windows service already uses this pool's service name",
            ));
        }
        let path = installation(&pool)?;
        if path.exists() {
            return Err(usage("unrecognized service installation directory").with_path(&path));
        }
    }
    if request.action != "install" && request.action != "uninstall" {
        return Err(usage("unknown Windows service installation operation"));
    }
    let state = pool.join(".plasmite-serve");
    if previous.is_none() {
        access_store::create_private_dir(&state)?;
    }
    let lock_path = state.join("service.lock");
    let lock = match previous.as_ref() {
        Some(record) => io_result(shared(record)?.open_lock(&lock_path), &lock_path)?,
        None => io_result(windows_private::open_lock(&lock_path), &lock_path)?,
    };
    lock.try_lock_exclusive()
        .map_err(|err| io_error("another service operation is in progress", &lock_path, err))?;
    if request.action == "uninstall" {
        let record =
            previous.ok_or_else(|| usage("this pool directory has no installed server"))?;
        let service = open(&pool, SERVICE_ALL_ACCESS, true)?
            .map(|_| verify_service(&record, SERVICE_ALL_ACCESS))
            .transpose()?;
        if let Some(service) = &service {
            stop(service)?;
        }
        let policy = shared(&record)?;
        pool_access(&record, false)?;
        walk(&state, &mut |path| io_result(policy.revoke(path), path))?;
        ancestor_access(&record, false)?;
        if let Some(service) = service {
            if unsafe { DeleteService(service.0) } == 0 {
                return Err(win_error("failed to remove Windows service"));
            }
        }
        // Keep the authenticated manifest until executable cleanup succeeds,
        // so a retry can finish after native service deletion.
        let instance = installation(&pool)?;
        for name in [
            "plasmite.exe",
            "plasmite.new.exe",
            "plasmite.previous.exe",
            "setup.previous.json",
            "identity.previous.json",
            "setup.new.json",
        ] {
            let path = instance.join(name);
            match fs::remove_file(&path) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => {
                    return Err(io_error(
                        "service stopped; executable cleanup remains",
                        &path,
                        error,
                    )
                    .with_hint("Retry `serve uninstall` to finish cleanup."));
                }
            }
        }
        fs::remove_file(manifest(&pool)?)
            .map_err(|err| io_error("service stopped; setup cleanup remains", &pool, err))?;
        fs::remove_dir(&instance).map_err(|err| io_error("service stopped; installation directory cleanup remains", &instance, err)
            .with_hint(format!("In an administrator terminal, inspect and remove the empty directory {} before reinstalling.", instance.display())))?;
        return Ok(());
    }
    if previous.is_some() && open(&pool, SERVICE_QUERY_CONFIG, false)?.is_none() {
        return Err(usage("the installed Windows service is missing")
            .with_hint("Run `serve uninstall` to finish cleanup, then reinstall."));
    }
    let run = merge(previous.as_ref().map(|p| &p.setup), &request.options)?;
    if previous.is_none()
        && serve_registry::running()?
            .iter()
            .any(|s| s.pool_dir == pool)
    {
        return Err(
            usage("a foreground server already owns this pool directory")
                .with_hint("Stop that server with Ctrl+C, then retry `serve install`."),
        );
    }
    let source = fs::canonicalize(&request.source).map_err(|err| {
        io_error(
            "failed to inspect candidate executable",
            &request.source,
            err,
        )
    })?;
    if source
        != fs::canonicalize(
            std::env::current_exe()
                .map_err(|err| io_error("failed to locate installer", &source, err))?,
        )
        .map_err(|err| io_error("failed to locate installer", &source, err))?
    {
        return Err(usage("candidate must be the approved installer executable"));
    }
    let candidate = std::process::Command::new(&source)
        .arg("--version")
        .output()
        .map_err(|err| io_error("candidate executable cannot run", &source, err))?;
    if !candidate.status.success() || !candidate.stdout.starts_with(b"plasmite ") {
        return Err(usage("candidate is not a working Plasmite executable"));
    }
    let program_root = root()?;
    let parent = program_root.parent().unwrap();
    create_install_directory(parent, None, None)?;
    create_install_directory(&program_root, None, None)?;
    let instance = installation(&pool)?;
    let setup = Setup {
        pool_dir: pool.clone(),
        program: instance.join("plasmite.exe"),
        account: account(&pool),
        home: request.home.clone(),
        run: run.clone(),
    };
    let staging = instance.join("plasmite.new.exe");
    let backup = instance.join("plasmite.previous.exe");
    let settings_backup = instance.join("setup.previous.json");
    let identity_backup = instance.join("identity.previous.json");
    if backup.exists() || settings_backup.exists() || identity_backup.exists() {
        return Err(usage("a previous service update needs recovery").with_path(&instance));
    }
    let staged: Result<Service, Error> = (|| {
        // A new instance root starts administrator-only. Read access comes after the
        // virtual account exists and its SID can be resolved.
        create_install_directory(
            &instance,
            previous.as_ref().map(|p| p.owner.as_str()),
            previous.as_ref().map(|p| p.service_sid.as_str()),
        )?;
        fs::copy(&source, &staging)
            .map_err(|err| io_error("failed to stage candidate executable", &staging, err))?;
        protected(
            &staging,
            Some(&request.owner),
            previous.as_ref().map(|p| p.service_sid.as_str()),
        )?;
        let service = if let Some(record) = &previous {
            verify_service(record, SERVICE_ALL_ACCESS)?
        } else {
            create_service(&setup)?
        };
        Ok(service)
    })();
    let service = match staged {
        Ok(service) => service,
        Err(error) => {
            if staging.exists() {
                fs::remove_file(&staging).map_err(|err| {
                    io_error("failed to remove service staging file", &staging, err)
                })?;
            }
            if previous.is_none() && instance.exists() {
                fs::remove_dir(&instance).map_err(|err| {
                    io_error(
                        "failed to remove service preparation directory",
                        &instance,
                        err,
                    )
                })?;
            }
            return Err(error);
        }
    };
    let prepared: Result<_, Error> = (|| {
        let sid = lookup_sid(&setup.account)?;
        let record = Installed {
            setup,
            owner: request.owner.clone(),
            service_sid: sid,
        };
        service_permissions(&service, &record.owner, &record.service_sid)?;
        protected(&instance, Some(&record.owner), Some(&record.service_sid))?;
        protected(&staging, Some(&record.owner), Some(&record.service_sid))?;
        let was_running = query(&service)?.dwCurrentState != SERVICE_STOPPED;
        let previous_start_type = start_type(&service)?;
        if let Some(previous) = &previous {
            fs::copy(&previous.setup.program, &backup)
                .map_err(|err| io_error("failed to retain previous executable", &backup, err))?;
            protected(&backup, Some(&record.owner), Some(&record.service_sid))?;
            fs::copy(manifest(&pool)?, &settings_backup).map_err(|err| {
                io_error("failed to retain previous setup", &settings_backup, err)
            })?;
            protected(
                &settings_backup,
                Some(&record.owner),
                Some(&record.service_sid),
            )?;
        }
        let identity = state.join("identity.json");
        if identity.exists() {
            let bytes = if let Some(previous) = &previous {
                io_result(shared(previous)?.read(&identity), &identity)?
            } else {
                io_result(windows_private::read(&identity), &identity)?
            };
            fs::write(&identity_backup, bytes).map_err(|err| {
                io_error("failed to retain server identity", &identity_backup, err)
            })?;
            protected(
                &identity_backup,
                Some(&record.owner),
                Some(&record.service_sid),
            )?;
        }
        Ok((record, was_running, previous_start_type))
    })();
    let (mut record, was_running, previous_start_type) = match prepared {
        Ok(value) => value,
        Err(error) => {
            if previous.is_none() && unsafe { DeleteService(service.0) } == 0 {
                return Err(
                    usage("service preparation failed and cleanup needs attention")
                        .with_path(&instance)
                        .with_hint(error.to_string()),
                );
            }
            for path in [&staging, &backup, &settings_backup, &identity_backup] {
                if path.exists() {
                    fs::remove_file(path)
                        .map_err(|err| io_error("failed to remove preparation file", path, err))?;
                }
            }
            if previous.is_none() {
                fs::remove_dir(&instance).map_err(|err| {
                    io_error(
                        "failed to remove service preparation directory",
                        &instance,
                        err,
                    )
                })?;
            }
            return Err(error);
        }
    };
    let mut retained_tls = Vec::new();
    let mut granted_ancestors = Vec::new();
    let mut program_replaced = false;
    let mut setup_saved = false;
    let mut identity_may_change = false;
    let mut recovery_cleared = false;
    let mut startup_disabled = false;
    let result = (|| {
        // A failed candidate must not queue a restart that can undo rollback's
        // stopped state. Disable startup while replacing its executable too.
        set_recovery(&service, false)?;
        recovery_cleared = true;
        set_start_type(&service, SERVICE_DISABLED)?;
        startup_disabled = true;
        stop(&service)?;
        replace_installed(&staging, &record.setup.program)?;
        program_replaced = true;
        let initial_store = if previous.is_none() {
            identity_may_change = true;
            Some(access_store::AccessStore::open(
                &pool,
                record.setup.run.server.as_deref(),
                record
                    .setup
                    .run
                    .tls_cert
                    .as_deref()
                    .zip(record.setup.run.tls_key.as_deref()),
                record.setup.run.front_cert.as_deref(),
            )?)
        } else {
            None
        };
        let server_lock = if initial_store.is_none() {
            let path = state.join("lock");
            let lock = io_result(shared(&record)?.open_lock(&path), &path)?;
            lock.try_lock_exclusive()
                .map_err(|err| io_error("another server owns this pool directory", &path, err))?;
            Some(lock)
        } else {
            None
        };
        if previous.is_none() {
            let policy = shared(&record)?;
            for path in record.setup.pool_dir.ancestors().skip(1) {
                io_result(policy.ancestor_access(path, true), path)?;
                granted_ancestors.push(path.to_path_buf());
            }
        }
        grant_state(&record)?;
        import_tls(
            &mut record,
            previous.as_ref(),
            &request.result,
            &mut retained_tls,
        )?;
        save(&record)?;
        setup_saved = true;
        drop(server_lock);
        drop(initial_store);
        set_start_type(&service, SERVICE_AUTO_START)?;
        // StartService can time out after spawning the candidate process.
        identity_may_change = true;
        start(&service)?;
        wait_ready(&record)?;
        set_recovery(&service, true)
    })();
    if let Err(error) = &result {
        let recovery = (|| {
            if !program_replaced {
                // The old image and setup still govern the service. Compensate
                // only successful policy changes, even if stopping failed.
                if let Some(previous) = &previous {
                    let recovery = if recovery_cleared {
                        set_recovery(&service, true)
                    } else {
                        Ok(())
                    };
                    let startup = if startup_disabled && was_running {
                        // A disabled service may have been running before the
                        // update. Restore its boot policy after attempting start.
                        (|| {
                            set_start_type(&service, SERVICE_AUTO_START)?;
                            let restarted = start(&service);
                            let restored = set_start_type(&service, previous_start_type);
                            restarted?;
                            restored?;
                            wait_ready(previous)
                        })()
                    } else if startup_disabled {
                        set_start_type(&service, previous_start_type)
                    } else {
                        Ok(())
                    };
                    recovery?;
                    startup?;
                } else if unsafe { DeleteService(service.0) } == 0 {
                    return Err(win_error("failed to remove failed service"));
                }
                return Ok(());
            }
            set_start_type(&service, SERVICE_DISABLED)?;
            set_recovery(&service, false)?;
            stop(&service)?;
            // Restore only identity, never the access-key records committed by
            // another owner command while the candidate ran.
            if identity_may_change && identity_backup.exists() {
                let bytes = io_result(
                    windows_private::read_installed(&identity_backup),
                    &identity_backup,
                )?;
                restore_identity(&record, &bytes)?;
            }
            for path in &retained_tls {
                fs::remove_file(path).map_err(|err| {
                    io_error("failed to remove candidate TLS material", path, err)
                })?;
            }
            if let Some(previous) = &previous {
                if program_replaced {
                    replace_installed(&backup, &previous.setup.program)?;
                }
                if setup_saved {
                    replace_installed(&settings_backup, &manifest(&pool)?)?;
                }
                set_recovery(&service, true)?;
                // A previously running disabled service needs AUTO long enough
                // to start. Restore its boot policy even if start/readiness fails.
                if was_running {
                    set_start_type(&service, SERVICE_AUTO_START)?;
                }
                let restarted = if was_running { start(&service) } else { Ok(()) };
                set_start_type(&service, previous_start_type)?;
                restarted?;
                if was_running {
                    wait_ready(previous)?;
                }
            } else {
                let policy = shared(&record)?;
                pool_access(&record, false)?;
                walk(&state, &mut |path| io_result(policy.revoke(path), path))?;
                for path in granted_ancestors.iter().rev() {
                    io_result(policy.ancestor_access(path, false), path)?;
                }
                if unsafe { DeleteService(service.0) } == 0 {
                    return Err(win_error("failed to remove failed service"));
                }
                for path in [manifest(&pool)?, record.setup.program.clone()] {
                    if path.exists() {
                        fs::remove_file(&path).map_err(|err| {
                            io_error("failed to remove failed installation", &path, err)
                        })?;
                    }
                }
            }
            Ok(())
        })();
        if let Err(recovery) = recovery {
            return Err(usage("service update failed and recovery needs attention")
                .with_path(&instance)
                .with_hint(format!(
                    "Update: {error}. Recovery: {recovery}. Recovery files remain."
                )));
        }
    }
    for path in [staging, backup, settings_backup, identity_backup] {
        if path.exists() {
            fs::remove_file(&path)
                .map_err(|err| io_error("failed to remove service staging file", &path, err))?;
        }
    }
    if result.is_err() && previous.is_none() {
        fs::remove_dir(&instance)
            .map_err(|err| io_error("failed to remove failed service directory", &instance, err))?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_state_keeps_client_checks_private() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let held = windows_private::create_dir_all(&state)?;
        let path = state.join("identity.json");
        drop(windows_private::create_file(&path)?);
        let policy =
            windows_private::Shared::new(&windows_private::current_sid()?, "S-1-5-80-1-2-3-4-5")?;
        policy.grant(&state)?;
        policy.grant(&path)?;
        assert!(policy.read(&path).is_ok());
        assert_eq!(
            windows_private::read(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let staged = state.join("staged");
        let mut file = policy.create_file(&staged)?;
        file.write_all(b"updated")?;
        drop(file);
        policy.replace(&staged, &path)?;
        assert_eq!(policy.read(&path)?, b"updated");
        let widened = std::process::Command::new("icacls")
            .arg(&path)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()?;
        assert!(widened.status.success());
        assert_eq!(
            policy.read(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(held);
        Ok(())
    }
    #[test]
    fn service_command_quotes_windows_arguments() {
        assert_eq!(
            quote("C:\\pools with space\\"),
            "\"C:\\pools with space\\\\\""
        );
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("雪"), "\"雪\"");
    }
}
