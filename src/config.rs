use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "auth_type", rename_all = "snake_case")]
pub enum Credentials {
    Standard {
        token: String,
    },
    Browser {
        xoxc_token: String,
        xoxd_token: String,
        workspace_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_agent: Option<String>,
    },
}

impl Credentials {
    pub fn kind(&self) -> &'static str {
        match self {
            Credentials::Standard { token } if token.starts_with("xoxb-") => "bot",
            Credentials::Standard { .. } => "user",
            Credentials::Browser { .. } => "browser",
        }
    }

    pub fn is_browser(&self) -> bool {
        matches!(self, Credentials::Browser { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub team_id: String,
    pub team_name: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user: String,
    #[serde(flatten)]
    pub credentials: Credentials,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace: Option<String>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SLK_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("slk"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| AppError::config("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config").join("slk"))
}

fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("workspaces.json"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| AppError::config(format!("{} is not valid: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic write: temp file in the same dir (0600), then rename.
    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(dir.join("workspaces.json"))
            .map_err(|e| AppError::from(e.error))?;
        Ok(())
    }

    /// Resolve a selector (profile key, team id or team name) to a stored workspace.
    pub fn select(&self, selector: Option<&str>) -> Result<(String, Workspace)> {
        let selector = match selector.or(self.default_workspace.as_deref()) {
            Some(s) => s,
            None if self.workspaces.len() == 1 => {
                let (k, w) = self.workspaces.iter().next().unwrap();
                return Ok((k.clone(), w.clone()));
            }
            None if self.workspaces.is_empty() => {
                return Err(AppError::auth(
                    "no workspace configured; run `slk auth login --token ...` or set SLK_TOKEN",
                ));
            }
            None => {
                return Err(AppError::input(
                    "several workspaces configured; pass --workspace or run `slk auth set-default`",
                ));
            }
        };
        if let Some(w) = self.workspaces.get(selector) {
            return Ok((selector.to_string(), w.clone()));
        }
        let lower = selector.to_lowercase();
        let hits: Vec<_> = self
            .workspaces
            .iter()
            .filter(|(_, w)| w.team_id == selector || w.team_name.to_lowercase() == lower)
            .collect();
        match hits.as_slice() {
            [(k, w)] => Ok(((*k).clone(), (*w).clone())),
            [] => Err(AppError::not_found(format!(
                "workspace '{selector}' not found; see `slk auth list`"
            ))),
            many => Err(AppError::ambiguous(
                format!("workspace '{selector}' matches several profiles"),
                many.iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>()
                    .into(),
            )),
        }
    }
}

/// Browser sessions must only ever talk to an https *.slack.com origin.
pub fn normalize_workspace_url(input: &str) -> Result<String> {
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&with_scheme)
        .map_err(|_| AppError::input(format!("invalid workspace url: {input}")))?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https" || !host.ends_with(".slack.com") || host == ".slack.com" {
        return Err(AppError::input(format!(
            "workspace url must be https://<team>.slack.com, got {input}"
        )));
    }
    Ok(format!("https://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, name: &str) -> Workspace {
        Workspace {
            team_id: id.into(),
            team_name: name.into(),
            user_id: String::new(),
            user: String::new(),
            credentials: Credentials::Standard {
                token: "xoxp-1".into(),
            },
        }
    }

    #[test]
    fn select_by_key_id_name_and_default() {
        let mut cfg = Config::default();
        cfg.workspaces.insert("acme".into(), ws("T1", "Acme"));
        cfg.workspaces.insert("other".into(), ws("T2", "Other"));
        assert_eq!(cfg.select(Some("acme")).unwrap().0, "acme");
        assert_eq!(cfg.select(Some("T2")).unwrap().0, "other");
        assert_eq!(cfg.select(Some("ACME")).unwrap().0, "acme");
        assert_eq!(cfg.select(None).unwrap_err().code, "invalid_input");
        cfg.default_workspace = Some("other".into());
        assert_eq!(cfg.select(None).unwrap().0, "other");
        assert_eq!(cfg.select(Some("nope")).unwrap_err().code, "not_found");
    }

    #[test]
    fn workspace_url_must_be_slack_https() {
        assert_eq!(
            normalize_workspace_url("acme.slack.com").unwrap(),
            "https://acme.slack.com"
        );
        assert_eq!(
            normalize_workspace_url("https://acme.slack.com/client/T1").unwrap(),
            "https://acme.slack.com"
        );
        assert!(normalize_workspace_url("http://acme.slack.com").is_err());
        assert!(normalize_workspace_url("https://evil.com").is_err());
        assert!(normalize_workspace_url("https://slack.com.evil.com").is_err());
    }

    #[test]
    fn credentials_roundtrip_tagged() {
        let w = Workspace {
            credentials: Credentials::Browser {
                xoxc_token: "xoxc-1".into(),
                xoxd_token: "xoxd-1".into(),
                workspace_url: "https://a.slack.com".into(),
                user_agent: None,
            },
            ..ws("T1", "A")
        };
        let raw = serde_json::to_value(&w).unwrap();
        assert_eq!(raw["auth_type"], "browser");
        let back: Workspace = serde_json::from_value(raw).unwrap();
        assert_eq!(back.credentials, w.credentials);
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test-only env mutation; this is the only test touching SLK_CONFIG_DIR.
        unsafe { std::env::set_var("SLK_CONFIG_DIR", dir.path()) };
        let mut cfg = Config::default();
        cfg.workspaces.insert("acme".into(), ws("T1", "Acme"));
        cfg.save().unwrap();
        let meta = std::fs::metadata(dir.path().join("workspaces.json")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(Config::load().unwrap().workspaces.len(), 1);
    }
}
