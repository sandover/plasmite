use super::{AccessKey, encode_hex, key_from_payload, store_payload};
use crate::core::error::{Error, ErrorKind};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

const STORE_VERSION: u32 = 1;

#[cfg(windows)]
type DirectoryGuard = crate::windows_private::Directory;
#[cfg(not(windows))]
type DirectoryGuard = ();

#[derive(Serialize, Deserialize)]
struct CredentialFile {
    version: u32,
    connections: BTreeMap<String, String>,
}

pub(super) fn load(destination: &str) -> Result<Option<AccessKey>, Error> {
    let paths = StorePaths::new()?;
    if !paths.exists()? {
        return Ok(None);
    }
    #[cfg(windows)]
    let _directory = paths.prepare_dir()?;
    #[cfg(not(windows))]
    paths.prepare_dir()?;
    let lock = paths.open_lock()?;
    FileExt::lock_shared(&lock)
        .map_err(|error| store_io_error("failed to lock saved connections", error))?;
    let file = match paths.read_file()? {
        Some(file) => file,
        None => {
            let _ = FileExt::unlock(&lock);
            return Ok(None);
        }
    };
    if file.version != STORE_VERSION {
        let _ = FileExt::unlock(&lock);
        return Err(Error::new(ErrorKind::Corrupt)
            .with_message("saved connection file has an unsupported version"));
    }
    let encoded = file.connections.get(destination);
    let result = encoded
        .map(|value| decode_record(destination, value))
        .transpose();
    let _ = FileExt::unlock(&lock);
    result
}

/// Read destination names without decrypting saved credentials.
pub(super) fn list() -> Result<Vec<String>, Error> {
    let paths = StorePaths::new()?;
    if !paths.exists()? {
        return Ok(Vec::new());
    }
    #[cfg(windows)]
    let _directory = paths.prepare_dir()?;
    #[cfg(not(windows))]
    paths.prepare_dir()?;
    let lock = paths.open_lock()?;
    FileExt::lock_shared(&lock)
        .map_err(|error| store_io_error("failed to lock saved connections", error))?;
    let result = (|| {
        let Some(file) = paths.read_file()? else {
            return Ok(Vec::new());
        };
        if file.version != STORE_VERSION {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("saved connection file has an unsupported version"));
        }
        Ok(file.connections.into_keys().collect())
    })();
    let _ = FileExt::unlock(&lock);
    result
}

pub(super) fn save(destination: &str, key: &AccessKey) -> Result<(), Error> {
    let paths = StorePaths::new()?;
    #[cfg(windows)]
    let _directory = paths.prepare_dir()?;
    #[cfg(not(windows))]
    paths.prepare_dir()?;
    let lock = paths.open_lock()?;
    FileExt::lock_exclusive(&lock)
        .map_err(|error| store_io_error("failed to lock saved connections", error))?;
    let result = (|| {
        let mut file = paths.read_file()?.unwrap_or(CredentialFile {
            version: STORE_VERSION,
            connections: BTreeMap::new(),
        });
        if file.version != STORE_VERSION {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("saved connection file has an unsupported version"));
        }
        file.connections
            .insert(destination.to_owned(), encode_record(destination, key)?);
        paths.write_file(&file)
    })();
    let _ = FileExt::unlock(&lock);
    result
}

pub(super) fn remove(destination: &str) -> Result<(), Error> {
    let paths = StorePaths::new()?;
    if !paths.exists()? {
        return Ok(());
    }
    #[cfg(windows)]
    let _directory = paths.prepare_dir()?;
    #[cfg(not(windows))]
    paths.prepare_dir()?;
    let lock = paths.open_lock()?;
    FileExt::lock_exclusive(&lock)
        .map_err(|error| store_io_error("failed to lock saved connections", error))?;
    let result = (|| {
        let Some(mut file) = paths.read_file()? else {
            return Ok(());
        };
        if file.version != STORE_VERSION {
            return Err(Error::new(ErrorKind::Corrupt)
                .with_message("saved connection file has an unsupported version"));
        }
        if file.connections.remove(destination).is_some() {
            paths.write_file(&file)?;
        }
        Ok(())
    })();
    let _ = FileExt::unlock(&lock);
    result
}

fn encode_record(destination: &str, key: &AccessKey) -> Result<String, Error> {
    let payload = store_payload(destination, key);
    let protected = protect(&payload)?;
    Ok(encode_hex(&protected))
}

fn decode_record(destination: &str, value: &str) -> Result<AccessKey, Error> {
    let encoded = super::decode_hex(value).ok_or_else(|| {
        Error::new(ErrorKind::Corrupt).with_message("saved connection is invalid")
    })?;
    let payload = unprotect(&encoded)?;
    key_from_payload(destination, &payload)
}

struct StorePaths {
    dir: PathBuf,
    file: PathBuf,
    lock: PathBuf,
}

impl StorePaths {
    fn exists(&self) -> Result<bool, Error> {
        match fs::symlink_metadata(&self.dir) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(store_io_error(
                "failed to inspect saved connection directory",
                error,
            )),
        }
    }

    fn new() -> Result<Self, Error> {
        let dir = if let Some(path) = std::env::var_os("PLASMITE_ACCESS_HOME") {
            if path.is_empty() {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("PLASMITE_ACCESS_HOME must name a directory"));
            }
            PathBuf::from(path)
        } else {
            default_store_dir()?
        };
        Ok(Self {
            file: dir.join("connections.json"),
            lock: dir.join("connections.lock"),
            dir,
        })
    }

    fn prepare_dir(&self) -> Result<DirectoryGuard, Error> {
        #[cfg(windows)]
        let directory = crate::windows_private::create_dir_all(&self.dir).map_err(|error| {
            store_io_error("failed to create private saved connection directory", error)
        })?;
        #[cfg(not(windows))]
        fs::create_dir_all(&self.dir).map_err(|error| {
            store_io_error("failed to create saved connection directory", error)
        })?;
        let metadata = fs::symlink_metadata(&self.dir).map_err(|error| {
            store_io_error("failed to inspect saved connection directory", error)
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::new(ErrorKind::Permission)
                .with_message("saved connection path must be a real directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(Error::new(ErrorKind::Permission)
                    .with_message("saved connection directory is owned by another user"));
            }
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
                store_io_error("failed to protect saved connection directory", error)
            })?;
        }
        #[cfg(windows)]
        {
            Ok(directory)
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }

    fn open_lock(&self) -> Result<File, Error> {
        if let Ok(metadata) = fs::symlink_metadata(&self.lock) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(Error::new(ErrorKind::Permission)
                    .with_message("saved connection lock must be a regular file"));
            }
        }
        #[cfg(windows)]
        let opened = crate::windows_private::open_lock(&self.lock);
        #[cfg(not(windows))]
        let opened = {
            let mut options = OpenOptions::new();
            options.create(true).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(&self.lock)
        };
        let file = opened
            .map_err(|error| store_io_error("failed to open saved connection lock", error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = file.metadata().map_err(|error| {
                store_io_error("failed to inspect saved connection lock", error)
            })?;
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(Error::new(ErrorKind::Permission)
                    .with_message("saved connection lock is owned by another user"));
            }
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    store_io_error("failed to protect saved connection lock", error)
                })?;
        }
        Ok(file)
    }

    fn read_file(&self) -> Result<Option<CredentialFile>, Error> {
        let metadata = match fs::symlink_metadata(&self.file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(store_io_error("failed to inspect saved connections", error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::new(ErrorKind::Permission)
                .with_message("saved connection file must be a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(Error::new(ErrorKind::Permission)
                    .with_message("saved connection file is owned by another user"));
            }
            fs::set_permissions(&self.file, fs::Permissions::from_mode(0o600))
                .map_err(|error| store_io_error("failed to protect saved connections", error))?;
        }
        #[cfg(windows)]
        let read = crate::windows_private::read(&self.file);
        #[cfg(not(windows))]
        let read = fs::read(&self.file);
        let bytes =
            read.map_err(|error| store_io_error("failed to read saved connections", error))?;
        let file: CredentialFile = serde_json::from_slice(&bytes).map_err(|error| {
            Error::new(ErrorKind::Corrupt)
                .with_message("saved connection file is invalid")
                .with_source(error)
        })?;
        Ok(Some(file))
    }

    fn write_file(&self, file: &CredentialFile) -> Result<(), Error> {
        let data = serde_json::to_vec(file).map_err(|error| {
            Error::new(ErrorKind::Internal)
                .with_message("failed to encode saved connections")
                .with_source(error)
        })?;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message(format!("failed to create saved connection file: {error}"))
        })?;
        let temp = self.dir.join(format!(
            "connections.{}.{}.tmp",
            std::process::id(),
            encode_hex(&random)
        ));
        #[cfg(windows)]
        let created = crate::windows_private::create_file(&temp);
        #[cfg(not(windows))]
        let created = {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(&temp)
        };
        let result = (|| {
            let mut output = created
                .map_err(|error| store_io_error("failed to create saved connection file", error))?;
            output
                .write_all(&data)
                .map_err(|error| store_io_error("failed to write saved connections", error))?;
            output
                .sync_all()
                .map_err(|error| store_io_error("failed to flush saved connections", error))?;
            drop(output);
            replace_file(&temp, &self.file)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(target_os = "macos")]
fn default_store_dir() -> Result<PathBuf, Error> {
    home_dir().map(|home| home.join("Library/Application Support/Plasmite"))
}

#[cfg(windows)]
fn default_store_dir() -> Result<PathBuf, Error> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Plasmite"))
        .ok_or_else(|| {
            Error::new(ErrorKind::Io)
                .with_message("cannot find the current user's application data directory")
        })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_store_dir() -> Result<PathBuf, Error> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("plasmite"));
    }
    home_dir().map(|home| home.join(".config/plasmite"))
}

#[cfg(not(any(unix, windows)))]
fn default_store_dir() -> Result<PathBuf, Error> {
    Err(Error::new(ErrorKind::Usage)
        .with_message("saved connections are not supported on this platform"))
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf, Error> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        Error::new(ErrorKind::Io).with_message("cannot find the current user's home directory")
    })
}

fn store_io_error(message: &str, source: std::io::Error) -> Error {
    Error::new(if source.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::Permission
    } else {
        ErrorKind::Io
    })
    .with_message(message)
    .with_source(source)
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), Error> {
    fs::rename(source, destination)
        .map_err(|error| store_io_error("failed to save connection", error))
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), Error> {
    crate::windows_private::replace(source, destination)
        .map_err(|error| store_io_error("failed to save connection", error))
}

#[cfg(windows)]
fn protect(payload: &[u8]) -> Result<Vec<u8>, Error> {
    dpapi(payload, true)
}

#[cfg(windows)]
fn unprotect(payload: &[u8]) -> Result<Vec<u8>, Error> {
    dpapi(payload, false)
}

#[cfg(windows)]
fn dpapi(payload: &[u8], protect: bool) -> Result<Vec<u8>, Error> {
    use std::ffi::c_void;
    #[repr(C)]
    struct DataBlob {
        size: u32,
        data: *mut u8,
    }
    #[link(name = "Crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            input: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *const c_void,
            flags: u32,
            output: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *const c_void,
            flags: u32,
            output: *mut DataBlob,
        ) -> i32;
    }
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let size = u32::try_from(payload.len())
        .map_err(|_| Error::new(ErrorKind::Usage).with_message("saved connection is too large"))?;
    let mut input = DataBlob {
        size,
        data: payload.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        size: 0,
        data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                0x1,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                0x1,
                &mut output,
            )
        }
    };
    input.data = std::ptr::null_mut();
    if ok == 0 {
        return Err(Error::new(ErrorKind::Permission)
            .with_message("could not protect saved connection for this Windows user")
            .with_source(std::io::Error::last_os_error()));
    }
    if output.data.is_null() || output.size == 0 {
        if !output.data.is_null() {
            unsafe { LocalFree(output.data.cast()) };
        }
        return Err(Error::new(ErrorKind::Corrupt)
            .with_message("Windows returned an empty protected connection"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.data, output.size as usize) }.to_vec();
    unsafe { LocalFree(output.data.cast()) };
    Ok(bytes)
}

#[cfg(not(windows))]
fn protect(payload: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(payload.to_vec())
}

#[cfg(not(windows))]
fn unprotect(payload: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(payload.to_vec())
}
