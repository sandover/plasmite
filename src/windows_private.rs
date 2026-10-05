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
    MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE,
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

    fn descriptor(&self, shared: Option<&Shared>) -> io::Result<LocalMemory> {
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
        let sddl: Vec<u16> = format!(
            "O:{sid}D:P{}(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
            shared
                .map(|policy| format!(
                    "(A;OICI;FA;;;{})(A;OICI;FA;;;{})",
                    policy.owner, policy.service
                ))
                .unwrap_or_else(|| format!("(A;OICI;FA;;;{sid})"))
        )
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
            FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
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
        handles.push(directory_handle(directory).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to pin directory {}: {error}", directory.display()),
            )
        })?);
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
    open_directory_policy(path, None)
}

fn open_directory_policy(path: &Path, shared: Option<&Shared>) -> io::Result<Directory> {
    let handles = pin_directories(path)?;
    let directory = handles.last().expect("absolute paths have a root");
    verify_volume(directory)?;
    let checked = open_verified_policy(path, 0, OPEN_EXISTING, shared)?;
    verify(&checked, &Identity::current()?, shared)?;
    Ok(Directory { _handles: handles })
}

fn verify(file: &File, identity: &Identity, shared: Option<&Shared>) -> io::Result<()> {
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
    let allowed = |sid| -> io::Result<bool> {
        if let Some(policy) = shared {
            policy.contains(sid)
        } else {
            Ok(unsafe { EqualSid(sid, identity.sid()) } != 0)
        }
    };
    if !allowed(identity.sid())? || owner.is_null() || !allowed(owner)? || acl.is_null() {
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
        if !allowed(sid)?
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
    open_verified_policy(path, access, disposition, None)
}

fn open_verified_policy(
    path: &Path,
    access: u32,
    disposition: u32,
    shared: Option<&Shared>,
) -> io::Result<File> {
    let path = std::path::absolute(path)?;
    let path = path.as_path();
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    verify_volume(_parents.last().expect("absolute paths have a root"))?;
    if disposition != OPEN_EXISTING {
        open_verified_policy(
            path.parent().unwrap_or_else(|| Path::new(".")),
            0,
            OPEN_EXISTING,
            shared,
        )?;
    }
    let identity = Identity::current()?;
    let descriptor = identity.descriptor(shared)?;
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
    verify(&file, &identity, shared)?;
    Ok(file)
}

pub(crate) fn ensure_private(path: &Path) -> io::Result<()> {
    open_verified(path, 0, OPEN_EXISTING).map(drop)
}

pub(crate) fn create_dir(path: &Path) -> io::Result<Directory> {
    create_dir_policy(path, None)
}

fn create_dir_policy(path: &Path, shared: Option<&Shared>) -> io::Result<Directory> {
    let path = std::path::absolute(path)?;
    let path = path.as_path();
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    verify_volume(_parents.last().expect("absolute paths have a root"))?;
    let identity = Identity::current()?;
    let descriptor = identity.descriptor(shared)?;
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
    open_directory_policy(path, shared)
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
    replace_policy(source, destination, None)
}

fn replace_policy(source: &Path, destination: &Path, shared: Option<&Shared>) -> io::Result<()> {
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
    let _parents = open_directory_policy(
        destination.parent().unwrap_or_else(|| Path::new(".")),
        shared,
    )?;
    open_verified_policy(source, 0, OPEN_EXISTING, shared)?;
    open_verified_policy(
        destination.parent().unwrap_or_else(|| Path::new(".")),
        0,
        OPEN_EXISTING,
        shared,
    )?;
    match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            open_verified_policy(destination, 0, OPEN_EXISTING, shared)?;
        }
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

/// A server-only policy. Callers must authenticate its SID pair against an
/// administrator-controlled installation before using it. Client credentials
/// continue to use the owner-only entry points above.
#[derive(Clone)]
pub struct Shared {
    owner: String,
    service: String,
}

impl Shared {
    pub fn new(owner: &str, service: &str) -> io::Result<Self> {
        for sid in [owner, service] {
            sid_memory(sid)?;
        }
        Ok(Self {
            owner: owner.into(),
            service: service.into(),
        })
    }

    fn contains(&self, sid: *mut c_void) -> io::Result<bool> {
        for member in [&self.owner, &self.service] {
            let member = sid_memory(member)?;
            if unsafe { EqualSid(sid, member.0) } != 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn ensure_private(&self, path: &Path) -> io::Result<()> {
        open_verified_policy(path, 0, OPEN_EXISTING, Some(self)).map(drop)
    }
    pub fn open_directory(&self, path: &Path) -> io::Result<Directory> {
        open_directory_policy(path, Some(self))
    }
    pub fn create_dir_all(&self, path: &Path) -> io::Result<Directory> {
        let _parent = match path
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && !p.exists())
        {
            Some(parent) => Some(self.create_dir_all(parent)?),
            None => None,
        };
        create_dir_policy(path, Some(self))
    }
    pub fn open_lock(&self, path: &Path) -> io::Result<File> {
        open_verified_policy(path, GENERIC_READ | GENERIC_WRITE, OPEN_ALWAYS, Some(self))
    }
    pub fn create_file(&self, path: &Path) -> io::Result<File> {
        open_verified_policy(path, GENERIC_WRITE, CREATE_NEW, Some(self))
    }
    pub fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        open_verified_policy(path, GENERIC_READ, OPEN_EXISTING, Some(self))?
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    pub fn replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        replace_policy(source, destination, Some(self))
    }
    /// Migrate only state already safe under the owner-only or this shared policy.
    pub fn grant(&self, path: &Path) -> io::Result<()> {
        if self.ensure_private(path).is_err() {
            ensure_private(path)?;
        }
        set_descriptor(
            path,
            &format!(
                "D:P(A;OICI;FA;;;{})(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
                self.owner, self.service
            ),
            false,
        )
    }

    /// Preserve existing pool permissions while adding/removing this service's
    /// ordinary modify rights. Private state uses its separate narrow policy.
    pub fn pool_access(&self, path: &Path, grant: bool) -> io::Result<()> {
        self.change_access(path, grant, false)
    }
    /// Synchronous directory pinning needs list and synchronization access so
    /// Windows enforces the absence of delete sharing. This grants directory
    /// names, with no file data, write or
    /// inherited rights, through the selected pool's ancestors.
    pub fn ancestor_access(&self, path: &Path, grant: bool) -> io::Result<()> {
        self.change_access(path, grant, true)
    }
    fn change_access(&self, path: &Path, grant: bool, ancestor: bool) -> io::Result<()> {
        use windows_sys::Win32::Security::Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, REVOKE_ACCESS, SetEntriesInAclW, SetSecurityInfo,
            TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        };
        use windows_sys::Win32::Security::SUB_CONTAINERS_AND_OBJECTS_INHERIT;
        use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
        let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
        let handle = unsafe {
            CreateFileW(
                wide(path)?.as_ptr(),
                READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
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
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks > 1 {
            return Err(denied("pool permissions require real files"));
        }
        let (mut owner, mut acl, mut descriptor) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
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
        let _old = LocalMemory(descriptor);
        if owner.is_null()
            || (!ancestor && !self.contains(owner)? && !administrator_owner(owner))
            || acl.is_null()
        {
            return Err(denied("selected pool must belong to its owner or service"));
        }
        let service = sid_memory(&self.service)?;
        let account = sid_memory(&self.owner)?;
        let trustee = |sid: *mut c_void| TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
            ..Default::default()
        };
        let mut entries = vec![EXPLICIT_ACCESS_W {
            grfAccessPermissions: if ancestor {
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE
            } else {
                0x0013_01bf
            },
            grfAccessMode: if grant { GRANT_ACCESS } else { REVOKE_ACCESS },
            grfInheritance: if ancestor {
                0
            } else {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            },
            Trustee: trustee(service.0),
        }];
        if grant && !ancestor {
            entries.push(EXPLICIT_ACCESS_W {
                // Keep the owner able to manage files the service creates.
                grfAccessPermissions: 0x001f_01ff,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
                Trustee: trustee(account.0),
            });
        }
        let mut updated = std::ptr::null_mut();
        let status =
            unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), acl, &mut updated) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _updated = LocalMemory(updated.cast());
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                updated,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }
    pub fn revoke(&self, path: &Path) -> io::Result<()> {
        if self.ensure_private(path).is_err() {
            ensure_private(path)?;
        }
        set_descriptor(
            path,
            &format!(
                "O:{}D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
                self.owner, self.owner
            ),
            true,
        )
    }
}

fn sid_memory(text: &str) -> io::Result<LocalMemory> {
    let mut sid = std::ptr::null_mut();
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    if unsafe {
        windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW(text.as_ptr(), &mut sid)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalMemory(sid))
}

pub fn current_sid() -> io::Result<String> {
    let identity = Identity::current()?;
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(identity.sid(), &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalMemory(text.cast());
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|_| denied("invalid Windows SID"))
}

/// Check pool ownership before an elevated installer changes any state. A pool
/// created from an elevated terminal may belong to Administrators.
pub fn ensure_owner(path: &Path) -> io::Result<()> {
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let handle = unsafe {
        CreateFileW(
            wide(path)?.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
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
        return Err(denied("pool directory must be a real directory"));
    }
    let (mut owner, mut descriptor) = (std::ptr::null_mut(), std::ptr::null_mut());
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _descriptor = LocalMemory(descriptor);
    if owner.is_null()
        || (unsafe { EqualSid(owner, Identity::current()?.sid()) } == 0
            && !administrator_owner(owner))
    {
        return Err(denied("selected pool belongs to another Windows account"));
    }
    Ok(())
}

fn administrator_owner(sid: *mut c_void) -> bool {
    unsafe {
        IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
            && windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0
    }
}

/// Apply an explicit protected descriptor through a pinned, non-reparse handle.
pub fn set_descriptor(path: &Path, sddl: &str, change_owner: bool) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{WRITE_DAC, WRITE_OWNER};
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let handle = unsafe {
        CreateFileW(
            wide(path)?.as_ptr(),
            READ_CONTROL | WRITE_DAC | if change_owner { WRITE_OWNER } else { 0 },
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
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied("permissions require a real file or directory"));
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.nNumberOfLinks > 1 {
        return Err(denied("permissions cannot change a hard link"));
    }
    let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
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
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalMemory(descriptor);
    let (mut acl, mut owner) = (std::ptr::null_mut(), std::ptr::null_mut());
    let (mut present, mut defaulted) = (0, 0);
    if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) } == 0
        || present == 0
    {
        return Err(denied("descriptor requires an access list"));
    }
    if change_owner
        && unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION
                | if change_owner {
                    OWNER_SECURITY_INFORMATION
                } else {
                    0
                },
            owner,
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// Read only administrator-owned files whose ACL grants write access solely to
/// administrators/SYSTEM. Installer manifests contain no credentials.
pub fn read_installed(path: &Path) -> io::Result<Vec<u8>> {
    let _parents = pin_directories(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let handle = unsafe {
        CreateFileW(
            wide(path)?.as_ptr(),
            GENERIC_READ | READ_CONTROL | FILE_READ_ATTRIBUTES,
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
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks > 1 {
        return Err(denied("installed state must use real files"));
    }
    let (mut owner, mut acl, mut descriptor) = (
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
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
    let trusted = |sid| unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
    };
    if owner.is_null() || !trusted(owner) || acl.is_null() {
        return Err(denied("installed state must belong to administrators"));
    }
    let (mut control, mut revision) = (0, 0);
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(denied("installed state must disable inherited permissions"));
    }
    for index in 0..unsafe { (*acl).AceCount } as u32 {
        let mut raw = std::ptr::null_mut();
        if unsafe { GetAce(acl, index, &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*raw.cast::<ACE_HEADER>() };
        if header.AceType != 0 || header.AceSize < std::mem::size_of::<ACCESS_ALLOWED_ACE>() as u16
        {
            return Err(denied("unsupported installed access rule"));
        }
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
        if unsafe { IsValidSid(sid) } == 0
            || unsafe { GetLengthSid(sid) } as usize
                > header.AceSize as usize - std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart)
        {
            return Err(denied("invalid installed access rule"));
        }
        // Write data/append, add file/directory, delete child/object, alter ACL or
        // owner, and generic write/all must remain administrator-only.
        if !trusted(sid) && ace.Mask & 0x500d_0156 != 0 {
            return Err(denied(
                "installed state grants non-administrator write access",
            ));
        }
    }
    if file.metadata()?.is_dir() {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Command;

    #[test]
    fn ancestor_grant_supports_synchronous_pinning_without_write_access() -> io::Result<()> {
        use windows_sys::Win32::Security::{
            CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, ImpersonateLoggedOnUser, RevertToSelf,
            SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_IMPERSONATE,
        };

        struct Impersonation;
        impl Drop for Impersonation {
            fn drop(&mut self) {
                // Continuing under an unexpected token would invalidate later checks.
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }

        let temp = tempfile::tempdir()?;
        let directory = temp.path().join("ancestor");
        std::fs::create_dir(&directory)?;
        let owner = current_sid()?;
        let service = "S-1-5-80-1-2-3-4-5";
        set_descriptor(
            &directory,
            &format!("D:P(A;;FA;;;{owner})(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x81;;;{service})"),
            false,
        )?;

        let mut token = std::ptr::null_mut();
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_IMPERSONATE,
                &mut token,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let sid = sid_memory(service)?;
        let restriction = SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: 0,
        };
        let mut restricted = std::ptr::null_mut();
        if unsafe {
            CreateRestrictedToken(
                token.as_raw_handle(),
                DISABLE_MAX_PRIVILEGE,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &restriction,
                &mut restricted,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let restricted = unsafe { OwnedHandle::from_raw_handle(restricted) };
        let impersonate = || -> io::Result<Impersonation> {
            if unsafe { ImpersonateLoggedOnUser(restricted.as_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Impersonation)
        };
        {
            let _token = impersonate()?;
            assert_eq!(
                directory_handle(&directory)
                    .expect_err("missing synchronization right")
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        Shared::new(&owner, service)?.ancestor_access(&directory, true)?;
        {
            let _token = impersonate()?;
            drop(directory_handle(&directory)?);
            assert_eq!(
                File::create(directory.join("forbidden"))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        Ok(())
    }

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
