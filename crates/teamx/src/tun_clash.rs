//! tun_clash.rs — Clash configuration compatibility for the tun0 proxy.
//!
//! `teamx tun0 start --clash-config <path>` reads a Clash YAML config and maps
//! its routing intent onto teamx's route table:
//!
//!   Clash rule                     -> teamx route
//!   DOMAIN-SUFFIX,example.com,..   -> *.example.com
//!   DOMAIN,example.com,..          -> example.com (exact)
//!   IP-CIDR,10.0.0.0/8,..          -> 10.0.0.0/8
//!   IP-CIDR6,2001:db8::/32,..      -> 2001:db8::/32
//!   MATCH,..                       -> (becomes the route table `default`)
//!
//! Only the `rules` + `mode` are used. `proxies`/`proxy-groups` are ignored —
//! the exit is chosen from teamx's own egress set (`--exit` or `--routes`).
//! DIRECT/REJECT actions are not supported (v1): rules targeting them are
//! skipped (the flow then follows the default exit).

use crate::routes::{MatchType, RouteRule, RouteTable};

/// Parse a Clash config file and extract a route table.
/// Returns the table (default exit = MATCH target if present, else a fallback
/// passed by the caller).
pub fn parse_clash_config(path: &std::path::Path, fallback_default: &str) -> Result<RouteTable, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("clash config {}: {e}", path.display()))?;
    parse_clash_yaml(&text, fallback_default)
}

/// Parse Clash YAML text into a teamx RouteTable.
pub fn parse_clash_yaml(text: &str, fallback_default: &str) -> Result<RouteTable, String> {
    let v: serde_yaml::Value = serde_yaml::from_str(text)
        .map_err(|e| format!("clash config: invalid YAML: {e}"))?;

    let mut rules: Vec<RouteRule> = Vec::new();
    let mut default: Option<String> = None;

    if let Some(arr) = v.get("rules").and_then(|r| r.as_sequence()) {
        for (i, item) in arr.iter().enumerate() {
            let line = item.as_str().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            // Clash rule format: "TYPE,ARG,ACTION" (or with no-paren "TYPE,ARG")
            let parts: Vec<&str> = line.split(',').collect();

            let rule_type = parts[0].to_uppercase();

            // MATCH is a catch-all: "MATCH,PROXY" (2 parts) -> default exit.
            if rule_type == "MATCH" && parts.len() >= 2 {
                if default.is_none() {
                    default = Some(parts[1].trim().to_string());
                }
                continue;
            }

            if parts.len() < 3 {
                continue;
            }
            let arg = parts[1].trim();
            // Action is the exit name — keep case-sensitive (it maps to a
            // teamx egress name like `egress` / `egress2` / `PROXY`).
            let action = parts[2].trim();

            // v1: DIRECT / REJECT actions are not supported -> skip.
            let action_upper = action.to_uppercase();
            if action_upper == "DIRECT" || action_upper == "REJECT" {
                continue;
            }

            match rule_type.as_str() {
                "DOMAIN-SUFFIX" => {
                    if !arg.is_empty() {
                        rules.push(RouteRule {
                            match_type: MatchType::SuffixDomain,
                            pattern: format!("*.{arg}"),
                            exit: action.to_string(),
                        });
                    }
                }
                "DOMAIN" => {
                    if !arg.is_empty() {
                        rules.push(RouteRule {
                            match_type: MatchType::ExactDomain,
                            pattern: arg.to_string(),
                            exit: action.to_string(),
                        });
                    }
                }
                "IP-CIDR" | "IP-CIDR6" => {
                    if !arg.is_empty() {
                        // Validate it parses as a CIDR (routes.rs does at resolve time,
                        // but catch obvious mistakes here).
                        if arg.contains('/') {
                            rules.push(RouteRule {
                                match_type: MatchType::Cidr,
                                pattern: arg.to_string(),
                                exit: action.to_string(),
                            });
                        }
                    }
                }
                // Unknown rule types (GEOIP, PROCESS-NAME, etc.) are ignored.
                _ => {}
            }
            let _ = i;
        }
    }

    let default_exit = default.unwrap_or_else(|| fallback_default.to_string());
    Ok(RouteTable { default: default_exit, rules })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y(text: &str) -> RouteTable {
        parse_clash_yaml(text, "egress").expect("parse")
    }

    #[test]
    fn empty_config_uses_fallback() {
        let t = y("mode: rule\n");
        assert_eq!(t.default, "egress");
        assert!(t.rules.is_empty());
    }

    #[test]
    fn domain_suffix_maps_to_wildcard() {
        let t = y("rules:\n  - DOMAIN-SUFFIX,google.com,PROXY\n");
        assert_eq!(t.rules.len(), 1);
        assert_eq!(t.rules[0].pattern, "*.google.com");
        assert_eq!(t.rules[0].exit, "PROXY");
        assert_eq!(t.resolve("www.google.com"), "PROXY");
    }

    #[test]
    fn exact_domain_maps() {
        let t = y("rules:\n  - DOMAIN,example.com,myexit\n");
        assert_eq!(t.rules[0].pattern, "example.com");
        assert_eq!(t.rules[0].match_type, MatchType::ExactDomain);
        assert_eq!(t.resolve("example.com"), "myexit");
        assert_eq!(t.resolve("api.example.com"), "egress"); // not matched
    }

    #[test]
    fn ip_cidr_maps() {
        let t = y("rules:\n  - IP-CIDR,10.0.0.0/8,PROXY\n  - IP-CIDR6,2001:db8::/32,PROXY\n");
        assert_eq!(t.rules.len(), 2);
        assert_eq!(t.rules[0].pattern, "10.0.0.0/8");
        assert_eq!(t.rules[0].match_type, MatchType::Cidr);
        assert_eq!(t.resolve("10.1.2.3"), "PROXY");
        assert_eq!(t.rules[1].pattern, "2001:db8::/32");
    }

    #[test]
    fn match_becomes_default() {
        let t = y("rules:\n  - MATCH,PROXY\n");
        assert_eq!(t.default, "PROXY");
        assert_eq!(t.resolve("anything.example"), "PROXY");
    }

    #[test]
    fn direct_and_reject_skipped() {
        let t = y("rules:\n  - DOMAIN-SUFFIX,cn.com,DIRECT\n  - DOMAIN,ads.example,REJECT\n  - DOMAIN-SUFFIX,google.com,PROXY\n");
        assert_eq!(t.rules.len(), 1); // only google.com kept
        assert_eq!(t.rules[0].pattern, "*.google.com");
        assert_eq!(t.resolve("cn.com"), "egress"); // DIRECT skipped -> fallback
    }

    #[test]
    fn first_match_wins_like_clash() {
        let t = y("rules:\n  - DOMAIN-SUFFIX,example.com,exitA\n  - DOMAIN-SUFFIX,example.com,exitB\n");
        assert_eq!(t.resolve("www.example.com"), "exitA");
    }

    #[test]
    fn real_world_config_snippet() {
        let cfg = r#"
mode: rule
proxies:
  - name: node1
    type: ss
    server: 1.2.3.4
    port: 8388
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - DOMAIN-SUFFIX,youtube.com,PROXY
  - DOMAIN-SUFFIX,baidu.com,DIRECT
  - IP-CIDR,192.168.0.0/16,DIRECT
  - MATCH,PROXY
"#;
        let t = y(cfg);
        assert_eq!(t.default, "PROXY");
        assert_eq!(t.rules.len(), 2); // two PROXY rules; DIRECT skipped
        assert_eq!(t.rules[0].pattern, "*.google.com");
        assert_eq!(t.rules[1].pattern, "*.youtube.com");
        assert_eq!(t.resolve("www.google.com"), "PROXY");
        assert_eq!(t.resolve("www.baidu.com"), "PROXY"); // DIRECT skipped -> default
    }
}
