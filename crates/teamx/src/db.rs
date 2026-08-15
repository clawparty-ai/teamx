use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

pub const DB_ENV: &str = "TEAMX_DB";

/// Resolve the global database path:
/// 1. `--db <path>` (handled by CLI, stored in `Cli.db`)
/// 2. `TEAMX_DB` env var
/// 3. `~/.teamx/teamx.db`
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var(DB_ENV) {
        return PathBuf::from(p);
    }
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".teamx");
    base.join("teamx.db")
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
    conn.pragma_update(None, "user_version", 3)?;
    Ok(())
}

/// Default role catalog, seeded per team at creation.
pub const DEFAULT_ROLES: &[(&str, &str, &str)] = &[
    ("owner", "Owner", "创建团队、起草目标、审批成员、验证并关闭目标。"),
    ("observer", "Observer", "只读观察团队动态与目标进展，不承担执行任务。"),
    ("supervisor", "Supervisor", "监督进度与质量，向成员提出澄清与调整建议。"),
    ("contributor", "Contributor", "承担实现与交付工作，定期汇报进展。"),
    ("subtask-implementer", "Subtask Implementer", "负责某个子任务的实现与交付。"),
    ("reviewer", "Reviewer", "审查交付物与进度，提供评审意见。"),
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
