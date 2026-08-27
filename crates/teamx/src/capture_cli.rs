//! capture_cli.rs — `teamx capture ...` command handling.
//!
//! The capture daemon runs as a plain foreground process: it owns the SQLite
//! store and the tap channel receiver, and stays alive until stopped. Data
//! producers (proxy / tun0 / tunnel) connect by loading the same DB path with
//! `--capture`; they each open their own sender to the well-known socket pair.
//!
//! v1 wiring: producers embed a `CaptureTap` whose channel is drained by an
//! in-process writer (spawned at producer start). `capture start/stop` manage
//! the *daemon* which only handles keylog import + prune + queries. This keeps
//! the data path single-hop while CLI management stays uniform.

use std::path::PathBuf;

use crate::capture::{self, CaptureStore};

pub fn capture_home() -> PathBuf {
    // Allow tests/scripts to redirect the capture DB to a temp path.
    if let Ok(p) = std::env::var("TEAMX_CAPTURE_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::db::teamx_home().join("capture.db")
}

pub fn handle_capture(cmd: &crate::cli::CaptureCmd) -> Result<serde_json::Value, String> {
    use crate::cli::CaptureCmd;
    match cmd {
        CaptureCmd::Start { out, sources } => {
            let db = out.clone().unwrap_or_else(capture_home);
            let store = CaptureStore::open(&db)?;
            // The daemon mode keeps a writer alive so the store is initialized
            // and the schema is created. Producers have their own in-process
            // taps that write to the same DB file via separate connections.
            let (_tx, rx) = std::sync::mpsc::channel();
            let _writer = capture::spawn_writer(store, rx);
            println!(
                "ok capture: started db={} sources={} (daemon, Ctrl-C to stop)",
                db.display(),
                sources.as_deref().unwrap_or("all")
            );
            // Block until stdin closes (Ctrl-C), so `teamx capture start` is a
            // long-lived foreground process like serve.
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            Ok(serde_json::json!({ "ok": true, "note": "capture stopped" }))
        }
        CaptureCmd::Stop => Ok(serde_json::json!({ "ok": true, "note": "use Ctrl-C to stop a running capture daemon" })),
        CaptureCmd::Status => {
            let db = capture_home();
            let exists = db.exists();
            Ok(serde_json::json!({
                "ok": true,
                "db": db.display().to_string(),
                "exists": exists,
            }))
        }
        CaptureCmd::List { sni, limit, json } => {
            let db = capture_home();
            let mut store = CaptureStore::open(&db)?;
            list_flows(store.conn(), sni.as_deref(), *limit, *json)
        }
        CaptureCmd::Prune { max_size } => {
            let db = capture_home();
            let mut store = CaptureStore::open(&db)?;
            let max = parse_size(max_size.as_deref().unwrap_or("512MB"))?;
            let (removed, size) = store.prune_max_size(max)?;
            let orphans = store.cleanup_orphaned_keys()?;
            Ok(serde_json::json!({
                "ok": true, "removed_flows": removed,
                "cleaned_keys": orphans,
                "db_bytes": size,
            }))
        }
        CaptureCmd::ImportKeylog { path } => {
            let db = capture_home();
            let mut store = CaptureStore::open(&db)?;
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let n = store.import_keylog(&text)?;
            Ok(serde_json::json!({ "ok": true, "imported": n }))
        }
        CaptureCmd::Export { flow, pcap, keylog } => {
            let db = capture_home();
            let mut store = CaptureStore::open(&db)?;
            if let Some(kpath) = keylog {
                let n = crate::capture::export_keylog(&mut store, &kpath)
                    .map_err(|e| e.to_string())?;
                return Ok(serde_json::json!({ "ok": true, "exported": n, "path": kpath.display().to_string() }));
            }
            if let (Some(fid), Some(ppath)) = (flow, pcap) {
                let n = crate::capture::export_pcap(&mut store, *fid, &ppath)
                    .map_err(|e| e.to_string())?;
                return Ok(serde_json::json!({ "ok": true, "packets": n, "path": ppath.display().to_string() }));
            }
            Err("export requires --keylog <path> or --flow <id> --pcap <path>".to_string())
        }
    }
}

fn list_flows(
    conn: &mut rusqlite::Connection,
    sni: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<serde_json::Value, String> {
    let sql = if sni.is_some() {
        "SELECT id, started_at, source, proto, remote_addr, sni, \
              bytes_up, bytes_down, decrypted \
         FROM flows WHERE sni LIKE ?1 ORDER BY started_at DESC LIMIT ?2"
    } else {
        "SELECT id, started_at, source, proto, remote_addr, sni, \
              bytes_up, bytes_down, decrypted \
         FROM flows ORDER BY started_at DESC LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let pattern = sni.map(|s| s.replace('*', "%")).unwrap_or_default();
    let q = |r: &rusqlite::Row| -> rusqlite::Result<(i64, f64, String, String, String, Option<String>, i64, i64, i64)> {
        Ok((
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
            r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?
        ))
    };
    let limit_i64 = limit as i64;
    let rows: Vec<_> = if sni.is_some() {
        stmt.query_map(rusqlite::params![pattern, limit_i64], q)
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    } else {
        stmt.query_map(rusqlite::params![limit_i64], q)
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };

    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(id, ts, source, proto, remote, sni, up, down, dec)| {
                serde_json::json!({
                    "id": id, "started_at": ts, "source": source,
                    "proto": proto, "remote": remote, "sni": sni,
                    "bytes_up": up, "bytes_down": down, "decrypted": dec,
                })
            })
            .collect();
        return Ok(serde_json::json!({ "ok": true, "flows": items }));
    }

    for (id, ts, source, proto, remote, sni, up, down, _dec) in &rows {
        println!(
            "#{id} {ts:.0} [{source}] {proto:<6} {remote:<28} {:<24} ↑{up} ↓{down}",
            sni.clone().unwrap_or_else(|| "-".into())
        );
    }
    Ok(serde_json::json!({ "ok": true, "count": rows.len() }))
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().to_uppercase();
    let (num, mult) = if let Some(n) = s.strip_suffix("GB") {
        (n, 1024u64 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("MB") {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("KB") {
        (n, 1024)
    } else if let Some(n) = s.strip_suffix('B') {
        (n, 1)
    } else {
        (s.as_str(), 1)
    };
    num.trim()
        .parse::<u64>()
        .map(|n| n * mult)
        .map_err(|_| format!("invalid size `{s}` (expected e.g. 512MB)"))
}
