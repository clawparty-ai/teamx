use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const DB_ENV: &str = "TEAMX_DB";

/// Resolve the teamx home directory — the current workspace's `.teamx/`.
///
/// Project-scoped by design: each opencode instance / CLI run uses the
/// `.teamx/` directory under its current working directory as its private
/// space (letters, mTLS certs, local DB, CA, instance id). `TEAMX_HOME`
/// overrides the location explicitly (used on servers and CI).
pub fn teamx_home() -> PathBuf {
    if let Ok(p) = std::env::var("TEAMX_HOME") {
        return PathBuf::from(p);
    }
    std::env::current_dir()
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join(".teamx")
}

/// The caller's workspace root (the directory holding `.teamx/`), when the
/// process actually runs inside a teamx workspace. `None` otherwise, so a
/// one-off command run from `/tmp` never pollutes the member's recorded path.
pub fn current_workspace() -> Option<String> {
    let home = teamx_home();
    if !home.is_dir() {
        return None;
    }
    let root = home.parent()?;
    let s = root.to_string_lossy().trim().to_string();
    if s.is_empty() || s.len() > 512 {
        None
    } else {
        Some(s)
    }
}

/// Attach the caller's workspace to an RPC args object. The server persists it
/// on the member (`members.workspace`) so `team status` / the console can show
/// where each member actually works. No-op outside a workspace, and never
/// overwrites a caller-provided value.
pub fn attach_workspace(mut args: Value) -> Value {
    if let Some(ws) = current_workspace() {
        if let Value::Object(map) = &mut args {
            map.entry("workspace".to_string()).or_insert(Value::String(ws));
        }
    }
    args
}

/// Resolve the local database path:
/// 1. `--db <path>` (handled by CLI, stored in `Cli.db`)
/// 2. `TEAMX_DB` env var
/// 3. `<workspace>/.teamx/teamx.db`
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var(DB_ENV) {
        return PathBuf::from(p);
    }
    teamx_home().join("teamx.db")
}

/// Open (creating if needed) the SQLite DB with WAL, busy timeout and FKs.
pub fn open(db_path: &Path) -> rusqlite::Result<Connection> {    if let Some(dir) = db_path.parent() {
        fs::create_dir_all(dir).ok();
    }
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Single-writer serialization: wait up to 5s for a concurrent writer.
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// The stable per-workspace instance id, persisted at `<workspace>/.teamx/instance.json`
/// (same file the plugin uses). Created on first use.
pub fn instance_id() -> String {
    let file = teamx_home().join("instance.json");
    if let Ok(raw) = std::fs::read_to_string(&file) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(id) = v.get("instance_id").and_then(|x| x.as_str()) {
                if !id.is_empty() {
                    return id.to_string();
                }
            }
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all(teamx_home()).ok();
    let _ = std::fs::write(&file, format!("{{\n  \"instance_id\": \"{id}\"\n}}\n"));
    id
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS teams (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL,
  owner_member_id TEXT,
  goal_id         TEXT,
  state           TEXT NOT NULL DEFAULT 'forming',
  invite_token    TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  -- 团队发现（design/015 SD-8/SD-16）：可见性与加入策略正交；默认最保守。
  visibility      TEXT NOT NULL DEFAULT 'private',
  join_policy     TEXT NOT NULL DEFAULT 'invite_only',
  public_summary  TEXT
);

CREATE TABLE IF NOT EXISTS members (
  id            TEXT PRIMARY KEY,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  session_key   TEXT NOT NULL,
  display_name  TEXT NOT NULL,
  role          TEXT,
  state         TEXT NOT NULL DEFAULT 'pending',
  loopx_project TEXT,
  last_seen_at  TEXT,
  last_ip       TEXT,
  joined_at     TEXT NOT NULL,
  left_at       TEXT,
  is_lead       INTEGER NOT NULL DEFAULT 0,
  user_id       TEXT NOT NULL DEFAULT '',
  discussion_quota INTEGER NOT NULL DEFAULT 20
);

CREATE TABLE IF NOT EXISTS goals (
  id         TEXT PRIMARY KEY,
  team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  title      TEXT NOT NULL,
  body       TEXT,
  state      TEXT NOT NULL DEFAULT 'proposed',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  closed_at  TEXT
);

-- Users (persons): a stable cross-device identity shared by one person's
-- multiple members/agents. Members reference this via `members.user_id`.
CREATE TABLE IF NOT EXISTS users (
  id           TEXT PRIMARY KEY,
  display_name TEXT NOT NULL UNIQUE,
  email        TEXT,
  created_by   TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS roles (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  team_id          TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  key              TEXT NOT NULL,
  label            TEXT NOT NULL,
  description      TEXT,
  permissions_json TEXT,
  state            TEXT NOT NULL DEFAULT 'approved',
  proposed_by      TEXT,
  UNIQUE(team_id, key)
);

-- append-only event ledger; current state is a projection over these rows
CREATE TABLE IF NOT EXISTS events (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  member_id    TEXT,
  seq          INTEGER NOT NULL,
  type         TEXT NOT NULL,
  payload_json TEXT,
  created_at   TEXT NOT NULL,
  UNIQUE(team_id, seq)
);

-- open questions (clarification.asked/responded)
CREATE TABLE IF NOT EXISTS questions (
  id                TEXT PRIMARY KEY,
  team_id           TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  asker_member_id   TEXT NOT NULL,
  target_member_id  TEXT NOT NULL,
  question          TEXT NOT NULL,
  answer            TEXT,
  state             TEXT NOT NULL DEFAULT 'open',
  created_at        TEXT NOT NULL,
  answered_at       TEXT
);

-- per-session sync cursors (advanced automatically by `teamx sync`)
CREATE TABLE IF NOT EXISTS sync_cursors (
  session_key TEXT NOT NULL,
  team_id     TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  last_seq    INTEGER NOT NULL,
  PRIMARY KEY(session_key, team_id)
);

-- invitation letters (network mode): owner issues a member cert + letter; the
-- pre-allocated member_id is carried by the cert CN so the server can map a
-- verified certificate back to this row and the future member.
CREATE TABLE IF NOT EXISTS invitations (
  id            TEXT PRIMARY KEY,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  member_id     TEXT NOT NULL,
  role_key      TEXT NOT NULL,
  role_label    TEXT,
  role_desc     TEXT,
  cert_serial   TEXT,
  cert_cn       TEXT,
  user_id       TEXT NOT NULL DEFAULT '',
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  used_by       TEXT,
  used_at       TEXT,
  revoked_at    TEXT,
  auto_approve  INTEGER NOT NULL DEFAULT 0,
  as_lead       INTEGER NOT NULL DEFAULT 0
);

-- Enterprise analytics: per-node member activity (time/duration/tokens/cost/
-- work content) pushed to the team lead's serve database. `node_id` is the
-- source machine's instance_id; sensitive fields are kept in full (audit).
CREATE TABLE IF NOT EXISTS activity (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  team_id           TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  member_id         TEXT NOT NULL,
  node_id           TEXT NOT NULL,
  node_name         TEXT,
  started_at        TEXT NOT NULL,
  ended_at          TEXT,
  duration_ms       INTEGER,
  kind              TEXT NOT NULL,
  detail            TEXT,
  tokens_input      INTEGER,
  tokens_output     INTEGER,
  tokens_reasoning  INTEGER,
  cost              REAL,
  model_id          TEXT,
  provider_id       TEXT,
  tokens_cache_read  INTEGER,
  tokens_cache_write INTEGER,
  cost_source       TEXT NOT NULL DEFAULT 'none',
  cost_input        REAL,
  cost_output       REAL,
  cost_cache_read   REAL,
  cost_cache_write  REAL,
  has_human         INTEGER NOT NULL DEFAULT 0,
  created_at        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_activity_team_time ON activity(team_id, started_at);
CREATE INDEX IF NOT EXISTS idx_activity_member_time ON activity(member_id, started_at);
CREATE INDEX IF NOT EXISTS idx_activity_node ON activity(node_id);

-- Enterprise cost: per-(provider,model) token pricing (USD / 1M tokens).
-- Source priority at read time: owner > opencode > builtin (single row per key).
CREATE TABLE IF NOT EXISTS model_pricing (
  provider_id          TEXT NOT NULL,
  model_id             TEXT NOT NULL,
  price_input          REAL NOT NULL,
  price_output         REAL NOT NULL,
  price_cache_read     REAL NOT NULL DEFAULT 0,
  price_cache_write    REAL NOT NULL DEFAULT 0,
  over200k_input       REAL,
  over200k_output      REAL,
  over200k_cache_read  REAL,
  over200k_cache_write REAL,
  source               TEXT NOT NULL DEFAULT 'builtin',
  updated_at           TEXT NOT NULL,
  PRIMARY KEY (provider_id, model_id)
);

-- Enterprise presence: per-member real-time state (latest snapshot only, no
-- history). `updated_at` is the heartbeat time; readers apply a TTL to treat
-- stale rows as offline.
CREATE TABLE IF NOT EXISTS presence (
  member_id        TEXT PRIMARY KEY,
  active_sessions  INTEGER NOT NULL DEFAULT 0,
  running_tools    INTEGER NOT NULL DEFAULT 0,
  open_terminals   INTEGER NOT NULL DEFAULT 0,
  updated_at       TEXT NOT NULL
);

-- Connection audit log: every authenticated member connection (long-lived
-- WS/tunnel) records peer IP + timestamps for auditing.
CREATE TABLE IF NOT EXISTS member_connections (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  member_id        TEXT NOT NULL,
  team_id          TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  ip               TEXT NOT NULL,
  endpoint         TEXT NOT NULL,
  connected_at     TEXT NOT NULL,
  disconnected_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_conn_member ON member_connections(member_id, connected_at);
CREATE INDEX IF NOT EXISTS idx_conn_team ON member_connections(team_id, connected_at);

-- proxy exit routing table (local consumer config, see routes.rs).
-- Holds the ordered per-target egress rules used by `teamx proxy start`
-- when started without -f/--routes. `default` lives in proxy_settings.
CREATE TABLE IF NOT EXISTS proxy_routes (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  seq      INTEGER NOT NULL,
  match    TEXT NOT NULL,
  exit     TEXT NOT NULL,
  UNIQUE (seq)
);

-- Small key/value settings for the proxy consumer (e.g. default exit).
CREATE TABLE IF NOT EXISTS proxy_settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Local client config: each local member connects to a different server.
CREATE TABLE IF NOT EXISTS local_members (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  member_key   TEXT UNIQUE NOT NULL,
  display_name TEXT NOT NULL,
  server_url   TEXT NOT NULL,
  letter_id    TEXT,
  proxy_port   INTEGER NOT NULL DEFAULT 1080,
  dns_port     INTEGER NOT NULL DEFAULT 53,
  enabled      INTEGER NOT NULL DEFAULT 1,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

-- Local environment state (e.g. whether this machine runs a server, which
-- member is active in the GUI).
CREATE TABLE IF NOT EXISTS local_settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Git repositories (network mode): each team can have multiple repos.
CREATE TABLE IF NOT EXISTS git_repos (
  id            TEXT PRIMARY KEY,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  path          TEXT NOT NULL,  -- server-side filesystem path
  description   TEXT,
  is_bare       INTEGER NOT NULL DEFAULT 1,  -- bare repo
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL,
  UNIQUE(team_id, name)
);

-- Git repository permissions: per-member access control.
CREATE TABLE IF NOT EXISTS git_repo_permissions (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  repo_id       TEXT NOT NULL REFERENCES git_repos(id) ON DELETE CASCADE,
  member_id     TEXT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
  permission    TEXT NOT NULL DEFAULT 'read',  -- read, write, admin
  granted_by    TEXT NOT NULL,
  granted_at    TEXT NOT NULL,
  UNIQUE(repo_id, member_id)
);

-- Active reverse-tunnel registry (network mode). Written by `teamx serve`
-- when a provider registers/unregisters a tunnel so DB-reading tools (e.g.
-- the admin-console /api/topology) can show live reverse-tunnel edges even
-- though the authoritative registry lives in the server process memory.
CREATE TABLE IF NOT EXISTS tunnels (
  team_id            TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  name               TEXT NOT NULL,
  provider_member_id TEXT NOT NULL,
  mode               TEXT NOT NULL DEFAULT 'local',  -- local | frp | proxy
  port               INTEGER NOT NULL,
  target_port        INTEGER,
  lan_ip             TEXT,
  created_at         TEXT NOT NULL,
  updated_at         TEXT NOT NULL,
  PRIMARY KEY (team_id, name)
);
";

/// Apply the schema (idempotent) + migrations.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    // taskx (server-backed tasks for network teams, see docs/33-server-taskx-rpc.cn.md).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS taskx (
           id           TEXT NOT NULL,
           team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
           title        TEXT NOT NULL,
           body         TEXT,
           state        TEXT NOT NULL DEFAULT 'assigned',
           mode         TEXT NOT NULL DEFAULT 'direct',
           executor     TEXT NOT NULL DEFAULT 'either',
           priority     TEXT NOT NULL DEFAULT 'medium',
           assignee_member_id TEXT,
           role_key     TEXT,
           created_by_member_id TEXT NOT NULL,
           created_at   TEXT NOT NULL,
           updated_at   TEXT NOT NULL,
           PRIMARY KEY (team_id, id)
         );
         CREATE INDEX IF NOT EXISTS idx_taskx_team   ON taskx(team_id);
         CREATE INDEX IF NOT EXISTS idx_taskx_assign ON taskx(assignee_member_id);
         CREATE INDEX IF NOT EXISTS idx_taskx_role   ON taskx(role_key);",
    )?;
    // Unified per-member inbox: last nudge seq the member has read (server-side
    // cursor, shared by every session/device of that member).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inbox_cursors (
           member_id TEXT NOT NULL,
           team_id   TEXT NOT NULL,
           last_seq  INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (member_id, team_id)
         );",
    )?;
    // IM outbound notifier (docs/57): pending outbound pushes keyed by the
    // ledger seq (idempotency: PRIMARY KEY (team_id, seq) makes re-collection
    // after restart a no-op), plus a tiny kv store for the notifier's
    // collection cursor / 429 backoff deadline. Converge-style: brand-new
    // tables with no legacy data, so we deliberately do NOT bump
    // user_version — the version numbering collided across product lines
    // before (see converge_columns); a shape-check `CREATE TABLE IF NOT
    // EXISTS` is idempotent and conflict-free with parallel schema bumps.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS im_outbox (
           team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
           seq         INTEGER NOT NULL,
           event_type  TEXT NOT NULL,
           payload_json TEXT,
           direct      INTEGER NOT NULL DEFAULT 0,
           state      TEXT NOT NULL DEFAULT 'pending',
           attempts   INTEGER NOT NULL DEFAULT 0,
           last_error TEXT,
           created_at TEXT NOT NULL,
           pushed_at  TEXT,
           PRIMARY KEY (team_id, seq)
         );
         CREATE INDEX IF NOT EXISTS idx_im_outbox_state ON im_outbox(state);
         CREATE TABLE IF NOT EXISTS im_state (
           key   TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );",
    )?;
    // 平台面（design/014 §6/§7）：一次性 bootstrap token + 平台管理员 + **独立审计**。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS platform_tokens (
           id           TEXT PRIMARY KEY,
           token_hash   TEXT NOT NULL UNIQUE,
           purpose      TEXT NOT NULL DEFAULT 'bootstrap',
           state        TEXT NOT NULL DEFAULT 'issued',
           created_at   TEXT NOT NULL,
           expires_at   TEXT,
           redeemed_at  TEXT,
           redeemed_by  TEXT
         );
         CREATE TABLE IF NOT EXISTS platform_admins (
           user_id     TEXT PRIMARY KEY,
           user_name   TEXT NOT NULL,
           granted_at  TEXT NOT NULL,
           granted_via TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS platform_audit (
           id     INTEGER PRIMARY KEY AUTOINCREMENT,
           at     TEXT NOT NULL,
           actor  TEXT NOT NULL,
           action TEXT NOT NULL,
           detail TEXT
         );",
    )?;
    // 多租户平台（design/014 第二步）：一个租户 = 一个 team + 一个 namespace + 一个独立 CA。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tenants (
           team_id        TEXT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
           namespace      TEXT NOT NULL,
           ca_fingerprint TEXT NOT NULL,
           server_image   TEXT NOT NULL DEFAULT '',
           created_at     TEXT NOT NULL,
           -- 租户 server 的**可达端点**（operator 显式配置；O-5 未定案前靠它，
           -- 例如同机 port-forward 的 https://127.0.0.1:5790）。空 = 未配置。
           endpoint       TEXT NOT NULL DEFAULT ''
         );",
    )?;
    // 云控制台用户（Phase 1：密码登录；GitHub 见 Phase 3）。
    // password_hash 格式 `pbkdf2-sha256$<iter>$<salt-b64>$<hash-b64>`（ring，无新依赖；
    // 前缀版本化，将来可迁 argon2id 而不断旧行）。github_id 为空 = 纯密码用户。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS platform_users (
           id            TEXT PRIMARY KEY,
           display_name  TEXT NOT NULL,
           github_id     TEXT NOT NULL DEFAULT '',
           password_hash TEXT NOT NULL DEFAULT '',
           disabled_at   TEXT,
           created_at    TEXT NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_platform_users_github
           ON platform_users(github_id) WHERE github_id != '';
         CREATE UNIQUE INDEX IF NOT EXISTS idx_platform_users_name
           ON platform_users(display_name);
         CREATE TABLE IF NOT EXISTS platform_sessions (
           token      TEXT PRIMARY KEY,
           user_id    TEXT NOT NULL REFERENCES platform_users(id) ON DELETE CASCADE,
           created_at TEXT NOT NULL,
           expires_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_platform_sessions_user ON platform_sessions(user_id);",
    )?;
    // 用户归属 team Phase 2：租户归属 + 配额（新表；不 ALTER 老表）。
    // tenant_owners：租户 → 平台用户（自助建租户的用户）；无行 = 历史/operator 租户（仅 operator 可见）。
    // platform_quotas：用户配额（无行 = 默认配额，见 platform::DEFAULT_TENANT_QUOTA）。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tenant_owners (
           team_id    TEXT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
           user_id    TEXT NOT NULL,
           created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_tenant_owners_user ON tenant_owners(user_id);
         CREATE TABLE IF NOT EXISTS platform_quotas (
           user_id      TEXT PRIMARY KEY,
           tenant_quota INTEGER NOT NULL,
           updated_at   TEXT NOT NULL
         );",
    )?;
    // 签发台账 O-2（design/014 §4）：server 能按 CA 分代枚举自己签发的有效证书。
    // 所有经 CA 签发的证书（member/server/service）都在签发处落一行；
    // 平台库记平台侧签发，租户 server 库记租户侧签发，ca-audit 合并两边。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS issued_certs (
           serial_hex     TEXT PRIMARY KEY,
           cn             TEXT NOT NULL,
           kind           TEXT NOT NULL,
           role           TEXT NOT NULL DEFAULT '',
           member_id      TEXT NOT NULL DEFAULT '',
           team_id        TEXT NOT NULL DEFAULT '',
           ca_fingerprint TEXT NOT NULL,
           issued_at      TEXT NOT NULL,
           expires_at     TEXT,
           revoked_at     TEXT,
           issuer         TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX IF NOT EXISTS idx_issued_certs_ca ON issued_certs(ca_fingerprint);
         CREATE INDEX IF NOT EXISTS idx_issued_certs_team ON issued_certs(team_id);",
    )?;
    // Cloud-host provisioning (design/006: invite 时开通云主机，server 侧 AWS CLI 执行).
    // Converge-style: brand-new tables, no user_version bump needed.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS nodes (
           id              TEXT PRIMARY KEY,
           team_id         TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
           user_id         TEXT NOT NULL DEFAULT '',
           member_id       TEXT NOT NULL,
           provider        TEXT NOT NULL DEFAULT 'aws',
           region          TEXT,
           instance_id     TEXT,
           ip              TEXT,
           state           TEXT NOT NULL DEFAULT 'requested',
           im_enabled      INTEGER NOT NULL DEFAULT 0,
           os_user         TEXT,
           keypair_ref     TEXT,
           created_at      TEXT NOT NULL,
           ready_at        TEXT,
           terminated_at   TEXT,
           cost_snapshot   TEXT,
           compliance_tags TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_nodes_team   ON nodes(team_id);
         CREATE INDEX IF NOT EXISTS idx_nodes_user   ON nodes(user_id);
         CREATE INDEX IF NOT EXISTS idx_nodes_member ON nodes(member_id);
         CREATE INDEX IF NOT EXISTS idx_nodes_state  ON nodes(state);

         CREATE TABLE IF NOT EXISTS provision_jobs (
           id          TEXT PRIMARY KEY,
           node_id     TEXT,
           team_id     TEXT NOT NULL,
           member_id   TEXT NOT NULL,
           state       TEXT NOT NULL DEFAULT 'requested',
           attempts    INTEGER NOT NULL DEFAULT 0,
           last_error  TEXT,
           idem_key    TEXT NOT NULL,
           created_at  TEXT NOT NULL,
           updated_at  TEXT NOT NULL
         );
         -- 幂等：同 (team_id, member_id) 只允许一个未终结的 job（C4 模式）。
         CREATE UNIQUE INDEX IF NOT EXISTS idx_provision_jobs_idem
           ON provision_jobs(idem_key) WHERE state <> 'terminated';
         CREATE INDEX IF NOT EXISTS idx_provision_jobs_state ON provision_jobs(state);

         CREATE TABLE IF NOT EXISTS node_secrets (
           node_id    TEXT NOT NULL,
           kind       TEXT NOT NULL,
           ref        TEXT NOT NULL,
           created_at TEXT NOT NULL,
           PRIMARY KEY (node_id, kind)
         );",
    )?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    // v2-era rebuild only applies to legacy DBs that still have the `sessions`
    // table with a single-column primary key.
    let has_sessions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
        [],
        |r| r.get(0),
    )?;
    if version < 2 && has_sessions > 0 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions_new (
               session_key TEXT NOT NULL,
               team_id     TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               member_id   TEXT NOT NULL,
               created_at  TEXT NOT NULL,
               PRIMARY KEY (session_key, team_id),
               UNIQUE (member_id)
             );
             INSERT OR IGNORE INTO sessions_new (session_key, team_id, member_id, created_at)
               SELECT session_key, team_id, member_id, created_at FROM sessions;
             DROP TABLE sessions;
             ALTER TABLE sessions_new RENAME TO sessions;",
        )?;
    }

    if version < 3 {
        // v3: drop the redundant `sessions` registry (members already carries
        // session_key); dedupe members/goals; enforce uniqueness.
        conn.execute_batch(
            "DROP TABLE IF EXISTS sessions;
             DELETE FROM members WHERE rowid NOT IN (
               SELECT MAX(rowid) FROM members GROUP BY team_id, session_key
             );
             DELETE FROM goals WHERE rowid NOT IN (
               SELECT MAX(rowid) FROM goals GROUP BY team_id
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_members_team_session ON members(team_id, session_key);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_goals_team ON goals(team_id);",
        )?;
    }
    if version < 4 {
        // v4: custom roles — roles gain a state (proposed/approved) and a
        // proposer, so members can propose their own job role and the owner
        // approves it. Existing roles stay approved. Idempotent: fresh DBs
        // already create the columns via SCHEMA, so only add when missing.
        let cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(roles)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        if !cols.iter().any(|c| c == "state") {
            conn.execute_batch("ALTER TABLE roles ADD COLUMN state TEXT NOT NULL DEFAULT 'approved';")?;
        }
        if !cols.iter().any(|c| c == "proposed_by") {
            conn.execute_batch("ALTER TABLE roles ADD COLUMN proposed_by TEXT;")?;
        }
    }
    if version < 6 {
        // v6: proxy exit routing table. Idempotent: fresh DBs create these via
        // SCHEMA, so only the version bump matters here.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS proxy_routes (
               id       INTEGER PRIMARY KEY AUTOINCREMENT,
               seq      INTEGER NOT NULL,
               match    TEXT NOT NULL,
               exit     TEXT NOT NULL,
               UNIQUE (seq)
             );
             CREATE TABLE IF NOT EXISTS proxy_settings (
               key   TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );")?;
    }
    if version < 7 {
        // v7 (merged from both branches):
        //  - enterprise multi-goal support: goals.closed_at marks a goal as
        //    finished; the UNIQUE(team_id) index is dropped so several goals
        //    can exist per team.
        //  - connection audit: member_connections table + members.last_ip.
        let goal_cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(goals)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        if !goal_cols.iter().any(|c| c == "closed_at") {
            conn.execute_batch("ALTER TABLE goals ADD COLUMN closed_at TEXT;")?;
        }
        conn.execute_batch("DROP INDEX IF EXISTS idx_goals_team;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS member_connections (
               id               INTEGER PRIMARY KEY AUTOINCREMENT,
               member_id        TEXT NOT NULL,
               team_id          TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               ip               TEXT NOT NULL,
               endpoint         TEXT NOT NULL,
               connected_at     TEXT NOT NULL,
               disconnected_at  TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_conn_member ON member_connections(member_id, connected_at);
             CREATE INDEX IF NOT EXISTS idx_conn_team ON member_connections(team_id, connected_at);",
        )?;
        let mcols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        if !mcols.iter().any(|c| c == "last_ip") {
            conn.execute_batch("ALTER TABLE members ADD COLUMN last_ip TEXT;")?;
        }
    }
    if version < 8 {
        // v8: local client config — local_members (per-member server/ports) +
        // local_settings (local serve state, active member). SCHEMA covers fresh
        // DBs; idempotent for existing ones.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_members (
               id           INTEGER PRIMARY KEY AUTOINCREMENT,
               member_key   TEXT UNIQUE NOT NULL,
               display_name TEXT NOT NULL,
               server_url   TEXT NOT NULL,
               letter_id    TEXT,
               proxy_port   INTEGER NOT NULL DEFAULT 1080,
               dns_port     INTEGER NOT NULL DEFAULT 53,
               enabled      INTEGER NOT NULL DEFAULT 1,
               created_at   TEXT NOT NULL,
               updated_at   TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS local_settings (
               key   TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );",
        )?;
    }
    if version < 9 {
        // v9 (enterprise): activity analytics table for existing DBs; SCHEMA
        // already covers fresh DBs, so this is purely a backfill step.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS activity (
               id               INTEGER PRIMARY KEY AUTOINCREMENT,
               team_id          TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               member_id        TEXT NOT NULL,
               node_id          TEXT NOT NULL,
               node_name        TEXT,
               started_at       TEXT NOT NULL,
               ended_at         TEXT,
               duration_ms      INTEGER,
               kind             TEXT NOT NULL,
               detail           TEXT,
               tokens_input     INTEGER,
               tokens_output    INTEGER,
               tokens_reasoning INTEGER,
               cost             REAL,
               has_human        INTEGER NOT NULL DEFAULT 0,
               created_at       TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_activity_team_time ON activity(team_id, started_at);
             CREATE INDEX IF NOT EXISTS idx_activity_member_time ON activity(member_id, started_at);
             CREATE INDEX IF NOT EXISTS idx_activity_node ON activity(node_id);
             -- v9 (main): git repositories — git_repos + git_repo_permissions.
             CREATE TABLE IF NOT EXISTS git_repos (
               id            TEXT PRIMARY KEY,
               team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               name          TEXT NOT NULL,
               path          TEXT NOT NULL,
               description   TEXT,
               is_bare       INTEGER NOT NULL DEFAULT 1,
               created_by    TEXT NOT NULL,
               created_at    TEXT NOT NULL,
               updated_at    TEXT NOT NULL,
               UNIQUE(team_id, name)
             );
             CREATE TABLE IF NOT EXISTS git_repo_permissions (
               id            INTEGER PRIMARY KEY AUTOINCREMENT,
               repo_id       TEXT NOT NULL REFERENCES git_repos(id) ON DELETE CASCADE,
               member_id     TEXT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
               permission    TEXT NOT NULL DEFAULT 'read',
               granted_by    TEXT NOT NULL,
               granted_at    TEXT NOT NULL,
               UNIQUE(repo_id, member_id)
             );",
        )?;
    }
    if version < 10 {
        // v10 (enterprise cost): model_pricing table (SCHEMA covers fresh DBs) +
        // activity gains model attribution, cache token columns, and the cost
        // decomposition columns (cost_source / cost_input / ...). Idempotent:
        // only add a column when missing.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS model_pricing (
               provider_id          TEXT NOT NULL,
               model_id             TEXT NOT NULL,
               price_input          REAL NOT NULL,
               price_output         REAL NOT NULL,
               price_cache_read     REAL NOT NULL DEFAULT 0,
               price_cache_write    REAL NOT NULL DEFAULT 0,
               over200k_input       REAL,
               over200k_output      REAL,
               over200k_cache_read  REAL,
               over200k_cache_write REAL,
               source               TEXT NOT NULL DEFAULT 'builtin',
               updated_at           TEXT NOT NULL,
               PRIMARY KEY (provider_id, model_id)
             );",
        )?;
        let activity_cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(activity)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        let add_cols: &[(&str, &str)] = &[
            ("model_id", "TEXT"),
            ("provider_id", "TEXT"),
            ("tokens_cache_read", "INTEGER"),
            ("tokens_cache_write", "INTEGER"),
            ("cost_source", "TEXT NOT NULL DEFAULT 'none'"),
            ("cost_input", "REAL"),
            ("cost_output", "REAL"),
            ("cost_cache_read", "REAL"),
            ("cost_cache_write", "REAL"),
        ];
        for (name, decl) in add_cols {
            if !activity_cols.iter().any(|c| c == name) {
                conn.execute_batch(&format!("ALTER TABLE activity ADD COLUMN {name} {decl};"))?;
            }
        }
        // Seed the built-in pricing table (lowest priority; owner/opencode rows
        // overwrite these).
        crate::activity::seed_builtin_pricing(conn)?;
    }
    if version < 11 {
        // v11 (enterprise presence): per-member real-time state table. SCHEMA
        // covers fresh DBs; idempotent for existing ones.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS presence (
               member_id        TEXT PRIMARY KEY,
               active_sessions  INTEGER NOT NULL DEFAULT 0,
               running_tools    INTEGER NOT NULL DEFAULT 0,
               open_terminals   INTEGER NOT NULL DEFAULT 0,
               updated_at       TEXT NOT NULL
             );",
        )?;
    }
    if version < 12 {
        // v12: multiple team leads — members gain an `is_lead` flag so the owner
        // can promote backup team leads (co-leads). Idempotent.
        let cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };
        if !cols.iter().any(|c| c == "is_lead") {
            conn.execute_batch("ALTER TABLE members ADD COLUMN is_lead INTEGER NOT NULL DEFAULT 0;")?;
        }
    }
    if version < 13 {
        // v13 (main): user identity — a `users` table (person, cross-device) plus
        // `user_id` columns on members and invitations. Merged from main's v11;
        // renumbered to v13 because enterprise already uses v11 (presence) and
        // v12 (is_lead). Idempotent: SCHEMA covers fresh DBs; the ALTERs are
        // guarded by column existence.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
               id           TEXT PRIMARY KEY,
               display_name TEXT NOT NULL UNIQUE,
               email        TEXT,
               created_by   TEXT NOT NULL,
               created_at   TEXT NOT NULL,
               updated_at   TEXT NOT NULL
             );",
        )?;
        let mcols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !mcols.iter().any(|c| c == "user_id") {
            conn.execute_batch("ALTER TABLE members ADD COLUMN user_id TEXT NOT NULL DEFAULT '';")?;
        }
        let icols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(invitations)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !icols.iter().any(|c| c == "user_id") {
            conn.execute_batch("ALTER TABLE invitations ADD COLUMN user_id TEXT NOT NULL DEFAULT '';")?;
        }
    }
    if version < 14 {
        // v14: invitation flags — `auto_approve` (member becomes active on
        // import, skipping owner approval) and `as_lead` (member is granted
        // co-lead on import). Owner-only for `as_lead`. SCHEMA covers fresh
        // DBs; ALTERs are guarded by column existence.
        let icols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(invitations)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !icols.iter().any(|c| c == "auto_approve") {
            conn.execute_batch("ALTER TABLE invitations ADD COLUMN auto_approve INTEGER NOT NULL DEFAULT 0;")?;
        }
        if !icols.iter().any(|c| c == "as_lead") {
            conn.execute_batch("ALTER TABLE invitations ADD COLUMN as_lead INTEGER NOT NULL DEFAULT 0;")?;
        }
    }
    if version < 15 {
        // v15: per-member discussion participation quota (per-topic max active
        // replies). Default 20 (raised from 5: a 6-question grill needs 6+
        // active posts per topic); owner/lead may raise/lower per member via
        // `member discussion-quota`. Mention replies never consume quota.
        let mcols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(members)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !mcols.iter().any(|c| c == "discussion_quota") {
            conn.execute_batch("ALTER TABLE members ADD COLUMN discussion_quota INTEGER NOT NULL DEFAULT 20;")?;
        }
    }
    if version < 16 {
        // v16: discussion subscriptions — a member opts in to be woken for a
        // discussion's new messages (G5 inbox trigger).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS discussion_subs (
               team_id       TEXT NOT NULL,
               member_id     TEXT NOT NULL,
               discussion_id TEXT NOT NULL,
               created_at    TEXT NOT NULL,
               PRIMARY KEY (team_id, member_id, discussion_id)
             );",
        )?;
    }

    if version < 17 {
        // v17: taskx id is scoped per team — PK `(team_id, id)` so two teams can
        // each own `task-1` (issue #13). The old global PK was stricter, so every
        // existing row already satisfies the new constraint (no dedup needed).
        // SQLite cannot alter a PK in place → rebuild the table.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS taskx_new (
               id           TEXT NOT NULL,
               team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               title        TEXT NOT NULL,
               body         TEXT,
               state        TEXT NOT NULL DEFAULT 'assigned',
               mode         TEXT NOT NULL DEFAULT 'direct',
               executor     TEXT NOT NULL DEFAULT 'either',
               priority     TEXT NOT NULL DEFAULT 'medium',
               assignee_member_id TEXT,
               role_key     TEXT,
               created_by_member_id TEXT NOT NULL,
               created_at   TEXT NOT NULL,
               updated_at   TEXT NOT NULL,
               PRIMARY KEY (team_id, id)
             );
             INSERT OR IGNORE INTO taskx_new
               SELECT id, team_id, title, body, state, mode, executor, priority,
                      assignee_member_id, role_key, created_by_member_id,
                      created_at, updated_at
                 FROM taskx;
             DROP TABLE taskx;
             ALTER TABLE taskx_new RENAME TO taskx;
             CREATE INDEX IF NOT EXISTS idx_taskx_team   ON taskx(team_id);
             CREATE INDEX IF NOT EXISTS idx_taskx_assign ON taskx(assignee_member_id);
             CREATE INDEX IF NOT EXISTS idx_taskx_role   ON taskx(role_key);",
        )?;
    }

    if version < 17 {
        // v17: taskx `id` is scoped per team — PRIMARY KEY (team_id, id) so two
        // teams on one server may each own `task-1` (issue #13). The previous
        // global PK is *stricter* than the new one, so every existing row
        // already satisfies the new constraint — no dedup is needed.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS taskx_v17 (
               id           TEXT NOT NULL,
               team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               title        TEXT NOT NULL,
               body         TEXT,
               state        TEXT NOT NULL DEFAULT 'assigned',
               mode         TEXT NOT NULL DEFAULT 'direct',
               executor     TEXT NOT NULL DEFAULT 'either',
               priority     TEXT NOT NULL DEFAULT 'medium',
               assignee_member_id TEXT,
               role_key     TEXT,
               created_by_member_id TEXT NOT NULL,
               created_at   TEXT NOT NULL,
               updated_at   TEXT NOT NULL,
               PRIMARY KEY (team_id, id)
             );
             INSERT OR IGNORE INTO taskx_v17
               SELECT id, team_id, title, body, state, mode, executor, priority,
                      assignee_member_id, role_key, created_by_member_id,
                      created_at, updated_at
                 FROM taskx;
             DROP TABLE taskx;
             ALTER TABLE taskx_v17 RENAME TO taskx;
             CREATE INDEX IF NOT EXISTS idx_taskx_team   ON taskx(team_id);
             CREATE INDEX IF NOT EXISTS idx_taskx_assign ON taskx(assignee_member_id);
             CREATE INDEX IF NOT EXISTS idx_taskx_role   ON taskx(role_key);",
        )?;
    }

    if version < 18 {
        // v18: member-to-member direct-connect candidates (data-plane rendezvous).
        // The server is the control plane: it records each member's LAN IPs and
        // its server-reflexive address (peer IP:port as seen by the server), so
        // same-team members can attempt a direct connection before falling back
        // to the tunnel relay.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS direct_endpoints (
               member_id      TEXT NOT NULL,
               team_id        TEXT NOT NULL,
               lan_ips        TEXT NOT NULL DEFAULT '[]',
               reflexive_ip   TEXT,
               reflexive_port INTEGER,
               listener_port  INTEGER,
               updated_at     TEXT NOT NULL,
               PRIMARY KEY (member_id, team_id)
             );",
        )?;
    }

    if version < 19 {
        // v19: one-time pairing tickets for direct connections. `direct.probe`
        // issues a short-TTL nonce bound to (from_member → to_member); the P1
        // direct handshake must present it, so "knowing the IP" is not enough to
        // connect. Also serves as the probe rate-limit ledger (recent rows).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS direct_tickets (
               ticket      TEXT PRIMARY KEY,
               team_id     TEXT NOT NULL,
               from_member TEXT NOT NULL,
               to_member   TEXT NOT NULL,
               created_at  TEXT NOT NULL,
               expires_at  TEXT NOT NULL,
               used_at     TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_direct_tickets_pair ON direct_tickets(from_member, to_member, created_at);",
        )?;
    }
    if version < 20 {
        // v20: IM identity binding (IM P1 — docs/design/im-access-requirements.md
        // §5.2/§6). A binding code is pre-associated with a member, single-use,
        // 5-minute TTL and stored **as a hash only**; the binding table is the
        // identity source for inbound IM actions (the gateway never carries a
        // member id). `im_bind_attempts` backs per-IM-user throttling; it stores
        // epoch seconds so the window filter never depends on text formatting.
        // (Renumbered twice: v17 was taken by the issue-13 taskx PK rebuild,
        // v18/v19 by the direct-connect P0/P0.5 migrations — see 交接 decision
        // #8; next free version is now v21.)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS im_bind_codes (
               code_hash  TEXT PRIMARY KEY,
               member_id  TEXT NOT NULL,
               team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               scope      TEXT NOT NULL DEFAULT 'self',
               created_by TEXT NOT NULL,
               created_at TEXT NOT NULL,
               expires_at TEXT NOT NULL,
               used_at    TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_im_bind_codes_member ON im_bind_codes(member_id, team_id);
             CREATE TABLE IF NOT EXISTS im_bindings (
               platform     TEXT NOT NULL,
               im_user_id   TEXT NOT NULL,
               member_id    TEXT NOT NULL,
               team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               bound_at     TEXT NOT NULL,
               bound_by     TEXT NOT NULL,
               revoked_at   TEXT,
               last_seen_at TEXT,
               PRIMARY KEY (platform, im_user_id, team_id)
             );
             CREATE INDEX IF NOT EXISTS idx_im_bindings_member ON im_bindings(member_id);
             CREATE TABLE IF NOT EXISTS im_bind_attempts (
               platform     TEXT NOT NULL,
               im_user_id   TEXT NOT NULL,
               attempted_at INTEGER NOT NULL,
               ok           INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_im_bind_attempts
               ON im_bind_attempts(platform, im_user_id, attempted_at);",
        )?;
    }

    // IM inbound gateway (im-p1-inbound-gateway-security): one-time nonce
    // replay protection. PRIMARY KEY(platform, nonce, team_id) makes a replay
    // INSERT fail; rows older than the 7d TTL are pruned opportunistically.
    // Converged-style: no user_version bump (same口径 as parallel tracks).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS im_seen (
           platform   TEXT NOT NULL,
           nonce      TEXT NOT NULL,
           team_id    TEXT NOT NULL,
           im_user_id TEXT NOT NULL,
           created_at INTEGER NOT NULL,
           PRIMARY KEY (platform, nonce, team_id)
         );",
    )?;

    // IM action whitelist (P2 foundation, design §5.3): the frozen
    // method→min_role→source/phase surface, seeded idempotently from
    // im/whitelist.rs. Converged-style (no user_version bump).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS im_action_whitelist (
           action   TEXT PRIMARY KEY,
           min_role TEXT NOT NULL,
           source   TEXT NOT NULL,
           phase    TEXT NOT NULL
         );",
    )?;
    crate::im::whitelist::seed(conn)?;

    // TCoin 平台侧账本（design/017 ADR-0003）：**append-only 流水**，余额 = SUM(delta)。
    // account = 'tenant'（租户总账）或 'member:<id>'（owner 分配给某成员）。
    // 币的发行方是平台（grant）；消耗由租户侧结算（settle）；错误用反向分录（reverse）。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tcoin_entries (
           seq      INTEGER PRIMARY KEY AUTOINCREMENT,
           at       TEXT NOT NULL,
           team_id  TEXT NOT NULL,
           account  TEXT NOT NULL DEFAULT 'tenant',
           delta    INTEGER NOT NULL,
           reason   TEXT NOT NULL,
           ref      TEXT,
           actor    TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX IF NOT EXISTS idx_tcoin_team ON tcoin_entries(team_id, account);",
    )?;
    // 加入申请（design/015 SD-11/SD-26）：平台库为申请落点；owner 审批；
    // 同一 (team, user) 只允许一个 pending（部分唯一索引）；拒绝有冷却期。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS team_join_requests (
           id          TEXT PRIMARY KEY,
           team_id     TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
           user_id     TEXT NOT NULL,
           status      TEXT NOT NULL DEFAULT 'pending',
           message     TEXT,
           created_at  TEXT NOT NULL,
           decided_at  TEXT,
           decided_by  TEXT,
           cooldown_until TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_join_req_team ON team_join_requests(team_id, status);
         CREATE INDEX IF NOT EXISTS idx_join_req_user ON team_join_requests(user_id, status);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_join_req_one_pending
           ON team_join_requests(team_id, user_id) WHERE status = 'pending';",
    )?;

    // Custom nudge templates (Issue #15): owner/lead-configurable message
    // text for the four nudge kinds, per team. Converged-style (brand-new
    // table, no legacy data — keeps user_version untouched; next free
    // versioned slot is v21 per the team ledger).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS nudge_templates (
           team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
           kind       TEXT NOT NULL,
           template   TEXT NOT NULL,
           updated_by TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY (team_id, kind)
         );",
    )?;

    // IM interactive cards (im-p2-feishu-interactive-cards): one-time
    // confirm tokens for the double-confirmation flow. PRIMARY KEY(token)
    // + opportunistic TTL pruning; consumed on first use.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS im_card_tokens (
           token      TEXT PRIMARY KEY,
           action     TEXT NOT NULL,
           task_id    TEXT NOT NULL,
           team_id    TEXT NOT NULL,
           im_user_id TEXT NOT NULL,
           platform   TEXT NOT NULL,
           im_msg_id  TEXT,
           created_at INTEGER NOT NULL,
           expires_at INTEGER NOT NULL
         );",
    )?;

    // C4 first-strike idempotency: issue-token dedup keyed by im_msg_id.
    // Converge for DBs created by 0.2.4 (column added post-release).
    ensure_column(conn, "im_card_tokens", "im_msg_id", "TEXT")?;

    converge_columns(conn)?;
    conn.pragma_update(None, "user_version", 20)?;
    Ok(())
}

/// Column names of `table` (empty when the table does not exist).
fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// Add `column` to `table` when the table exists but the column is missing.
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let cols = table_columns(conn, table)?;
    if !cols.is_empty() && !cols.iter().any(|c| c == column) {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}

/// Version-independent convergence pass.
///
/// `user_version` numbering collided across the public and enterprise lines:
/// a DB created by the public 0.2.x/0.3.0 releases can carry user_version
/// 7..=11 WITHOUT any of the enterprise migration content for those versions
/// (public v7-v11 are different migrations). Pure version gating then skips
/// the ALTERs the current schema needs, producing runtime failures like
/// `no such column: goals.closed_at` on team.list / sync after upgrading.
///
/// Every step below is idempotent and guarded by an existence check, so it is
/// safe to run on every migrate() call regardless of user_version: fresh DBs
/// get everything from SCHEMA (all no-ops here), in-branch legacy DBs already
/// received these via the versioned blocks, and cross-fork DBs get converged.
fn converge_columns(conn: &Connection) -> rusqlite::Result<()> {
    // v7 (enterprise): multi-goal support + connection audit.
    ensure_column(conn, "goals", "closed_at", "TEXT")?;
    ensure_column(conn, "members", "last_ip", "TEXT")?;
    // workspace 目录：客户端（插件/CLI/console）上报，用于 `team status` 展示。
    ensure_column(conn, "members", "workspace", "TEXT")?;
    // k8e 路径需要知道成员该连哪个 server（Pod 的 env / 隧道 / git clone）。
    ensure_column(conn, "nodes", "server_url", "TEXT")?;
    // TCoin 结算标记（design/017 SD-14）：已结算的 activity 行打时间戳，保证**幂等不重复计**。
    ensure_column(conn, "activity", "tcoin_settled_at", "TEXT")?;
    // 团队发现（design/015）：可见性 / 加入策略 / 公开简介（列表与详情白名单用）。
    ensure_column(conn, "teams", "visibility", "TEXT NOT NULL DEFAULT 'private'")?;
    ensure_column(conn, "teams", "join_policy", "TEXT NOT NULL DEFAULT 'invite_only'")?;
    ensure_column(conn, "teams", "public_summary", "TEXT")?;
    ensure_column(conn, "tenants", "endpoint", "TEXT NOT NULL DEFAULT ''")?;
    // 可见性投影状态（SD-24）：上次投影到 k8s label 的值；与 teams.visibility 不一致 = 待补偿。
    ensure_column(conn, "tenants", "visibility_projected", "TEXT NOT NULL DEFAULT ''")?;
    // owner 邀请函（bootstrap 时生成）——「Owner 邀请函」二级菜单要能**取回**它。
    ensure_column(conn, "tenants", "owner_letter", "TEXT NOT NULL DEFAULT ''")?;
    // owner 在租户侧的座位 id（bootstrap 时确定）—— owner 自己的工作区状态要用它。
    ensure_column(conn, "tenants", "owner_member_id", "TEXT NOT NULL DEFAULT ''")?;
    // 加入申请的交付字段（SD-11/SD-23）：批准后在租户侧建的座位 + 邀请函 + 拉 pod 结果。
    ensure_column(conn, "team_join_requests", "member_id", "TEXT")?;
    ensure_column(conn, "team_join_requests", "letter", "TEXT")?;
    ensure_column(conn, "team_join_requests", "pod_state", "TEXT")?;
    ensure_column(conn, "team_join_requests", "pod_detail", "TEXT")?;
    // The pre-multi-goal UNIQUE(team_id) index (created by the public line's
    // v3) rejects a team's second goal; dropping a missing index is a no-op.
    conn.execute_batch("DROP INDEX IF EXISTS idx_goals_team;")?;

    // v4: custom-role workflow columns (public v4 content differs).
    ensure_column(conn, "roles", "state", "TEXT NOT NULL DEFAULT 'approved'")?;
    ensure_column(conn, "roles", "proposed_by", "TEXT")?;

    // v10 (enterprise cost): activity attribution/cache/cost columns. SCHEMA
    // creates the full table for fresh DBs; these ALTERs only fire when an
    // older `activity` table already exists.
    let activity_cols: &[(&str, &str)] = &[
        ("model_id", "TEXT"),
        ("provider_id", "TEXT"),
        ("tokens_cache_read", "INTEGER"),
        ("tokens_cache_write", "INTEGER"),
        ("cost_source", "TEXT NOT NULL DEFAULT 'none'"),
        ("cost_input", "REAL"),
        ("cost_output", "REAL"),
        ("cost_cache_read", "REAL"),
        ("cost_cache_write", "REAL"),
    ];
    for (name, decl) in activity_cols {
        ensure_column(conn, "activity", name, decl)?;
    }

    // Builtin pricing rows use INSERT OR IGNORE — safe to re-run every time,
    // and required for cross-fork DBs that skipped the versioned v10 block.
    crate::activity::seed_builtin_pricing(conn)?;
    Ok(())
}

/// Default role catalog, seeded per team at creation.
pub const DEFAULT_ROLES: &[(&str, &str, &str)] = &[
    ("owner", "Owner", "Creates team, drafts goals, approves members, verifies and closes goals."),
    ("observer", "Observer", "Read-only observer of team dynamics and goal progress; does not execute tasks."),
    ("supervisor", "Supervisor", "Monitors progress and quality; provides clarification and adjustment suggestions."),
    ("contributor", "Contributor", "Takes on implementation and delivery work; reports progress regularly."),
    ("subtask-implementer", "Subtask Implementer", "Responsible for implementing and delivering a specific subtask."),
    ("reviewer", "Reviewer", "Reviews deliverables and progress; provides review feedback."),
];

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Record a new authenticated member connection (audit).
pub fn log_connection(
    conn: &Connection,
    member_id: &str,
    team_id: &str,
    ip: &str,
    endpoint: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO member_connections (member_id, team_id, ip, endpoint, connected_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![member_id, team_id, ip, endpoint, now()],
    )?;
    // Snapshot the last known IP on the member for quick display.
    conn.execute("UPDATE members SET last_ip = ?1 WHERE id = ?2", rusqlite::params![ip, member_id])?;
    Ok(conn.last_insert_rowid())
}

/// Mark the most recent open connection for a member/endpoint as disconnected.
pub fn close_connection(conn: &Connection, member_id: &str, endpoint: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE member_connections SET disconnected_at = ?1
         WHERE member_id = ?2 AND endpoint = ?3 AND disconnected_at IS NULL",
        rusqlite::params![now(), member_id, endpoint],
    )?;
    Ok(())
}

/// Upsert an active reverse-tunnel registration (server side). The server's
/// in-memory `TunnelRegistry` is authoritative; this table is a mirror that
/// lets DB-reading tools (admin-console /api/topology) show live edges.
#[allow(clippy::too_many_arguments)] // mirrors the tunnel table column set
pub fn tunnel_upsert(
    conn: &Connection,
    team_id: &str,
    name: &str,
    provider_member_id: &str,
    mode: &str,
    port: i64,
    target_port: Option<i64>,
    lan_ip: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tunnels (team_id, name, provider_member_id, mode, port, target_port, lan_ip, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(team_id, name) DO UPDATE SET
           provider_member_id = excluded.provider_member_id,
           mode = excluded.mode,
           port = excluded.port,
           target_port = excluded.target_port,
           lan_ip = excluded.lan_ip,
           updated_at = excluded.updated_at",
        rusqlite::params![team_id, name, provider_member_id, mode, port, target_port, lan_ip, now()],
    )?;
    Ok(())
}

/// Remove a tunnel registration (unregister / provider disconnect).
pub fn tunnel_remove(conn: &Connection, team_id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM tunnels WHERE team_id = ?1 AND name = ?2", rusqlite::params![team_id, name])?;
    Ok(())
}

/// Active tunnel registrations for a team, newest first.
pub fn active_tunnels(conn: &Connection, team_id: &str) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT name, provider_member_id, mode, port, target_port, lan_ip, updated_at
         FROM tunnels WHERE team_id = ?1 ORDER BY updated_at DESC",
    ) {
        if let Ok(rows) = stmt.query_map([team_id], |r| {
            Ok(json!({
                "name": r.get::<_, String>(0)?,
                "provider_member_id": r.get::<_, String>(1)?,
                "mode": r.get::<_, String>(2)?,
                "port": r.get::<_, i64>(3)?,
                "target_port": r.get::<_, Option<i64>>(4)?,
                "lan_ip": r.get::<_, Option<String>>(5)?,
                "updated_at": r.get::<_, String>(6)?,
            }))
        }) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
    }
    out
}

/// The IP of the member's most recent *open* (undisconnected) connection,
/// or their last known IP if none is currently open.
pub fn member_ip(conn: &Connection, member_id: &str) -> Option<String> {    conn.query_row(
        "SELECT ip FROM member_connections
         WHERE member_id = ?1 AND disconnected_at IS NULL
         ORDER BY connected_at DESC LIMIT 1",
        [member_id],
        |r| r.get(0),
    )
    .ok()
    .or_else(|| {
        conn.query_row("SELECT last_ip FROM members WHERE id = ?1", [member_id], |r| r.get(0)).ok()
    })
}

/// Whether the member currently has an open connection.
/// Whether the member is currently "online".
///
/// Two independent signals count:
///   1. an open connection row (long-lived WS / reverse tunnel), or
///   2. a fresh presence heartbeat (`presence.updated_at` within
///      `PRESENCE_TTL_MS`).
///
/// The heartbeat covers clients that are actively pushing presence but whose
/// long-lived WS is not established — e.g. a letter imported *after* the plugin
/// initialised (see issue #6), or a transient WS drop.
pub fn member_online(conn: &Connection, member_id: &str) -> bool {
    let connected = conn
        .query_row(
            "SELECT COUNT(*) FROM member_connections
             WHERE member_id = ?1 AND disconnected_at IS NULL",
            [member_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if connected {
        return true;
    }

    // Fall back to a fresh presence heartbeat.
    let updated: Option<String> = conn
        .query_row(
            "SELECT updated_at FROM presence WHERE member_id = ?1",
            [member_id],
            |r| r.get(0),
        )
        .ok();
    let Some(updated) = updated else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(&updated)
        .ok()
        .map(|ts| {
            (chrono::Utc::now() - ts.with_timezone(&chrono::Utc)).num_milliseconds()
                < crate::activity::PRESENCE_TTL_MS
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Local client config: per-member server/ports + local environment settings.
// ---------------------------------------------------------------------------

/// One local member config (client-side; each may connect to a different
/// teamx server).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocalMember {
    pub id: i64,
    pub member_key: String,
    pub display_name: String,
    pub server_url: String,
    pub letter_id: Option<String>,
    pub proxy_port: i64,
    pub dns_port: i64,
    pub enabled: bool,
}

pub fn list_local_members(conn: &Connection) -> rusqlite::Result<Vec<LocalMember>> {
    let mut stmt = conn.prepare(
        "SELECT id, member_key, display_name, server_url, letter_id, proxy_port, dns_port, enabled
         FROM local_members ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(LocalMember {
            id: r.get(0)?,
            member_key: r.get(1)?,
            display_name: r.get(2)?,
            server_url: r.get(3)?,
            letter_id: r.get(4)?,
            proxy_port: r.get(5)?,
            dns_port: r.get(6)?,
            enabled: r.get::<_, i64>(7)? != 0,
        })
    })?;
    rows.collect()
}

#[allow(dead_code)] // retained for future use (single-member lookup); list covers the CLI today
pub fn get_local_member(conn: &Connection, key: &str) -> rusqlite::Result<Option<LocalMember>> {
    let mut stmt = conn.prepare(
        "SELECT id, member_key, display_name, server_url, letter_id, proxy_port, dns_port, enabled
         FROM local_members WHERE member_key = ?1",
    )?;
    let mut rows = stmt.query_map([key], |r| {
        Ok(LocalMember {
            id: r.get(0)?,
            member_key: r.get(1)?,
            display_name: r.get(2)?,
            server_url: r.get(3)?,
            letter_id: r.get(4)?,
            proxy_port: r.get(5)?,
            dns_port: r.get(6)?,
            enabled: r.get::<_, i64>(7)? != 0,
        })
    })?;
    rows.next().transpose()
}

/// Add a local member. Returns its member_key.
pub fn add_local_member(
    conn: &Connection,
    key: &str,
    name: &str,
    server_url: &str,
    letter_id: Option<&str>,
    proxy_port: i64,
    dns_port: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO local_members
         (member_key, display_name, server_url, letter_id, proxy_port, dns_port, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
        rusqlite::params![key, name, server_url, letter_id, proxy_port, dns_port, now()],
    )?;
    Ok(())
}

pub fn update_local_member(
    conn: &Connection,
    key: &str,
    name: Option<&str>,
    server_url: Option<&str>,
    letter_id: Option<&str>,
    proxy_port: Option<i64>,
    dns_port: Option<i64>,
) -> rusqlite::Result<()> {
    if let Some(n) = name {
        conn.execute("UPDATE local_members SET display_name = ?1 WHERE member_key = ?2", rusqlite::params![n, key])?;
    }
    if let Some(s) = server_url {
        conn.execute("UPDATE local_members SET server_url = ?1 WHERE member_key = ?2", rusqlite::params![s, key])?;
    }
    if let Some(l) = letter_id {
        conn.execute("UPDATE local_members SET letter_id = ?1 WHERE member_key = ?2", rusqlite::params![l, key])?;
    }
    if let Some(p) = proxy_port {
        conn.execute("UPDATE local_members SET proxy_port = ?1 WHERE member_key = ?2", rusqlite::params![p, key])?;
    }
    if let Some(d) = dns_port {
        conn.execute("UPDATE local_members SET dns_port = ?1 WHERE member_key = ?2", rusqlite::params![d, key])?;
    }
    conn.execute("UPDATE local_members SET updated_at = ?1 WHERE member_key = ?2", rusqlite::params![now(), key])?;
    Ok(())
}

pub fn remove_local_member(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    let n = conn.execute("DELETE FROM local_members WHERE member_key = ?1", [key])?;
    Ok(n > 0)
}

/// Read a local setting value.
pub fn get_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM local_settings WHERE key = ?1", [key], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
}

/// Write a local setting value.
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO local_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Wrap a write in a single-writer transaction with busy retry.
/// The closure must be re-callable (`FnMut`); do not move captured values out.
pub fn with_write<F, T>(conn: &mut Connection, mut f: F) -> rusqlite::Result<T>
where
    F: FnMut(&mut rusqlite::Transaction) -> rusqlite::Result<T>,
{
    for _ in 0..20 {
        let mut tx = match conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(e),
        };
        let result = f(&mut tx);
        match result {
            Ok(v) => return tx.commit().map(|_| v),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                // drop tx (rollback) then retry
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("teamx: write timed out after repeated busy retries".into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a DB created by the public 0.2.x/0.3.0 line: it carries
    /// user_version=11 but NONE of the enterprise migration content for
    /// v7..=11 (the version numbers collided across the two lines). Notably
    /// `goals` has no `closed_at` and still has the pre-multi-goal UNIQUE
    /// index, `members` has no `last_ip`, and `roles` has no state columns.
    fn legacy_public_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE teams (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, owner_member_id TEXT,
               goal_id TEXT, state TEXT NOT NULL DEFAULT 'forming',
               invite_token TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE TABLE members (
               id TEXT PRIMARY KEY,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               session_key TEXT NOT NULL, display_name TEXT NOT NULL,
               role TEXT, state TEXT NOT NULL DEFAULT 'pending',
               loopx_project TEXT, last_seen_at TEXT,
               joined_at TEXT NOT NULL, left_at TEXT
             );
             CREATE TABLE goals (
               id TEXT PRIMARY KEY,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               title TEXT NOT NULL, body TEXT, state TEXT NOT NULL DEFAULT 'proposed',
               created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX idx_goals_team ON goals(team_id);
             CREATE TABLE roles (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               key TEXT NOT NULL, label TEXT NOT NULL, description TEXT,
               permissions_json TEXT, UNIQUE(team_id, key)
             );
             CREATE TABLE events (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               member_id TEXT, seq INTEGER NOT NULL, type TEXT NOT NULL,
               payload_json TEXT, created_at TEXT NOT NULL, UNIQUE(team_id, seq)
             );
             CREATE TABLE questions (
               id TEXT PRIMARY KEY,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               asker_member_id TEXT NOT NULL, target_member_id TEXT NOT NULL,
               question TEXT NOT NULL, answer TEXT, state TEXT NOT NULL DEFAULT 'open',
               created_at TEXT NOT NULL, answered_at TEXT
             );
             CREATE TABLE sync_cursors (
               session_key TEXT NOT NULL,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               last_seq INTEGER NOT NULL, PRIMARY KEY(session_key, team_id)
             );
             CREATE TABLE invitations (
               id TEXT PRIMARY KEY,
               team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
               member_id TEXT NOT NULL, role_key TEXT NOT NULL, role_label TEXT,
               role_desc TEXT, cert_serial TEXT, cert_cn TEXT,
               created_by TEXT NOT NULL, created_at TEXT NOT NULL,
               used_by TEXT, used_at TEXT, revoked_at TEXT
             );
             INSERT INTO teams (id, name, invite_token, created_at, updated_at)
               VALUES ('t1', 'legacy', 'tok', 'now', 'now');
             INSERT INTO goals (id, team_id, title, created_at, updated_at)
               VALUES ('g1', 't1', 'first goal', 'now', 'now');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 11).unwrap();
        conn
    }

    #[test]
    fn migrate_converges_legacy_public_db() {
        let conn = legacy_public_conn();
        migrate(&conn).unwrap();

        // The original production failure: team.list/sync SELECT closed_at.
        let gcols = table_columns(&conn, "goals").unwrap();
        assert!(
            gcols.iter().any(|c| c == "closed_at"),
            "goals.closed_at missing after migrate: {gcols:?}"
        );
        conn.query_row(
            "SELECT id, team_id, title, body, state, closed_at FROM goals WHERE id = 'g1'",
            [],
            |r| r.get::<_, Option<String>>(5),
        )
        .unwrap();

        // v7 connection-audit column on the pre-existing members table.
        let mcols = table_columns(&conn, "members").unwrap();
        for c in ["last_ip", "is_lead", "user_id", "discussion_quota"] {
            assert!(mcols.iter().any(|x| x == c), "members.{c} missing: {mcols:?}");
        }

        // v4 custom-role columns on the pre-existing roles table.
        let rcols = table_columns(&conn, "roles").unwrap();
        for c in ["state", "proposed_by"] {
            assert!(rcols.iter().any(|x| x == c), "roles.{c} missing: {rcols:?}");
        }

        // The pre-multi-goal UNIQUE(team_id) index must be gone: a second
        // goal for the same team must insert cleanly.
        conn.execute(
            "INSERT INTO goals (id, team_id, title, created_at, updated_at)
             VALUES ('g2', 't1', 'second goal', 'now', 'now')",
            [],
        )
        .unwrap();

        // Builtin pricing seeded even though the versioned v10 block was
        // skipped by the foreign user_version.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_pricing WHERE source = 'builtin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n > 0, "builtin pricing not seeded");

        // Version converges to the current schema version.
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 20);

        // Idempotent: migrating again must succeed and change nothing.
        migrate(&conn).unwrap();
    }

    #[test]
    fn migrate_fresh_db_is_converged_and_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let gcols = table_columns(&conn, "goals").unwrap();
        assert!(gcols.iter().any(|c| c == "closed_at"));
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 20);
    }
}
