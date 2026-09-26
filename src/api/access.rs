//! Native access keys, pinned connections, and per-user connection status.
#![allow(clippy::result_large_err)]

#[path = "access_store.rs"]
mod access_store;
#[path = "access_tls.rs"]
mod access_tls;

use crate::api::RemoteClient;
use crate::core::error::{Error, ErrorKind};
use sha2::{Digest, Sha256};
use std::fmt;
use url::Url;

type ApiResult<T> = Result<T, Error>;

/// An access key binds a bearer secret to one server public key.
///
/// Its debug representation never includes the secret.
#[derive(Clone, Eq, PartialEq)]
pub struct AccessKey {
    spki_fingerprint: [u8; 32],
    secret: [u8; 32],
}

impl AccessKey {
    /// Parse `pk1.<64 lowercase hex SPKI digest>.<64 lowercase hex secret>`.
    pub fn parse(value: &str) -> ApiResult<Self> {
        let mut fields = value.split('.');
        let (Some(version), Some(fingerprint), Some(secret), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid_access_key());
        };
        if version != "pk1" {
            return Err(invalid_access_key());
        }
        Ok(Self {
            spki_fingerprint: decode_32_byte_hex(fingerprint).ok_or_else(invalid_access_key)?,
            secret: decode_32_byte_hex(secret).ok_or_else(invalid_access_key)?,
        })
    }

    /// SHA-256 of the complete DER SubjectPublicKeyInfo value.
    pub fn spki_fingerprint(&self) -> &[u8; 32] {
        &self.spki_fingerprint
    }

    /// The lowercase hexadecimal SPKI fingerprint.
    pub fn spki_fingerprint_hex(&self) -> String {
        encode_hex(&self.spki_fingerprint)
    }

    /// The lowercase hexadecimal bearer secret for the Authorization header.
    pub fn secret_hex(&self) -> String {
        encode_hex(&self.secret)
    }

    /// SHA-256 verifier for storage on the server.
    pub fn secret_verifier(&self) -> [u8; 32] {
        Sha256::digest(self.secret).into()
    }

    pub(crate) fn from_parts(spki_fingerprint: [u8; 32], secret: [u8; 32]) -> Self {
        Self {
            spki_fingerprint,
            secret,
        }
    }
}

impl fmt::Debug for AccessKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessKey")
            .field("spki_fingerprint", &self.spki_fingerprint_hex())
            .field("secret", &"[redacted]")
            .finish()
    }
}

/// Derive the lowercase SHA-256 fingerprint of a certificate's DER SPKI.
pub fn spki_fingerprint(cert_der: &[u8]) -> ApiResult<String> {
    Ok(encode_hex(&spki_digest(cert_der)?))
}

pub(super) fn spki_digest(cert_der: &[u8]) -> ApiResult<[u8; 32]> {
    let certificate = ureq::rustls::pki_types::CertificateDer::from(cert_der);
    ureq::rustls::server::ParsedCertificate::try_from(&certificate)
        .map_err(|_| invalid_certificate())?;
    Ok(Sha256::digest(certificate_spki(cert_der)?).into())
}

/// Status for one saved native connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionStatus {
    pub destination: String,
    pub credentials_saved: bool,
    pub reachable: Option<bool>,
    pub accepted: Option<bool>,
    pub problem: Option<String>,
}

/// Verify a server and save its credential only after the server accepts it.
pub fn connect(destination: &str, access_key: &str) -> ApiResult<ConnectionStatus> {
    let destination = secure_destination(destination)?;
    let key = AccessKey::parse(access_key)?;
    let client = RemoteClient::from_access_key(destination.clone(), key.clone());
    let accepted = client.check_access().map_err(|error| {
        Error::new(ErrorKind::Io)
            .with_message(format!("could not verify or reach server at {destination}"))
            .with_hint("Check the URL, certificate name and validity, and access-key fingerprint. The access secret was withheld until those checks passed.")
            .with_source(error)
    })?;
    if !accepted {
        return Err(Error::new(ErrorKind::Permission)
            .with_message(format!("server rejected the access key for {destination}"))
            .with_hint("Ask the server owner for a current access key, then connect again."));
    }

    access_store::save(destination.as_str(), &key)?;
    Ok(ConnectionStatus {
        destination: destination.to_string(),
        credentials_saved: true,
        reachable: Some(true),
        accepted: Some(true),
        problem: None,
    })
}

/// Report reachability and whether a saved credential is still accepted.
pub fn status(destination: &str) -> ApiResult<ConnectionStatus> {
    let destination = secure_destination(destination)?;
    let saved = access_store::load(destination.as_str())?;
    let client = match saved.as_ref() {
        Some(key) => RemoteClient::from_access_key(destination.clone(), key.clone()),
        None => RemoteClient::for_access_probe(destination.clone()),
    };

    match client.check_access() {
        Ok(accepted) => {
            Ok(ConnectionStatus {
                destination: destination.to_string(),
                credentials_saved: saved.is_some(),
                reachable: Some(true),
                accepted: if saved.is_some() {
                    Some(accepted)
                } else {
                    None
                },
                problem: if saved.is_some() && !accepted {
                    Some("The server no longer accepts this access key. Ask its owner for a new key.".into())
                } else {
                    None
                },
            })
        }
        Err(error) => Ok(ConnectionStatus {
            destination: destination.to_string(),
            credentials_saved: saved.is_some(),
            reachable: Some(false),
            accepted: None,
            problem: Some(actionable_problem(&error)),
        }),
    }
}

pub(super) fn secure_destination(value: &str) -> ApiResult<Url> {
    let mut url = Url::parse(value).map_err(|error| {
        Error::new(ErrorKind::Usage)
            .with_message("invalid server URL")
            .with_hint("Use the HTTPS address supplied by the server owner.")
            .with_source(error)
    })?;
    if url.scheme() != "https" {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("server URL must use HTTPS")
            .with_hint("Use the HTTPS address supplied by the server owner."));
    }
    if url.host().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("server URL must name a host and must not contain credentials"));
    }
    if (url.path() != "/" && !url.path().is_empty())
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::new(ErrorKind::Usage)
            .with_message("server URL must be an origin without a path, query, or fragment")
            .with_hint("Use a URL such as https://pools.example.net:9743/"));
    }
    if url.as_str().len() > u16::MAX as usize {
        return Err(Error::new(ErrorKind::Usage).with_message("server URL is too long"));
    }
    url.set_path("/");
    Ok(url)
}

pub(super) fn certificate_spki(cert_der: &[u8]) -> ApiResult<&[u8]> {
    let outer = der_value(cert_der, 0).ok_or_else(invalid_certificate)?;
    if outer.tag != 0x30 || outer.end != cert_der.len() {
        return Err(invalid_certificate());
    }
    let tbs = der_value(outer.content, 0).ok_or_else(invalid_certificate)?;
    if tbs.tag != 0x30 {
        return Err(invalid_certificate());
    }

    let mut offset = 0;
    let mut field = der_value(tbs.content, offset).ok_or_else(invalid_certificate)?;
    if field.tag == 0xa0 {
        offset = field.end;
        field = der_value(tbs.content, offset).ok_or_else(invalid_certificate)?;
    }
    for expected in [0x02, 0x30, 0x30, 0x30, 0x30] {
        if field.tag != expected {
            return Err(invalid_certificate());
        }
        offset = field.end;
        field = der_value(tbs.content, offset).ok_or_else(invalid_certificate)?;
    }
    if field.tag != 0x30 {
        return Err(invalid_certificate());
    }
    let tbs_content_start = outer.header_len + tbs.header_len;
    Ok(&cert_der[(tbs_content_start + offset)..(tbs_content_start + field.end)])
}

pub(super) fn certificate_validity(cert_der: &[u8]) -> ApiResult<(i64, i64)> {
    let outer = der_value(cert_der, 0).ok_or_else(invalid_certificate)?;
    let tbs = der_value(outer.content, 0).ok_or_else(invalid_certificate)?;
    let mut offset = 0;
    let mut field = der_value(tbs.content, offset).ok_or_else(invalid_certificate)?;
    if field.tag == 0xa0 {
        offset = field.end;
        field = der_value(tbs.content, offset).ok_or_else(invalid_certificate)?;
    }
    for expected in [0x02, 0x30, 0x30] {
        if field.tag != expected {
            return Err(invalid_certificate());
        }
        offset = field.end;
        field = der_value(tbs.content, offset).ok_or_else(invalid_certificate)?;
    }
    if field.tag != 0x30 {
        return Err(invalid_certificate());
    }
    let validity = field.content;
    let not_before = der_value(validity, 0).ok_or_else(invalid_certificate)?;
    let not_after = der_value(validity, not_before.end).ok_or_else(invalid_certificate)?;
    if not_after.end != validity.len() {
        return Err(invalid_certificate());
    }
    Ok((
        parse_certificate_time(not_before)?,
        parse_certificate_time(not_after)?,
    ))
}

fn parse_certificate_time(value: DerValue<'_>) -> ApiResult<i64> {
    let raw = std::str::from_utf8(value.content).map_err(|_| invalid_certificate())?;
    if !raw.is_ascii() || !raw.ends_with('Z') {
        return Err(invalid_certificate());
    }
    let (year, rest) = match value.tag {
        0x17 if raw.len() == 13 => {
            let short_year = parse_decimal(&raw[0..2])?;
            (
                if short_year >= 50 {
                    1900 + short_year
                } else {
                    2000 + short_year
                },
                &raw[2..12],
            )
        }
        0x18 if raw.len() == 15 => (parse_decimal(&raw[0..4])?, &raw[4..14]),
        _ => return Err(invalid_certificate()),
    };
    let month = parse_decimal(&rest[0..2])? as u8;
    let day = parse_decimal(&rest[2..4])? as u8;
    let hour = parse_decimal(&rest[4..6])? as u8;
    let minute = parse_decimal(&rest[6..8])? as u8;
    let second = parse_decimal(&rest[8..10])? as u8;
    let date = time::Date::from_calendar_date(
        year,
        time::Month::try_from(month).map_err(|_| invalid_certificate())?,
        day,
    )
    .map_err(|_| invalid_certificate())?;
    let clock = time::Time::from_hms(hour, minute, second).map_err(|_| invalid_certificate())?;
    Ok(time::PrimitiveDateTime::new(date, clock)
        .assume_utc()
        .unix_timestamp())
}

fn parse_decimal(value: &str) -> ApiResult<i32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_certificate());
    }
    value.parse().map_err(|_| invalid_certificate())
}

#[derive(Clone, Copy)]
struct DerValue<'a> {
    tag: u8,
    content: &'a [u8],
    header_len: usize,
    end: usize,
}

fn der_value(data: &[u8], offset: usize) -> Option<DerValue<'_>> {
    let tag = *data.get(offset)?;
    let first_length = *data.get(offset + 1)?;
    let (header_len, length) = if first_length & 0x80 == 0 {
        (2, first_length as usize)
    } else {
        let length_bytes = (first_length & 0x7f) as usize;
        if length_bytes == 0 || length_bytes > std::mem::size_of::<usize>() {
            return None;
        }
        if data.get(offset + 2) == Some(&0) {
            return None;
        }
        let mut length = 0usize;
        for byte in data.get(offset + 2..offset + 2 + length_bytes)? {
            length = length.checked_mul(256)?.checked_add(*byte as usize)?;
        }
        if length < 128 {
            return None;
        }
        (2 + length_bytes, length)
    };
    let content_start = offset.checked_add(header_len)?;
    let end = content_start.checked_add(length)?;
    let content = data.get(content_start..end)?;
    Some(DerValue {
        tag,
        content,
        header_len,
        end,
    })
}

fn decode_32_byte_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut decoded = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        decoded[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(decoded)
}

pub(super) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

pub(super) fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn invalid_access_key() -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("invalid access key")
        .with_hint("Use the complete key supplied by the server owner.")
}

fn invalid_certificate() -> Error {
    Error::new(ErrorKind::Usage).with_message("server sent an invalid certificate")
}

fn actionable_problem(error: &Error) -> String {
    let message = error
        .message()
        .unwrap_or("could not verify or reach the server");
    let hint = error
        .hint()
        .unwrap_or("Check the server URL and network connection.");
    format!("{message}. {hint}")
}

fn encode_key_payload(destination: &str, key: &AccessKey) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 2 + destination.len() + 64);
    payload.push(1);
    payload.extend_from_slice(&(destination.len() as u16).to_be_bytes());
    payload.extend_from_slice(destination.as_bytes());
    payload.extend_from_slice(&key.spki_fingerprint);
    payload.extend_from_slice(&key.secret);
    payload
}

fn decode_key_payload(destination: &str, payload: &[u8]) -> ApiResult<AccessKey> {
    if payload.len() < 3 + 64 || payload[0] != 1 {
        return Err(Error::new(ErrorKind::Corrupt).with_message("saved connection is invalid"));
    }
    let destination_len = u16::from_be_bytes([payload[1], payload[2]]) as usize;
    let end = 3 + destination_len;
    if end + 64 != payload.len() || payload.get(3..end) != Some(destination.as_bytes()) {
        return Err(Error::new(ErrorKind::Corrupt)
            .with_message("saved connection destination does not match"));
    }
    let fingerprint: [u8; 32] = payload[end..end + 32].try_into().expect("fixed length");
    let secret: [u8; 32] = payload[end + 32..].try_into().expect("fixed length");
    Ok(AccessKey::from_parts(fingerprint, secret))
}

pub(super) fn store_payload(destination: &str, key: &AccessKey) -> Vec<u8> {
    encode_key_payload(destination, key)
}

pub(super) fn key_from_payload(destination: &str, payload: &[u8]) -> ApiResult<AccessKey> {
    decode_key_payload(destination, payload)
}

pub(super) fn load_saved_key(destination: &str) -> ApiResult<Option<AccessKey>> {
    access_store::load(destination)
}

pub(super) fn access_agent(expected_spki: Option<[u8; 32]>) -> ureq::Agent {
    access_tls::agent(expected_spki)
}
