use crate::cli::{Ctx, read_limit};
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
        } => {
            let limit = read_limit(limit, 100)?;
            let mut parts: Vec<String> = query.into_iter().collect();
            parts.extend(in_channel.map(|c| format!("in:{c}")));
            parts.extend(from.map(|u| format!("from:{u}")));
            let q = parts.join(" ");
            if q.trim().is_empty() {
                return Err(AppError::input("give a query, --in or --from"));
            }
            let r = client.call(
                "search.messages",
                vec![
                    ("query", q.clone()),
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
            let raw = r["messages"]["matches"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut matches = slim_all(&client, &mut users, &raw);
            for (m, src) in matches.iter_mut().zip(&raw) {
                m["channel"] =
                    json!({ "id": src["channel"]["id"], "name": src["channel"]["name"] });
                m["permalink"] = src["permalink"].clone();
            }
            let paging = &r["messages"]["paging"];
            let v = json!({
                "query": q,
                "total": r["messages"]["total"],
                "page": paging["page"],
                "pages": paging["pages"],
                "matches": matches,
            });
            ctx.out.emit(v, |v| {
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
            });
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
