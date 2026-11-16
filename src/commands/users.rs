use crate::cli::{Ctx, read_limit};
use crate::error::Result;
use crate::resolve::Users;
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Clone, Copy, ValueEnum)]
pub enum Status {
    All,
    Active,
    Deactivated,
}

#[derive(Subcommand)]
pub enum UsersCmd {
    /// Show one user (ID, @handle, name or email)
    Info { user: String },
    /// List users in the workspace
    List {
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[arg(long, value_enum, default_value = "active")]
        status: Status,
    },
}

pub fn run(ctx: &Ctx, cmd: UsersCmd) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        UsersCmd::Info { user } => {
            let id = Users::default().resolve_id(&client, &user)?;
            let r = client.call("users.info", vec![("user", id)])?;
            let u = slim_user(&r["user"]);
            ctx.out.emit(u, |u| {
                let mut s = String::new();
                for k in [
                    "id",
                    "name",
                    "real_name",
                    "display_name",
                    "title",
                    "email",
                    "tz",
                    "status",
                ] {
                    if let Some(v) = u[k].as_str().filter(|v| !v.is_empty()) {
                        s.push_str(&format!("{k:<13}{v}\n"));
                    }
                }
                if u["is_bot"] == true {
                    s.push_str("bot\n");
                }
                if u["deleted"] == true {
                    s.push_str("deactivated\n");
                }
                s
            });
        }
        UsersCmd::List { limit, status } => {
            let limit = read_limit(limit, 100_000)?;
            let mut out = Vec::new();
            let mut cursor: Option<String> = None;
            // The status filter runs locally, so keep paging until `limit` users pass it.
            loop {
                let mut params = vec![("limit", "200".to_string())];
                if let Some(c) = &cursor {
                    params.push(("cursor", c.clone()));
                }
                let r = client.call("users.list", params)?;
                for u in r["members"].as_array().into_iter().flatten() {
                    let deleted = u["deleted"] == true;
                    let keep = match status {
                        Status::All => true,
                        Status::Active => !deleted,
                        Status::Deactivated => deleted,
                    };
                    if keep && out.len() < limit {
                        out.push(slim_user(u));
                    }
                }
                let next = crate::client::next_cursor(&r);
                if out.len() >= limit || next.is_none() || next == cursor {
                    break;
                }
                cursor = next;
            }
            ctx.out.emit(json!({ "users": out }), users_text);
        }
    }
    Ok(())
}

pub fn slim_user(u: &Value) -> Value {
    let p = &u["profile"];
    let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let mut out = json!({
        "id": u["id"],
        "name": u["name"],
        "real_name": s(&u["real_name"]).or_else(|| s(&p["real_name"])),
        "display_name": s(&p["display_name"]),
        "title": s(&p["title"]),
        "email": s(&p["email"]),
        "tz": s(&u["tz"]),
        "status": s(&p["status_text"]),
        "is_bot": u["is_bot"] == true,
        "deleted": u["deleted"] == true,
    });
    if let Value::Object(map) = &mut out {
        map.retain(|_, v| !v.is_null());
    }
    out
}

pub fn users_text(v: &Value) -> String {
    v["users"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|u| {
            format!(
                "{:<12} {:<20} {}{}\n",
                u["id"].as_str().unwrap_or_default(),
                u["name"].as_str().unwrap_or_default(),
                u["real_name"].as_str().unwrap_or_default(),
                u["title"]
                    .as_str()
                    .map(|t| format!(" — {t}"))
                    .unwrap_or_default()
            )
        })
        .collect()
}
