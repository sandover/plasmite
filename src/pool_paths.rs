//! Purpose: Shared local pool-directory and pool-name path resolution helpers.
//! Exports: `default_pool_dir` and `resolve_named_pool_path`.
//! Role: Keep CLI and API-client path semantics aligned from one source.
//! Invariants: Default pool directory remains `~/.plasmite/pools`.
//! Invariants: Named pool refs must not contain path separators.

use std::path::{Path, PathBuf};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PoolNameResolveError {
    ContainsPathSeparator,
    InvalidName,
}

pub(crate) fn default_pool_dir() -> Option<PathBuf> {
    let home = resolve_home(std::env::var_os("HOME").map(PathBuf::from), account_home);
    home.map(|home| home.join(".plasmite").join("pools"))
}

fn resolve_home(
    home: Option<PathBuf>,
    account_home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(home) = home.filter(|path| path.is_absolute()) {
        return Some(home);
    }
    #[cfg(not(unix))]
    let _ = home;
    account_home().filter(|path| path.is_absolute())
}

#[cfg(unix)]
fn account_home() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    // getpwuid_r keeps account lookup independent of HOME and concurrent callers.
    let mut size = 1024;
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && size < 1024 * 1024 {
            size *= 2;
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        let directory = unsafe { (*result).pw_dir };
        if directory.is_null() {
            return None;
        }
        let bytes = unsafe { CStr::from_ptr(directory) }.to_bytes();
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
    }
}

#[cfg(windows)]
fn account_home() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath};
    if let Some(home) = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    {
        return Some(home);
    }
    let mut directory = std::ptr::null_mut();
    let status =
        unsafe { SHGetKnownFolderPath(&FOLDERID_Profile, 0, std::ptr::null_mut(), &mut directory) };
    if status < 0 || directory.is_null() {
        return None;
    }
    let mut length = 0;
    while unsafe { *directory.add(length) } != 0 {
        length += 1;
    }
    let home = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        std::slice::from_raw_parts(directory, length)
    }));
    unsafe { CoTaskMemFree(directory.cast()) };
    Some(home)
}

#[cfg(not(any(unix, windows)))]
fn account_home() -> Option<PathBuf> {
    None
}

pub(crate) fn resolve_named_pool_path(
    name: &str,
    pool_dir: &Path,
) -> Result<PathBuf, PoolNameResolveError> {
    if name.is_empty()
        || name.contains('\0')
        || Path::new(name)
            .components()
            .any(|part| matches!(part, std::path::Component::Prefix(_)))
    {
        return Err(PoolNameResolveError::InvalidName);
    }
    if name.chars().any(std::path::is_separator) {
        return Err(PoolNameResolveError::ContainsPathSeparator);
    }
    if name.ends_with(".plasmite") {
        return Ok(pool_dir.join(name));
    }
    Ok(pool_dir.join(format!("{name}.plasmite")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_resolution_requires_absolute_paths() {
        let account = std::env::current_dir().unwrap().join("account home 東京");
        for value in [None, Some(PathBuf::new()), Some(PathBuf::from("relative"))] {
            assert_eq!(
                resolve_home(value, || Some(account.clone())),
                Some(account.clone())
            );
        }
        assert_eq!(resolve_home(None, || None), None);
        assert_eq!(resolve_home(None, || Some(PathBuf::from("relative"))), None);
        #[cfg(unix)]
        assert_eq!(resolve_home(Some(account.clone()), || None), Some(account));
    }
    #[test]
    fn default_directory_environment_probe() {
        if let Some(expected) = std::env::var_os("PLASMITE_TEST_EXPECTED_POOL_DIRECTORY") {
            assert_eq!(default_pool_dir(), Some(PathBuf::from(expected)));
        }
    }

    #[test]
    fn default_directory_environment_matrix() {
        let account = account_home().expect("account home");
        let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let override_home = std::env::temp_dir().join("home space 東京");
        for value in [
            None,
            Some(PathBuf::new()),
            Some(PathBuf::from("relative")),
            Some(override_home.clone()),
        ] {
            let expected_home = value
                .as_ref()
                .filter(|home| home.is_absolute())
                .unwrap_or(&account);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args([
                "--exact",
                "pool_paths::tests::default_directory_environment_probe",
            ]);
            child.env(
                "PLASMITE_TEST_EXPECTED_POOL_DIRECTORY",
                expected_home.join(".plasmite").join("pools"),
            );
            if let Some(value) = value {
                child.env(key, value);
            } else {
                child.env_remove(key);
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
