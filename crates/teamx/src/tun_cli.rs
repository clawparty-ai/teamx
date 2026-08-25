//! tun_cli.rs — `teamx tun0 start|stop|status` command handling.
//!
//! - `start`: requires root; builds the fake-ip DNS + routing table, creates
//!   the TUN device and runs the bridge loop (`tun_socks::tun_proxy`).
//! - `stop`: removes the fake-ip route (best-effort). The TUN device is
//!   destroyed automatically when the `start` process exits.
//! - `status`: reports whether the device currently exists.

use std::net::Ipv4Addr;

use crate::cli::Tun0Cmd;
use crate::tun_dns::FakeIpDns;

/// Handle `teamx tun0 ...` (network-mode; needs root for start/stop).
pub fn handle_tun0(cmd: &Tun0Cmd) -> Result<serde_json::Value, String> {
    match cmd {
        Tun0Cmd::Start { server, exit, routes, rules_config, ip, net_prefix, net, max_conns, fake_dns } => {
            crate::tun_dev::require_root()?;

            // Watchdog heal-on-start: if a previous tun0 session died without
            // restoring system DNS (backup file left behind, no live process),
            // restore it first so we never stack stale 127.0.0.1 entries.
            if crate::tun_dev::dns_backup_pending()
                && std::process::Command::new("pgrep")
                    .args(["-f", "teamx tun0 start"])
                    .output()
                    .map(|o| o.stdout.is_empty())
                    .unwrap_or(true)
            {
                println!("ok watchdog: restoring leftover DNS backup from a dead session");
                let _ = crate::tun_dev::restore_system_dns();
            }

            let server_url = resolve_server_url(server.as_deref())?;

            // Route table source priority:
            //   1. --rules-config (external rules compat mode)
            //   2. -f/--routes JSON file
            //   3. SQLite route table
            let table = if let Some(path) = rules_config {
                Some(crate::rules_config::parse_rules_config(path, exit.as_deref().unwrap_or(""))?)
            } else {
                match routes {
                    Some(path) => {
                        let text = std::fs::read_to_string(path)
                            .map_err(|e| format!("routes file {}: {e}", path.display()))?;
                        Some(crate::routes::RouteTable::parse(&text)?)
                    }
                    None => {
                        // Read the SQLite route table from the default DB.
                        let db_path = std::env::var("TEAMX_DB")
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|_| crate::db::teamx_home().join("teamx.db"));
                        let conn = crate::db::open(&db_path).map_err(|e| format!("db open: {e}"))?;
                        crate::db::migrate(&conn).map_err(|e| format!("db migrate: {e}"))?;
                        match crate::routes::load_from_db(&conn) {
                            Ok(Some(t)) => Some(t),
                            Ok(None) => None,
                            Err(e) => return Err(e),
                        }
                    }
                }
            };
            let default_exit = match (&table, exit) {
                (Some(t), _) => t.default.clone(),
                (None, Some(e)) if !e.is_empty() => e.clone(),
                _ => return Err(
                    "tun0 start: no exit configured — pass --exit <name>, -f <routes.json>, \
                     --rules-config <rules.yaml>, or `teamx proxy routes set-default`".to_string(),
                ),
            };

            let dns = FakeIpDns::new(*net, *net_prefix);
            // Only fake-ip the domains the routing rules explicitly intercept;
            // other domains resolve to real IPs via the client's fallback DNS.
            // (When there are no explicit domain rules, keep intercepting
            // everything — the pre-existing global-proxy behavior.)
            if let Some(t) = &table {
                let patterns = t.intercept_patterns();
                if !patterns.is_empty() {
                    dns.set_intercept_patterns(patterns);
                }
            }
            let opts = crate::tun_socks::TunOptions {
                server_url,
                tun_ip: *ip,
                netmask: Ipv4Addr::new(255, 255, 0, 0),
                fake_net: (*net, *net_prefix),
                max_conns: *max_conns,
                default_exit,
                routes: table,
                dns,
                fake_dns: *fake_dns,
            };
            // Long-lived: blocks forever.
            crate::tun_socks::tun_proxy(opts)
        }
        Tun0Cmd::Stop { net, net_prefix, dev } => {
            crate::tun_dev::require_root()?;
            crate::tun_dev::restore_system_dns()?;
            crate::tun_dev::del_route(*net, *net_prefix, dev)?;
            Ok(serde_json::json!({ "ok": true, "note": format!("route removed; device {dev} freed when the start process exits") }))
        }
        Tun0Cmd::Status => {
            let exists = device_exists("tun0");
            Ok(serde_json::json!({ "ok": true, "device": "tun0", "exists": exists }))
        }
    }
}

/// Check whether a tun device with the given name exists (mac/linux).
fn device_exists(_name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/sys/class/net").join(_name).exists()
    }
    #[cfg(target_os = "macos")]
    {
        // utun devices are dynamic; just report tun0-style via ifconfig
        let out = std::process::Command::new("ifconfig")
            .arg(_name)
            .output();
        match out {
            Ok(o) if o.status.success() => true,
            _ => false,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// Resolve the network-mode server URL: `--server` flag > `TEAMX_SERVER_URL`
/// env > auto-discovered from an imported letter > default localhost.
fn resolve_server_url(explicit: Option<&str>) -> Result<String, String> {
    if let Some(u) = explicit {
        return Ok(u.to_string());
    }
    if let Ok(u) = std::env::var("TEAMX_SERVER_URL") {
        if !u.is_empty() {
            return Ok(u);
        }
    }
    if let Some(u) = crate::tunnel_client::discover_server_url() {
        return Ok(u);
    }
    Ok("https://127.0.0.1:5781".to_string())
}
