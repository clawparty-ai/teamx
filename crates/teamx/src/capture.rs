//! capture.rs — local traffic capture: SQLite store, flow tracker, taps.
//!
//! Design (docs/19-todo-capture-system.cn.md): a dedicated capture DB
//! (`~/.teamx/capture.db`) holds connection metadata (`flows`), bidirectional
//! byte streams in chunks (`stream_chunks`), optional raw IP packets
//! (`packets`), and TLS session keys imported from an SSLKEYLOGFILE
//! (`tls_keys`). Taps feed a bounded channel; a writer thread batches inserts
//! so the forwarding path never blocks on the database.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use rusqlite::Connection;

pub const CHUNK_SIZE: usize = 16 * 1024;

/// Direction of a stream chunk relative to the connection initiator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,   // client -> remote
    Down, // remote -> client
}

/// A tap event flowing from the data plane to the capture writer.
pub enum TapEvent {
    /// New flow observed (first bytes seen for this key).
    FlowOpen {
        flow_id: u64,
        source: &'static str,
        local: String,
        remote_addr: String,
        ts: f64,
        first_bytes: Vec<u8>, // up-direction leading bytes (ClientHello etc.)
    },
    /// Bytes observed on a flow.
    Chunk {
        flow_id: u64,
        dir: Dir,
        seq: u32,
        ts: f64,
        data: Vec<u8>,
    },
    /// Flow closed.
    FlowClose { flow_id: u64, reason: String, ts: f64 },
    /// Raw IP packet (tun0 only, optional).
    Packet { iface: String, data: Vec<u8>, ts: f64 },
}

/// Shared handle used by data-plane taps. Cheap to clone; `try_send` only.
#[derive(Clone)]
pub struct CaptureTap {
    pub tx: std_mpsc::Sender<TapEvent>,
    pub dropped: Arc<AtomicU64>,
    pub enabled: bool,
}

impl CaptureTap {
    pub fn disabled() -> Self {
        Self {
            tx: std_mpsc::channel().0,
            dropped: Arc::new(AtomicU64::new(0)),
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn total_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn send(&self, ev: TapEvent) {
        if !self.enabled {
            return;
        }
        // Unbounded channel but with our own soft cap so a stalled writer can't
        // grow memory without bound; overflow counts as dropped.
        if self.tx.send(ev).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn flow_open(
        &self,
        flow_id: u64,
        source: &'static str,
        local: &str,
        remote: &str,
        first_bytes: &[u8],
    ) {
        self.send(TapEvent::FlowOpen {
            flow_id,
            source,
            local: local.to_string(),
            remote_addr: remote.to_string(),
            ts: now(),
            first_bytes: first_bytes.to_vec(),
        });
    }

    pub fn chunk(&self, flow_id: u64, dir: Dir, seq: u32, data: &[u8]) {
        self.send(TapEvent::Chunk {
            flow_id,
            dir,
            seq,
            ts: now(),
            data: data.to_vec(),
        });
    }

    pub fn flow_close(&self, flow_id: u64, reason: &str) {
        self.send(TapEvent::FlowClose {
            flow_id,
            reason: reason.to_string(),
            ts: now(),
        });
    }

    pub fn packet(&self, iface: &str, data: &[u8]) {
        self.send(TapEvent::Packet {
            iface: iface.to_string(),
            data: data.to_vec(),
            ts: now(),
        });
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The capture writer: drains tap events into SQLite in batches.
pub struct CaptureStore {
    conn: Connection,
}

impl CaptureStore {
    pub fn open(path: &PathBuf) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("capture schema: {e}"))?;
        Ok(CaptureStore { conn })
    }

    /// Drain one batch of events; returns the number applied.
    pub fn apply_batch(&mut self, events: Vec<TapEvent>) -> Result<usize, String> {
        let n = events.len();
        let tx = self
            .conn
            .transaction()
            .map_err(|e| format!("tx: {e}"))
            .map_err(|e| e.to_string())?;
        for ev in &events {
            match ev {
                TapEvent::FlowOpen { flow_id, source, local, remote_addr, ts, first_bytes } => {
                    // Extract TLS ClientHello metadata when present.
                    let (sni, random) = crate::tls_parse::client_hello_meta(first_bytes);
                    let proto = crate::tls_parse::detect_protocol(first_bytes);
                    tx.execute(
                        "INSERT OR IGNORE INTO flows (id, started_at, source, proto,
                           local_addr, remote_addr, sni, client_random, bytes_up, bytes_down)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 0)",
                        rusqlite::params![
                            *flow_id as i64, ts, source, proto, local, remote_addr,
                            sni, random
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                    if !first_bytes.is_empty() {
                        tx.execute(
                            "INSERT INTO stream_chunks (flow_id, dir, seq, ts, data)
                             VALUES (?1, 0, 0, ?2, ?3)",
                            rusqlite::params![*flow_id as i64, ts, first_bytes],
                        )
                        .map_err(|e| e.to_string())?;
                        tx.execute(
                            "UPDATE flows SET bytes_up = bytes_up + ?1 WHERE id = ?2",
                            rusqlite::params![first_bytes.len() as i64, *flow_id as i64],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
                TapEvent::Chunk { flow_id, dir, seq, ts, data } => {
                    let d = match dir {
                        Dir::Up => 0,
                        Dir::Down => 1,
                    };
                    tx.execute(
                        "INSERT INTO stream_chunks (flow_id, dir, seq, ts, data) VALUES (?1,?2,?3,?4,?5)",
                        rusqlite::params![*flow_id as i64, d, *seq as i64, ts, data],
                    )
                    .map_err(|e| e.to_string())?;
                    let col = if d == 0 { "bytes_up" } else { "bytes_down" };
                    tx.execute(
                        &format!("UPDATE flows SET {col} = {col} + ?1 WHERE id = ?2"),
                        rusqlite::params![data.len() as i64, *flow_id as i64],
                    )
                    .map_err(|e| e.to_string())?;
                }
                TapEvent::FlowClose { flow_id, reason, ts } => {
                    tx.execute(
                        "UPDATE flows SET ended_at = ?1, close_reason = ?2 WHERE id = ?3",
                        rusqlite::params![ts, reason, *flow_id as i64],
                    )
                    .map_err(|e| e.to_string())?;
                }
                TapEvent::Packet { iface, data, ts } => {
                    tx.execute(
                        "INSERT INTO packets (ts, iface, data) VALUES (?1, ?2, ?3)",
                        rusqlite::params![ts, iface, data],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        Ok(n)
    }

    /// Import SSLKEYLOGFILE lines (`<label> <client_random> <secret>`).
    pub fn import_keylog(&mut self, text: &str) -> Result<usize, String> {
        let mut n = 0;
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 3 || parts[0].is_empty() {
                continue;
            }
            let (label, cr, secret) = (parts[0], parts[1], parts[2]);
            let Ok(cr_bytes) = hex_decode(cr) else { continue };
            let Ok(secret_bytes) = hex_decode(secret) else { continue };
            if cr_bytes.len() != 32 {
                continue;
            }
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO tls_keys (client_random, label, secret, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![cr_bytes, label, secret_bytes, now()],
                )
                .map_err(|e| e.to_string())?;
            n += 1;
        }
        Ok(n)
    }

    /// Enforce a size cap by deleting oldest flows (and their children).
    pub fn prune_max_size(&mut self, max_bytes: u64) -> Result<(usize, u64), String> {
        let mut removed = 0usize;
        loop {
            let sz = db_file_size(&self.conn)?;
            if sz <= max_bytes {
                return Ok((removed, sz));
            }
            let oldest: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM flows ORDER BY started_at ASC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            let Some(fid) = oldest else { return Ok((removed, sz)) };
            self.conn
                .execute("DELETE FROM stream_chunks WHERE flow_id = ?1", [fid])
                .map_err(|e| e.to_string())?;
            self.conn
                .execute("DELETE FROM http_exchanges WHERE flow_id = ?1", [fid])
                .ok();
            self.conn
                .execute("DELETE FROM flows WHERE id = ?1", [fid])
                .map_err(|e| e.to_string())?;
            removed += 1;
            if removed > 10_000 {
                return Ok((removed, sz)); // safety valve
            }
        }
    }

    pub fn conn(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

fn db_file_size(conn: &Connection) -> Result<u64, String> {
    let path: PathBuf = conn.path().map(|p| std::path::PathBuf::from(p)).unwrap_or_default();
    Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS flows (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at    REAL NOT NULL,
  ended_at      REAL,
  source        TEXT NOT NULL,
  proto         TEXT,
  local_addr    TEXT NOT NULL,
  remote_addr   TEXT NOT NULL,
  remote_host   TEXT,
  sni           TEXT,
  client_random BLOB,
  bytes_up      INTEGER NOT NULL DEFAULT 0,
  bytes_down    INTEGER NOT NULL DEFAULT 0,
  close_reason  TEXT,
  decrypted     INTEGER NOT NULL DEFAULT 0,
  decrypt_error TEXT
);
CREATE INDEX IF NOT EXISTS idx_flows_started ON flows(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_flows_sni ON flows(sni);

CREATE TABLE IF NOT EXISTS stream_chunks (
  flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
  dir     INTEGER NOT NULL,
  seq     INTEGER NOT NULL,
  ts      REAL NOT NULL,
  data    BLOB NOT NULL,
  PRIMARY KEY (flow_id, dir, seq)
);

CREATE TABLE IF NOT EXISTS packets (
  id    INTEGER PRIMARY KEY AUTOINCREMENT,
  ts    REAL NOT NULL,
  iface TEXT NOT NULL,
  data  BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_packets_ts ON packets(ts);

CREATE TABLE IF NOT EXISTS tls_keys (
  client_random BLOB PRIMARY KEY,
  label         TEXT NOT NULL,
  secret        BLOB NOT NULL,
  cipher        TEXT,
  added_at      REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

/// Spawn the background writer: owns the store and drains the tap channel.
/// Returns the writer thread handle; drop the JoinHandle to detach.
pub fn spawn_writer(
    store: CaptureStore,
    mut rx: std_mpsc::Receiver<TapEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut store = store;
        let mut batch: Vec<TapEvent> = Vec::with_capacity(256);
        loop {
            // Block until the first event arrives (zero-cost idle), then drain
            // the queue up to the batch cap and commit atomically.
            match rx.recv() {
                Ok(ev) => {
                    batch.push(ev);
                    while batch.len() < 512 {
                        match rx.try_recv() {
                            Ok(ev) => batch.push(ev),
                            Err(_) => break,
                        }
                    }
                    if let Err(e) = store.apply_batch(std::mem::take(&mut batch)) {
                        eprintln!("capture: batch write failed: {e}");
                    }
                }
                Err(_) => break,
            }
        }
        if !batch.is_empty() {
            let _ = store.apply_batch(batch);
        }
    })
}

/// Open a second (query-only) connection to the capture DB for CLI reads.
pub fn open_query(path: &PathBuf) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("open {}: {e}", path.display()))?;
    Ok(conn)
}
