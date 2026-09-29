//! Private Windows state shared by the server and saved-client store.
//!
//! New objects allow only the token user, SYSTEM, and administrators. Existing
//! objects must have the same owner and a protected, equally narrow access list.
//! Local administrators can already take ownership; they are part of this trust
//! boundary. Paths through junctions and other reparse points are rejected.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_NO_TOKEN, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, IsValidSid, IsWellKnownSid,
    OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, GetVolumeInformationByHandleW, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::FILE_PERSISTENT_ACLS;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

// Keep every directory in the path open without delete sharing. Windows then
// prevents a different user from renaming any ancestor during state access.
pub(crate) struct Directory {
    _handles: Vec<File>,
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

// TokenUser contains a pointer into this aligned buffer. Keep it alive until
// every SID comparison and security-descriptor conversion has finished.
struct Identity(Vec<usize>);

impl Identity {
    fn current() -> io::Result<Self> {
        let mut token = std::ptr::null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(error);
            }
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut length = 0;
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut length,
            )
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut data = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                data.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(data))
    }

    fn sid(&self) -> *mut c_void {
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn descriptor(&self) -> io::Result<LocalMemory> {
        let mut text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.sid(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let allocation = LocalMemory(text.cast());
        let mut length = 0;
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| denied("Windows returned an invalid user identity"))?;
        drop(allocation);
        let sddl: Vec<u16> =
            format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
                .encode_utf16()
                .chain(Some(0))
                .collect();
        let mut descriptor = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalMemory(descriptor))
    }
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let path: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a null character",
        ));
    }
    Ok(path.into_iter().chain(Some(0)).collect())
}

fn directory_handle(path: &Path) -> io::Result<File> {
    let handle = unsafe {
        CreateFileW(
            wide(path)?.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied("private state must use real Windows directories"));
    }
    Ok(file)
}

fn pin_directories(path: &Path) -> io::Result<Vec<File>> {
    let path = std::path::absolute(path)?;
    let mut handles = Vec::new();
    for directory in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        handles.push(directory_handle(directory)?);
    }
    Ok(handles)
}

fn verify_volume(file: &File) -> io::Result<()> {
    let mut flags = 0;
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(denied(
            "private state needs a filesystem that enforces Windows access permissions",
        ));
    }
    Ok(())
}

pub(crate) fn open_directory(path: &Path) -> io::Result<Directory> {
    let handles = pin_directories(path)?;
    let directory = handles.last().expect("absolute paths have a root");
    verify_volume(directory)?;
    verify(directory, &Identity::current()?)?;
    Ok(Directory { _handles: handles })
}

fn verify(file: &File, identity: &Identity) -> io::Result<()> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks > 1 {
        return Err(denied(
            "private state must not be a reparse point or hard link",
        ));
    }
    let mut owner = std::ptr::null_mut();
    let mut acl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _allocation = LocalMemory(descriptor);
    if owner.is_null() || unsafe { EqualSid(owner, identity.sid()) } == 0 || acl.is_null() {
        return Err(denied(
            "private state must belong to this Windows user and have restricted access",
        ));
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 || unsafe { (*acl).AceCount } == 0 {
        return Err(denied(
            "private state must disable inherited Windows access permissions",
        ));
    }
    for index in 0..unsafe { (*acl).AceCount } as u32 {
        let mut raw = std::ptr::null_mut();
        if unsafe { GetAce(acl, index, &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        // ACCESS_ALLOWED_ACE_TYPE is zero. Reject other ACE formats rather
        // than attempting to interpret unfamiliar grants or conditions.
        if header.AceType != 0 || header.AceSize < std::mem::size_of::<ACCESS_ALLOWED_ACE>() as u16
        {
            return Err(denied(
                "private state has an unsupported Windows access rule",
            ));
        }
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
        if unsafe { IsValidSid(sid) } == 0
            || unsafe { GetLengthSid(sid) } as usize
                > header.AceSize as usize - std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart)
        {
            return Err(denied("private state has an invalid Windows access rule"));
        }
        if unsafe { EqualSid(sid, identity.sid()) } == 0
            && unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } == 0
            && unsafe { IsWellKnownSid(sid, WinBuiltinAdministratorsSid) } == 0
        {
            return Err(denied(
                "private state grants access to another Windows user",
            ));
        }
    }
    Ok(())
}

fn open_verified(path: &Path, access: u32, disposition: u32) -> io::Result<File> {
    let path = std::path::absolute(path)?;
    let path = path.as_path();
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    verify_volume(_parents.last().expect("absolute paths have a root"))?;
    if disposition != OPEN_EXISTING {
        ensure_private(path.parent().unwrap_or_else(|| Path::new(".")))?;
    }
    let identity = Identity::current()?;
    let descriptor = identity.descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(path)?;
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access | READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    if access != 0 && file.metadata()?.is_dir() {
        return Err(denied("private state file must be a regular file"));
    }
    verify(&file, &identity)?;
    Ok(file)
}

pub(crate) fn ensure_private(path: &Path) -> io::Result<()> {
    open_verified(path, 0, OPEN_EXISTING).map(drop)
}

pub(crate) fn create_dir(path: &Path) -> io::Result<Directory> {
    let path = std::path::absolute(path)?;
    let path = path.as_path();
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    verify_volume(_parents.last().expect("absolute paths have a root"))?;
    let identity = Identity::current()?;
    let descriptor = identity.descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(wide(path)?.as_ptr(), &attributes) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    if !std::fs::symlink_metadata(path)?.is_dir() {
        return Err(denied("private state path must be a directory"));
    }
    open_directory(path)
}

pub(crate) fn create_dir_all(path: &Path) -> io::Result<Directory> {
    let _parent = if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && !parent.exists()
    {
        Some(create_dir_all(parent)?)
    } else {
        None
    };
    create_dir(path)
}

pub(crate) fn open_lock(path: &Path) -> io::Result<File> {
    open_verified(path, GENERIC_READ | GENERIC_WRITE, OPEN_ALWAYS)
}

pub(crate) fn create_file(path: &Path) -> io::Result<File> {
    open_verified(path, GENERIC_WRITE, CREATE_NEW)
}

pub(crate) fn read(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = open_verified(path, GENERIC_READ, OPEN_EXISTING)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = std::path::absolute(source)?;
    let destination = std::path::absolute(destination)?;
    let source = source.as_path();
    let destination = destination.as_path();
    if source.parent() != destination.parent() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state replacement must stay in one directory",
        ));
    }
    let _parents = open_directory(destination.parent().unwrap_or_else(|| Path::new(".")))?;
    ensure_private(source)?;
    ensure_private(destination.parent().unwrap_or_else(|| Path::new(".")))?;
    match std::fs::symlink_metadata(destination) {
        Ok(_) => ensure_private(destination)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if unsafe {
        MoveFileExW(
            wide(source)?.as_ptr(),
            wide(destination)?.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Command;

    #[test]
    fn private_state_survives_replacement_and_reopen() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("state");
        create_dir_all(&dir)?;
        let path = dir.join("identity");
        for value in [b"first".as_slice(), b"second".as_slice()] {
            let staged = dir.join("staged");
            let mut file = create_file(&staged)?;
            file.write_all(value)?;
            file.sync_all()?;
            drop(file);
            replace(&staged, &path)?;
            assert_eq!(read(&path)?, value);
            ensure_private(&dir)?;
        }
        Ok(())
    }

    #[test]
    fn reject_broadened_permissions_without_repairing_them() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("state");
        create_dir(&dir)?;
        let path = dir.join("secret");
        let mut file = create_file(&path)?;
        file.write_all(b"private")?;
        drop(file);
        let output = Command::new("icacls")
            .arg(&path)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()?;
        assert!(output.status.success(), "{output:?}");
        let permissions = Command::new("icacls").arg(&path).output()?.stdout;
        assert_eq!(
            read(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            Command::new("icacls").arg(&path).output()?.stdout,
            permissions
        );
        Ok(())
    }

    #[test]
    fn reject_hard_linked_state() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("state");
        create_dir(&dir)?;
        let path = dir.join("secret");
        drop(create_file(&path)?);
        std::fs::hard_link(&path, dir.join("alias"))?;
        assert_eq!(
            read(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        Ok(())
    }

    #[test]
    fn held_directory_chain_prevents_ancestor_replacement() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let parent = temp.path().join("parent");
        let state = parent.join("state");
        let held = create_dir_all(&state)?;
        for path in [&state, &parent] {
            let output = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command",
                    "$ErrorActionPreference='Stop'; Move-Item -LiteralPath $env:PLASMITE_RENAME_SOURCE -Destination $env:PLASMITE_RENAME_DESTINATION"])
                .env("PLASMITE_RENAME_SOURCE", path)
                .env("PLASMITE_RENAME_DESTINATION", temp.path().join("moved"))
                .output()?;
            assert!(
                !output.status.success(),
                "another process renamed a pinned directory"
            );
        }
        drop(held);
        std::fs::rename(&parent, temp.path().join("moved-parent"))?;
        Ok(())
    }

    #[test]
    fn reject_junction_ancestor_before_creating_state() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target");
        std::fs::create_dir(&target)?;
        let link = temp.path().join("junction");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:PLASMITE_TEST_LINK -Target $env:PLASMITE_TEST_TARGET | Out-Null"])
            .env("PLASMITE_TEST_LINK", &link)
            .env("PLASMITE_TEST_TARGET", &target)
            .output()?;
        assert!(output.status.success(), "{output:?}");
        let result = create_dir_all(&link.join("state"));
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
        assert!(!target.join("state").exists());
        Ok(())
    }
}
