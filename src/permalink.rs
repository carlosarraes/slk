use crate::error::{AppError, Result};

#[derive(Debug, PartialEq)]
pub struct Permalink {
    pub channel: String,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
}

pub fn looks_like_url(input: &str) -> bool {
    input.starts_with("https://") || input.starts_with("http://")
}

/// Parses `https://<team>.slack.com/archives/<channel>[/p<16 digits>][?thread_ts=..]`.
pub fn parse(input: &str) -> Result<Permalink> {
    let bad = || {
        AppError::input(format!(
            "not a Slack message link: {input} (expected https://<team>.slack.com/archives/<channel>/p<timestamp>)"
        ))
    };
    let url = url::Url::parse(input).map_err(|_| bad())?;
    if !url.host_str().is_some_and(|h| h.ends_with(".slack.com")) {
        return Err(bad());
    }
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let (channel, message) = match segments.as_slice() {
        ["archives", channel] => (*channel, None),
        ["archives", channel, message] => (*channel, Some(*message)),
        _ => return Err(bad()),
    };
    let ts = match message {
        Some(m) => Some(p_to_ts(m).ok_or_else(bad)?),
        None => None,
    };
    let thread_ts = url
        .query_pairs()
        .find(|(k, _)| k == "thread_ts")
        .map(|(_, v)| normalize_ts(&v))
        .transpose()?;
    Ok(Permalink {
        channel: channel.to_string(),
        ts,
        thread_ts,
    })
}

fn p_to_ts(segment: &str) -> Option<String> {
    let digits = segment.strip_prefix('p')?;
    if digits.len() != 16 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}.{}", &digits[..10], &digits[10..]))
}

/// Accepts `1700000000.123456`, `p1700000000123456` or `1700000000123456`.
pub fn normalize_ts(input: &str) -> Result<String> {
    let s = input.trim();
    if let Some(ts) = p_to_ts(s).or_else(|| p_to_ts(&format!("p{s}"))) {
        return Ok(ts);
    }
    let valid = s.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty() && !b.is_empty() && (a.bytes().chain(b.bytes())).all(|c| c.is_ascii_digit())
    }) || (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()));
    if valid {
        Ok(s.to_string())
    } else {
        Err(AppError::input(format!("invalid Slack timestamp: {input}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_and_thread_links() {
        let p = parse("https://acme.slack.com/archives/C0123ABC/p1700000000123456?thread_ts=1699999999.000100&cid=C0123ABC").unwrap();
        assert_eq!(p.channel, "C0123ABC");
        assert_eq!(p.ts.as_deref(), Some("1700000000.123456"));
        assert_eq!(p.thread_ts.as_deref(), Some("1699999999.000100"));

        let p = parse("https://acme.slack.com/archives/C0123ABC").unwrap();
        assert_eq!((p.ts, p.thread_ts), (None, None));
    }

    #[test]
    fn rejects_foreign_or_malformed() {
        assert!(parse("https://example.com/archives/C1/p1700000000123456").is_err());
        assert!(parse("https://acme.slack.com/archives/C1/p123").is_err());
        assert!(parse("https://acme.slack.com/team/U1").is_err());
    }

    #[test]
    fn normalizes_timestamps() {
        assert_eq!(
            normalize_ts("p1700000000123456").unwrap(),
            "1700000000.123456"
        );
        assert_eq!(
            normalize_ts("1700000000123456").unwrap(),
            "1700000000.123456"
        );
        assert_eq!(normalize_ts("1700000000.1").unwrap(), "1700000000.1");
        assert!(normalize_ts("abc").is_err());
    }
}
