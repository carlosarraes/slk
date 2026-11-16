//! Extract a browser session from a DevTools "Copy as cURL" of any Slack /api/ request.

use crate::error::{AppError, Result};
use regex::Regex;

#[derive(Debug, PartialEq)]
pub struct BrowserSession {
    pub workspace_url: String,
    pub xoxc: String,
    pub xoxd: String,
    pub user_agent: Option<String>,
}

pub fn parse(curl: &str) -> Result<BrowserSession> {
    let url_re = Regex::new(r#"https://([\w.-]+)\.slack\.com"#).unwrap();
    let host = url_re
        .captures(curl)
        .map(|c| c[1].to_string())
        .ok_or_else(|| AppError::input("no https://<team>.slack.com URL found in the cURL"))?;

    let xoxd_re = Regex::new(r#"(?:^|[\s;'"])d=(xoxd-[^;'"\s]+)"#).unwrap();
    let xoxd_raw = xoxd_re
        .captures(curl)
        .map(|c| c[1].to_string())
        .ok_or_else(|| AppError::input("no d=xoxd-... cookie found in the cURL"))?;
    let xoxd = url::form_urlencoded::parse(format!("d={xoxd_raw}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or(xoxd_raw);

    let xoxc_re = Regex::new(r"(xoxc-[A-Za-z0-9-]+)").unwrap();
    let xoxc = xoxc_re
        .captures(curl)
        .map(|c| c[1].to_string())
        .ok_or_else(|| AppError::input("no xoxc-... token found in the cURL body"))?;

    let ua_re = Regex::new(
        r#"(?:-H|--header)\s+['"][Uu]ser-[Aa]gent:\s*([^'"]+)['"]|(?:^|\s)(?:-A|--user-agent)\s+['"]([^'"]+)['"]"#,
    )
    .unwrap();
    let user_agent = ua_re
        .captures(curl)
        .and_then(|c| c.get(1).or(c.get(2)))
        .map(|m| m.as_str().trim().to_string())
        .filter(|ua| !ua.is_empty() && !ua.chars().any(char::is_control));

    Ok(BrowserSession {
        workspace_url: format!("https://{host}.slack.com"),
        xoxc,
        xoxd,
        user_agent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chrome_copy_as_curl() {
        let curl = r#"curl 'https://acme.slack.com/api/client.counts?_x_id=1' \
  -H 'accept: */*' \
  -b 'b=abc; d=xoxd-AbC%2Fdef%3D%3D; lc=1' \
  -H 'user-agent: Mozilla/5.0 (X11; Linux x86_64) Chrome/140' \
  --data-raw $'------WebKit\r\nContent-Disposition: form-data; name="token"\r\n\r\nxoxc-111-222-abc\r\n'"#;
        let s = parse(curl).unwrap();
        assert_eq!(s.workspace_url, "https://acme.slack.com");
        assert_eq!(s.xoxd, "xoxd-AbC/def==");
        assert_eq!(s.xoxc, "xoxc-111-222-abc");
        assert_eq!(
            s.user_agent.as_deref(),
            Some("Mozilla/5.0 (X11; Linux x86_64) Chrome/140")
        );
    }

    #[test]
    fn cookie_header_form_and_missing_parts() {
        let curl = "curl 'https://x.slack.com/api/a' -H 'Cookie: d=xoxd-1' --data 'token=xoxc-9'";
        let s = parse(curl).unwrap();
        assert_eq!((s.xoxd.as_str(), s.xoxc.as_str()), ("xoxd-1", "xoxc-9"));
        assert!(s.user_agent.is_none());
        assert!(parse("curl 'https://x.slack.com' --data 'token=xoxc-9'").is_err());
        assert!(parse("curl 'https://example.com' -b 'd=xoxd-1'").is_err());
    }
}
