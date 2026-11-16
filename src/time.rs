use crate::error::{AppError, Result};
use chrono::{NaiveDate, TimeZone, Utc};

/// Parses `30m`, `2h`, `7d`, `2w`, `YYYY-MM-DD` or an RFC 3339 time into epoch ms.
pub fn parse_since(input: &str) -> Result<i64> {
    parse_at(input, Utc::now().timestamp_millis())
}

fn parse_at(input: &str, now_ms: i64) -> Result<i64> {
    let s = input.trim();
    let bad = || {
        AppError::input(format!(
            "invalid time '{input}': use 30m, 2h, 7d, 2w or YYYY-MM-DD"
        ))
    };
    if let Some(unit) = s.chars().last().filter(|c| c.is_ascii_alphabetic()) {
        let n: i64 = s[..s.len() - 1].parse().map_err(|_| bad())?;
        let ms = match unit {
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            'w' => 7 * 86_400_000,
            _ => return Err(bad()),
        };
        return Ok(now_ms - n * ms);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc
            .from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .timestamp_millis());
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .map_err(|_| bad())
}

pub fn ms_to_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_and_absolute() {
        let now = 1_000_000_000_000;
        assert_eq!(parse_at("2h", now).unwrap(), now - 7_200_000);
        assert_eq!(parse_at("7d", now).unwrap(), now - 7 * 86_400_000);
        assert_eq!(parse_at("2026-01-02", now).unwrap(), 1_767_312_000_000);
        assert!(parse_at("soon", now).is_err());
        assert!(parse_at("5y", now).is_err());
        assert_eq!(ms_to_date(1_767_312_000_000), "2026-01-02");
    }
}
