//! Browser trust for the exact certificate of a saved native connection.
//!
//! The native access key verifies the server first. Browser trust is a separate,
//! explicit operating-system action. It never carries the access secret.
#![allow(clippy::result_large_err)]

use super::access::{
    certificate_validity, encode_hex, load_saved_key, secure_destination, verified_browser_leaf,
};
use crate::core::error::{Error, ErrorKind};
use sha2::{Digest, Sha256};
use url::Url;

type ApiResult<T> = Result<T, Error>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTrustStatus {
    pub destination: String,
    pub certificate_sha256: String,
    pub expires_at: i64,
    pub names: Vec<String>,
    /// On Windows, the exact certificate is present in CurrentUser Root.
    /// Browser and managed-device policies can still reject it.
    pub installed: bool,
}

/// Describe the server's current leaf certificate after a pinned TLS check.
/// The server must be reachable and a native connection must already be saved.
pub fn status(destination: &str) -> ApiResult<BrowserTrustStatus> {
    let (destination, der) = current_certificate(destination)?;
    let names = inspect_leaf(&der)?;
    let fingerprint = encode_hex(&Sha256::digest(&der));
    let (_, expires_at) = certificate_validity(&der)?;
    Ok(BrowserTrustStatus {
        destination: destination.to_string(),
        installed: installed(&fingerprint, &der, &destination)?,
        certificate_sha256: fingerprint,
        expires_at,
        names,
    })
}

/// Ask the operating system to trust the current, pinned leaf certificate.
/// Call this only after showing the user the certificate and receiving consent.
pub fn install(destination: &str, expected_sha256: &str) -> ApiResult<BrowserTrustStatus> {
    if cfg!(target_os = "windows") {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("Windows browser trust setup needs signed-in validation")
            .with_hint("Use an HTTPS certificate already trusted by the browser. Native Plasmite access remains available."));
    }
    let (destination, der) = current_certificate(destination)?;
    let names = inspect_leaf(&der)?;
    let fingerprint = encode_hex(&Sha256::digest(&der));
    if !fingerprint.eq_ignore_ascii_case(expected_sha256) {
        return Err(Error::new(ErrorKind::Io)
            .with_message("server certificate changed before browser trust approval")
            .with_hint("Review the new certificate and approve it separately."));
    }
    let (_, expires_at) = certificate_validity(&der)?;
    if !installed(&fingerprint, &der, &destination)? {
        platform::install(&der)?;
    }
    let installed = installed(&fingerprint, &der, &destination)?;
    if !installed {
        return Err(Error::new(ErrorKind::Io)
            .with_message("the operating system did not install browser trust"));
    }
    Ok(BrowserTrustStatus {
        destination: destination.to_string(),
        certificate_sha256: fingerprint,
        expires_at,
        names,
        installed,
    })
}

fn installed(fingerprint: &str, der: &[u8], destination: &Url) -> ApiResult<bool> {
    if !platform::contains(fingerprint)? {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        platform::trusts(der, destination)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (der, destination);
        Ok(true)
    }
}

/// Remove one exact certificate from this user's browser trust store.
/// This works after the server renews or goes offline.
pub fn remove(certificate_sha256: &str) -> ApiResult<()> {
    if certificate_sha256.len() != 64
        || !certificate_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("certificate fingerprint must be 64 hexadecimal characters"));
    }
    platform::remove(&certificate_sha256.to_ascii_lowercase())
}

/// Open the exact server address after browser trust has been installed.
pub fn open(destination: &str) -> ApiResult<()> {
    let destination = secure_destination(destination)?;
    platform::open(&destination)
}

fn current_certificate(destination: &str) -> ApiResult<(Url, Vec<u8>)> {
    let destination = secure_destination(destination)?;
    let key = load_saved_key(destination.as_str())?.ok_or_else(|| {
        Error::new(ErrorKind::Permission)
            .with_message("no native connection is saved for this server")
            .with_hint("Run access connect before setting up browser trust.")
    })?;
    let der = verified_browser_leaf(&destination, *key.spki_fingerprint())?;
    Ok((destination, der))
}

// macOS's SSL trust setting is only appropriate for a leaf. Fail closed if
// Basic Constraints is absent or grants certificate-signing authority.
fn inspect_leaf(der: &[u8]) -> ApiResult<Vec<String>> {
    let invalid = || {
        Error::new(ErrorKind::Usage)
            .with_message("server certificate is not a restricted TLS leaf")
            .with_hint("Ask the owner for a certificate with critical CA:false, digital-signature use, and server-authentication use, or restart a server using its generated certificate.")
    };
    let outer = der_content(der, 0).ok_or_else(invalid)?;
    if outer.0 != 0x30 || outer.2 != der.len() {
        return Err(invalid());
    }
    let tbs = der_content(outer.1, 0).ok_or_else(invalid)?;
    if tbs.0 != 0x30 {
        return Err(invalid());
    }
    let mut offset = 0;
    while offset < tbs.1.len() {
        let (tag, content, next) = der_content(tbs.1, offset).ok_or_else(invalid)?;
        if tag == 0xa3 {
            let extensions = der_content(content, 0).ok_or_else(invalid)?;
            if extensions.0 != 0x30 || extensions.2 != content.len() {
                return Err(invalid());
            }
            let mut extension_offset = 0;
            let mut found_non_ca = false;
            let mut found_leaf_key_usage = false;
            let mut found_server_auth = false;
            let mut names = Vec::new();
            while extension_offset < extensions.1.len() {
                let extension = der_content(extensions.1, extension_offset).ok_or_else(invalid)?;
                if extension.0 != 0x30 {
                    return Err(invalid());
                }
                let oid = der_content(extension.1, 0).ok_or_else(invalid)?;
                if oid.0 != 0x06 {
                    return Err(invalid());
                }
                if oid.1 == [0x55, 0x1d, 0x13] {
                    if found_non_ca {
                        return Err(invalid());
                    }
                    let mut value_offset = oid.2;
                    let mut value = der_content(extension.1, value_offset).ok_or_else(invalid)?;
                    if value.0 == 0x01 && value.1 == [0xff] {
                        value_offset = value.2;
                        value = der_content(extension.1, value_offset).ok_or_else(invalid)?;
                    } else {
                        return Err(invalid());
                    }
                    if value.0 != 0x04 || value.2 != extension.1.len() {
                        return Err(invalid());
                    }
                    let constraints = der_content(value.1, 0).ok_or_else(invalid)?;
                    if constraints.0 != 0x30 || constraints.2 != value.1.len() {
                        return Err(invalid());
                    }
                    if !constraints.1.is_empty() {
                        let ca = der_content(constraints.1, 0).ok_or_else(invalid)?;
                        if ca.0 != 0x01 || ca.1 != [0] || ca.2 != constraints.1.len() {
                            return Err(invalid());
                        }
                    }
                    found_non_ca = true;
                } else if oid.1 == [0x55, 0x1d, 0x11] {
                    let mut value = der_content(extension.1, oid.2).ok_or_else(invalid)?;
                    if value.0 == 0x01 {
                        value = der_content(extension.1, value.2).ok_or_else(invalid)?;
                    }
                    if value.0 != 0x04 || value.2 != extension.1.len() {
                        return Err(invalid());
                    }
                    let san = der_content(value.1, 0).ok_or_else(invalid)?;
                    if san.0 != 0x30 || san.2 != value.1.len() {
                        return Err(invalid());
                    }
                    let mut san_offset = 0;
                    while san_offset < san.1.len() {
                        let entry = der_content(san.1, san_offset).ok_or_else(invalid)?;
                        match entry.0 {
                            0x82 => {
                                let name = std::str::from_utf8(entry.1).map_err(|_| invalid())?;
                                names.push(name.to_owned());
                            }
                            0x87 if entry.1.len() == 4 => {
                                let ip = std::net::Ipv4Addr::new(
                                    entry.1[0], entry.1[1], entry.1[2], entry.1[3],
                                );
                                names.push(ip.to_string());
                            }
                            0x87 if entry.1.len() == 16 => {
                                let bytes: [u8; 16] = entry.1.try_into().map_err(|_| invalid())?;
                                names.push(std::net::Ipv6Addr::from(bytes).to_string());
                            }
                            _ => {}
                        }
                        san_offset = entry.2;
                    }
                } else if oid.1 == [0x55, 0x1d, 0x0f] {
                    if found_leaf_key_usage {
                        return Err(invalid());
                    }
                    let usage = extension_octets(extension.1, oid.2).ok_or_else(invalid)?;
                    let bits = der_content(usage, 0).ok_or_else(invalid)?;
                    if bits.0 != 0x03
                        || bits.2 != usage.len()
                        || bits.1.len() < 2
                        || bits.1[1] & 0x80 == 0
                        || bits.1[1] & 0x04 != 0
                    {
                        return Err(invalid());
                    }
                    found_leaf_key_usage = true;
                } else if oid.1 == [0x55, 0x1d, 0x25] {
                    if found_server_auth {
                        return Err(invalid());
                    }
                    let usage = extension_octets(extension.1, oid.2).ok_or_else(invalid)?;
                    let purposes = der_content(usage, 0).ok_or_else(invalid)?;
                    if purposes.0 != 0x30 || purposes.2 != usage.len() {
                        return Err(invalid());
                    }
                    let purpose = der_content(purposes.1, 0).ok_or_else(invalid)?;
                    if purpose.0 != 0x06
                        || purpose.1 != [0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01]
                        || purpose.2 != purposes.1.len()
                    {
                        return Err(invalid());
                    }
                    found_server_auth = true;
                }
                extension_offset = extension.2;
            }
            return if found_non_ca && found_leaf_key_usage && found_server_auth && !names.is_empty()
            {
                Ok(names)
            } else {
                Err(invalid())
            };
        }
        offset = next;
    }
    Err(invalid())
}

fn extension_octets(data: &[u8], offset: usize) -> Option<&[u8]> {
    let mut value = der_content(data, offset)?;
    if value.0 == 0x01 {
        value = der_content(data, value.2)?;
    }
    if value.0 != 0x04 || value.2 != data.len() {
        return None;
    }
    Some(value.1)
}

fn der_content(data: &[u8], offset: usize) -> Option<(u8, &[u8], usize)> {
    let tag = *data.get(offset)?;
    let first = *data.get(offset + 1)?;
    let (header, length) = if first < 128 {
        (2, first as usize)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for byte in data.get(offset + 2..offset + 2 + count)? {
            length = length.checked_mul(256)?.checked_add(*byte as usize)?;
        }
        if length < 128 {
            return None;
        }
        (2 + count, length)
    };
    let start = offset.checked_add(header)?;
    let end = start.checked_add(length)?;
    Some((tag, data.get(start..end)?, end))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::Path;
    use std::process::Command;

    fn login_keychain() -> ApiResult<std::path::PathBuf> {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            Error::new(ErrorKind::Io)
                .with_message("cannot locate the current user's login keychain")
        })?;
        Ok(Path::new(&home).join("Library/Keychains/login.keychain-db"))
    }

    pub(super) fn contains(fingerprint: &str) -> ApiResult<bool> {
        let output = Command::new("security")
            .arg("find-certificate")
            .args(["-a", "-Z"])
            .arg(login_keychain()?)
            .output()
            .map_err(|err| io_error("inspect browser trust", err))?;
        if !output.status.success() {
            return Err(command_error("inspect browser trust", &output));
        }
        let sought = fingerprint.to_ascii_uppercase();
        Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.trim()
                .strip_prefix("SHA-256 hash:")
                .is_some_and(|hash| hash.trim().eq_ignore_ascii_case(&sought))
        }))
    }

    pub(super) fn trusts(der: &[u8], destination: &Url) -> ApiResult<bool> {
        let file = TempCertificate::new(der)?;
        let host = destination.host_str().ok_or_else(|| {
            Error::new(ErrorKind::Usage).with_message("server address has no host")
        })?;
        let output = Command::new("security")
            .arg("verify-cert")
            .arg("-c")
            .arg(&file.path)
            .args(["-p", "ssl", "-n", host, "-L", "-q", "-k"])
            .arg(login_keychain()?)
            .output()
            .map_err(|err| io_error("verify browser trust", err))?;
        Ok(output.status.success())
    }

    pub(super) fn install(der: &[u8]) -> ApiResult<()> {
        let file = TempCertificate::new(der)?;
        let add = |result| {
            Command::new("security")
                .arg("add-trusted-cert")
                .args(["-r", result, "-p", "ssl", "-k"])
                .arg(login_keychain()?)
                .arg(&file.path)
                .output()
                .map_err(|err| io_error("install browser trust", err))
        };
        let mut output = add("trustRoot")?;
        // trustRoot rejects a leaf signed by another issuer. Retry only when
        // macOS reports an invalid trust setting.
        if !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("SecTrustSettingsSetTrustSettings:")
            && String::from_utf8_lossy(&output.stderr)
                .contains("One or more parameters passed to a function were not valid")
        {
            output = add("trustAsRoot")?;
        }
        if output.status.success() {
            Ok(())
        } else {
            Err(command_error("install browser trust", &output))
        }
    }

    pub(super) fn remove(fingerprint: &str) -> ApiResult<()> {
        if !contains(fingerprint)? {
            return Ok(());
        }
        let output = Command::new("security")
            .arg("delete-certificate")
            .args(["-Z", fingerprint, "-t"])
            .arg(login_keychain()?)
            .output()
            .map_err(|err| io_error("remove browser trust", err))?;
        if !output.status.success() {
            return Err(command_error("remove browser trust", &output));
        }
        if contains(fingerprint)? {
            return Err(Error::new(ErrorKind::Io).with_message("browser trust remains installed"));
        }
        Ok(())
    }

    pub(super) fn open(destination: &Url) -> ApiResult<()> {
        let result = Command::new("open")
            .arg(destination.as_str())
            .status()
            .map_err(|err| io_error("open browser", err))?;
        if result.success() {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Io)
                .with_message("could not open the server address in a browser"))
        }
    }

    struct TempCertificate {
        path: std::path::PathBuf,
    }

    impl TempCertificate {
        fn new(der: &[u8]) -> ApiResult<Self> {
            use base64::Engine;
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message(format!("could not create temporary certificate: {err}"))
            })?;
            let path = std::env::temp_dir().join(format!(
                "plasmite-cert-{}-{}.pem",
                std::process::id(),
                encode_hex(&random)
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|err| io_error("create temporary certificate", err))?;
            let base64 = base64::engine::general_purpose::STANDARD.encode(der);
            let result = (|| -> std::io::Result<()> {
                file.write_all(b"-----BEGIN CERTIFICATE-----\n")?;
                for chunk in base64.as_bytes().chunks(64) {
                    file.write_all(chunk)?;
                    file.write_all(b"\n")?;
                }
                file.write_all(b"-----END CERTIFICATE-----\n")?;
                file.sync_all()
            })();
            if let Err(err) = result {
                let _ = std::fs::remove_file(&path);
                return Err(io_error("write temporary certificate", err));
            }
            Ok(Self { path })
        }
    }

    impl Drop for TempCertificate {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn io_error(action: &str, error: impl std::error::Error + Send + Sync + 'static) -> Error {
        Error::new(ErrorKind::Io)
            .with_message(format!("could not {action}"))
            .with_source(error)
    }

    fn command_error(action: &str, output: &std::process::Output) -> Error {
        let detail = String::from_utf8_lossy(&output.stderr);
        Error::new(ErrorKind::Io).with_message(format!("could not {action}: {}", detail.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_restricted_tls_leaf_can_enter_browser_trust() {
        use rcgen::{
            BasicConstraints, Certificate, CertificateParams, CustomExtension,
            ExtendedKeyUsagePurpose, IsCa, KeyUsagePurpose,
        };

        let mut leaf = CertificateParams::new(vec!["localhost".into()]);
        leaf.is_ca = IsCa::NoCa;
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let mut non_ca = CustomExtension::from_oid_content(&[2, 5, 29, 19], vec![0x30, 0x00]);
        non_ca.set_criticality(true);
        leaf.custom_extensions.push(non_ca);
        let leaf = Certificate::from_params(leaf).unwrap();
        assert!(
            inspect_leaf(&leaf.serialize_der().unwrap())
                .unwrap()
                .contains(&"localhost".to_string())
        );

        let old_leaf =
            Certificate::from_params(CertificateParams::new(vec!["localhost".into()])).unwrap();
        assert!(inspect_leaf(&old_leaf.serialize_der().unwrap()).is_err());

        let mut issuer = CertificateParams::new(vec!["localhost".into()]);
        issuer.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let issuer = Certificate::from_params(issuer).unwrap();
        assert!(inspect_leaf(&issuer.serialize_der().unwrap()).is_err());
    }

    #[test]
    fn removal_requires_an_exact_certificate_fingerprint() {
        assert!(remove("localhost").is_err());
        assert!(remove(&"f".repeat(63)).is_err());
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, ExitStatus, Output};

    // Compare full SHA-256 digests of certificate bytes. Windows' displayed
    // thumbprint is SHA-1, which is not an adequate selector for removal.
    fn find(fingerprint: &str) -> ApiResult<bool> {
        let script = "$ErrorActionPreference='Stop'; $sha=[Security.Cryptography.SHA256]::Create(); Get-ChildItem Cert:\\CurrentUser\\Root | ForEach-Object { [BitConverter]::ToString($sha.ComputeHash($_.RawData)).Replace('-','') }";
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", script])
            .output()
            .map_err(|err| io_error("inspect browser trust", err))?;
        if !output.status.success() {
            return Err(command_error("inspect browser trust", &output));
        }
        let mut found = false;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if line.trim().eq_ignore_ascii_case(fingerprint) {
                if found {
                    return Err(Error::new(ErrorKind::Io)
                        .with_message("more than one Root certificate has this fingerprint"));
                }
                found = true;
            }
        }
        Ok(found)
    }

    pub(super) fn contains(fingerprint: &str) -> ApiResult<bool> {
        find(fingerprint)
    }

    pub(super) fn install(der: &[u8]) -> ApiResult<()> {
        let file = TempCertificate::new(der)?;
        // Import-Certificate requests Windows confirmation for CurrentUser
        // Root. Inherit the signed-in command's console so the user can read
        // and approve that warning. A headless process fails rather than
        // bypassing it.
        let status = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "$ErrorActionPreference='Stop'; Import-Certificate -FilePath $env:PLASMITE_CERT_PATH -CertStoreLocation 'Cert:\\CurrentUser\\Root' | Out-Null",
            ])
            .env("PLASMITE_CERT_PATH", &file.path)
            .status()
            .map_err(|err| io_error("install browser trust", err))?;
        if status.success() {
            Ok(())
        } else {
            Err(status_error("install browser trust", status))
        }
    }

    pub(super) fn remove(fingerprint: &str) -> ApiResult<()> {
        if !find(fingerprint)? {
            return Ok(());
        }
        let script = r#"
$ErrorActionPreference = 'Stop'
$expected = $env:PLASMITE_CERT_SHA256.ToUpperInvariant()
$sha = [Security.Cryptography.SHA256]::Create()
$matches = @(Get-ChildItem 'Cert:\CurrentUser\Root' | Where-Object {
    [BitConverter]::ToString($sha.ComputeHash($_.RawData)).Replace('-', '') -eq $expected
})
if ($matches.Count -ne 1) { throw 'certificate selection changed' }
$thumbprint = $matches[0].Thumbprint.Replace(' ', '').ToUpperInvariant()
if ($thumbprint -notmatch '^[0-9A-F]{40}$') { throw 'certificate store path is invalid' }
$path = "Cert:\CurrentUser\Root\$thumbprint"
$selected = Get-Item -LiteralPath $path
$actual = [BitConverter]::ToString($sha.ComputeHash($selected.RawData)).Replace('-', '')
if ($actual -ne $expected) { throw 'certificate selection changed' }
Remove-Item -LiteralPath $path
"#;
        let status = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", script])
            .env("PLASMITE_CERT_SHA256", fingerprint.to_ascii_uppercase())
            .status()
            .map_err(|err| io_error("remove browser trust", err))?;
        if !status.success() {
            return Err(status_error("remove browser trust", status));
        }
        if contains(fingerprint)? {
            return Err(Error::new(ErrorKind::Io).with_message("browser trust remains installed"));
        }
        Ok(())
    }

    pub(super) fn open(destination: &Url) -> ApiResult<()> {
        let output = Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(destination.as_str())
            .output()
            .map_err(|err| io_error("open browser", err))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_error("open browser", &output))
        }
    }

    struct TempCertificate {
        path: PathBuf,
    }

    impl TempCertificate {
        fn new(der: &[u8]) -> ApiResult<Self> {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message(format!("could not create temporary certificate: {err}"))
            })?;
            let path = std::env::temp_dir().join(format!(
                "plasmite-cert-{}-{}.cer",
                std::process::id(),
                encode_hex(&random)
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|err| io_error("create temporary certificate", err))?;
            if let Err(err) = file.write_all(der).and_then(|_| file.sync_all()) {
                let _ = std::fs::remove_file(&path);
                return Err(io_error("write temporary certificate", err));
            }
            Ok(Self { path })
        }
    }

    impl Drop for TempCertificate {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn io_error(action: &str, error: std::io::Error) -> Error {
        Error::new(ErrorKind::Io)
            .with_message(format!("could not {action}"))
            .with_source(error)
    }

    fn command_error(action: &str, output: &Output) -> Error {
        let detail = String::from_utf8_lossy(&output.stderr);
        Error::new(ErrorKind::Io).with_message(format!("could not {action}: {}", detail.trim()))
    }

    fn status_error(action: &str, status: ExitStatus) -> Error {
        Error::new(ErrorKind::Io).with_message(format!(
            "could not {action}: PowerShell exited with {status}"
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::*;
    pub(super) fn contains(_fingerprint: &str) -> ApiResult<bool> {
        Err(unsupported())
    }
    pub(super) fn install(_der: &[u8]) -> ApiResult<()> {
        Err(unsupported())
    }
    pub(super) fn remove(_fingerprint: &str) -> ApiResult<()> {
        Err(unsupported())
    }
    pub(super) fn open(_destination: &Url) -> ApiResult<()> {
        Err(unsupported())
    }
    fn unsupported() -> Error {
        Error::new(ErrorKind::Usage)
            .with_message("browser trust setup is not supported on this platform")
            .with_hint(
                "Use a certificate already trusted by the browser or an HTTPS reverse proxy.",
            )
    }
}
