//! replay_cli.rs — `teamx replay ...` command handling.

use crate::cli::{ReplayCmd, ReplayHttpCmd, ReplayTcpCmd};
use crate::replay::{self, HttpExchange};

pub fn handle(cmd: &ReplayCmd) -> Result<serde_json::Value, String> {
    match cmd {
        ReplayCmd::Serve { flow, port } => {
            let db = crate::capture_cli::capture_home();
            let mut store = crate::capture::CaptureStore::open(&db)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("runtime: {e}"))?;
            rt.block_on(replay::serve_flow(&mut store, *flow, *port))
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "ok": true }))
        }
        ReplayCmd::Tcp { cmd } => handle_tcp(cmd),
        ReplayCmd::Http { cmd } => handle_http(cmd),
    }
}

fn handle_tcp(cmd: &ReplayTcpCmd) -> Result<serde_json::Value, String> {
    let ReplayTcpCmd::Start { flow, to, diff, out, exit } = cmd;
    let db = crate::capture_cli::capture_home();
    let mut store = crate::capture::CaptureStore::open(&db)?;
    let server_url = crate::commands::resolve_server_url(None).map_err(|e| e.to_string())?;
    let exit_name = exit.clone().unwrap_or_else(|| "egress2".to_string());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let mut result = rt.block_on(replay::tcp_replay(
        &mut store,
        *flow,
        to.as_deref(),
        &server_url,
        &exit_name,
    ))?;
    if *diff {
        result["diff_requested"] = serde_json::Value::Bool(true);
    }
    if let Some(path) = out {
        std::fs::write(path, serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(result)
}

fn handle_http(cmd: &ReplayHttpCmd) -> Result<serde_json::Value, String> {
    let db = crate::capture_cli::capture_home();
    let mut store = crate::capture::CaptureStore::open(&db)?;
    match cmd {
        ReplayHttpCmd::Extract { flow } => {
            let e = replay::extract_exchange(&mut store, *flow)?;
            let id = replay::store_exchange(&mut store, &e)?;
            Ok(serde_json::json!({ "ok": true, "exchange": id, "flow": flow }))
        }
        ReplayHttpCmd::ExportHar { out } => {
            let har = replay::export_har(&mut store)?;
            let text = serde_json::to_string_pretty(&har).map_err(|e| e.to_string())?;
            std::fs::write(out, text).map_err(|e| format!("write {}: {e}", out.display()))?;
            Ok(serde_json::json!({ "ok": true, "out": out.display().to_string() }))
        }
        ReplayHttpCmd::ToCurl { exchange } => {
            let e = load(&mut store, *exchange)?;
            println!("{}", replay::to_curl(&e));
            Ok(serde_json::json!({ "ok": true }))
        }
        ReplayHttpCmd::Send { exchange, vars, maps, out } => {
            let e = load(&mut store, *exchange)?;
            let vars = parse_pairs(vars);
            let maps = parse_pairs(maps);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("runtime: {e}"))?;
            let (status, body) = rt.block_on(replay::http_send(&e, &vars, &maps))?;
            let result = serde_json::json!({
                "ok": true,
                "exchange": exchange,
                "response": {
                    "status": status,
                    "body_len": body.len(),
                    "body": String::from_utf8_lossy(&body).to_string(),
                }
            });
            if let Some(path) = out {
                std::fs::write(path, serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("write {}: {e}", path.display()))?;
            }
            Ok(result)
        }
    }
}

fn load(store: &mut crate::capture::CaptureStore, id: i64) -> Result<HttpExchange, String> {
    replay::load_exchange(store, id)
}

fn parse_pairs(items: &[String]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}
