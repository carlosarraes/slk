use crate::client::Client;
use crate::error::{AppError, Result};
use crate::permalink;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Above this many unknown users one paged `users.list` beats N `users.info` calls.
const BULK_THRESHOLD: usize = 20;

fn is_id(s: &str, prefixes: &[char]) -> bool {
    s.len() >= 7
        && s.starts_with(prefixes)
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

pub struct ChannelTarget {
    pub id: String,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
}

/// Accepts a channel ID, `#name`, `name`, `@user` (their DM) or a permalink.
pub fn channel(client: &Client, users: &mut Users, input: &str) -> Result<ChannelTarget> {
    if permalink::looks_like_url(input) {
        let p = permalink::parse(input)?;
        return Ok(ChannelTarget {
            id: p.channel,
            ts: p.ts,
            thread_ts: p.thread_ts,
        });
    }
    let plain = |id: String| ChannelTarget {
        id,
        ts: None,
        thread_ts: None,
    };
    if is_id(input, &['C', 'G', 'D']) {
        return Ok(plain(input.to_string()));
    }
    if let Some(user) = input.strip_prefix('@') {
        let user_id = users.resolve_id(client, user)?;
        let ims = client.paginate(
            "conversations.list",
            vec![("types", "im".into()), ("limit", "1000".into())],
            "channels",
            usize::MAX,
        )?;
        return ims
            .iter()
            .find(|c| c["user"] == user_id.as_str())
            .and_then(|c| c["id"].as_str())
            .map(|id| plain(id.to_string()))
            .ok_or_else(|| AppError::not_found(format!("no DM with @{user} found")));
    }
    let name = input.trim_start_matches('#').to_lowercase();
    let all = client.paginate(
        "conversations.list",
        vec![
            ("types", "public_channel,private_channel,mpim".into()),
            ("limit", "1000".into()),
        ],
        "channels",
        usize::MAX,
    )?;
    all.iter()
        .find(|c| c["name"].as_str().is_some_and(|n| n.to_lowercase() == name))
        .and_then(|c| c["id"].as_str())
        .map(|id| plain(id.to_string()))
        .ok_or_else(|| AppError::not_found(format!("channel #{name} not found or not visible")))
}

#[derive(Clone)]
pub struct UserLite {
    pub name: String,
    pub real_name: String,
}

impl UserLite {
    fn from_user(u: &Value) -> Self {
        let profile = &u["profile"];
        let pick = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
        let name = pick(&profile["display_name"])
            .or_else(|| pick(&u["name"]))
            .unwrap_or_default();
        let real_name = pick(&u["real_name"])
            .or_else(|| pick(&profile["real_name"]))
            .unwrap_or_else(|| name.clone());
        Self { name, real_name }
    }

    pub fn label(&self) -> &str {
        if self.real_name.is_empty() {
            &self.name
        } else {
            &self.real_name
        }
    }
}

/// In-process user directory; one run never looks up the same user twice.
#[derive(Default)]
pub struct Users {
    known: HashMap<String, Option<UserLite>>,
    all: Option<Vec<Value>>,
}

impl Users {
    pub fn get(&self, id: &str) -> Option<&UserLite> {
        self.known.get(id).and_then(Option::as_ref)
    }

    pub fn ensure<'a>(&mut self, client: &Client, ids: impl IntoIterator<Item = &'a str>) {
        let mut missing: Vec<&str> = ids
            .into_iter()
            .filter(|id| !self.known.contains_key(*id))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        if missing.len() > BULK_THRESHOLD && self.load_all(client).is_ok() {
            for id in missing {
                self.known.entry(id.to_string()).or_insert(None);
            }
            return;
        }
        for id in missing {
            let user = client
                .call("users.info", vec![("user", id.to_string())])
                .ok()
                .map(|r| UserLite::from_user(&r["user"]));
            self.known.insert(id.to_string(), user);
        }
    }

    fn load_all(&mut self, client: &Client) -> Result<&[Value]> {
        if self.all.is_none() {
            let list = client.paginate(
                "users.list",
                vec![("limit", "1000".into())],
                "members",
                usize::MAX,
            )?;
            for u in &list {
                if let Some(id) = u["id"].as_str() {
                    self.known
                        .insert(id.to_string(), Some(UserLite::from_user(u)));
                }
            }
            self.all = Some(list);
        }
        Ok(self.all.as_deref().unwrap_or_default())
    }

    /// Accepts a user ID, email, `@handle`, display name or real name.
    pub fn resolve_id(&mut self, client: &Client, input: &str) -> Result<String> {
        let input = input.trim_start_matches('@');
        if is_id(input, &['U', 'W', 'B']) {
            return Ok(input.to_string());
        }
        if input.contains('@') {
            let r = client.call("users.lookupByEmail", vec![("email", input.to_string())])?;
            return r["user"]["id"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| AppError::not_found(format!("no user with email {input}")));
        }
        let wanted = input.to_lowercase();
        let all = self.load_all(client)?;
        let hits: Vec<&Value> = all
            .iter()
            .filter(|u| {
                let p = &u["profile"];
                [
                    &u["name"],
                    &p["display_name"],
                    &u["real_name"],
                    &p["real_name"],
                ]
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.to_lowercase() == wanted))
            })
            .collect();
        match hits.as_slice() {
            [one] => Ok(one["id"].as_str().unwrap_or_default().to_string()),
            [] => Err(AppError::not_found(format!("user '{input}' not found"))),
            many => Err(AppError::ambiguous(
                format!("user '{input}' matches several people; use an ID"),
                many.iter()
                    .map(|u| json!({"id": u["id"], "name": u["name"], "real_name": u["real_name"]}))
                    .collect::<Vec<_>>()
                    .into(),
            )),
        }
    }
}
