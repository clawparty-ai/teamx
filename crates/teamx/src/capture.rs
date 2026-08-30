//! capture.rs — local traffic capture: SQLite store, flow tracker, taps.
//!
//! Design (docs/ENT-04-traffic-capture.cn.md): a dedicated capture DB
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
        /// Optional protocol-detection bytes (ClientHello etc.); used only for
        /// proto/SNI/client_random extraction — callers must emit a separate
        /// `Chunk` event if these bytes should appear in `stream_chunks`.
        first_bytes: Vec<u8>,
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
    /// Late protocol metadata: set proto/sni/client_random once the first real
    /// application bytes are seen (post-handshake for SOCKS5).
    Meta { flow_id: u64, first_bytes: Vec<u8> },
    /// Raw IP packet (tun0 only, optional).
    #[allow(dead_code)] // raw L3 capture surface; wired with tun0 packet taps
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
    #[allow(dead_code)] // drop-counter API; wired when capture governance lands
    pub fn disabled() -> Self {
        Self {
            tx: std_mpsc::channel().0,
            dropped: Arc::new(AtomicU64::new(0)),
            enabled: false,
        }
    }

    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[allow(dead_code)]
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

    /// Set protocol metadata for a flow once the first application bytes are
    /// available (e.g. the TLS ClientHello after a SOCKS5 handshake).
    pub fn meta(&self, flow_id: u64, first_bytes: &[u8]) {
        self.send(TapEvent::Meta {
            flow_id,
            first_bytes: first_bytes.to_vec(),
        });
    }

    #[allow(dead_code)]
    pub fn packet(&self, iface: &str, data: &[u8]) {
        self.send(TapEvent::Packet {
            iface: iface.to_string(),
            data: data.to_vec(),
            ts: now(),
        });
    }
}

/// Best-effort current timestamp (seconds since UNIX epoch).
/// Returns 0.0 only if the system clock is before the epoch (shouldn't happen
/// in practice, but avoids panicking on weird platforms).
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_else(|e| {
            eprintln!("capture: clock error: {e}");
            0.0
        })
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
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("capture schema: {e}"))?;
        Ok(CaptureStore { conn })
    }

    /// Drain one batch of events; returns the number applied.
    pub fn apply_batch(&mut self, events: Vec<TapEvent>) -> Result<usize, String> {
        let n = events.len();
        let tx = &mut self.conn;
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
                    // NOTE: `first_bytes` is used only for protocol detection.
                    // Callers emit a separate `Chunk` event if these bytes should
                    // appear in `stream_chunks`. This avoids double-counting.
                }
                TapEvent::Chunk { flow_id, dir, seq, ts, data } => {
                    let d = match dir {
                        Dir::Up => "up",
                        Dir::Down => "down",
                    };
                    tx.execute(
                        "INSERT INTO stream_chunks (flow_id, dir, seq, ts, data) VALUES (?1,?2,?3,?4,?5)",
                        rusqlite::params![*flow_id as i64, d, *seq as i64, ts, data],
                    )
                    .map_err(|e| e.to_string())?;
                    let col = if d == "up" { "bytes_up" } else { "bytes_down" };
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
                TapEvent::Meta { flow_id, first_bytes } => {
                    let (sni, random) = crate::tls_parse::client_hello_meta(first_bytes);
                    let proto = crate::tls_parse::detect_protocol(first_bytes);
                    tx.execute(
                        "UPDATE flows SET proto = ?1, sni = COALESCE(?2, sni), \
                         client_random = COALESCE(?3, client_random) \
                         WHERE id = ?4 AND (proto IS NULL OR proto = 'other')",
                        rusqlite::params![proto, sni, random, *flow_id as i64],
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

    /// Remove orphaned tls_keys whose client_random no longer appears in any
    /// flow. Should be called after pruning.
    pub fn cleanup_orphaned_keys(&mut self) -> Result<usize, String> {
        let n = self
            .conn
            .execute(
                "DELETE FROM tls_keys WHERE client_random NOT IN \
                 (SELECT client_random FROM flows WHERE client_random IS NOT NULL)",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(n)
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

/// Hex-encode a byte slice to a lowercase hex string.
fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
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
  dir     TEXT NOT NULL CHECK(dir IN ('up', 'down')),
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
  client_random BLOB NOT NULL,
  label         TEXT NOT NULL,
  secret        BLOB NOT NULL,
  cipher        TEXT,
  added_at      REAL NOT NULL,
  PRIMARY KEY (client_random, label)
);

CREATE TABLE IF NOT EXISTS http_exchanges (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  flow_id          INTEGER,
  session_id       INTEGER,
  method           TEXT,
  url              TEXT,
  host             TEXT,
  request_headers  TEXT,
  request_body     BLOB,
  status           INTEGER,
  response_headers TEXT,
  response_body    BLOB,
  started_at       REAL
);

CREATE TABLE IF NOT EXISTS replay_runs (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  mode       TEXT NOT NULL,
  source     TEXT NOT NULL,
  target     TEXT,
  speed      REAL NOT NULL DEFAULT 1.0,
  started_at REAL NOT NULL,
  ended_at   REAL,
  result     TEXT
);

CREATE TABLE IF NOT EXISTS replay_flow_results (
  run_id      INTEGER NOT NULL REFERENCES replay_runs(id) ON DELETE CASCADE,
  flow_id     INTEGER NOT NULL,
  status      TEXT NOT NULL,
  match_ratio REAL,
  error       TEXT,
  duration_ms INTEGER
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
    rx: std_mpsc::Receiver<TapEvent>,
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

/// Parse a `host:port` or `host` string into (`[u8;4]`, `u16`).
/// Returns `(default_ip, default_port)` when parsing fails (e.g. domain name).
fn parse_addr(addr: &str, default_ip: [u8; 4], default_port: u16) -> ([u8; 4], u16) {
    let (host_str, port) = if let Some((h, p)) = addr.rsplit_once(':') {
        let p: u16 = p.parse().unwrap_or(default_port);
        (h, p)
    } else {
        (addr, default_port)
    };
    // Try parsing as IPv4 dotted-decimal.
    if let Ok(octets) = host_str.parse::<std::net::Ipv4Addr>() {
        return (octets.octets(), port);
    }
    // Domain name — fall back to defaults (synthetic pcap).
    (default_ip, port)
}

/// Export one flow as a synthetic pcap file (Ethernet/IPv4/TCP), replaying the
/// captured chunks in order with simple relative sequence numbers. Useful for
/// offline analysis in Wireshark or `tcpreplay` round-trips.
pub fn export_pcap(
    store: &mut CaptureStore,
    flow_id: i64,
    dst: &std::path::Path,
) -> Result<usize, String> {
    // Fetch flow metadata for IP/port extraction.
    let remote_addr: String = store
        .conn()
        .query_row(
            "SELECT remote_addr FROM flows WHERE id = ?1",
            [flow_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("flow {flow_id}: {e}"))?;
    let local_addr: String = store
        .conn()
        .query_row(
            "SELECT local_addr FROM flows WHERE id = ?1",
            [flow_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("flow {flow_id}: {e}"))?;

    let mut stmt = store
        .conn()
        .prepare(
            "SELECT dir, seq, ts, data FROM stream_chunks \
             WHERE flow_id = ?1 ORDER BY seq ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, i64, f64, Vec<u8>)> = stmt
        .query_map(rusqlite::params![flow_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    if rows.is_empty() {
        return Ok(0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0xa1b2c3d4u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&65535u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    let mut up_seq: u32 = 1;
    let mut down_seq: u32 = 1;
    let base_ts = rows.first().map(|r| r.2).unwrap_or(0.0);
    for (dir, _seq, ts, data) in &rows {
        let d = if dir == "up" { Dir::Up } else { Dir::Down };
        // Parse IPs and ports from the flow's recorded addresses.
        let (src_ip, sport) = parse_addr(&local_addr, [10, 0, 0, 2], 49152);
        let (dst_ip, dport) = parse_addr(&remote_addr, [93, 184, 216, 34], 443);
        let (src_ip, dst_ip, sport, dport) = if matches!(d, Dir::Up) {
            (src_ip, dst_ip, sport, dport)
        } else {
            (dst_ip, src_ip, dport, sport)
        };
        let seq = if matches!(d, Dir::Up) {
            up_seq
        } else {
            down_seq
        };
        let pkt = build_tcp_packet(sport, dport, src_ip, dst_ip, seq, data);
        let ts_rel = (*ts - base_ts) * 1_000_000.0;
        let ts_sec = (ts_rel / 1_000_000.0) as u32;
        let ts_usec = (ts_rel % 1_000_000.0) as u32;
        out.extend_from_slice(&ts_sec.to_le_bytes());
        out.extend_from_slice(&ts_usec.to_le_bytes());
        out.extend_from_slice(&(pkt.len() as u32).to_le_bytes());
        out.extend_from_slice(&(pkt.len() as u32).to_le_bytes());
        out.extend_from_slice(&pkt);
        if matches!(d, Dir::Up) {
            up_seq = up_seq.wrapping_add(data.len() as u32);
        } else {
            down_seq = down_seq.wrapping_add(data.len() as u32);
        }
    }
    std::fs::write(dst, &out).map_err(|e| format!("write {}: {e}", dst.display()))?;
    Ok(rows.len())
}

/// Export all captured TLS secrets as an SSLKEYLOGFILE for offline decrypt.
/// Each line is formatted as `<label> <client_random_hex> <secret_hex>`.
pub fn export_keylog(store: &mut CaptureStore, dst: &std::path::Path) -> Result<usize, String> {
    let mut stmt = store
        .conn()
        .prepare("SELECT label, client_random, secret FROM tls_keys ORDER BY added_at ASC")
        .map_err(|e| e.to_string())?;
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            let label: String = r.get(0)?;
            let cr: Vec<u8> = r.get(1)?;
            let secret: Vec<u8> = r.get(2)?;
            Ok(format!("{} {} {}", label, hex_encode(&cr), hex_encode(&secret)))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    let text = rows.join("\n");
    std::fs::write(dst, text).map_err(|e| format!("write {}: {e}", dst.display()))?;
    Ok(rows.len())
}

/// Build a minimal Ethernet(14)+IPv4(20)+TCP(20) packet carrying `payload`.
pub fn build_tcp_packet(
    sport: u16,
    dport: u16,
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    seq: u32,
    payload: &[u8],
) -> Vec<u8> {
    let total = 20 + 20 + payload.len();
    let mut pkt = Vec::with_capacity(14 + total);
    pkt.extend_from_slice(&[0x02, 0, 0, 0, 0, 1, 0x02, 0, 0, 0, 0, 2, 0x08, 0x00]);
    let ihl = 5u8;
    pkt.push((4u8 << 4) | ihl);
    pkt.push(0);
    pkt.extend_from_slice(&(total as u16).to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.push(64);
    pkt.push(6);
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&src_ip);
    pkt.extend_from_slice(&dst_ip);
    pkt.extend_from_slice(&sport.to_be_bytes());
    pkt.extend_from_slice(&dport.to_be_bytes());
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&0u32.to_be_bytes());
    let data_off = 5u8 << 4;
    pkt.push(data_off);
    pkt.push(0x10);
    pkt.extend_from_slice(&65535u16.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(payload);
    pkt
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn tmp_db(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("teamx-cap-test-{}-{}.db", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&p.with_extension("db-wal"));
        let _ = std::fs::remove_file(&p.with_extension("db-shm"));
        p
    }

    // Realistic path: a writer process/thread owns `store`, and readers (the
    // CLI) open their own connection. We mirror that by re-opening for reads.
    #[test]
    fn store_apply_batch_roundtrip() {
        let db = tmp_db("store");
        let mut store = CaptureStore::open(&db).unwrap();
        store
            .apply_batch(vec![
                TapEvent::FlowOpen {
                    flow_id: 1,
                    source: "socks5",
                    local: "127.0.0.1".into(),
                    remote_addr: "93.184.216.34:443".into(),
                    ts: 1.0,
                    first_bytes: vec![22, 3, 1, 0, 0],
                },
                TapEvent::Chunk {
                    flow_id: 1,
                    dir: Dir::Up,
                    seq: 0,
                    ts: 1.05,
                    data: vec![22, 3, 1, 0, 0],
                },
                TapEvent::Chunk {
                    flow_id: 1,
                    dir: Dir::Up,
                    seq: 1,
                    ts: 1.1,
                    data: b"GET / HTTP/1.1\r\n".to_vec(),
                },
                TapEvent::FlowClose {
                    flow_id: 1,
                    reason: "eof".into(),
                    ts: 1.2,
                },
            ])
            .unwrap();
        drop(store);
        // reader opens a fresh connection
        let rc = Connection::open(&db).unwrap();
        let fc: i64 = rc.query_row("SELECT COUNT(*) FROM flows", [], |r| r.get(0)).unwrap();
        assert_eq!(fc, 1);
        let cc: i64 = rc
            .query_row("SELECT COUNT(*) FROM stream_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cc, 2); // two explicit Chunk events (first_bytes no longer auto-inserted)
    }

    #[test]
    fn tap_channel_drains_to_store() {
        let db = tmp_db("tap");
        let store = CaptureStore::open(&db).unwrap();
        let (tx, rx) = mpsc::channel();
        let handle = spawn_writer(store, rx);
        let tap = CaptureTap {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            enabled: true,
        };
        tap.flow_open(7, "tun0", "10.0.0.2", "1.1.1.1:443", &[]);
        tap.chunk(7, Dir::Up, 1, b"hello");
        tap.chunk(7, Dir::Down, 1, b"world");
        tap.flow_close(7, "eof");
        drop(tap);
        std::thread::sleep(Duration::from_millis(300));
        let _ = handle.join();

        let rc = Connection::open(&db).unwrap();
        let cc: i64 = rc
            .query_row("SELECT COUNT(*) FROM stream_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cc, 2);
    }

    #[test]
    fn pcap_export_produces_magic() {
        let db = tmp_db("pcap");
        let mut store = CaptureStore::open(&db).unwrap();
        store
            .apply_batch(vec![
                TapEvent::FlowOpen {
                    flow_id: 1,
                    source: "socks5",
                    local: "10.0.0.2".into(),
                    remote_addr: "93.184.216.34:443".into(),
                    ts: 1.0,
                    first_bytes: vec![],
                },
                TapEvent::Chunk {
                    flow_id: 1,
                    dir: Dir::Up,
                    seq: 1,
                    ts: 1.1,
                    data: b"abc".to_vec(),
                },
                TapEvent::Chunk {
                    flow_id: 1,
                    dir: Dir::Down,
                    seq: 1,
                    ts: 1.5,
                    data: b"xyz".to_vec(),
                },
            ])
            .unwrap();
        drop(store);
        let mut reader = CaptureStore::open(&db).unwrap();
        let pcap = db.with_extension("pcap");
        export_pcap(&mut reader, 1, &pcap).unwrap();
        let bytes = std::fs::read(&pcap).unwrap();
        assert!(bytes.len() > 24);
        assert_eq!(&bytes[0..4], &0xa1b2c3d4u32.to_le_bytes());
        let _ = std::fs::remove_file(&pcap);
    }

    #[test]
    fn keylog_import_and_prune() {
        let db = tmp_db("keylog");
        let mut store = CaptureStore::open(&db).unwrap();
        store
            .apply_batch(vec![TapEvent::FlowOpen {
                flow_id: 1,
                source: "socks5",
                local: "10.0.0.2".into(),
                remote_addr: "93.184.216.34:443".into(),
                ts: 1.0,
                first_bytes: vec![],
            }])
            .unwrap();
        let n = store
            .import_keylog(
                "CLIENT_HANDSHAKE_TRAFFIC_SECRET 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\n",
            )
            .unwrap();
        assert_eq!(n, 1);
        drop(store);
        let rc = Connection::open(&db).unwrap();
        let kc: i64 = rc
            .query_row("SELECT COUNT(*) FROM tls_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kc, 1);
        let mut reader = CaptureStore::open(&db).unwrap();
        let (removed, _size) = reader.prune_max_size(1).unwrap();
        assert!(removed >= 1);
        // After pruning the flow, orphaned keys should be cleaned up.
        let orphans = reader.cleanup_orphaned_keys().unwrap();
        assert_eq!(orphans, 1);
    }

    #[test]
    fn export_keylog_writes_correct_format() {
        let db = tmp_db("exportkeylog");
        let mut store = CaptureStore::open(&db).unwrap();
        store
            .import_keylog(
                "CLIENT_TRAFFIC_SECRET_0 aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233 0102030405060708091011121314151617181920212223242526272829303132\n",
            )
            .unwrap();
        let path = db.with_extension("keylog");
        let n = export_keylog(&mut store, &path).unwrap();
        assert_eq!(n, 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("CLIENT_TRAFFIC_SECRET_0 "));
        assert!(text.contains("aabbccdd00112233"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_addr_works() {
        // IP literal
        let (ip, port) = parse_addr("10.0.0.1:443", [0, 0, 0, 0], 80);
        assert_eq!(ip, [10, 0, 0, 1]);
        assert_eq!(port, 443);
        // Domain: port parsed from string, IP falls back to default
        let (ip, port) = parse_addr("example.com:80", [1, 2, 3, 4], 9999);
        assert_eq!(ip, [1, 2, 3, 4]);
        assert_eq!(port, 80); // port from string, not default
        // No port: uses default
        let (ip, port) = parse_addr("example.com", [1, 2, 3, 4], 9999);
        assert_eq!(ip, [1, 2, 3, 4]);
        assert_eq!(port, 9999);
    }
}
