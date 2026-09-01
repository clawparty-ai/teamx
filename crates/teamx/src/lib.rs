//! teamx — shared-goal team collaboration state kernel (Rust CLI + SQLite).
//!
//! Library crate shared by two binaries:
//! - `teamx`     : the full CLI (team/role/events, network mode, tun0, …)
//! - `teamx-win` : Windows GUI launcher — opens the member-side panel
//!                 (`gui-member`) directly on double-click.
//!
//! All command logic lives in [`commands`]; `main.rs`/`serve.rs` only translate
//! a CLI invocation or RPC request into a `Command` and render the result.

pub mod broadcast;
pub mod cli;
pub mod commands;
pub mod db;
// dns_proxy + tun_dns 只服务 tun0 透明代理链路（被 tun_socks/tun_cli 使用），
// 与 tun0 一样是 Unix-only；Windows 上 stub。
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod dns_proxy;
pub mod doc_flow;
pub mod events;
pub mod git_client;
pub mod git_service;
#[cfg(feature = "gui")]
pub mod gui;
#[cfg(feature = "gui")]
pub mod gui_panel;
#[cfg(feature = "gui")]
pub mod gui_member_panel;
pub mod loopx;
pub mod metrics;
pub mod pki;
pub mod routes;
pub mod serve;
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

use cli::Cli;

/// Open the DB, migrate, and run one CLI command. Shared by `teamx` (CLI) and
/// the RPC layer in `serve` (via `commands::execute`).
pub fn run(cli: &Cli, db_path: &std::path::Path) -> Result<serde_json::Value, String> {
    let mut conn = db::open(db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    db::migrate(&conn).map_err(|e| format!("schema init failed: {e}"))?;
    commands::execute(cli, &mut conn).map_err(|e| e.to_string())
}
