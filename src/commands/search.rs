use crate::cli::{Ctx, read_limit};
use crate::client::Client;
use crate::commands::conversations::{channel_label, messages_text, slim_all, slim_channel};
use crate::commands::users::slim_user;
use crate::error::{AppError, Result};
use crate::resolve::Users;
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Clone, Copy, ValueEnum)]
pub enum Sort {
    Score,
    Timestamp,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Subcommand)]
pub enum SearchCmd {
    /// Search messages; the query accepts Slack operators (in:, from:, before:, after:, has:, is:)
    Messages {
        query: Option<String>,
        /// Appends in:<channel> to the query
        #[arg(long = "in")]
        in_channel: Option<String>,
        /// Appends from:<user> to the query
        #[arg(long)]
        from: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 1)]
        page: usize,
        #[arg(long, value_enum, default_value = "timestamp")]
        sort: Sort,
        #[arg(long, value_enum, default_value = "desc")]
        sort_dir: SortDir,
        /// Match this exact phrase
        #[arg(long)]
        exact: Option<String>,
        /// Only messages since (2h, 3d, 2026-11-01)
        #[arg(long)]
        since: Option<String>,
        /// Only messages until (1d = up to a day ago)
        #[arg(long)]
        until: Option<String>,
    },
    /// Find channels by name, topic or purpose
    Channels {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Find people by name, display name, title or email
    People {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

pub fn run(ctx: &Ctx, cmd: SearchCmd) -> Result<()> {
    let client = ctx.client()?;
    let mut users = Users::default();
    match cmd {
        SearchCmd::Messages {
            query,
            in_channel,
            from,
            limit,
            page,
            sort,
            sort_dir,
            exact,
            since,
            until,
        } => {
            let limit = read_limit(limit, 100)?;
            let mut parts: Vec<String> = query.into_iter().collect();
            parts.extend(exact.map(|e| format!("\"{}\"", e.replace('"', ""))));
            parts.extend(in_channel.map(|c| format!("in:{c}")));
            parts.extend(from.map(|u| format!("from:{u}")));
            let window = Window::parse(since.as_deref(), until.as_deref())?;
            let q = parts.join(" ");
            if q.trim().is_empty() {
                return Err(AppError::input("give a query, --exact, --in or --from"));
            }
            let v = search_messages(
                &client, &mut users, &q, limit, page, sort, sort_dir, &window,
            )?;
            ctx.out.emit(v, matches_text);
        }
        SearchCmd::Channels { query, limit } => {
            let limit = read_limit(limit, 1000)?;
            let channels: Vec<Value> = if client.is_browser() {
                modules(&client, &query, "channels", limit)?
                    .iter()
                    .map(slim_channel)
                    .collect()
            } else {
                let q = query.to_lowercase();
                client
                    .paginate(
                        "conversations.list",
                        vec![
                            ("types", "public_channel,private_channel".into()),
                            ("limit", "1000".into()),
                            ("exclude_archived", "true".into()),
                        ],
                        "channels",
                        usize::MAX,
                    )?
                    .iter()
                    .map(slim_channel)
                    .filter(|c| {
                        ["name", "topic", "purpose"].iter().any(|k| {
                            c[*k]
                                .as_str()
                                .is_some_and(|s| s.to_lowercase().contains(&q))
                        })
                    })
                    .take(limit)
                    .collect()
            };
            ctx.out.emit(json!({ "channels": channels }), |v| {
                v["channels"]
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
                    .collect()
            });
        }
        SearchCmd::People { query, limit } => {
            let limit = read_limit(limit, 1000)?;
            let people: Vec<Value> = if client.is_browser() {
                modules(&client, &query, "people", limit)?
                    .iter()
                    .map(slim_user)
                    .collect()
            } else {
                let q = query.to_lowercase();
                client
                    .paginate(
                        "users.list",
                        vec![("limit", "1000".into())],
                        "members",
                        usize::MAX,
                    )?
                    .iter()
                    .filter(|u| u["deleted"] != true)
                    .map(slim_user)
                    .filter(|u| {
                        ["name", "real_name", "display_name", "title", "email"]
                            .iter()
                            .any(|k| {
                                u[*k]
                                    .as_str()
                                    .is_some_and(|s| s.to_lowercase().contains(&q))
                            })
                    })
                    .take(limit)
                    .collect()
            };
            ctx.out.emit(
                json!({ "users": people }),
                crate::commands::users::users_text,
            );
        }
    }
    Ok(())
}

/// A `--since`/`--until` window. Slack's `after:`/`before:` are whole days and
/// exclusive, so the query is widened by a day and trimmed by timestamp locally.
#[derive(Default)]
pub struct Window {
    since_ms: Option<i64>,
    until_ms: Option<i64>,
}

impl Window {
    pub fn parse(since: Option<&str>, until: Option<&str>) -> Result<Self> {
        Ok(Self {
            since_ms: since.map(crate::time::parse_since).transpose()?,
            until_ms: until.map(crate::time::parse_since).transpose()?,
        })
    }

    fn operators(&self) -> Vec<String> {
        const DAY: i64 = 86_400_000;
        let mut ops = Vec::new();
        if let Some(s) = self.since_ms {
            ops.push(format!("after:{}", crate::time::ms_to_date(s - DAY)));
        }
        if let Some(u) = self.until_ms {
            ops.push(format!("before:{}", crate::time::ms_to_date(u + DAY)));
        }
        ops
    }

    fn contains(&self, ts: &str) -> bool {
        let ms = ts
            .split('.')
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .map_or(0, |s| s * 1000);
        self.since_ms.is_none_or(|s| ms >= s) && self.until_ms.is_none_or(|u| ms <= u)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn search_messages(
    client: &Client,
    users: &mut Users,
    query: &str,
    limit: usize,
    page: usize,
    sort: Sort,
    sort_dir: SortDir,
    window: &Window,
) -> Result<Value> {
    let mut q = query.to_string();
    for op in window.operators() {
        q.push(' ');
        q.push_str(&op);
    }
    let r = client.call(
        "search.messages",
        vec![
            ("query", q.trim().to_string()),
            ("count", limit.to_string()),
            ("page", page.to_string()),
            (
                "sort",
                if matches!(sort, Sort::Score) {
                    "score"
                } else {
                    "timestamp"
                }
                .into(),
            ),
            (
                "sort_dir",
                if matches!(sort_dir, SortDir::Asc) {
                    "asc"
                } else {
                    "desc"
                }
                .into(),
            ),
            ("highlight", "false".into()),
        ],
    )?;
    let raw: Vec<Value> = r["messages"]["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| window.contains(m["ts"].as_str().unwrap_or_default()))
        .cloned()
        .collect();
    let mut matches = slim_all(client, users, &raw);
    for (m, src) in matches.iter_mut().zip(&raw) {
        m["channel"] = json!({ "id": src["channel"]["id"], "name": src["channel"]["name"] });
        m["permalink"] = src["permalink"].clone();
    }
    let paging = &r["messages"]["paging"];
    Ok(json!({
        "query": q.trim(),
        "total": r["messages"]["total"],
        "page": paging["page"],
        "pages": paging["pages"],
        "matches": matches,
    }))
}

pub fn matches_text(v: &Value) -> String {
    let mut s = format!(
        "{} results (page {}/{})\n\n",
        v["total"], v["page"], v["pages"]
    );
    for m in v["matches"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "#{} ",
            m["channel"]["name"].as_str().unwrap_or("?")
        ));
        s.push_str(&messages_text(&json!({ "messages": [m] })));
        s.push_str(&format!(
            "  {}\n\n",
            m["permalink"].as_str().unwrap_or_default()
        ));
    }
    s
}

/// Messages that mention me or one of my user groups, newest first.
pub fn mentions(ctx: &Ctx, since: &str, limit: usize) -> Result<()> {
    let limit = read_limit(limit, 100)?;
    let client = ctx.client()?;
    let mut users = Users::default();
    let me = client.call("auth.test", vec![])?["user_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut tokens = vec![format!("<@{me}>")];
    // User groups need the usergroups:read scope; without it, mention search still works.
    if let Ok(r) = client.call("usergroups.list", vec![("include_users", "true".into())]) {
        for g in r["usergroups"].as_array().into_iter().flatten() {
            let mine = g["users"]
                .as_array()
                .is_some_and(|u| u.iter().any(|x| x == me.as_str()));
            if mine && let Some(id) = g["id"].as_str() {
                tokens.push(format!("<!subteam^{id}>"));
            }
        }
    }
    let window = Window::parse(Some(since), None)?;
    let mut all: Vec<Value> = Vec::new();
    for t in &tokens {
        let r = search_messages(
            &client,
            &mut users,
            t,
            limit,
            1,
            Sort::Timestamp,
            SortDir::Desc,
            &window,
        )?;
        for m in r["matches"].as_array().into_iter().flatten() {
            if !all.iter().any(|x| x["permalink"] == m["permalink"]) {
                all.push(m.clone());
            }
        }
    }
    all.sort_by(|a, b| {
        let ts = |m: &Value| {
            m["ts"]
                .as_str()
                .unwrap_or_default()
                .parse::<f64>()
                .unwrap_or(0.0)
        };
        ts(b)
            .partial_cmp(&ts(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all.truncate(limit);
    let v =
        json!({ "mentions_of": tokens, "total": all.len(), "page": 1, "pages": 1, "matches": all });
    ctx.out.emit(v, matches_text);
    Ok(())
}

/// Browser-only search.modules; the undocumented response lists hits under `items`.
fn modules(
    client: &crate::client::Client,
    query: &str,
    module: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let r = client.call(
        "search.modules",
        vec![
            ("query", query.to_string()),
            ("module", module.to_string()),
            ("count", limit.min(100).to_string()),
        ],
    )?;
    Ok(r["items"].as_array().cloned().unwrap_or_default())
}
