use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

pub const DB_ENV: &str = "TEAMX_DB";

/// Resolve the teamx home directory (default `~/.teamx`, or `TEAMX_HOME`).
pub fn teamx_home() -> PathBuf {
    if let Ok(p) = std::env::var("TEAMX_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".teamx")
}

/// Resolve the global database path:
/// 1. `--db <path>` (handled by CLI, stored in `Cli.db`)
/// 2. `TEAMX_DB` env var
/// 3. `~/.teamx/teamx.db`
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var(DB_ENV) {
        return PathBuf::from(p);
    }
    teamx_home().join("teamx.db")
}

/// Open (creating if needed) the SQLite DB with WAL, busy timeout and FKs.
pub fn open(db_path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = db_path.parent() {
        fs::create_dir_all(dir).ok();
    }
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Single-writer serialization: wait up to 5s for a concurrent writer.
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
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
  updated_at      TEXT NOT NULL
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
  left_at       TEXT
);

CREATE TABLE IF NOT EXISTS goals (
  id         TEXT PRIMARY KEY,
  team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  title      TEXT NOT NULL,
  body       TEXT,
  state      TEXT NOT NULL DEFAULT 'proposed',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
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
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  used_by       TEXT,
  used_at       TEXT,
  revoked_at    TEXT
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
";

/// Apply the schema (idempotent) + migrations.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
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
    // v7: connection audit — member_connections table (SCHEMA covers fresh
    // DBs) + members.last_ip snapshot. Idempotent for existing DBs.
    if version < 7 {
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
    // v8: local client config — local_members (per-member server/ports) +
    // local_settings (local serve state, active member). SCHEMA covers fresh
    // DBs; idempotent for existing ones.
    if version < 8 {
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
    conn.pragma_update(None, "user_version", 8)?;
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

/// The IP of the member's most recent *open* (undisconnected) connection,
/// or their last known IP if none is currently open.
pub fn member_ip(conn: &Connection, member_id: &str) -> Option<String> {
    conn.query_row(
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
pub fn member_online(conn: &Connection, member_id: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM member_connections
         WHERE member_id = ?1 AND disconnected_at IS NULL",
        [member_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
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
