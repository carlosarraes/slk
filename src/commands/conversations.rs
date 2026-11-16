use crate::cli::{Ctx, read_limit};
use crate::client::{Client, next_cursor};
use crate::commands::unread;
use crate::error::{AppError, Result};
use crate::permalink;
use crate::render::{self, mentioned_users, slim_message};
use crate::resolve::{self, Users};
use clap::Subcommand;
use serde_json::{Value, json};

#[derive(Subcommand)]
pub enum ConvCmd {
    /// List channels, DMs and group DMs you can see
    List {
        /// Comma list of public_channel,private_channel,mpim,im
        #[arg(long, default_value = "public_channel,private_channel,mpim,im")]
        types: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        exclude_archived: bool,
        /// Resume from a previous next_cursor
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read channel history, or a thread when a thread ts / thread permalink is given
    Read {
        /// Channel ID, #name, @user (DM) or message permalink
        channel: String,
        /// Read this thread instead of channel history
        #[arg(long)]
        thread_ts: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Only messages after this ts
        #[arg(long)]
        oldest: Option<String>,
        /// Only messages before this ts
        #[arg(long)]
        latest: Option<String>,
        /// Only messages since (2h, 1d, 2026-11-01); overrides --oldest
        #[arg(long)]
        since: Option<String>,
        /// Only messages until (1d = up to a day ago); overrides --latest
        #[arg(long)]
        until: Option<String>,
        /// Drop thread replies broadcast into the channel
        #[arg(long)]
        exclude_replies: bool,
        /// Inline each thread's replies under its parent
        #[arg(long)]
        with_replies: bool,
        /// With --with-replies: max replies per thread
        #[arg(long, default_value_t = 20)]
        reply_limit: usize,
        /// Resume from a previous next_cursor
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Cheap triage: message count, who spoke, busiest threads
    Digest {
        channel: String,
        #[arg(long, default_value = "1d")]
        since: String,
        /// Max messages scanned
        #[arg(long, default_value_t = 1000)]
        scan: usize,
        /// Threads listed
        #[arg(long, default_value_t = 5)]
        top: usize,
    },
    /// Fetch one message by channel + ts, or by permalink
    Get {
        /// Channel ID/#name, or a message permalink
        channel: String,
        ts: Option<String>,
    },
    /// Show conversation metadata (name, topic, purpose, members count)
    Info { channel: String },
    /// List the members of a conversation
    Members {
        channel: String,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Conversations with unread messages, mentions first
    Unread(unread::UnreadArgs),
    /// Followed threads with unread replies (browser session only)
    Threads {
        /// Max unread replies kept per thread
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

pub fn run(ctx: &Ctx, cmd: ConvCmd) -> Result<()> {
    let client = ctx.client()?;
    let mut users = Users::default();
    match cmd {
        ConvCmd::List {
            types,
            limit,
            exclude_archived,
            cursor,
        } => {
            let limit = read_limit(limit, 1000)?;
            let mut params = vec![
                ("types", types),
                ("limit", limit.to_string()),
                ("exclude_archived", exclude_archived.to_string()),
            ];
            if let Some(c) = cursor {
                params.push(("cursor", c));
            }
            let r = client.call("conversations.list", params)?;
            let channels: Vec<Value> = r["channels"]
                .as_array()
                .into_iter()
                .flatten()
                .map(slim_channel)
                .collect();
            let v = json!({ "channels": channels, "next_cursor": next_cursor(&r) });
            ctx.out.emit(v, |v| {
                let mut s: String = v["channels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        format!(
                            "{:<12} {}\n",
                            c["id"].as_str().unwrap_or_default(),
                            channel_label(c)
                        )
                    })
                    .collect();
                if let Some(c) = v["next_cursor"].as_str() {
                    s.push_str(&format!("\nmore: --cursor {c}\n"));
                }
                s
            });
        }
        ConvCmd::Read {
            channel,
            thread_ts,
            limit,
            oldest,
            latest,
            since,
            until,
            exclude_replies,
            with_replies,
            reply_limit,
            cursor,
        } => {
            let limit = read_limit(limit, 1000)?;
            let reply_limit = read_limit(reply_limit, 1000)?;
            let oldest = since.as_deref().map(slack_ts_since).transpose()?.or(oldest);
            let latest = until.as_deref().map(slack_ts_since).transpose()?.or(latest);
            let target = resolve::channel(&client, &mut users, &channel)?;
            let thread = match thread_ts {
                Some(t) => Some(permalink::normalize_ts(&t)?),
                None => target.thread_ts.or(target.ts),
            };
            let mut params = vec![("channel", target.id.clone()), ("limit", limit.to_string())];
            for (k, v) in [("oldest", oldest), ("latest", latest), ("cursor", cursor)] {
                if let Some(v) = v {
                    params.push((k, v));
                }
            }
            let (r, thread) = match thread {
                Some(ts) => read_thread(&client, params, ts)?,
                None => (client.call("conversations.history", params)?, None),
            };
            let mut raw: Vec<Value> = r["messages"].as_array().cloned().unwrap_or_default();
            if exclude_replies {
                raw.retain(|m| m["thread_ts"].is_null() || m["thread_ts"] == m["ts"]);
            }
            if thread.is_none() {
                // history is newest-first; agents read top-down, so flip to chronological.
                raw.reverse();
            }
            let mut messages = slim_all(&client, &mut users, &raw);
            if with_replies && thread.is_none() {
                for (m, src) in messages.iter_mut().zip(&raw) {
                    if src["reply_count"].as_u64().unwrap_or(0) == 0 {
                        continue;
                    }
                    let r = client.call(
                        "conversations.replies",
                        vec![
                            ("channel", target.id.clone()),
                            ("ts", src["ts"].as_str().unwrap_or_default().to_string()),
                            ("limit", (reply_limit + 1).to_string()),
                        ],
                    )?;
                    // The first entry is the parent itself.
                    let replies: Vec<Value> = r["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|x| x["ts"] != src["ts"])
                        .take(reply_limit)
                        .cloned()
                        .collect();
                    m["replies"] = json!(slim_all(&client, &mut users, &replies));
                }
            }
            let v = json!({
                "channel": target.id,
                "thread_ts": thread,
                "messages": messages,
                "has_more": r["has_more"].as_bool().unwrap_or(false),
                "next_cursor": next_cursor(&r),
            });
            ctx.out.emit(v, messages_text);
        }
        ConvCmd::Digest {
            channel,
            since,
            scan,
            top,
        } => {
            let target = resolve::channel(&client, &mut users, &channel)?;
            let oldest = slack_ts_since(&since)?;
            let raw = client.paginate(
                "conversations.history",
                vec![
                    ("channel", target.id.clone()),
                    ("oldest", oldest.clone()),
                    ("limit", "200".into()),
                ],
                "messages",
                read_limit(scan, 10_000)?,
            )?;
            let ids: Vec<&str> = raw.iter().filter_map(|m| m["user"].as_str()).collect();
            users.ensure(&client, ids);
            let v = digest(&target.id, &oldest, &raw, top, &users);
            ctx.out.emit(v, digest_text);
        }
        ConvCmd::Get { channel, ts } => {
            let target = resolve::channel(&client, &mut users, &channel)?;
            let ts = match ts {
                Some(t) => permalink::normalize_ts(&t)?,
                None => target
                    .ts
                    .ok_or_else(|| AppError::input("give a ts or a message permalink"))?,
            };
            let raw = find_message(&client, &target.id, &ts, target.thread_ts.as_deref())?;
            let message = slim_all(&client, &mut users, std::slice::from_ref(&raw)).remove(0);
            let v = json!({ "channel": target.id, "message": message });
            ctx.out.emit(v, |v| render::message_text(&v["message"]));
        }
        ConvCmd::Info { channel } => {
            let target = resolve::channel(&client, &mut users, &channel)?;
            let r = client.call(
                "conversations.info",
                vec![
                    ("channel", target.id),
                    ("include_num_members", "true".into()),
                ],
            )?;
            let v = slim_channel(&r["channel"]);
            ctx.out.emit(v, |c| {
                format!(
                    "{} {}\ntopic:   {}\npurpose: {}\nmembers: {}\n",
                    c["id"].as_str().unwrap_or_default(),
                    channel_label(c),
                    c["topic"].as_str().unwrap_or_default(),
                    c["purpose"].as_str().unwrap_or_default(),
                    c["num_members"]
                )
            });
        }
        ConvCmd::Members { channel, limit } => {
            let target = resolve::channel(&client, &mut users, &channel)?;
            let ids = client.paginate(
                "conversations.members",
                vec![("channel", target.id.clone()), ("limit", "1000".into())],
                "members",
                limit,
            )?;
            let ids: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
            users.ensure(&client, ids.iter().copied());
            let members: Vec<Value> = ids
                .iter()
                .map(|id| {
                    let u = users.get(id);
                    json!({ "id": id, "name": u.map(|u| u.name.as_str()), "real_name": u.map(|u| u.real_name.as_str()) })
                })
                .collect();
            ctx.out
                .emit(json!({ "channel": target.id, "members": members }), |v| {
                    v["members"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|m| {
                            format!(
                                "{:<12} {}\n",
                                m["id"].as_str().unwrap_or_default(),
                                m["real_name"].as_str().unwrap_or_default()
                            )
                        })
                        .collect()
                });
        }
        ConvCmd::Unread(args) => unread::unread(ctx, &client, &mut users, args)?,
        ConvCmd::Threads { limit } => {
            unread::threads(ctx, &client, &mut users, read_limit(limit, 1000)?)?
        }
    }
    Ok(())
}

/// `2h`/`1d`/date to a Slack ts (`secs.micros`).
pub fn slack_ts_since(input: &str) -> Result<String> {
    let ms = crate::time::parse_since(input)?;
    Ok(format!("{}.{:06}", ms / 1000, (ms % 1000) * 1000))
}

fn digest(channel: &str, oldest: &str, raw: &[Value], top: usize, users: &Users) -> Value {
    let mut speakers: Vec<(String, u64)> = Vec::new();
    for m in raw {
        let who = m["user"]
            .as_str()
            .and_then(|u| users.get(u).map(|x| x.label().to_string()))
            .or_else(|| m["username"].as_str().map(str::to_string))
            .or_else(|| m["bot_profile"]["name"].as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into());
        match speakers.iter_mut().find(|(n, _)| *n == who) {
            Some((_, c)) => *c += 1,
            None => speakers.push((who, 1)),
        }
    }
    speakers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut threads: Vec<&Value> = raw
        .iter()
        .filter(|m| m["reply_count"].as_u64().unwrap_or(0) > 0)
        .collect();
    threads.sort_by_key(|m| std::cmp::Reverse(m["reply_count"].as_u64().unwrap_or(0)));
    let first_line = |m: &Value| -> String {
        let t = render::plain_text(m["text"].as_str().unwrap_or_default(), users);
        t.lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(140)
            .collect()
    };
    json!({
        "channel": channel,
        "since": oldest,
        "messages": raw.len(),
        "threads": threads.len(),
        "speakers": speakers.iter().map(|(n, c)| json!({ "name": n, "messages": c })).collect::<Vec<_>>(),
        "top_threads": threads.iter().take(top).map(|m| json!({
            "ts": m["ts"],
            "replies": m["reply_count"],
            "author": m["user"].as_str().and_then(|u| users.get(u).map(|x| x.label().to_string())),
            "text": first_line(m),
        })).collect::<Vec<_>>(),
        "first_ts": raw.last().map(|m| m["ts"].clone()),
        "last_ts": raw.first().map(|m| m["ts"].clone()),
    })
}

fn digest_text(v: &Value) -> String {
    let mut s = format!(
        "{} messages, {} threads since {}

Who spoke:
",
        v["messages"],
        v["threads"],
        render::fmt_ts(v["since"].as_str().unwrap_or_default())
    );
    for sp in v["speakers"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "  {:>4}  {}
",
            sp["messages"],
            sp["name"].as_str().unwrap_or_default()
        ));
    }
    let threads = v["top_threads"].as_array().cloned().unwrap_or_default();
    if !threads.is_empty() {
        s.push_str(
            "
Busiest threads:
",
        );
        for t in &threads {
            s.push_str(&format!(
                "  {:>3} replies  {}: {}  (ts {})
",
                t["replies"],
                t["author"].as_str().unwrap_or("?"),
                t["text"].as_str().unwrap_or_default(),
                t["ts"].as_str().unwrap_or_default()
            ));
        }
    }
    s
}

/// Reads a thread; when `ts` is a reply, re-reads from its parent.
fn read_thread(
    client: &Client,
    params: Vec<(&'static str, String)>,
    ts: String,
) -> Result<(Value, Option<String>)> {
    let mut with_ts = params.clone();
    with_ts.push(("ts", ts.clone()));
    let r = client.call("conversations.replies", with_ts)?;
    let parent = r["messages"][0]["thread_ts"].as_str().map(str::to_string);
    match parent {
        Some(p) if p != ts => {
            let mut again = params;
            again.push(("ts", p.clone()));
            Ok((client.call("conversations.replies", again)?, Some(p)))
        }
        _ => Ok((r, Some(ts))),
    }
}

/// History holds top-level messages; replies only show up via conversations.replies.
pub fn find_message(
    client: &Client,
    channel: &str,
    ts: &str,
    thread_ts: Option<&str>,
) -> Result<Value> {
    let exact = |r: &Value| {
        r["messages"]
            .as_array()
            .and_then(|m| m.iter().find(|m| m["ts"] == ts).cloned())
    };
    if thread_ts.is_none() {
        let r = client.call(
            "conversations.history",
            vec![
                ("channel", channel.to_string()),
                ("oldest", ts.to_string()),
                ("latest", ts.to_string()),
                ("inclusive", "true".into()),
                ("limit", "1".into()),
            ],
        )?;
        if let Some(m) = exact(&r) {
            return Ok(m);
        }
    }
    let r = client.call(
        "conversations.replies",
        vec![
            ("channel", channel.to_string()),
            ("ts", thread_ts.unwrap_or(ts).to_string()),
            ("oldest", ts.to_string()),
            ("inclusive", "true".into()),
            ("limit", "1".into()),
        ],
    )?;
    exact(&r).ok_or_else(|| AppError::not_found(format!("message {ts} not found in {channel}")))
}

/// Resolves every author and mention in one batch, then slims each message.
pub fn slim_all(client: &Client, users: &mut Users, raw: &[Value]) -> Vec<Value> {
    let mut ids: Vec<&str> = Vec::new();
    for m in raw {
        ids.extend(m["user"].as_str());
        ids.extend(mentioned_users(m["text"].as_str().unwrap_or_default()));
    }
    users.ensure(client, ids);
    raw.iter().map(|m| slim_message(m, users)).collect()
}

pub fn slim_channel(c: &Value) -> Value {
    let mut v = json!({
        "id": c["id"],
        "name": c["name"],
        "is_private": c["is_private"].as_bool().unwrap_or(false),
        "is_im": c["is_im"].as_bool().unwrap_or(false),
        "is_mpim": c["is_mpim"].as_bool().unwrap_or(false),
        "is_archived": c["is_archived"].as_bool().unwrap_or(false),
    });
    for (key, src) in [
        ("user", &c["user"]),
        ("num_members", &c["num_members"]),
        ("topic", &c["topic"]["value"]),
        ("purpose", &c["purpose"]["value"]),
    ] {
        if !src.is_null() && src != "" {
            v[key] = src.clone();
        }
    }
    v
}

pub fn channel_label(c: &Value) -> String {
    if c["is_im"] == true {
        return format!("DM {}", c["user"].as_str().unwrap_or_default());
    }
    let prefix = if c["is_private"] == true || c["is_mpim"] == true {
        "🔒"
    } else {
        "#"
    };
    format!("{prefix}{}", c["name"].as_str().unwrap_or_default())
}

pub fn messages_text(v: &Value) -> String {
    let mut s: String = v["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let mut out = render::message_text(m);
            for r in m["replies"].as_array().into_iter().flatten() {
                for line in render::message_text(r).lines() {
                    out.push_str(&format!("    │ {line}\n"));
                }
            }
            out
        })
        .collect();
    if s.is_empty() {
        s.push_str("No messages.\n");
    }
    if let Some(c) = v["next_cursor"].as_str() {
        s.push_str(&format!("\nmore: --cursor {c}\n"));
    }
    s
}
