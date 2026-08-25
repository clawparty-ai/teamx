# Proxy Egress Routing (Split by Target Domain/IP) Design

- Doc type: design
- Related: `06-design-proxy.md` (SOCKS5 outbound proxy), `20-manual-tunnel-proxy-cli.md` (operations manual)
- Date: 2026-08-22
- Target version: teamx CLI (Rust) >= 0.2.1 (on top of the tunnel WS heartbeat + auto-reconnect fixes)

## 1. Background & Motivation

Multiple cloud hosts each run `proxy exit <name>` (egress / egress2 / ...), and locally you can start several `proxy start --port <local port> --exit <name>` at once. Currently **one local SOCKS5 port can bind only one exit**, so all traffic goes through the same egress. The user wants:

> On **one local SOCKS5 port**, automatically choose different egress exits based on the **target domain or IP**.

For example: `*.cn` goes through egress2 (35.79.166.197), `example.com` goes through egress (81.70.41.108), everything else goes through the default egress.

## 2. Feasibility Conclusion (code evidence)

Splitting by target is **entirely feasible** at the protocol layer and requires no extra DNS resolution:

- `socks5.rs:14-17`: `parse_connect_request` returns `SocksTarget { host, port }`, where **host is the domain or IP exactly as in the CONNECT request** (`curl --socks5-hostname` sends domains, `--socks5` sends resolved IPs). Locally, the data already knows who the target is.
- `tunnel_client.rs:556-576`: each connection parses `target` **before** establishing the WS and sending `{"type":"connect","name":"<exit>","target":"host:port"}` (`:604`).
- `tunnel_client.rs:510`: in `run_socks5_proxy(server_url, exit_name, local_port)` the `exit_name` is currently a fixed parameter.

**Conclusion**: replacing "fixed exit_name" with "after parsing the target, look up a `host/IP → exit` routing table to get exit_name" achieves per-target splitting. Changes concentrate on the local consumer side; **zero changes on server and exit sides**.

## 3. Design

### 3.1 New Route Table Format (JSON file, `--routes <path>`)

```json
{
  "default": "egress",
  "rules": [
    { "match": "*.cn",             "exit": "egress2" },
    { "match": "api.example.com",  "exit": "egress" },
    { "match": "10.0.0.0/8",       "exit": "egress2" },
    { "match": "192.168.1.5",      "exit": "egress" },
    { "match": "2001:db8::/32",    "exit": "egress2" }
  ]
}
```

- `default` (required): exit name used when no rule matches.
- `rules[]` (optional): ordered rules, **first match wins** (first-match).
- `match` supports three forms:
  | Form | Example | Semantics |
  |------|------|------|
  | Domain wildcard | `*.cn`, `api.example.com` | Suffix matching; `*.cn` matches `www.baidu.com`, `a.b.cn`, does **not** match `cn.com` |
  | Exact domain | `example.com` | Exactly equal to that domain (does not match `api.example.com`) |
  | IPv4/IPv6 CIDR | `10.0.0.0/8`, `2001:db8::/32` | When target is an IP, match by subnet |
  | Exact IP | `192.168.1.5` | When target is an IP, exact match (shorthand for CIDR /32 or /128) |

Recommended matching order (specificity before rule type):
1. If the target is a domain: check **exact domains** first, then **longest-suffix wildcards** (`api.example.com` over `*.example.com` over `*.com`) — but the **order in the rules file remains the final arbiter** (first-match, see §5 disputes).
2. If the target is an IP: exact IP > more specific CIDR (longer prefix first).

### 3.2 CLI Changes (`cli.rs`)

```rust
Start {
    port: u16,                    // unchanged
    exit: Option<String>,         // now optional: fixed exit when no SQLite config and no -f
    routes: Option<PathBuf>,      // new: route table JSON file; -f is a short alias
    server: Option<String>,       // unchanged
}
```

**Route source priority** (`proxy start`):
1. `-f <file>` / `--routes <file>` — explicit JSON file (temporary for this invocation, not written to DB)
2. **SQLite route table** (`proxy_routes` / `proxy_settings`, default behavior)
3. `--exit <name>` — fixed exit (backward compatible)
4. All missing → startup error, suggesting `proxy routes set-default` / `-f` / `--exit`

**New `proxy routes` subcommands** (managing the SQLite route table):

```
teamx proxy routes list                 # show default + rules
teamx proxy routes add <match> <exit> [--seq N]   # append or insert at position
teamx proxy routes remove <match>       # delete by match
teamx proxy routes set-default <exit>   # set default exit
teamx proxy routes import <file.json>   # import from JSON (full table replace)
teamx proxy routes clear                # clear rules (keep default)
```

### 3.3 New Module `routes.rs` (pure functions + SQLite persistence, unit-testable)

```rust
pub struct RouteRule {
    pub match_type: MatchType,   // ExactDomain | SuffixDomain | Cidr
    pub pattern: String,         // raw pattern text (domain/CIDR)
}

pub struct RouteTable {
    pub default: String,
    pub rules: Vec<RouteRule>,
}

impl RouteTable {
    /// Parse from JSON text (validates default exists and rule matches are legal).
    pub fn parse(json: &str) -> Result<RouteTable, String>;
    /// Given a SOCKS5 target host (domain or IP), return the exit name to use.
    pub fn resolve(&self, host: &str) -> &str;
}
```

- Inside `resolve`:
  - Try to parse `host` as an IP (`IpAddr::from_str`); on success → match by CIDR/exact IP.
  - On failure → match as a domain (exact + wildcard suffix).
  - No hit → `default`.
- Failed `IpAddr` parsing means it's a domain (no panic); empty host goes straight to `default`.

**SQLite persistence** (`proxy_routes` table stores rules + `proxy_settings` stores the default):

```rust
pub fn load_from_db(conn) -> Result<Option<RouteTable>, String>;  // None = not configured
pub fn save_to_db(conn, table) -> Result<(), String>;             // full table replace
pub fn upsert_rule(conn, seq, pattern, exit) -> Result<i64, String>;
pub fn remove_rule(conn, pattern) -> Result<bool, String>;
pub fn set_default(conn, exit) -> Result<(), String>;
pub fn clear_rules(conn) -> Result<(), String>;
pub fn to_json(table) -> serde_json::Value;                       // list output
```

### 3.4 Consumer-Side Changes (`tunnel_client.rs`)

- `socks5_proxy(server_url, exit_name, local_port, routes: Option<RouteTable>)` —
  adds the `routes` parameter (`None` → fixed exit, backward compatible).
- In the `run_socks5_proxy` loop: after parsing `target`,
  ```rust
  let exit = routes.as_ref().map(|t| t.resolve(&target.host)).unwrap_or(exit_name);
  ```
  then establish the WS connection to `exit` as usual (one per connection, dynamic exit name).
- **Resolved once per connection**: each SOCKS5 connection sends only one CONNECT — naturally precise.

### 3.5 Wiring (`commands.rs`)

```rust
ProxyCmd::Start { port, exit, routes, server } => {
    let table = match routes {
        Some(path) => RouteTable::parse(&fs::read_to_string(path)?),   // file first
        None => load_from_db(conn)?,                                   // SQLite fallback
    };
    let exit_name = table's default  || --exit || error;
    socks5_proxy(&url, &exit_name, *port, table)
}
ProxyCmd::Routes(rc) => proxy_routes_cmd(conn, rc),  // manage SQLite table
```

### 3.6 Server / Exit Side

**Zero changes**. The exit side only receives `{"type":"connect","name":"<exit>","target":...}`; the name comes from the local routing decision; the server side just looks up tunnels by name. Multiple coexisting exits are already supported (`tunnel.rs:178` name uniqueness constraint).

### 3.7 Data Model (`db.rs` migration v6)

```sql
CREATE TABLE IF NOT EXISTS proxy_routes (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  seq      INTEGER NOT NULL,          -- rule order (first-match)
  match    TEXT NOT NULL,             -- "*.cn" / "10.0.0.0/8" / IP / domain
  exit     TEXT NOT NULL,
  UNIQUE (seq)
);
CREATE TABLE IF NOT EXISTS proxy_settings (
  key   TEXT PRIMARY KEY,             -- 'default_exit'
  value TEXT NOT NULL
);
```

- Machine-global (not per-team): one SOCKS5 port has one route set — fits the use case.
- `proxy_routes` stores rules; `proxy_settings` stores the default exit.

## 4. Test Plan

### 4.1 Unit Tests (embedded `#[cfg(test)]` in `routes.rs`)

| Case | Input | Expected |
|------|------|------|
| Default fallback | no rule hits | returns `default` |
| Exact domain | `api.example.com` rule hit | returns corresponding exit |
| Wildcard suffix | `*.cn` matches `www.baidu.com`; `cn.com` not matched | correct split |
| Wildcard doesn't overreach | `*.example.com` does not match `example.com` itself | no hit |
| Exact IP | `192.168.1.5` | hit |
| CIDR IPv4 | `10.0.0.0/8` matches `10.1.2.3` | hit |
| CIDR IPv6 | `2001:db8::/32` | hit |
| Rule order | first-match | earliest hit wins |
| Empty host | `""` | returns default |
| Invalid JSON | missing default / bad match | parse error |
| SQLite round-trip | save → load equal | equal |
| SQLite default | load after set_default + add | default + rules correct |
| SQLite upsert | specified seq overwrites | in-place replacement |

### 4.2 Integration Tests (`tests/proxy-routes-test.ts`)

End-to-end verification (reusing existing serve/mTLS/SOCKS5 infrastructure):
1. Start **two** proxy exits (`egress` exposing IPv4 service svc-a, `egress2` exposing IPv6 service svc-b).
2. `proxy start --routes routes.json` (default=egress, `::1 → egress2`):
   - CONNECT `::1` → hits egress2 → returns svc-b content.
   - CONNECT `127.0.0.1` → default egress → returns svc-a content.
3. Regression: `proxy start --exit egress2` (no routes) → fixed egress still works.
4. **SQLite routes**: `proxy routes set-default egress` + `proxy routes add ::1 egress2`,
   then `proxy start` (no `--exit` / no `-f`) → reads from DB, splits correctly.

## 5. Trade-offs & Decision Records

| Question | Options | Decision | Rationale |
|------|------|------|------|
| Rule-match priority | longest suffix vs file order | **File-order first-match** (with an implicit exact-first convention) | Intuitive, predictable; users just sort by intent |
| Domains vs IPs in one table | yes | yes | Host strings enter `resolve` uniformly; internally split by "parses as IP or not" |
| Hot reload | read once at startup vs watch | Read once at startup (v1) | Minimal change; hot reload listed as future enhancement |
| Legacy entry compatibility | breaking signature change vs new entry | **Add `Option<RouteTable>` parameter to `socks5_proxy`** | Doesn't break `proxy exit` or existing tests |
| Relationship of `--exit` vs `-f/--routes` | conflict vs complementary | `-f` takes priority when given, SQLite next, `--exit` last resort | Flexible and backward compatible |
| Config storage | JSON file vs **SQLite** | **SQLite by default** (managed via `proxy routes`); `-f` temporary file override | Same DB as team state; persistent, queryable, manageable via commands |

## 6. Implementation Steps (completed)

1. Add `crates/teamx/src/routes.rs`: matcher (`MatchType` / `RouteRule` / `RouteTable`) + SQLite read/write + unit tests. ✅
2. `main.rs`: register `mod routes;`. ✅
3. `cli.rs`: add `-f/--routes` to `ProxyCmd::Start`; make `exit` optional; add `Routes(RoutesCmd)` subcommand (list/add/remove/set-default/import/clear). ✅
4. `tunnel_client.rs`: add `routes: Option<RouteTable>` to `socks5_proxy`; `run_socks5_proxy` resolves exit dynamically per target. ✅
5. `commands.rs`: `proxy start` route priority (file > SQLite > --exit); wire up the `proxy routes` subcommands. ✅
6. `db.rs`: migration v6 adding `proxy_routes` + `proxy_settings` tables. ✅
7. Tests: unit (routes.rs matching + SQLite round-trip) + integration (`tests/proxy-routes-test.ts` file routing + SQLite routing + fixed-exit regression). ✅
8. Docs: `docs/08-design-proxy-routes.md` (this file) + `docs/20-manual-tunnel-proxy-cli.md` (usage examples) + CHANGELOG.

## 7. Usage Examples

```bash
# Option 1: SQLite config (default)
teamx proxy routes set-default egress
teamx proxy routes add '*.cn' egress2
teamx proxy routes add '10.0.0.0/8' egress2
teamx proxy start --port 1080        # no --exit / no -f, routes read from SQLite

# Option 2: temporary JSON file (not written to DB)
cat > routes.json <<'EOF'
{ "default": "egress", "rules": [ { "match": "*.cn", "exit": "egress2" } ] }
EOF
teamx proxy start --port 1080 -f routes.json

# Option 3: fixed egress (backward compatible)
teamx proxy start --port 1080 --exit egress2
```

## 8. Non-Goals (out of scope for v1)

- Route-table hot reload / dynamic re-read
- Splitting by port / by protocol (TCP/UDP)
- Rule-hit statistics / logging
- Automatic failover based on exit online status (could be layered on top of the route table, but out of scope this time)
