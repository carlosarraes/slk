//! `slk auth list --check` driven through the real binary against a fake Slack
//! API on 127.0.0.1 (SLK_API_BASE_URL). No real tokens, no real network.

// The base-URL override is compiled out of release builds.
#![cfg(debug_assertions)]

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// Records `(method, identity)` for each request; identity is the bearer token
/// or the `token=` form field, which is how the two credential kinds auth.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

fn fake_slack() -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || handle(stream, &log));
        }
    });
    (addr, seen)
}

fn handle(mut stream: TcpStream, log: &Seen) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 2048];
    let head_end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while raw.len() < head_end + length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
    let body = String::from_utf8_lossy(&raw[head_end..]).to_string();
    let method = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .trim_start_matches('/')
        .trim_start_matches("api/")
        .to_string();
    let bearer = head
        .lines()
        .find_map(|l| l.strip_prefix("authorization: Bearer "))
        .map(str::to_string);
    let identity = bearer
        .or_else(|| {
            body.split('&')
                .find_map(|kv| kv.strip_prefix("token=").map(str::to_string))
        })
        .unwrap_or_default();
    log.lock().unwrap().push((method, identity.clone()));

    let (status, payload) = match identity.as_str() {
        "xoxp-fake-1" => (
            "200 OK",
            json!({"ok": true, "team": "Acme", "team_id": "T1", "user": "ana", "user_id": "U1"}),
        ),
        "xoxc-fake-6" => (
            "200 OK",
            json!({"ok": true, "team": "Browser", "team_id": "T6", "user": "bob", "user_id": "U6"}),
        ),
        "xoxp-fake-2" => ("200 OK", json!({"ok": false, "error": "token_revoked"})),
        "xoxp-fake-3" => ("200 OK", json!({"ok": false, "error": "token_expired"})),
        "xoxp-fake-4" => ("200 OK", json!({"ok": false, "error": "invalid_auth"})),
        "xoxp-fake-5" => ("500 Internal Server Error", json!({})),
        _ => ("200 OK", json!({"ok": false, "error": "not_authed"})),
    };
    let payload = payload.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
}

fn standard(team: &str, id: &str, token: &str) -> Value {
    json!({"team_id": id, "team_name": team, "user_id": "U", "user": "u",
           "auth_type": "standard", "token": token})
}

fn config_dir(workspaces: Value, default: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let cfg = json!({"default_workspace": default, "workspaces": workspaces});
    std::fs::write(dir.path().join("workspaces.json"), cfg.to_string()).unwrap();
    dir
}

fn slk(cfg: &tempfile::TempDir, base: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slk"))
        .args(args)
        .env_clear()
        .env("SLK_CONFIG_DIR", cfg.path())
        .env("SLK_API_BASE_URL", base)
        .output()
        .unwrap()
}

fn base(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

fn out_json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&o.stdout)
        )
    })
}

fn row<'a>(v: &'a Value, profile: &str) -> &'a Value {
    v["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["profile"] == profile)
        .unwrap_or_else(|| panic!("no row for {profile}"))
}

/// No token may reach stdout or stderr, whatever the outcome.
fn assert_no_tokens(o: &Output) {
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!all.contains("xoxp-fake"), "token leaked:\n{all}");
    assert!(!all.contains("xoxc-fake"), "token leaked:\n{all}");
    assert!(!all.contains("xoxd-fake"), "cookie leaked:\n{all}");
}

fn mixed_accounts() -> tempfile::TempDir {
    config_dir(
        json!({
            "acme": standard("Acme", "T1", "xoxp-fake-1"),
            "revoked": standard("Revoked", "T2", "xoxp-fake-2"),
            "expired": standard("Expired", "T3", "xoxp-fake-3"),
            "invalid": standard("Invalid", "T4", "xoxp-fake-4"),
            "flaky": standard("Flaky", "T5", "xoxp-fake-5"),
            "browser": {"team_id": "T6", "team_name": "Browser", "user_id": "U6", "user": "bob",
                        "auth_type": "browser", "xoxc_token": "xoxc-fake-6",
                        "xoxd_token": "xoxd-fake-6", "workspace_url": "https://b.slack.com"},
        }),
        "acme",
    )
}

#[test]
fn check_reports_every_account_and_exits_with_first_failure_code() {
    let (addr, seen) = fake_slack();
    let cfg = mixed_accounts();
    let o = slk(&cfg, &base(addr), &["auth", "list", "--check"]);
    assert_no_tokens(&o);

    // First failing profile in key order is "expired" (authentication -> 3).
    assert_eq!(
        o.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v = out_json(&o);
    let want = [
        ("acme", true, "ok"),
        ("browser", true, "ok"),
        ("expired", false, "expired"),
        ("flaky", false, "network"),
        ("invalid", false, "invalid"),
        ("revoked", false, "revoked"),
    ];
    for (profile, valid, status) in want {
        let r = row(&v, profile);
        assert_eq!(r["valid"], valid, "{profile}: {r}");
        assert_eq!(r["status"], status, "{profile}: {r}");
        assert_eq!(r.get("error").is_some(), !valid, "{profile}: {r}");
    }
    assert!(
        row(&v, "revoked")["error"]
            .as_str()
            .unwrap()
            .contains("token_revoked")
    );
    assert_eq!(row(&v, "acme")["default"], true);
    assert_eq!(row(&v, "acme")["team_name"], "Acme");

    let err: Value = serde_json::from_slice(&o.stderr).unwrap();
    assert_eq!(err["error"]["code"], "authentication");
    assert_eq!(
        err["error"]["details"]["failed"],
        json!(["expired", "flaky", "invalid", "revoked"])
    );

    // One auth.test per account, never retried (the 500 included).
    let mut calls = seen.lock().unwrap().clone();
    calls.sort();
    assert_eq!(calls.len(), 6, "{calls:?}");
    assert!(calls.iter().all(|(m, _)| m == "auth.test"), "{calls:?}");
}

#[test]
fn check_text_output_marks_each_account() {
    let (addr, _) = fake_slack();
    let cfg = mixed_accounts();
    let o = slk(&cfg, &base(addr), &["auth", "list", "--check", "--text"]);
    assert_no_tokens(&o);
    let text = String::from_utf8_lossy(&o.stdout);
    let line = |p: &str| text.lines().find(|l| l.contains(p)).unwrap().to_string();
    assert!(
        line("Acme").starts_with("* ") && line("Acme").ends_with("  ✓"),
        "{text}"
    );
    assert!(
        line("Revoked").contains("  ✗ revoked: auth.test: token_revoked"),
        "{text}"
    );
    assert!(line("Expired").contains("  ✗ expired:"), "{text}");
    assert!(line("Flaky").contains("  ✗ network:"), "{text}");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.starts_with("error: 4 of 6 workspaces failed the check"),
        "{stderr}"
    );
}

#[test]
fn check_exits_zero_when_all_accounts_work() {
    let (addr, seen) = fake_slack();
    let cfg = config_dir(
        json!({"acme": standard("Acme", "T1", "xoxp-fake-1")}),
        "acme",
    );
    let o = slk(&cfg, &base(addr), &["auth", "list", "--check"]);
    assert_no_tokens(&o);
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty());
    let v = out_json(&o);
    assert_eq!(row(&v, "acme")["status"], "ok");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn check_reports_unreachable_server_as_network_error() {
    // Bind then drop to get a port that refuses connections.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let cfg = config_dir(
        json!({"acme": standard("Acme", "T1", "xoxp-fake-1")}),
        "acme",
    );
    let o = slk(&cfg, &base(dead), &["auth", "list", "--check"]);
    assert_no_tokens(&o);
    assert_eq!(o.status.code(), Some(7));
    let v = out_json(&o);
    assert_eq!(row(&v, "acme")["status"], "network");
    assert_eq!(row(&v, "acme")["valid"], false);
    let err: Value = serde_json::from_slice(&o.stderr).unwrap();
    assert_eq!(err["error"]["code"], "network");
    assert_eq!(err["error"]["retryable"], true);
}

#[test]
fn list_without_check_is_offline_and_unchanged() {
    let (addr, seen) = fake_slack();
    let cfg = mixed_accounts();
    let o = slk(&cfg, &base(addr), &["auth", "list"]);
    assert_no_tokens(&o);
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty());
    let v = out_json(&o);
    assert_eq!(v["workspaces"].as_array().unwrap().len(), 6);
    for r in v["workspaces"].as_array().unwrap() {
        assert!(r.get("valid").is_none() && r.get("status").is_none(), "{r}");
    }
    assert_eq!(row(&v, "acme")["auth_type"], "user");
    assert!(
        seen.lock().unwrap().is_empty(),
        "list without --check must not touch the network"
    );
}

#[test]
fn api_base_override_refuses_non_loopback_hosts() {
    let cfg = config_dir(
        json!({"acme": standard("Acme", "T1", "xoxp-fake-1")}),
        "acme",
    );
    let o = slk(
        &cfg,
        "https://evil.example.com",
        &["auth", "list", "--check"],
    );
    assert_no_tokens(&o);
    assert_eq!(o.status.code(), Some(9));
    let v = out_json(&o);
    assert_eq!(row(&v, "acme")["status"], "error");
}
