//! Parse message-time filters for the CLI and MCP interfaces.

use time::OffsetDateTime;
use time::error::Parse;
use time::format_description::well_known::Rfc3339;

pub(crate) fn parse_rfc3339_ns(input: &str) -> Result<u64, Parse> {
    let timestamp = OffsetDateTime::parse(input.trim(), &Rfc3339)?;
    Ok(timestamp.unix_timestamp_nanos().clamp(0, u64::MAX as i128) as u64)
}

pub(crate) fn parse_relative_ns(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    let unit_offset = trimmed.bytes().position(|byte| !byte.is_ascii_digit())?;
    let (digits, unit) = trimmed.split_at(unit_offset);
    let value: u64 = digits.parse().ok()?;
    let nanos_per_unit = match unit.to_ascii_lowercase().as_str() {
        "ns" => 1,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 3600 * 1_000_000_000,
        "d" => 86400 * 1_000_000_000,
        _ => return None,
    };
    Some(value.saturating_mul(nanos_per_unit))
}

#[cfg(test)]
mod tests {
    use super::{parse_relative_ns, parse_rfc3339_ns};

    #[test]
    fn time_selection_clamps_instants_and_parses_durations() {
        assert_eq!(parse_rfc3339_ns("1969-12-31T23:59:59Z").unwrap(), 0);
        assert_eq!(parse_rfc3339_ns("9999-12-31T23:59:59Z").unwrap(), u64::MAX);
        assert_eq!(parse_relative_ns("5m"), Some(300_000_000_000));
        assert_eq!(parse_relative_ns("5é"), None);
    }
}
