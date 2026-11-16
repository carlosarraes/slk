use crate::resolve::Users;
use regex::{Captures, Regex};
use serde_json::{Value, json};
use std::sync::LazyLock;

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>]+)>").unwrap());
static USER_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<@([UWB][A-Z0-9]+)(?:\|[^>]*)?>").unwrap());

/// User IDs mentioned in a message's text, so they can be fetched in one batch.
pub fn mentioned_users(text: &str) -> impl Iterator<Item = &str> {
    USER_REF
        .captures_iter(text)
        .map(|c| c.get(1).unwrap().as_str())
}

/// Rewrites Slack's `<...>` markup into plain text an agent can read without lookups.
pub fn plain_text(text: &str, users: &Users) -> String {
    let replaced = TOKEN.replace_all(text, |c: &Captures| {
        let inner = &c[1];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        if let Some(id) = target.strip_prefix('@') {
            let name = users.get(id).map(|u| u.label().to_string());
            return format!(
                "@{}",
                name.or(label.map(str::to_string))
                    .unwrap_or_else(|| id.to_string())
            );
        }
        if let Some(id) = target.strip_prefix('#') {
            return format!("#{}", label.unwrap_or(id));
        }
        if let Some(special) = target.strip_prefix('!') {
            if let Some(l) = label {
                return l.to_string();
            }
            let word = special.split('^').next().unwrap_or(special);
            return format!("@{word}");
        }
        match label {
            Some(l) if l != target && !target.ends_with(l) => format!("{l} ({target})"),
            _ => target.trim_start_matches("mailto:").to_string(),
        }
    });
    replaced
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn author(raw: &Value, users: &Users) -> String {
    if let Some(u) = raw["user"].as_str().and_then(|id| users.get(id)) {
        return u.label().to_string();
    }
    for v in [
        &raw["user_profile"]["real_name"],
        &raw["username"],
        &raw["bot_profile"]["name"],
        &raw["user"],
    ] {
        if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    "unknown".to_string()
}

/// Reduces a raw Slack message to what an agent needs to read and cite it.
pub fn slim_message(raw: &Value, users: &Users) -> Value {
    let text = plain_text(raw["text"].as_str().unwrap_or_default(), users);
    let mut out = json!({
        "ts": raw["ts"],
        "user": raw["user"].as_str().or(raw["bot_id"].as_str()),
        "author": author(raw, users),
        "text": text,
    });
    for key in ["thread_ts", "reply_count", "latest_reply", "subtype"] {
        if !raw[key].is_null() {
            out[key] = raw[key].clone();
        }
    }
    if raw["edited"].is_object() {
        out["edited"] = json!(true);
    }
    if let Some(r) = raw["reactions"].as_array().filter(|r| !r.is_empty()) {
        out["reactions"] = r
            .iter()
            .map(|x| json!({ "name": x["name"], "count": x["count"] }))
            .collect();
    }
    if let Some(f) = raw["files"].as_array().filter(|f| !f.is_empty()) {
        out["files"] = f
            .iter()
            .map(|x| {
                json!({
                    "name": x["name"].as_str().or(x["title"].as_str()),
                    "mimetype": x["mimetype"],
                    "permalink": x["permalink"],
                })
            })
            .collect();
    }
    // Bot/integration messages often carry their content only in attachments.
    let attachments: Vec<Value> = raw["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| slim_attachment(a, users))
        .collect();
    if !attachments.is_empty() {
        out["attachments"] = attachments.into();
    }
    out
}

fn slim_attachment(a: &Value, users: &Users) -> Option<Value> {
    let text = a["text"]
        .as_str()
        .or(a["fallback"].as_str())
        .map(|t| plain_text(t, users))
        .filter(|t| !t.is_empty());
    let title = a["title"].as_str().filter(|t| !t.is_empty());
    if text.is_none() && title.is_none() {
        return None;
    }
    let mut v = json!({});
    if let Some(t) = title {
        v["title"] = json!(t);
    }
    if let Some(t) = text {
        v["text"] = json!(t);
    }
    if let Some(u) = a["title_link"].as_str().or(a["from_url"].as_str()) {
        v["url"] = json!(u);
    }
    Some(v)
}

pub fn fmt_ts(ts: &str) -> String {
    let secs = ts
        .split('.')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| ts.to_string())
}

/// Text rendering of a slimmed message, indented under its author line.
pub fn message_text(m: &Value) -> String {
    let mut s = format!(
        "[{}] {}  (ts {})\n",
        fmt_ts(m["ts"].as_str().unwrap_or_default()),
        m["author"].as_str().unwrap_or("unknown"),
        m["ts"].as_str().unwrap_or_default()
    );
    for line in m["text"].as_str().unwrap_or_default().lines() {
        s.push_str("  ");
        s.push_str(line);
        s.push('\n');
    }
    if let Some(n) = m["reply_count"].as_u64().filter(|n| *n > 0) {
        s.push_str(&format!("  ↳ {n} replies\n"));
    }
    for f in m["files"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "  📎 {} {}\n",
            f["name"].as_str().unwrap_or("file"),
            f["permalink"].as_str().unwrap_or_default()
        ));
    }
    for a in m["attachments"].as_array().into_iter().flatten() {
        let title = a["title"]
            .as_str()
            .map(|t| format!("{t}: "))
            .unwrap_or_default();
        let body = a["text"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "\n    ");
        s.push_str(&format!("  ▸ {title}{body}\n"));
    }
    if let Some(r) = m["reactions"].as_array().filter(|r| !r.is_empty()) {
        let list: Vec<String> = r
            .iter()
            .map(|x| format!(":{}: {}", x["name"].as_str().unwrap_or("?"), x["count"]))
            .collect();
        s.push_str(&format!("  {}\n", list.join("  ")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_markup() {
        let users = Users::default();
        let t = "hi <@U123ABC|bob> see <#C1|general> <!here> <!subteam^S1|@devs> \
                 <https://x.io|docs> <https://y.io> <mailto:a@b.c|a@b.c> a &lt;b&gt; &amp;";
        assert_eq!(
            plain_text(t, &users),
            "hi @bob see #general @here @devs docs (https://x.io) https://y.io a@b.c a <b> &"
        );
        assert_eq!(plain_text("<@U999ZZZ>", &users), "@U999ZZZ");
    }

    #[test]
    fn slims_message_fields() {
        let raw = json!({
            "ts": "1.0", "user": "U1AAAAA", "text": "hi", "thread_ts": "1.0", "reply_count": 2,
            "edited": {"user": "U1AAAAA"}, "blocks": [{"type": "rich_text"}],
            "reactions": [{"name": "+1", "count": 3, "users": ["U2"]}],
            "files": [{"name": "a.png", "mimetype": "image/png", "permalink": "https://p", "url_private": "x"}],
            "attachments": [{"fallback": "CI failed", "title": "build", "title_link": "https://ci"}, {"color": "f00"}],
        });
        let m = slim_message(&raw, &Users::default());
        assert_eq!(m["reply_count"], 2);
        assert_eq!(m["edited"], true);
        assert_eq!(m["reactions"], json!([{"name": "+1", "count": 3}]));
        assert_eq!(
            m["files"][0],
            json!({"name": "a.png", "mimetype": "image/png", "permalink": "https://p"})
        );
        assert_eq!(
            m["attachments"],
            json!([{"title": "build", "text": "CI failed", "url": "https://ci"}])
        );
        assert!(m.get("blocks").is_none());
        assert!(message_text(&m).contains("▸ build: CI failed"));
    }

    #[test]
    fn finds_mentions() {
        let ids: Vec<_> = mentioned_users("<@U1AAAAA> and <@W2BBBBB|x>").collect();
        assert_eq!(ids, ["U1AAAAA", "W2BBBBB"]);
    }
}
