use crate::config::Credentials;
use crate::error::{AppError, Result};
use serde_json::Value;
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Every Slack method slk may call. Anything else is refused before it hits the
/// network, so no code path can post, edit, react or mark-read.
pub const READ_METHODS: &[&str] = &[
    "auth.test",
    "client.counts",
    "conversations.history",
    "conversations.info",
    "conversations.list",
    "conversations.members",
    "conversations.replies",
    "search.messages",
    "search.modules",
    "subscriptions.thread.getView",
    "team.info",
    "users.info",
    "users.list",
    "users.lookupByEmail",
];

const MIN_SPACING: Duration = Duration::from_millis(200);
const MAX_RETRIES: u32 = 3;
const FALLBACK_UA: &str = concat!("slk/", env!("CARGO_PKG_VERSION"));

pub type Params<'a> = Vec<(&'a str, String)>;

pub struct Client {
    http: reqwest::blocking::Client,
    credentials: Credentials,
    base: String,
    last_call: Cell<Option<Instant>>,
    verbose: bool,
}

impl Client {
    pub fn new(credentials: Credentials, verbose: bool) -> Result<Self> {
        let base = match &credentials {
            Credentials::Standard { .. } => "https://slack.com/api".to_string(),
            Credentials::Browser { workspace_url, .. } => {
                format!(
                    "{}/api",
                    crate::config::normalize_workspace_url(workspace_url)?
                )
            }
        };
        Self::with_base(credentials, base, verbose)
    }

    pub fn with_base(credentials: Credentials, base: String, verbose: bool) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::network(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            http,
            credentials,
            base,
            last_call: Cell::new(None),
            verbose,
        })
    }

    pub fn is_browser(&self) -> bool {
        self.credentials.is_browser()
    }

    pub fn call(&self, method: &str, params: Params) -> Result<Value> {
        if !READ_METHODS.contains(&method) {
            return Err(AppError::new(
                "forbidden",
                format!("{method} is not a read-only method; slk refuses to call it"),
            ));
        }
        let mut attempt = 0;
        loop {
            self.pace();
            let started = Instant::now();
            let outcome = self.send(method, &params);
            if self.verbose {
                eprintln!(
                    "slk: {method} attempt={} {}ms",
                    attempt + 1,
                    started.elapsed().as_millis()
                );
            }
            match outcome {
                Ok(value) => return Ok(value),
                Err((err, wait)) if err.retryable && attempt < MAX_RETRIES => {
                    let wait = wait.unwrap_or(Duration::from_secs(1 << attempt));
                    if self.verbose {
                        eprintln!("slk: retrying {method} in {}s: {}", wait.as_secs(), err);
                    }
                    std::thread::sleep(wait);
                    attempt += 1;
                }
                Err((err, _)) => return Err(err),
            }
        }
    }

    /// Follows `response_metadata.next_cursor` until `limit` items of `key` are collected.
    pub fn paginate(
        &self,
        method: &str,
        params: Params,
        key: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut page = params.clone();
            if let Some(c) = &cursor {
                page.push(("cursor", c.clone()));
            }
            let resp = self.call(method, page)?;
            if let Some(arr) = resp.get(key).and_then(Value::as_array) {
                items.extend(arr.iter().cloned());
            }
            let next = next_cursor(&resp);
            if items.len() >= limit || next.is_none() || next == cursor {
                break;
            }
            cursor = next;
        }
        items.truncate(limit);
        Ok(items)
    }

    fn pace(&self) {
        if let Some(last) = self.last_call.get() {
            let elapsed = last.elapsed();
            if elapsed < MIN_SPACING {
                std::thread::sleep(MIN_SPACING - elapsed);
            }
        }
        self.last_call.set(Some(Instant::now()));
    }

    fn send(
        &self,
        method: &str,
        params: &Params,
    ) -> std::result::Result<Value, (AppError, Option<Duration>)> {
        let url = format!("{}/{method}", self.base);
        let mut form: Vec<(&str, &str)> = Vec::with_capacity(params.len() + 1);
        let request = match &self.credentials {
            Credentials::Standard { token } => {
                form.extend(params.iter().map(|(k, v)| (*k, v.as_str())));
                self.http
                    .post(&url)
                    .bearer_auth(token)
                    .header("User-Agent", FALLBACK_UA)
            }
            Credentials::Browser {
                xoxc_token,
                xoxd_token,
                user_agent,
                ..
            } => {
                form.push(("token", xoxc_token.as_str()));
                form.extend(params.iter().map(|(k, v)| (*k, v.as_str())));
                let cookie: String =
                    url::form_urlencoded::byte_serialize(xoxd_token.as_bytes()).collect();
                self.http
                    .post(&url)
                    .header("Cookie", format!("d={cookie}"))
                    .header("Origin", "https://app.slack.com")
                    // Replaying the captured UA matters: strict workspaces sign the
                    // session out everywhere when it reappears under a different one.
                    .header("User-Agent", user_agent.as_deref().unwrap_or(FALLBACK_UA))
            }
        };
        let response = request
            .form(&form)
            .send()
            .map_err(|e| (AppError::network(format!("{method}: {e}")), None))?;
        let status = response.status();
        if status.as_u16() == 429 {
            let wait = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| Duration::from_secs(s.min(60)));
            let mut err = AppError::new("rate_limited", format!("{method}: rate limited by Slack"));
            err.retryable = true;
            return Err((err, wait));
        }
        if status.is_redirection() {
            return Err((
                AppError::auth(format!(
                    "{method}: Slack redirected (session likely expired); log in again"
                )),
                None,
            ));
        }
        if status.is_server_error() {
            return Err((AppError::network(format!("{method}: HTTP {status}")), None));
        }
        let body: Value = response.json().map_err(|e| {
            (
                AppError::network(format!("{method}: invalid JSON ({e})")),
                None,
            )
        })?;
        if body.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(body);
        }
        let slack_error = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        Err((AppError::slack(method, slack_error, &body), None))
    }
}

pub fn next_cursor(resp: &Value) -> Option<String> {
    resp.pointer("/response_metadata/next_cursor")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// One-shot HTTP server returning `body`; sends back the raw request it saw.
    fn serve(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            head.push_str(&String::from_utf8(body).unwrap());
            (&stream).write_all(response.as_bytes()).unwrap();
            tx.send(head).unwrap();
        });
        (format!("http://{addr}/api"), rx)
    }

    #[test]
    fn refuses_write_methods() {
        let c = Client::with_base(
            Credentials::Standard {
                token: "xoxp-1".into(),
            },
            "http://127.0.0.1:9".into(),
            false,
        )
        .unwrap();
        for m in [
            "chat.postMessage",
            "conversations.mark",
            "reactions.add",
            "chat.delete",
        ] {
            assert_eq!(c.call(m, vec![]).unwrap_err().code, "forbidden");
        }
    }

    #[test]
    fn standard_uses_bearer() {
        let (base, rx) = serve("200 OK", r#"{"ok":true,"user":"me"}"#);
        let c = Client::with_base(
            Credentials::Standard {
                token: "xoxp-abc".into(),
            },
            base,
            false,
        )
        .unwrap();
        let v = c.call("auth.test", vec![("x", "1".into())]).unwrap();
        assert_eq!(v["user"], "me");
        let req = rx.recv().unwrap().to_lowercase();
        assert!(req.starts_with("post /api/auth.test"));
        assert!(req.contains("authorization: bearer xoxp-abc"));
        assert!(req.ends_with("x=1"));
    }

    #[test]
    fn browser_sends_token_in_form_and_cookie() {
        let (base, rx) = serve("200 OK", r#"{"ok":true}"#);
        let creds = Credentials::Browser {
            xoxc_token: "xoxc-1".into(),
            xoxd_token: "xoxd-a/b=".into(),
            workspace_url: "https://a.slack.com".into(),
            user_agent: Some("UA/1".into()),
        };
        Client::with_base(creds, base, false)
            .unwrap()
            .call("client.counts", vec![("q", "v".into())])
            .unwrap();
        let req = rx.recv().unwrap();
        let lower = req.to_lowercase();
        assert!(lower.contains("cookie: d=xoxd-a%2fb%3d"));
        assert!(lower.contains("origin: https://app.slack.com"));
        assert!(lower.contains("user-agent: ua/1"));
        assert!(!lower.contains("authorization"));
        assert!(req.ends_with("token=xoxc-1&q=v"));
    }

    #[test]
    fn ok_false_maps_to_typed_error() {
        let (base, _rx) = serve("200 OK", r#"{"ok":false,"error":"channel_not_found"}"#);
        let c = Client::with_base(
            Credentials::Standard {
                token: "xoxp".into(),
            },
            base,
            false,
        )
        .unwrap();
        let err = c.call("conversations.info", vec![]).unwrap_err();
        assert_eq!((err.code.as_str(), err.exit_code()), ("not_found", 4));
    }

    #[test]
    fn redirect_is_auth_error_not_followed() {
        let (base, _rx) = serve("302 Found\r\nLocation: https://evil.example", "");
        let c = Client::with_base(
            Credentials::Standard {
                token: "xoxp".into(),
            },
            base,
            false,
        )
        .unwrap();
        assert_eq!(
            c.call("auth.test", vec![]).unwrap_err().code,
            "authentication"
        );
    }
}
