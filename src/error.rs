use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Box<Value>,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: Box::new(json!({})),
        }
    }

    pub fn input(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::new("authentication", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn ambiguous(message: impl Into<String>, candidates: Value) -> Self {
        Self::new("ambiguous", message).with_details(json!({ "candidates": candidates }))
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new("configuration", message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        let mut err = Self::new("network", message);
        err.retryable = true;
        err
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Box::new(details);
        self
    }

    /// Maps a Slack `ok:false` error string onto a stable code with a hint.
    pub fn slack(method: &str, slack_error: &str, payload: &Value) -> Self {
        let (code, hint) = match slack_error {
            "invalid_auth" | "not_authed" | "token_revoked" | "token_expired"
            | "account_inactive" => (
                "authentication",
                "credentials are invalid or expired; run `slk auth login` again",
            ),
            "missing_scope" => (
                "permission",
                "the token lacks a required scope for this method",
            ),
            "not_allowed_token_type" => (
                "permission",
                "this method needs a user token (xoxp) or browser session (xoxc), not a bot token",
            ),
            "channel_not_found" | "user_not_found" | "thread_not_found" | "message_not_found"
            | "users_not_found" => (
                "not_found",
                "the target does not exist or is not visible to you",
            ),
            "ratelimited" => ("rate_limited", "Slack rate limit hit; retry later"),
            "invalid_cursor" | "invalid_ts_oldest" | "invalid_ts_latest" | "invalid_arguments" => {
                ("invalid_input", "Slack rejected the arguments")
            }
            _ => ("slack_api", "Slack returned an error"),
        };
        let mut err = Self::new(code, format!("{method}: {slack_error} ({hint})"));
        err.retryable = code == "rate_limited";
        let mut details = json!({ "method": method, "slack_error": slack_error });
        if let Some(needed) = payload.get("needed") {
            details["needed"] = needed.clone();
        }
        err.details = Box::new(details);
        err
    }

    pub fn exit_code(&self) -> i32 {
        match self.code.as_str() {
            "invalid_input" => 2,
            "authentication" | "permission" => 3,
            "not_found" => 4,
            "ambiguous" => 5,
            "rate_limited" => 6,
            "network" => 7,
            "io" | "configuration" => 9,
            _ => 8,
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::new("io", format!("I/O error: {err}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        Self::new("io", format!("JSON error: {err}"))
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
