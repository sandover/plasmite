//! Server state may share access with its authenticated native service account.
//! Saved native client credentials continue to use windows_private directly.
use crate::windows_private::{self, Directory, Shared};
use std::{fs::File, io, path::Path};
fn policy(path: &Path) -> io::Result<Option<Shared>> {
    crate::serve_service::windows::shared_policy(path)
        .map_err(|err| io::Error::new(io::ErrorKind::PermissionDenied, err))
}
pub(crate) fn read(path: &Path) -> io::Result<Vec<u8>> {
    match policy(path)? {
        Some(p) => p.read(path),
        None => windows_private::read(path),
    }
}
pub(crate) fn ensure_private(path: &Path) -> io::Result<()> {
    match policy(path)? {
        Some(p) => p.ensure_private(path),
        None => windows_private::ensure_private(path),
    }
}
pub(crate) fn open_directory(path: &Path) -> io::Result<Directory> {
    match policy(path)? {
        Some(p) => p.open_directory(path),
        None => windows_private::open_directory(path),
    }
}
pub(crate) fn create_dir_all(path: &Path) -> io::Result<Directory> {
    match policy(path)? {
        Some(p) => p.create_dir_all(path),
        None => windows_private::create_dir_all(path),
    }
}
pub(crate) fn open_lock(path: &Path) -> io::Result<File> {
    match policy(path)? {
        Some(p) => p.open_lock(path),
        None => windows_private::open_lock(path),
    }
}
pub(crate) fn create_file(path: &Path) -> io::Result<File> {
    match policy(path)? {
        Some(p) => p.create_file(path),
        None => windows_private::create_file(path),
    }
}
pub(crate) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    match policy(destination)? {
        Some(p) => p.replace(source, destination),
        None => windows_private::replace(source, destination),
    }
}
