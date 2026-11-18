use crate::cli::Ctx;
use crate::client::Client;
use crate::config::{Config, Credentials, Workspace, normalize_workspace_url};
use crate::curl;
use crate::error::{AppError, Result};
use clap::Subcommand;
use serde_json::{Value, json};
use std::io::Read;

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Store a bot (xoxb) or user (xoxp) token
    Login {
        /// Prefer SLK_LOGIN_TOKEN: argv is visible in `ps` and shell history
        #[arg(long, env = "SLK_LOGIN_TOKEN", hide_env_values = true)]
        token: String,
        /// Profile name to store under (default: team name)
        #[arg(long)]
        profile: Option<String>,
    },
    /// Store a browser session (xoxc token + d cookie)
    LoginBrowser {
        /// Prefer the env var: argv is visible in `ps` and shell history
        #[arg(long, env = "SLK_XOXC", hide_env_values = true)]
        xoxc: String,
        #[arg(long, env = "SLK_XOXD", hide_env_values = true)]
        xoxd: String,
        #[arg(long)]
        workspace_url: String,
        /// Browser User-Agent to replay (recommended)
        #[arg(long)]
        user_agent: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Parse a DevTools "Copy as cURL" of a Slack /api/ request (stdin recommended)
    ParseCurl {
        /// The cURL text; omit to read stdin (keeps secrets out of argv)
        curl: Option<String>,
        /// Store the parsed session as a workspace
        #[arg(long)]
        login: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// List stored workspaces
    List {
        /// Verify each stored credential with auth.test
        #[arg(long)]
        check: bool,
    },
    /// Show the identity behind the selected workspace
    Whoami,
    /// Make a workspace the default
    SetDefault { workspace: String },
    /// Forget a stored workspace
    Remove { workspace: String },
}

pub fn run(ctx: &Ctx, cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Login { token, profile } => {
            if !token.starts_with("xox") {
                return Err(AppError::input("token must start with xoxb- or xoxp-"));
            }
            login(ctx, Credentials::Standard { token }, profile)
        }
        AuthCmd::LoginBrowser {
            xoxc,
            xoxd,
            workspace_url,
            user_agent,
            profile,
        } => {
            let creds = Credentials::Browser {
                xoxc_token: xoxc,
                xoxd_token: xoxd,
                workspace_url: normalize_workspace_url(&workspace_url)?,
                user_agent,
            };
            login(ctx, creds, profile)
        }
        AuthCmd::ParseCurl {
            curl,
            login: store,
            profile,
        } => {
            let input = match curl {
                Some(c) => c,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let s = curl::parse(&input)?;
            if store {
                let creds = Credentials::Browser {
                    xoxc_token: s.xoxc,
                    xoxd_token: s.xoxd,
                    workspace_url: s.workspace_url,
                    user_agent: s.user_agent,
                };
                return login(ctx, creds, profile);
            }
            let v = json!({
                "workspace_url": s.workspace_url,
                "xoxc": redact(&s.xoxc),
                "xoxd": redact(&s.xoxd),
                "user_agent": s.user_agent,
            });
            ctx.out.emit(v, |v| {
                format!(
                    "workspace  {}\nxoxc       {}\nxoxd       {}\nuser-agent {}\n\nRe-run with --login to store it.\n",
                    v["workspace_url"].as_str().unwrap_or_default(),
                    v["xoxc"].as_str().unwrap_or_default(),
                    v["xoxd"].as_str().unwrap_or_default(),
                    v["user_agent"].as_str().unwrap_or("(none)")
                )
            });
            Ok(())
        }
        AuthCmd::List { check } => {
            let cfg = Config::load()?;
            let mut rows = Vec::new();
            for (key, ws) in &cfg.workspaces {
                let mut row = json!({
                    "profile": key,
                    "team_id": ws.team_id,
                    "team_name": ws.team_name,
                    "user": ws.user,
                    "auth_type": ws.credentials.kind(),
                    "default": cfg.default_workspace.as_deref() == Some(key.as_str()),
                });
                if check {
                    let outcome = Client::new(ws.credentials.clone(), ctx.verbose)
                        .and_then(|c| c.call_once("auth.test", vec![]));
                    row["valid"] = json!(outcome.is_ok());
                    match outcome {
                        Ok(_) => row["status"] = json!("ok"),
                        Err(e) => {
                            row["status"] = json!(check_status(&e));
                            row["error"] = json!(e.message);
                        }
                    }
                }
                rows.push(row);
            }
            ctx.out.emit(json!({ "workspaces": rows }), |v| {
                let rows = v["workspaces"].as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    return "No workspaces. Run `slk auth login --token ...`.\n".into();
                }
                rows.iter()
                    .map(|r| {
                        format!(
                            "{} {:<16} {:<24} {:<8} {}{}\n",
                            if r["default"] == true { "*" } else { " " },
                            r["profile"].as_str().unwrap_or_default(),
                            r["team_name"].as_str().unwrap_or_default(),
                            r["auth_type"].as_str().unwrap_or_default(),
                            r["user"].as_str().unwrap_or_default(),
                            match r.get("valid") {
                                Some(Value::Bool(true)) => "  ✓".to_string(),
                                Some(_) => format!(
                                    "  ✗ {}: {}",
                                    r["status"].as_str().unwrap_or_default(),
                                    r["error"].as_str().unwrap_or_default()
                                ),
                                None => String::new(),
                            }
                        )
                    })
                    .collect()
            });
            // Like `ntn auth list --check`: failures live in the rows, the exit stays 0.
            Ok(())
        }
        AuthCmd::Whoami => {
            let r = ctx.client()?.call("auth.test", vec![])?;
            let v = json!({
                "team": r["team"], "team_id": r["team_id"], "url": r["url"],
                "user": r["user"], "user_id": r["user_id"], "bot_id": r.get("bot_id"),
            });
            ctx.out.emit(v, |v| {
                format!(
                    "{} ({}) in {} ({})\n",
                    v["user"].as_str().unwrap_or_default(),
                    v["user_id"].as_str().unwrap_or_default(),
                    v["team"].as_str().unwrap_or_default(),
                    v["url"].as_str().unwrap_or_default()
                )
            });
            Ok(())
        }
        AuthCmd::SetDefault { workspace } => {
            let mut cfg = Config::load()?;
            let (key, _) = cfg.select(Some(&workspace))?;
            cfg.default_workspace = Some(key.clone());
            cfg.save()?;
            ctx.out.emit(json!({ "default_workspace": key }), |_| {
                format!("Default workspace: {key}\n")
            });
            Ok(())
        }
        AuthCmd::Remove { workspace } => {
            let mut cfg = Config::load()?;
            let (key, _) = cfg.select(Some(&workspace))?;
            cfg.workspaces.remove(&key);
            if cfg.default_workspace.as_deref() == Some(key.as_str()) {
                cfg.default_workspace = None;
            }
            cfg.save()?;
            ctx.out
                .emit(json!({ "removed": key }), |_| format!("Removed {key}\n"));
            Ok(())
        }
    }
}

fn login(ctx: &Ctx, credentials: Credentials, profile: Option<String>) -> Result<()> {
    let r = Client::new(credentials.clone(), ctx.verbose)?.call("auth.test", vec![])?;
    let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
    let ws = Workspace {
        team_id: s("team_id"),
        team_name: s("team"),
        user_id: s("user_id"),
        user: s("user"),
        credentials,
    };
    let key = profile.unwrap_or_else(|| slug(&ws.team_name, &ws.team_id));
    let mut cfg = Config::load()?;
    cfg.workspaces.insert(key.clone(), ws.clone());
    if cfg.default_workspace.is_none() {
        cfg.default_workspace = Some(key.clone());
    }
    cfg.save()?;
    let v = json!({
        "profile": key, "team": ws.team_name, "team_id": ws.team_id,
        "user": ws.user, "auth_type": ws.credentials.kind(),
    });
    ctx.out.emit(v, |v| {
        format!(
            "Logged in to {} as {} (profile {}, {})\n",
            v["team"].as_str().unwrap_or_default(),
            v["user"].as_str().unwrap_or_default(),
            v["profile"].as_str().unwrap_or_default(),
            v["auth_type"].as_str().unwrap_or_default()
        )
    });
    Ok(())
}

/// Coarse, stable label for a failed `auth list --check` probe.
fn check_status(err: &AppError) -> &'static str {
    match err.details["slack_error"].as_str() {
        Some("token_expired") => "expired",
        Some("token_revoked" | "account_inactive") => "revoked",
        Some("invalid_auth" | "not_authed") => "invalid",
        _ => match err.code.as_str() {
            // Slack answers a dead browser session with a redirect to sign-in.
            "authentication" => "expired",
            "network" => "network",
            _ => "error",
        },
    }
}

fn slug(name: &str, fallback: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn redact(secret: &str) -> String {
    let head: String = secret.chars().take(9).collect();
    format!("{head}…({} chars)", secret.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_status_labels() {
        let slack = |e| AppError::slack("auth.test", e, &json!({}));
        assert_eq!(check_status(&slack("token_expired")), "expired");
        assert_eq!(check_status(&slack("token_revoked")), "revoked");
        assert_eq!(check_status(&slack("account_inactive")), "revoked");
        assert_eq!(check_status(&slack("invalid_auth")), "invalid");
        assert_eq!(check_status(&slack("ratelimited")), "error");
        assert_eq!(check_status(&AppError::network("x")), "network");
        assert_eq!(check_status(&AppError::auth("redirected")), "expired");
        assert_eq!(check_status(&AppError::config("bad url")), "error");
    }
}
