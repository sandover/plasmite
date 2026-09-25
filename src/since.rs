//! Parse message-time filters for the CLI and MCP interfaces.

use time::OffsetDateTime;
use time::error::Parse;
use time::format_description::well_known::Rfc3339;

pub(crate) fn parse_since_ns(input: &str, now_ns: u64) -> Result<u64, Parse> {
    if let Some(duration_ns) = parse_relative_ns(input) {
        return Ok(now_ns.saturating_sub(duration_ns));
    }
    parse_rfc3339_ns(input)
}

pub(crate) fn parse_rfc3339_ns(input: &str) -> Result<u64, Parse> {
    let timestamp = OffsetDateTime::parse(input.trim(), &Rfc3339)?;
    Ok(timestamp.unix_timestamp_nanos().max(0) as u64)
}

fn parse_relative_ns(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (unit_offset, _) = trimmed.char_indices().next_back()?;
    let (digits, unit) = trimmed.split_at(unit_offset);
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    let seconds = match unit {
        "s" | "S" => value,
        "m" | "M" => value.saturating_mul(60),
        "h" | "H" => value.saturating_mul(60 * 60),
        "d" | "D" => value.saturating_mul(60 * 60 * 24),
        _ => return None,
    };
    Some(seconds.saturating_mul(1_000_000_000))
}

#[cfg(test)]
mod tests {
    use super::parse_since_ns;

    #[test]
    fn since_time_clamps_before_epoch_and_handles_relative_time() {
        assert_eq!(
            parse_since_ns("1969-12-31T23:59:59Z", 0).expect("absolute"),
            0
        );
        assert_eq!(
            parse_since_ns("5m", 600_000_000_000).expect("relative"),
            300_000_000_000
        );
        assert!(parse_since_ns("5é", 0).is_err());
    }
}
