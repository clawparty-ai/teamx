//! teamx — shared-goal team collaboration state kernel (Rust CLI + SQLite).
//!
//! Library crate shared by two binaries:
//! - `teamx`     : the full CLI (team/role/events, network mode, tun0, …)
//! - `teamx-win` : Windows GUI launcher — opens the member-side panel
//!   (`gui-member`) directly on double-click.
//!
//! All command logic lives in [`commands`]; `main.rs`/`serve.rs` only translate
//! a CLI invocation or RPC request into a `Command` and render the result.

pub mod activity;
pub mod agents;
pub mod local_team;
pub mod broadcast;
pub mod buildinfo;
pub mod cli;
pub mod commands;
pub mod db;
pub mod direct;
pub mod direct_client;
// dns_proxy + tun_dns 只服务 tun0 透明代理链路（被 tun_socks/tun_cli 使用），
// 与 tun0 一样是 Unix-only；Windows 上 stub。
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod dns_proxy;
pub mod doc_flow;
pub mod events;
pub mod util;
pub mod git_client;
pub mod git_service;
pub mod im;
#[cfg(feature = "gui")]
pub mod gui;
#[cfg(feature = "gui")]
pub mod gui_panel;
#[cfg(feature = "gui")]
pub mod gui_member_panel;
pub mod loopx;
pub mod metrics;
pub mod nudge_tpl;
pub mod pki;
pub mod platform;
pub mod issued;
pub mod discovery;
pub mod credit;
pub mod site;
pub mod tcoin;
pub mod platform_console;
pub mod procwatch;
pub mod k8s;
pub mod provision;
pub mod routes;
pub mod serve;
pub mod server_tasks;
pub mod socks5;
pub mod state;
pub mod teamfile;
pub mod tunnel;
pub mod tunnel_client;
// rules_config 只被 tun_cli（Unix-only）使用。
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod rules_config;
// tun0 透明代理是 Unix-only（macOS utun / Linux tunN）。Windows 上 stub
// 掉这几个模块；`tun_dev` 保留（`system_dns_servers` 被 `dns list` 使用）。
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod tun_cli;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod tun_socks;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod tun_stack;
pub mod tun_dev;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod tun_dns;

// Enterprise modules (traffic capture/analysis/replay, analytics UI).
pub mod analyze;
pub mod capture;
pub mod capture_cli;
pub mod replay;
pub mod replay_cli;
pub mod tls_parse;
pub mod ui;
pub mod console;
pub mod fs;
pub mod worker;
pub mod console_data;
pub mod console_html;
pub mod discussion;

use cli::Cli;

/// Open the DB, migrate, and run one CLI command. Shared by `teamx` (CLI) and
/// the RPC layer in `serve` (via `commands::execute`).
pub fn run(cli: &Cli, db_path: &std::path::Path) -> Result<serde_json::Value, String> {
    let mut conn = db::open(db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;
    let before = match commands::max_event_id(&conn) {
        Ok(id) => Some(id),
        Err(e) => {
            eprintln!("teamx: max_event_id query failed ({e}); skipping WS notify");
            None
        }
    };
    let out = commands::execute(cli, &mut conn).map_err(|e| e.to_string())?;
    // A direct local DB write bypasses the server's WS fan-out (only the RPC
    // path publishes). Notify the server best-effort so online members see
    // the new events live instead of at their next poll/sync.
    if let Some(before) = before {
        notify_ws_broadcast(&conn, before);
    }
    Ok(out)
}

/// Best-effort synchronous trigger: ask the configured/discovered server to
/// WS-broadcast the events this invocation wrote directly to the DB. The
/// server's `events.rebroadcast` handler verifies each event against its own
/// ledger (and the caller's membership) before fan-out, so a divergent local
/// DB can never inject phantom events. Never affects the command's exit
/// status; disable entirely with TEAMX_NO_WS_NOTIFY=1.
fn notify_ws_broadcast(conn: &rusqlite::Connection, before: i64) {
    if std::env::var_os("TEAMX_NO_WS_NOTIFY").is_some() {
        return;
    }
    let events = match commands::events_after(conn, before) {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let Some(url) = resolve_notify_url() else { return };
    if let Err(e) = tunnel_client::run_rpc_with_timeout(
        &url,
        "events.rebroadcast",
        serde_json::json!({ "events": events }),
        3,
    ) {
        eprintln!("teamx: ws broadcast notify failed (offline?): {e}");
    }
}

/// Server URL for the broadcast notify: explicit env first, then the most
/// recently imported letter (same discovery as the tunnel/RPC clients).
fn resolve_notify_url() -> Option<String> {
    resolve_notify_url_from(std::env::var("TEAMX_SERVER_URL").ok().as_deref())
}

fn resolve_notify_url_from(env: Option<&str>) -> Option<String> {
    env.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(tunnel_client::discover_server_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_url_prefers_non_empty_env_over_discovery() {
        // a set env wins (and is trimmed)
        assert_eq!(
            resolve_notify_url_from(Some("  https://127.0.0.1:5781  ")).as_deref(),
            Some("https://127.0.0.1:5781")
        );
        // empty/whitespace env falls back to discovery — the result depends on
        // local letters, but it must never come back empty
        for fallback in [resolve_notify_url_from(Some("   ")), resolve_notify_url_from(None)] {
            assert!(fallback.map(|s| !s.trim().is_empty()).unwrap_or(true));
        }
    }
}
