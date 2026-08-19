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

/// The stable per-machine instance id, persisted at `~/.teamx/instance.json`
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

-- enterprise analytics: per-node member activity (time/duration/tokens/cost/
-- work content) pushed to the team lead's serve database. `node_id` is the
-- source machine's instance_id; sensitive fields are kept in full (audit).
CREATE TABLE IF NOT EXISTS activity (
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
    conn.pragma_update(None, "user_version", 6)?;
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
