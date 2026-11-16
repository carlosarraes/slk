use crate::cli::{Ctx, read_limit};
use crate::client::Client;
use crate::commands::conversations::{messages_text, slim_all};
use crate::error::{AppError, Result};
use crate::resolve::Users;
use clap::Args;
use serde_json::{Value, json};
use std::cmp::Ordering;

const MAX_THREAD_PAGES: usize = 5;

#[derive(Args)]
pub struct UnreadArgs {
    /// Comma list of channels,dms,groups
    #[arg(long, default_value = "channels,dms,groups")]
    types: String,
    /// Also fetch the unread messages themselves
    #[arg(long)]
    messages: bool,
    /// With --messages: how many conversations to read (mentions first)
    #[arg(long, default_value_t = 10)]
    max_conversations: usize,
    /// With --messages: max messages per conversation
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

pub fn unread(ctx: &Ctx, client: &Client, users: &mut Users, args: UnreadArgs) -> Result<()> {
    let limit = read_limit(args.limit, 999)?;
    let (mut channels, threads) = if client.is_browser() {
        browser_counts(client, users)?
    } else {
        (standard_counts(client)?, Value::Null)
    };
    let wanted: Vec<&str> = args.types.split(',').map(str::trim).collect();
    channels.retain(|c| {
        let (im, mpim) = (c["is_im"] == true, c["is_mpim"] == true);
        (wanted.contains(&"channels") && !im && !mpim)
            || (wanted.contains(&"dms") && im)
            || (wanted.contains(&"groups") && mpim)
    });
    channels.sort_by(|a, b| {
        let m = |c: &Value| c["mention_count"].as_u64().unwrap_or(0);
        m(b).cmp(&m(a))
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });

    if args.messages {
        for c in channels.iter_mut().take(args.max_conversations) {
            let id = c["id"].as_str().unwrap_or_default().to_string();
            let last_read = c["last_read"]
                .as_str()
                .filter(|ts| usable_cursor(ts))
                .map(str::to_string);
            let mut params = vec![("channel", id), ("limit", limit.to_string())];
            if let Some(lr) = &last_read {
                params.push(("oldest", lr.clone()));
            }
            let r = client.call("conversations.history", params)?;
            let (raw, capped) = cut_at_last_read(
                r["messages"].as_array().cloned().unwrap_or_default(),
                last_read.as_deref(),
                limit,
            );
            c["messages"] = json!(slim_all(client, users, &raw));
            c["has_more"] = json!(capped || r["has_more"] == true);
        }
    }

    let v = json!({ "channels": channels, "threads": threads });
    ctx.out.emit(v, |v| {
        let list = v["channels"].as_array().cloned().unwrap_or_default();
        let mut s = if list.is_empty() {
            "All caught up! No unread conversations.\n".to_string()
        } else {
            format!("{} conversations with unread messages\n", list.len())
        };
        for c in &list {
            let mentions = c["mention_count"].as_u64().unwrap_or(0);
            s.push_str(&format!(
                "  {:<12} {}{}\n",
                c["id"].as_str().unwrap_or_default(),
                c["name"].as_str().unwrap_or_default(),
                if mentions > 0 {
                    format!("  ({mentions} mentions)")
                } else {
                    String::new()
                }
            ));
            if c["messages"].is_array() {
                for line in messages_text(c).lines() {
                    s.push_str(&format!("      {line}\n"));
                }
            }
        }
        if v["threads"]["has_unreads"] == true {
            s.push_str("Unread thread replies: run `slk conversations threads`\n");
        }
        s
    });
    Ok(())
}

fn browser_counts(client: &Client, users: &mut Users) -> Result<(Vec<Value>, Value)> {
    let r = client.call("client.counts", vec![])?;
    let mut out = Vec::new();
    for key in ["channels", "mpims", "ims"] {
        for c in r[key].as_array().into_iter().flatten() {
            let mentions = c["mention_count"].as_u64().unwrap_or(0);
            if c["has_unreads"] != true && mentions == 0 {
                continue;
            }
            let id = c["id"].as_str().unwrap_or_default().to_string();
            let mut entry = json!({ "id": id, "mention_count": mentions, "has_unreads": c["has_unreads"] == true });
            for k in ["last_read", "latest"] {
                if c[k].is_string() {
                    entry[k] = c[k].clone();
                }
            }
            describe(client, users, &mut entry, &id);
            out.push(entry);
        }
    }
    let threads = json!({
        "has_unreads": r["threads"]["has_unreads"] == true,
        "mention_count": r["threads"]["mention_count"].as_u64().unwrap_or(0),
    });
    Ok((out, threads))
}

fn describe(client: &Client, users: &mut Users, entry: &mut Value, id: &str) {
    let Ok(info) = client.call("conversations.info", vec![("channel", id.to_string())]) else {
        entry["name"] = json!(id);
        return;
    };
    let ch = &info["channel"];
    for k in ["is_im", "is_mpim", "is_private"] {
        entry[k] = json!(ch[k] == true);
    }
    let name = match ch["user"].as_str().filter(|_| ch["is_im"] == true) {
        Some(uid) => {
            users.ensure(client, [uid]);
            users
                .get(uid)
                .map(|u| format!("@{}", u.label()))
                .unwrap_or_else(|| uid.to_string())
        }
        None => ch["name"].as_str().unwrap_or(id).to_string(),
    };
    entry["name"] = json!(name);
}

fn standard_counts(client: &Client) -> Result<Vec<Value>> {
    let all = client.paginate(
        "conversations.list",
        vec![
            ("types", "public_channel,private_channel,mpim,im".into()),
            ("limit", "1000".into()),
            ("exclude_archived", "true".into()),
        ],
        "channels",
        usize::MAX,
    )?;
    Ok(all
        .iter()
        .filter(|c| {
            (c["is_member"] == true || c["is_im"] == true)
                && (c["unread_count"].as_u64().unwrap_or(0) > 0 || c["unread_count_display"].as_u64().unwrap_or(0) > 0)
        })
        .map(|c| {
            json!({
                "id": c["id"], "name": c["name"].as_str().or(c["user"].as_str()),
                "mention_count": c["mention_count_display"].as_u64().unwrap_or(0),
                "unread_count": c["unread_count_display"].as_u64().or(c["unread_count"].as_u64()).unwrap_or(0),
                "has_unreads": true,
                "is_im": c["is_im"] == true, "is_mpim": c["is_mpim"] == true, "is_private": c["is_private"] == true,
            })
        })
        .collect())
}

/// A never-read conversation reports `0000000000.000000`, which history rejects.
fn usable_cursor(ts: &str) -> bool {
    ts.contains(|c: char| ('1'..='9').contains(&c))
        && ts.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn cmp_ts(a: &str, b: &str) -> Ordering {
    let parse = |s: &str| {
        let (sec, frac) = s.split_once('.').unwrap_or((s, "0"));
        (sec.parse::<u64>().unwrap_or(0), format!("{frac:0<6}"))
    };
    parse(a).cmp(&parse(b))
}

/// Unread part of a page, oldest first, capped at `limit`. With a cursor the
/// earliest unread are kept (reading continues in order); without, the newest.
fn cut_at_last_read(
    mut raw: Vec<Value>,
    last_read: Option<&str>,
    limit: usize,
) -> (Vec<Value>, bool) {
    raw.retain(|m| {
        m["ts"]
            .as_str()
            .is_some_and(|ts| last_read.is_none_or(|lr| cmp_ts(ts, lr) == Ordering::Greater))
    });
    raw.sort_by(|a, b| {
        cmp_ts(
            a["ts"].as_str().unwrap_or_default(),
            b["ts"].as_str().unwrap_or_default(),
        )
    });
    if raw.len() <= limit {
        return (raw, false);
    }
    if last_read.is_some() {
        raw.truncate(limit);
    } else {
        raw.drain(..raw.len() - limit);
    }
    (raw, true)
}

pub fn threads(ctx: &Ctx, client: &Client, users: &mut Users, limit: usize) -> Result<()> {
    if !client.is_browser() {
        return Err(AppError::new(
            "permission",
            "the threads view needs a browser session; run `slk auth parse-curl --login`",
        ));
    }
    let mut out: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut complete = false;
    for _ in 0..MAX_THREAD_PAGES {
        let params = cursor.iter().map(|c| ("current_ts", c.clone())).collect();
        let r = client.call("subscriptions.thread.getView", params)?;
        let Some(page) = r["threads"].as_array() else {
            return Err(AppError::new(
                "slack_api",
                "subscriptions.thread.getView returned an unexpected shape",
            ));
        };
        let mut saw_read = false;
        for t in page {
            let root = &t["root_msg"];
            let replies = t["unread_replies"].as_array().cloned().unwrap_or_default();
            if replies.is_empty() {
                saw_read = true;
                continue;
            }
            let channel = root["channel"].as_str().unwrap_or_default();
            let thread_ts = root["thread_ts"]
                .as_str()
                .or(root["ts"].as_str())
                .unwrap_or_default();
            if out
                .iter()
                .any(|o| o["channel"] == channel && o["thread_ts"] == thread_ts)
            {
                continue;
            }
            let (replies, capped) = cut_at_last_read(replies, None, usize::MAX);
            let mut slim = slim_all(client, users, std::slice::from_ref(root));
            let mut entry = json!({
                "channel": channel,
                "thread_ts": thread_ts,
                "root": slim.remove(0),
                "unread_replies": slim_all(client, users, &replies[..replies.len().min(limit)]),
            });
            if capped || replies.len() > limit {
                entry["has_more"] = json!(true);
            }
            out.push(entry);
        }
        // The view lists unread threads first, so a read one means we're done.
        let last = page.last();
        let next = last
            .and_then(|t| t["root_msg"]["latest_reply"].as_str())
            .filter(|ts| usable_cursor(ts))
            .map(str::to_string);
        if saw_read || r["has_more"] != true || next.is_none() || next == cursor {
            complete = true;
            break;
        }
        cursor = next;
    }
    let v = json!({ "threads": out, "complete": complete });
    ctx.out.emit(v, |v| {
        let list = v["threads"].as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            return "No unread threads.\n".into();
        }
        let mut s = String::new();
        for t in &list {
            s.push_str(&format!(
                "── {} thread {}\n",
                t["channel"].as_str().unwrap_or_default(),
                t["thread_ts"].as_str().unwrap_or_default()
            ));
            s.push_str(&crate::render::message_text(&t["root"]));
            for r in t["unread_replies"].as_array().into_iter().flatten() {
                for line in crate::render::message_text(r).lines() {
                    s.push_str(&format!("    {line}\n"));
                }
            }
        }
        s
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(ts: &[&str]) -> Vec<Value> {
        ts.iter().map(|t| json!({ "ts": t })).collect()
    }

    fn tss(v: &[Value]) -> Vec<&str> {
        v.iter().map(|m| m["ts"].as_str().unwrap()).collect()
    }

    #[test]
    fn cut_keeps_earliest_unread_with_cursor() {
        let (m, more) = cut_at_last_read(msgs(&["5.0", "4.0", "3.0", "2.0"]), Some("2.0"), 2);
        assert_eq!((tss(&m), more), (vec!["3.0", "4.0"], true));
    }

    #[test]
    fn cut_keeps_newest_without_cursor() {
        let (m, more) = cut_at_last_read(msgs(&["3.0", "1.0", "2.0"]), None, 2);
        assert_eq!((tss(&m), more), (vec!["2.0", "3.0"], true));
    }

    #[test]
    fn ts_ordering_handles_fraction_width() {
        assert_eq!(cmp_ts("10.5", "10.000400"), Ordering::Greater);
        assert_eq!(cmp_ts("9.9", "10.0"), Ordering::Less);
    }

    #[test]
    fn zero_cursor_is_unusable() {
        assert!(!usable_cursor("0000000000.000000"));
        assert!(usable_cursor("1700000000.000100"));
    }
}
