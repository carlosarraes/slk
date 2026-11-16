use crate::error::AppError;
use serde_json::{Map, Value};
use std::io::IsTerminal;

/// Keys holding the primary list of a response; `--fields` projects these.
const LIST_KEYS: &[&str] = &[
    "messages",
    "channels",
    "users",
    "members",
    "matches",
    "threads",
    "workspaces",
];

pub struct Output {
    json: bool,
    fields: Option<Vec<String>>,
}

impl Output {
    pub fn new(json: bool, text: bool, fields: Option<String>) -> Self {
        let json = json || (!text && !std::io::stdout().is_terminal());
        let fields = fields.map(|f| {
            f.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });
        Self { json, fields }
    }

    pub fn emit(&self, value: Value, text: impl FnOnce(&Value) -> String) {
        let value = match &self.fields {
            Some(f) => project(value, f),
            None => value,
        };
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
        } else {
            print!("{}", text(&value));
        }
    }

    pub fn error(&self, err: &AppError) {
        if self.json {
            let body = serde_json::json!({ "error": err });
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
        } else {
            eprintln!("error: {}", err.message);
        }
    }
}

fn pick(item: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        if let Some(v) = item.pointer(&format!("/{}", f.replace('.', "/"))) {
            out.insert(f.clone(), v.clone());
        }
    }
    Value::Object(out)
}

fn project(value: Value, fields: &[String]) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|i| pick(i, fields)).collect()),
        Value::Object(mut map) => {
            match LIST_KEYS
                .iter()
                .find(|k| map.get(**k).is_some_and(Value::is_array))
            {
                Some(key) => {
                    let list = map.remove(*key).unwrap_or_default();
                    map.insert(key.to_string(), project(list, fields));
                    Value::Object(map)
                }
                None => pick(&Value::Object(map), fields),
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_primary_list_with_dot_paths() {
        let v = json!({"channel":"C1","messages":[{"ts":"1","text":"a","user":{"id":"U1","x":2}}]});
        let p = project(v, &["ts".into(), "user.id".into()]);
        assert_eq!(
            p,
            json!({"channel":"C1","messages":[{"ts":"1","user.id":"U1"}]})
        );
    }

    #[test]
    fn projects_plain_object() {
        let p = project(
            json!({"id":"U1","name":"bob","tz":"x"}),
            &["id".into(), "name".into()],
        );
        assert_eq!(p, json!({"id":"U1","name":"bob"}));
    }
}
