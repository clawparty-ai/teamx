//! routes.rs — per-target egress routing for the SOCKS5 proxy consumer.
//!
//! `teamx proxy start --routes <table.json>` lets a single local SOCKS5 port
//! pick which proxy exit each CONNECT target uses, based on the target host
//! (domain or IP) parsed from the SOCKS5 request. Without `--routes` the
//! classic `--exit <name>` fixed-exit behaviour is preserved.
//!
//! Matching is first-match over an ordered rule list:
//!   - domain exact:    `example.com`
//!   - domain suffix:   `*.cn`  (matches `www.baidu.com`, not `cn.com`)
//!   - IP CIDR:         `10.0.0.0/8`, `2001:db8::/32` (target parsed as IP)
//!   - exact IP:        `192.168.1.5`  (CIDR /32 or /128 shorthand)
//!
//! Table shape:
//! ```json
//! {
//!   "default": "egress",
//!   "rules": [
//!     { "match": "*.cn",      "exit": "egress2" },
//!     { "match": "10.0.0.0/8","exit": "egress2" }
//!   ]
//! }
//! ```

use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;

/// Key in `proxy_settings` for the default exit.
pub const DEFAULT_EXIT_KEY: &str = "default_exit";

/// How a rule matches a CONNECT target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchType {
    /// Exact domain equality (`example.com`).
    ExactDomain,
    /// Domain suffix wildcard (`*.cn` matches any domain ending in `.cn`).
    SuffixDomain,
    /// IP network (CIDR) or exact IP.
    Cidr,
}

/// One routing rule: how to match + which exit to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRule {
    pub match_type: MatchType,
    /// Raw pattern text (domain / `*.domain` / CIDR / IP).
    pub pattern: String,
    /// Exit name to route matched targets through.
    pub exit: String,
}

/// Parsed routing table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTable {
    /// Exit used when no rule matches.
    pub default: String,
    /// Ordered rules; first match wins.
    pub rules: Vec<RouteRule>,
}

/// Parse a `match` pattern into a MatchType, or an error if unsupported.
fn parse_match(pattern: &str) -> Result<MatchType, String> {
    if pattern.is_empty() {
        return Err("empty match pattern".to_string());
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        if suffix.is_empty() || suffix.contains('*') {
            return Err(format!("invalid wildcard match `{pattern}`"));
        }
        return Ok(MatchType::SuffixDomain);
    }
    if pattern.contains('*') {
        return Err(format!("unsupported wildcard match `{pattern}` (only `*.domain` allowed)"));
    }
    // Could be a CIDR (contains '/') or an IP.
    if pattern.contains('/') {
        // CIDR: validate by attempting to build the netmask check later; here
        // only ensure the prefix part parses as an IP.
        let (net, _prefix) = pattern.split_once('/').ok_or_else(|| format!("bad CIDR `{pattern}`"))?;
        if IpAddr::from_str(net).is_err() {
            return Err(format!("bad CIDR network `{pattern}`"));
        }
        return Ok(MatchType::Cidr);
    }
    if IpAddr::from_str(pattern).is_ok() {
        return Ok(MatchType::Cidr); // bare IP = /32 or /128
    }
    Ok(MatchType::ExactDomain)
}

impl RouteTable {
    /// Parse a table from JSON text. Requires a `default`; `rules` is optional.
    pub fn parse(json: &str) -> Result<RouteTable, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("routes: invalid JSON: {e}"))?;
        let default = v.get("default").and_then(|d| d.as_str()).unwrap_or("").to_string();
        if default.is_empty() {
            return Err("routes: missing `default` exit".to_string());
        }
        let mut rules = Vec::new();
        if let Some(arr) = v.get("rules").and_then(|r| r.as_array()) {
            for (i, item) in arr.iter().enumerate() {
                let pat = item.get("match").and_then(|m| m.as_str()).unwrap_or("").to_string();
                let exit = item.get("exit").and_then(|e| e.as_str()).unwrap_or("").to_string();
                if exit.is_empty() {
                    return Err(format!("routes: rule[{i}] missing `exit`"));
                }
                let mt = parse_match(&pat).map_err(|e| format!("routes: rule[{i}]: {e}"))?;
                rules.push(RouteRule { match_type: mt, pattern: pat, exit });
            }
        }
        Ok(RouteTable { default, rules })
    }

    /// Resolve the exit name for a CONNECT target host (domain or IP string).
    /// Falls back to `default` when nothing matches.
    pub fn resolve(&self, host: &str) -> &str {
        let ip = IpAddr::from_str(host).ok();
        for rule in &self.rules {
            match rule.match_type {
                MatchType::ExactDomain => {
                    if ip.is_none() && host.eq_ignore_ascii_case(&rule.pattern) {
                        return &rule.exit;
                    }
                }
                MatchType::SuffixDomain => {
                    if ip.is_none() {
                        // pattern = "*.cn" -> suffix = ".cn"
                        let suffix = format!(".{}", &rule.pattern[2..]);
                        if host.len() > suffix.len()
                            && host[host.len() - suffix.len()..].eq_ignore_ascii_case(&suffix)
                        {
                            return &rule.exit;
                        }
                    }
                }
                MatchType::Cidr => {
                    if let Some(ip) = ip {
                        if cidr_contains(&rule.pattern, ip) {
                            return &rule.exit;
                        }
                    }
                }
            }
        }
        &self.default
    }

    /// Domain patterns (exact / `*.suffix`) that the rules explicitly
    /// intercept. Used by the fake-ip DNS to only fake-ip these domains; all
    /// other queries are dropped so the client falls back to its real DNS.
    pub fn intercept_patterns(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| {
                matches!(r.match_type, MatchType::ExactDomain | MatchType::SuffixDomain)
            })
            .map(|r| r.pattern.clone())
            .collect()
    }

    /// IPv4 CIDR networks (or bare /32 IPs) the rules explicitly intercept.
    /// Used by IP-routing mode to add network routes through the tun.
    pub fn intercept_cidrs(&self) -> Vec<(Ipv4Addr, u8)> {
        let mut out = Vec::new();
        for r in &self.rules {
            if r.match_type != MatchType::Cidr {
                continue;
            }
            if let Some((net, prefix)) = r.pattern.split_once('/') {
                if let (Ok(ip), Ok(p)) = (net.parse::<Ipv4Addr>(), prefix.parse::<u8>()) {
                    out.push((ip, p));
                }
            } else if let Ok(ip) = r.pattern.parse::<Ipv4Addr>() {
                out.push((ip, 32));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// SQLite persistence for the proxy routing table.
//
// The table + default exit live in the teamx DB (proxy_routes /
// proxy_settings), so `teamx proxy start` with no -f/--routes uses the
// persisted config. Commands under `teamx proxy routes` manage it.
// ---------------------------------------------------------------------------

const SETTINGS_TABLE: &str = "proxy_settings";
const ROUTES_TABLE: &str = "proxy_routes";

/// Load the routing table (rules + default exit) from the DB.
/// Returns `None` when no default exit is configured.
pub fn load_from_db(conn: &rusqlite::Connection) -> Result<Option<RouteTable>, String> {
    let default: Option<String> = conn
        .query_row(
            &format!("SELECT value FROM {SETTINGS_TABLE} WHERE key = ?1"),
            [crate::routes::DEFAULT_EXIT_KEY],
            |r| r.get(0),
        )
        .ok();
    let default = match default {
        Some(d) if !d.is_empty() => d,
        _ => return Ok(None),
    };
    let mut rules = Vec::new();
    {
        let mut stmt = conn
            .prepare(&format!("SELECT seq, match, exit FROM {ROUTES_TABLE} ORDER BY seq"))
            .map_err(|e| format!("routes db prepare: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("routes db query: {e}"))?;
        for row in rows {
            let (seq, pat, exit) = row.map_err(|e| format!("routes db row: {e}"))?;
            let mt = parse_match(&pat).map_err(|e| format!("routes db rule[seq={seq}]: {e}"))?;
            rules.push(RouteRule { match_type: mt, pattern: pat, exit });
        }
    }
    Ok(Some(RouteTable { default, rules }))
}

/// Persist the routing table to the DB (replaces existing rules + default).
pub fn save_to_db(conn: &rusqlite::Connection, table: &RouteTable) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| format!("routes db tx: {e}"))?;
    tx.execute(&format!("DELETE FROM {ROUTES_TABLE}"), [])
        .map_err(|e| format!("routes db clear: {e}"))?;
    for (i, rule) in table.rules.iter().enumerate() {
        tx.execute(
            &format!("INSERT INTO {ROUTES_TABLE} (seq, match, exit) VALUES (?1, ?2, ?3)"),
            rusqlite::params![i as i64, rule.pattern, rule.exit],
        )
        .map_err(|e| format!("routes db insert: {e}"))?;
    }
    tx.execute(
        &format!(
            "INSERT INTO {SETTINGS_TABLE} (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value"
        ),
        rusqlite::params![crate::routes::DEFAULT_EXIT_KEY, table.default],
    )
    .map_err(|e| format!("routes db default: {e}"))?;
    tx.commit().map_err(|e| format!("routes db commit: {e}"))?;
    Ok(())
}

/// Clear all routing rules but keep the default exit.
pub fn clear_rules(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute(&format!("DELETE FROM {ROUTES_TABLE}"), [])
        .map_err(|e| format!("routes db clear: {e}"))?;
    Ok(())
}

/// Add or replace a rule at the end of the rule list (or at `seq` when given).
pub fn upsert_rule(
    conn: &rusqlite::Connection,
    seq: Option<i64>,
    pattern: &str,
    exit: &str,
) -> Result<i64, String> {
    // validate the pattern parses
    parse_match(pattern).map_err(|e| format!("routes rule: {e}"))?;
    if exit.is_empty() {
        return Err("routes rule: missing exit".to_string());
    }
    let tx = conn.unchecked_transaction().map_err(|e| format!("routes db tx: {e}"))?;
    let next_seq = match seq {
        Some(s) => s,
        None => {
            let max: Option<i64> = tx
                .query_row(&format!("SELECT MAX(seq) FROM {ROUTES_TABLE}"), [], |r| r.get(0))
                .ok()
                .flatten();
            max.map(|m| m + 1).unwrap_or(0)
        }
    };
    tx.execute(
        &format!(
            "INSERT INTO {ROUTES_TABLE} (seq, match, exit) VALUES (?1, ?2, ?3)
             ON CONFLICT(seq) DO UPDATE SET match = excluded.match, exit = excluded.exit"
        ),
        rusqlite::params![next_seq, pattern, exit],
    )
    .map_err(|e| format!("routes db upsert: {e}"))?;
    tx.commit().map_err(|e| format!("routes db commit: {e}"))?;
    Ok(next_seq)
}

/// Remove a rule by its match pattern (removes the first matching row).
pub fn remove_rule(conn: &rusqlite::Connection, pattern: &str) -> Result<bool, String> {
    let n = conn
        .execute(&format!("DELETE FROM {ROUTES_TABLE} WHERE match = ?1"), [pattern])
        .map_err(|e| format!("routes db remove: {e}"))?;
    Ok(n > 0)
}

/// Set the default exit.
pub fn set_default(conn: &rusqlite::Connection, exit: &str) -> Result<(), String> {
    if exit.is_empty() {
        return Err("routes: default exit cannot be empty".to_string());
    }
    conn.execute(
        &format!(
            "INSERT INTO {SETTINGS_TABLE} (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value"
        ),
        rusqlite::params![crate::routes::DEFAULT_EXIT_KEY, exit],
    )
    .map_err(|e| format!("routes db default: {e}"))?;
    Ok(())
}

/// Render the routing table as JSON for `proxy routes list` output.
pub fn to_json(table: &RouteTable) -> serde_json::Value {
    serde_json::json!({
        "default": table.default,
        "rules": table.rules.iter().map(|r| {
            serde_json::json!({ "match": r.pattern, "exit": r.exit })
        }).collect::<Vec<_>>(),
    })
}


/// Whether `ip` is inside the CIDR `cidr` (or equals the exact IP).
fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let (net, prefix): (&str, u32) = match cidr.split_once('/') {
        Some((net, p)) => (net, p.parse().unwrap_or(if ip.is_ipv4() { 32 } else { 128 })),
        None => (cidr, if ip.is_ipv4() { 32 } else { 128 }),
    };
    match (IpAddr::from_str(net).ok(), ip) {
        (Some(IpAddr::V4(net_v4)), IpAddr::V4(ipv4)) => {
            if prefix > 32 {
                return false;
            }
            let mask: u32 = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            (u32::from(ipv4) & mask) == (u32::from(net_v4) & mask)
        }
        (Some(IpAddr::V6(net_v6)), IpAddr::V6(ipv6)) => {
            if prefix > 128 {
                return false;
            }
            if prefix == 0 {
                return true;
            }
            let shift = 128 - prefix;
            (u128::from(ipv6) >> shift) == (u128::from(net_v6) >> shift)
        }
        _ => false, // type mismatch (v4 cidr vs v6 ip, etc.) or unparseable net
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(json: &str) -> RouteTable {
        RouteTable::parse(json).expect("table should parse")
    }

    #[test]
    fn default_fallback() {
        let t = table(r#"{"default":"egress"}"#);
        assert_eq!(t.resolve("anything.example"), "egress");
        assert_eq!(t.resolve("1.2.3.4"), "egress");
        assert_eq!(t.resolve(""), "egress");
    }

    #[test]
    fn exact_domain() {
        let t = table(r#"{"default":"d","rules":[{"match":"api.example.com","exit":"x"}]}"#);
        assert_eq!(t.resolve("api.example.com"), "x");
        assert_eq!(t.resolve("example.com"), "d");       // not matched (suffix)
        assert_eq!(t.resolve("a.api.example.com"), "d"); // not matched
        assert_eq!(t.resolve("API.Example.COM"), "x");   // case-insensitive
    }

    #[test]
    fn suffix_domain() {
        let t = table(r#"{"default":"d","rules":[{"match":"*.cn","exit":"x"}]}"#);
        assert_eq!(t.resolve("www.baidu.com"), "d");   // no .cn
        assert_eq!(t.resolve("www.baidu.cn"), "x");
        assert_eq!(t.resolve("a.b.cn"), "x");
        assert_eq!(t.resolve("cn.com"), "d");           // *.cn must not match cn.com
        assert_eq!(t.resolve("cn"), "d");               // no dot suffix
        assert_eq!(t.resolve("x.cn"), "x");
    }

    #[test]
    fn exact_ip() {
        let t = table(r#"{"default":"d","rules":[{"match":"192.168.1.5","exit":"x"}]}"#);
        assert_eq!(t.resolve("192.168.1.5"), "x");
        assert_eq!(t.resolve("192.168.1.6"), "d");
        // A domain that looks unrelated to the IP is not matched:
        assert_eq!(t.resolve("192.168.1.5.example.com"), "d");
    }

    #[test]
    fn cidr_ipv4() {
        let t = table(r#"{"default":"d","rules":[{"match":"10.0.0.0/8","exit":"x"}]}"#);
        assert_eq!(t.resolve("10.1.2.3"), "x");
        assert_eq!(t.resolve("10.255.255.255"), "x");
        assert_eq!(t.resolve("11.0.0.1"), "d");
        assert_eq!(t.resolve("192.168.1.1"), "d");
    }

    #[test]
    fn cidr_ipv6() {
        let t = table(r#"{"default":"d","rules":[{"match":"2001:db8::/32","exit":"x"}]}"#);
        assert_eq!(t.resolve("2001:db8::1"), "x");
        assert_eq!(t.resolve("2001:db8:ffff::1"), "x");
        assert_eq!(t.resolve("2001:db9::1"), "d");
    }

    #[test]
    fn first_match_wins() {
        let t = table(
            r#"{"default":"d","rules":[
                {"match":"*.cn","exit":"first"},
                {"match":"*.cn","exit":"second"}
            ]}"#,
        );
        assert_eq!(t.resolve("www.baidu.cn"), "first");
    }

    #[test]
    fn rule_order_over_precision() {
        // File order wins: the first matching rule is used even if a later
        // rule would be more specific.
        let t = table(
            r#"{"default":"d","rules":[
                {"match":"*.com","exit":"broad"},
                {"match":"api.example.com","exit":"specific"}
            ]}"#,
        );
        assert_eq!(t.resolve("api.example.com"), "broad");
    }

    #[test]
    fn mixed_domain_then_cidr() {
        let t = table(
            r#"{"default":"d","rules":[
                {"match":"*.internal","exit":"lan"},
                {"match":"192.168.0.0/16","exit":"lan"}
            ]}"#,
        );
        assert_eq!(t.resolve("host.internal"), "lan");
        assert_eq!(t.resolve("192.168.10.1"), "lan");
        assert_eq!(t.resolve("8.8.8.8"), "d");
        assert_eq!(t.resolve("public.com"), "d");
    }

    #[test]
    fn parse_errors() {
        assert!(RouteTable::parse(r#"{"rules":[]}"#).is_err());            // missing default
        assert!(RouteTable::parse(r#"{"default":""}"#).is_err());
        assert!(RouteTable::parse(r#"{"default":"d","rules":[{"match":"x"}]}"#).is_err()); // missing exit
        assert!(RouteTable::parse(r#"{"default":"d","rules":[{"match":"a**b","exit":"e"}]}"#).is_err()); // bad wildcard
        assert!(RouteTable::parse(r#"{"default":"d","rules":[{"match":"1.2.3.4/99","exit":"e"}]}"#).is_ok()); // prefix validated at resolve time
        assert!(RouteTable::parse("not json").is_err());
    }

    #[test]
    fn dotted_literal_is_domain_not_error() {
        // "300.1.1.1" is not a valid IP, but it is a valid domain literal, so
        // it parses as an ExactDomain rule (matching is by string compare).
        let t = table(r#"{"default":"d","rules":[{"match":"300.1.1.1","exit":"x"}]}"#);
        assert_eq!(t.rules[0].match_type, MatchType::ExactDomain);
        assert_eq!(t.resolve("300.1.1.1"), "x");
        assert_eq!(t.resolve("300.1.1.2"), "d");
    }

    #[test]
    fn parse_success_variants() {
        let t = table(
            r#"{"default":"d","rules":[
                {"match":"example.com","exit":"e1"},
                {"match":"*.cn","exit":"e2"},
                {"match":"10.0.0.0/8","exit":"e3"},
                {"match":"192.168.1.5","exit":"e4"},
                {"match":"2001:db8::/32","exit":"e5"}
            ]}"#,
        );
        assert_eq!(t.rules.len(), 5);
        assert_eq!(t.rules[0].match_type, MatchType::ExactDomain);
        assert_eq!(t.rules[1].match_type, MatchType::SuffixDomain);
        assert_eq!(t.rules[2].match_type, MatchType::Cidr);
        assert_eq!(t.rules[3].match_type, MatchType::Cidr);
        assert_eq!(t.rules[4].match_type, MatchType::Cidr);
    }

    #[test]
    fn db_roundtrip_and_default() {
        // in-memory DB with the proxy tables
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE proxy_routes (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               seq INTEGER NOT NULL,
               match TEXT NOT NULL,
               exit TEXT NOT NULL,
               UNIQUE (seq)
             );
             CREATE TABLE proxy_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();

        // nothing configured yet -> None
        assert!(load_from_db(&conn).unwrap().is_none());

        // set default + add rules, save, reload
        set_default(&conn, "egress").unwrap();
        upsert_rule(&conn, None, "*.cn", "egress2").unwrap();       // seq 0
        upsert_rule(&conn, None, "10.0.0.0/8", "egress2").unwrap(); // seq 1
        let t = load_from_db(&conn).unwrap().expect("should load");
        assert_eq!(t.default, "egress");
        assert_eq!(t.resolve("www.baidu.cn"), "egress2");
        assert_eq!(t.resolve("10.1.2.3"), "egress2");
        assert_eq!(t.resolve("other.com"), "egress"); // default
        assert_eq!(t.rules.len(), 2);

        // remove + clear
        assert!(remove_rule(&conn, "*.cn").unwrap());
        assert!(!remove_rule(&conn, "*.cn").unwrap()); // already gone
        clear_rules(&conn).unwrap();
        let t2 = load_from_db(&conn).unwrap().unwrap();
        assert!(t2.rules.is_empty());
        assert_eq!(t2.default, "egress"); // default preserved
    }

    #[test]
    fn upsert_by_seq_replaces_rule() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE proxy_routes (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               seq INTEGER NOT NULL,
               match TEXT NOT NULL,
               exit TEXT NOT NULL,
               UNIQUE (seq)
             );
             CREATE TABLE proxy_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        set_default(&conn, "d").unwrap();
        upsert_rule(&conn, None, "*.cn", "e2").unwrap();   // seq 0
        upsert_rule(&conn, None, "*.jp", "e3").unwrap();   // seq 1
        // replace seq 0 in place
        upsert_rule(&conn, Some(0), "*.cn", "e2x").unwrap();
        let t = load_from_db(&conn).unwrap().unwrap();
        assert_eq!(t.resolve("www.baidu.cn"), "e2x");
        assert_eq!(t.resolve("www.yahoo.jp"), "e3");
        assert_eq!(t.rules.len(), 2);
    }

    #[test]
    fn save_load_roundtrip_full_table() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE proxy_routes (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               seq INTEGER NOT NULL,
               match TEXT NOT NULL,
               exit TEXT NOT NULL,
               UNIQUE (seq)
             );
             CREATE TABLE proxy_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        let src = table(
            r#"{"default":"d","rules":[
                {"match":"example.com","exit":"e1"},
                {"match":"*.cn","exit":"e2"}
            ]}"#,
        );
        save_to_db(&conn, &src).unwrap();
        let loaded = load_from_db(&conn).unwrap().unwrap();
        assert_eq!(loaded, src);
    }
}
