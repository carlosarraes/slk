use crate::cli::{Ctx, read_limit};
use crate::client::Client;
use crate::error::{AppError, Result};
use crate::resolve::{self, Users};
use clap::Subcommand;
use serde_json::{Value, json};

const MAX_TEXT_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Subcommand)]
pub enum FilesCmd {
    /// Files shared in the workspace or a channel, newest first
    List {
        /// Channel ID, #name or @user
        #[arg(long = "in")]
        in_channel: Option<String>,
        /// Only files since (2h, 7d, 2026-11-01)
        #[arg(long)]
        since: Option<String>,
        /// Only files from this user
        #[arg(long)]
        user: Option<String>,
        /// Comma list: snippets,images,pdfs,spaces(canvases),gdocs,zips
        #[arg(long)]
        types: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Print a text-based file (snippet, post, code, csv, json...) to stdout
    Get {
        /// File ID (F...) or a file permalink
        file: String,
    },
}

#[derive(Subcommand)]
pub enum CanvasCmd {
    /// Render a canvas as Markdown
    Read {
        /// Canvas file ID (F...) or URL
        canvas: String,
    },
}

#[derive(Subcommand)]
pub enum ListCmd {
    /// Rows of a Slack List, keyed by column name
    Read {
        /// List file ID (F...) or URL
        list: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

/// Pulls the `F…` id out of an id, a `/files/U…/F…/name` link or a `/docs/T…/F…` link.
pub fn file_id(input: &str) -> Result<String> {
    let found = input.split(['/', '?', '#']).find(|seg| {
        seg.len() >= 9
            && seg.starts_with('F')
            && seg
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    });
    found
        .map(str::to_string)
        .ok_or_else(|| AppError::input(format!("no file id (F...) in '{input}'")))
}

fn file_info(client: &Client, id: &str) -> Result<Value> {
    Ok(client.call("files.info", vec![("file", id.to_string())])?["file"].clone())
}

fn slim_file(f: &Value, users: &Users) -> Value {
    let mut v = json!({
        "id": f["id"],
        "name": f["name"],
        "title": f["title"],
        "type": f["pretty_type"].as_str().or(f["filetype"].as_str()),
        "mimetype": f["mimetype"],
        "size": f["size"],
        "created": f["created"],
        "user": f["user"],
        "author": f["user"].as_str().and_then(|u| users.get(u)).map(|u| u.label().to_string()),
        "permalink": f["permalink"],
    });
    if let Some(ch) = f["channels"].as_array().filter(|c| !c.is_empty()) {
        v["channels"] = json!(ch);
    }
    v
}

fn is_text(f: &Value) -> bool {
    let mime = f["mimetype"].as_str().unwrap_or_default();
    let kind = f["filetype"].as_str().unwrap_or_default();
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/x-yaml"
                | "application/javascript"
        )
        || matches!(
            kind,
            "post"
                | "text"
                | "markdown"
                | "json"
                | "csv"
                | "yaml"
                | "xml"
                | "javascript"
                | "python"
                | "rust"
                | "go"
                | "shell"
                | "sql"
                | "diff"
        )
}

pub fn files(ctx: &Ctx, cmd: FilesCmd) -> Result<()> {
    let client = ctx.client()?;
    let mut users = Users::default();
    match cmd {
        FilesCmd::List {
            in_channel,
            since,
            user,
            types,
            limit,
        } => {
            let limit = read_limit(limit, 1000)?;
            let mut params = vec![("count", limit.min(100).to_string())];
            if let Some(c) = in_channel {
                params.push(("channel", resolve::channel(&client, &mut users, &c)?.id));
            }
            if let Some(s) = since {
                params.push((
                    "ts_from",
                    (crate::time::parse_since(&s)? / 1000).to_string(),
                ));
            }
            if let Some(u) = user {
                params.push(("user", users.resolve_id(&client, &u)?));
            }
            if let Some(t) = types {
                params.push(("types", t));
            }
            // files.list pages by number, not cursor.
            let mut raw: Vec<Value> = Vec::new();
            for page in 1.. {
                let mut p = params.clone();
                p.push(("page", page.to_string()));
                let r = client.call("files.list", p)?;
                raw.extend(r["files"].as_array().cloned().unwrap_or_default());
                let pages = r["paging"]["pages"].as_u64().unwrap_or(1);
                if raw.len() >= limit || page as u64 >= pages {
                    break;
                }
            }
            raw.truncate(limit);
            users.ensure(&client, raw.iter().filter_map(|f| f["user"].as_str()));
            let list: Vec<Value> = raw.iter().map(|f| slim_file(f, &users)).collect();
            ctx.out.emit(json!({ "files": list }), |v| {
                let list = v["files"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return "No files.\n".into();
                }
                list.iter()
                    .map(|f| {
                        format!(
                            "{}  {:<10} {}  ({}, {})\n  {}\n",
                            f["id"].as_str().unwrap_or_default(),
                            f["type"].as_str().unwrap_or_default(),
                            f["title"]
                                .as_str()
                                .or(f["name"].as_str())
                                .unwrap_or_default(),
                            f["author"].as_str().unwrap_or("?"),
                            crate::render::fmt_ts(&f["created"].to_string()),
                            f["permalink"].as_str().unwrap_or_default()
                        )
                    })
                    .collect()
            });
        }
        FilesCmd::Get { file } => {
            let f = file_info(&client, &file_id(&file)?)?;
            if !is_text(&f) {
                return Err(AppError::input(format!(
                    "{} is a {} file; only text-based files are printed (open {})",
                    f["name"].as_str().unwrap_or("file"),
                    f["pretty_type"].as_str().unwrap_or("binary"),
                    f["permalink"].as_str().unwrap_or_default()
                )));
            }
            let url = f["url_private_download"]
                .as_str()
                .or(f["url_private"].as_str())
                .ok_or_else(|| AppError::not_found("file has no download URL"))?;
            let bytes = client.fetch_file(url, MAX_TEXT_BYTES)?;
            let content = String::from_utf8_lossy(&bytes).into_owned();
            let v = json!({ "id": f["id"], "name": f["name"], "mimetype": f["mimetype"], "content": content });
            ctx.out
                .emit(v, |v| v["content"].as_str().unwrap_or_default().to_string());
        }
    }
    Ok(())
}

pub fn canvas(ctx: &Ctx, cmd: CanvasCmd) -> Result<()> {
    let CanvasCmd::Read { canvas } = cmd;
    let client = ctx.client()?;
    let f = file_info(&client, &file_id(&canvas)?)?;
    if f["filetype"] != "quip" && f["pretty_type"] != "Canvas" {
        return Err(AppError::input(format!(
            "{} is not a canvas ({})",
            f["id"].as_str().unwrap_or_default(),
            f["pretty_type"].as_str().unwrap_or("?")
        )));
    }
    let url = f["url_private_download"]
        .as_str()
        .or(f["url_private"].as_str())
        .ok_or_else(|| AppError::not_found("canvas has no download URL"))?;
    let html = String::from_utf8_lossy(&client.fetch_file(url, MAX_TEXT_BYTES)?).into_owned();
    if html.contains("<title>Slack</title>") && html.contains("signin") {
        return Err(AppError::auth(
            "Slack returned a sign-in page; log in again",
        ));
    }
    let markdown = htmd::convert(&html)
        .map_err(|e| AppError::new("io", format!("cannot convert canvas HTML: {e}")))?
        .trim()
        .to_string();
    let v = json!({
        "id": f["id"],
        "title": f["title"],
        "url": f["permalink"],
        "updated": f["updated"].as_i64().or(f["created"].as_i64()),
        "markdown": markdown,
    });
    ctx.out.emit(v, |v| {
        let md = v["markdown"].as_str().unwrap_or_default();
        // Canvases usually open with their own title as an H1.
        if md.starts_with("# ") {
            format!("{md}\n")
        } else {
            format!("# {}\n\n{md}\n", v["title"].as_str().unwrap_or("Canvas"))
        }
    });
    Ok(())
}

/// Readable value of one Slack List cell.
fn cell(field: &Value, users: &Users) -> Value {
    if let Some(t) = field["text"].as_str().filter(|t| !t.is_empty()) {
        return json!(crate::render::plain_text(t, users));
    }
    if let Some(u) = field["user"].as_array() {
        return json!(
            u.iter()
                .filter_map(Value::as_str)
                .map(|id| users
                    .get(id)
                    .map(|x| x.label().to_string())
                    .unwrap_or_else(|| id.to_string()))
                .collect::<Vec<_>>()
        );
    }
    for key in ["select", "date", "channel", "email", "phone", "link"] {
        if let Some(a) = field[key].as_array() {
            return json!(a);
        }
    }
    for key in ["number", "checkbox", "rating"] {
        if !field[key].is_null() {
            return field[key].clone();
        }
    }
    field["value"].clone()
}

pub fn list(ctx: &Ctx, cmd: ListCmd) -> Result<()> {
    let ListCmd::Read { list, limit } = cmd;
    let limit = read_limit(limit, 10_000)?;
    let client = ctx.client()?;
    let mut users = Users::default();
    let id = file_id(&list)?;
    let f = file_info(&client, &id)?;
    let schema = f["list_metadata"]["schema"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let column_name = |key: &str| -> String {
        schema
            .iter()
            .find(|c| c["id"] == key || c["key"] == key)
            .and_then(|c| c["name"].as_str())
            .unwrap_or(key)
            .to_string()
    };
    let items = client.paginate(
        "slackLists.items.list",
        vec![("list_id", id.clone()), ("limit", "100".into())],
        "items",
        limit,
    )?;
    let ids: Vec<String> = items
        .iter()
        .flat_map(|i| i["fields"].as_array().cloned().unwrap_or_default())
        .flat_map(|f| f["user"].as_array().cloned().unwrap_or_default())
        .filter_map(|u| u.as_str().map(str::to_string))
        .collect();
    users.ensure(&client, ids.iter().map(String::as_str));
    let rows: Vec<Value> = items
        .iter()
        .map(|item| {
            let mut row = serde_json::Map::new();
            row.insert("id".into(), item["id"].clone());
            for field in item["fields"].as_array().into_iter().flatten() {
                let key = field["column_id"]
                    .as_str()
                    .or(field["key"].as_str())
                    .unwrap_or("?");
                row.insert(column_name(key), cell(field, &users));
            }
            Value::Object(row)
        })
        .collect();
    let v = json!({ "id": id, "title": f["title"], "url": f["permalink"], "rows": rows });
    ctx.out.emit(v, |v| {
        let mut s = format!("{}\n\n", v["title"].as_str().unwrap_or("List"));
        for row in v["rows"].as_array().into_iter().flatten() {
            let cells: Vec<String> = row
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, val)| *k != "id" && !val.is_null())
                .map(|(k, val)| {
                    format!(
                        "{k}: {}",
                        val.as_str().map(str::to_string).unwrap_or(val.to_string())
                    )
                })
                .collect();
            s.push_str(&format!("• {}\n", cells.join("  ·  ")));
        }
        s
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_file_ids() {
        assert_eq!(file_id("F0ACH2GQXUP").unwrap(), "F0ACH2GQXUP");
        assert_eq!(
            file_id("https://acme.slack.com/docs/T03T3/F0ACH2GQXUP").unwrap(),
            "F0ACH2GQXUP"
        );
        assert_eq!(
            file_id("https://acme.slack.com/files/U0CEH/F0C33BQF2G3/x.json").unwrap(),
            "F0C33BQF2G3"
        );
        assert!(file_id("https://acme.slack.com/archives/C1").is_err());
    }

    #[test]
    fn text_detection() {
        assert!(is_text(
            &json!({"mimetype": "text/plain", "filetype": "text"})
        ));
        assert!(is_text(
            &json!({"mimetype": "application/json", "filetype": "json"})
        ));
        assert!(!is_text(
            &json!({"mimetype": "image/png", "filetype": "png"})
        ));
    }
}
