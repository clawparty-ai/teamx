//! worker.rs — `teamx worker`: an always-on autonomous executor for a member
//! NODE. It keeps a headless worker session productive WITHOUT an attached
//! TUI: it polls the unified inbox (server-side), and when there is something
//! actionable (an open task assigned to this member, or an unread question
//! directed at it), it runs a headless `opencode run` turn in the workspace so
//! the model actually does the work (retrieval / writing cards / git push /
//! publishing progress) via the teamx tools.
//!
//! Safety (cost control — autonomous model turns spend tokens):
//! - defaults to DRY-RUN: it only reports what it *would* do. Set
//!   `TEAMX_WORKER_ENABLED=1` to actually execute opencode turns.
//! - a processed marker (`.teamx/worker-done.json`) prevents re-running the
//!   same task/question.
//! - a cooldown (default 10 min) throttles repeats when nudges keep firing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::WorkerCmd;

#[derive(Serialize, Deserialize, Default)]
struct Done {
    items: HashMap<String, String>, // id -> at
}

fn done_file() -> PathBuf {
    crate::db::teamx_home().join("worker-done.json")
}

fn load_done() -> Done {
    let p = done_file();
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_done(d: &Done) {
    if let Ok(s) = serde_json::to_string_pretty(d) {
        let _ = std::fs::write(done_file(), s);
    }
}

fn rpc(url: &str, method: &str, args: Value) -> Result<Value, String> {
    crate::tunnel_client::run_rpc(url, method, args)
}

/// Entry: long-running loop (or `--once` single pass).
pub fn run(cmd: &WorkerCmd) -> Result<(), String> {
    let url = cmd
        .server_url
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| std::env::var("TEAMX_SERVER_URL").ok().filter(|u| !u.is_empty()))
        .or_else(crate::tunnel_client::discover_server_url)
        .ok_or_else(|| "teamx worker needs a network-mode server (TEAMX_SERVER_URL or a letter)".to_string())?;

    let enabled = std::env::var("TEAMX_WORKER_ENABLED").as_deref() == Ok("1");
    let interval = cmd.interval.max(10);
    let cooldown = Duration::from_secs(cmd.cooldown.max(60));
    let opencode_bin = cmd.opencode.clone().unwrap_or_else(|| "opencode".to_string());

    eprintln!(
        "teamx worker: member inbox worker (server {url})\n  workspace: {}\n  execute={} interval={interval}s cooldown={}s",
        std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
        enabled,
        cooldown.as_secs()
    );

    let mut last_run_at = Instant::now() - cooldown; // allow first run
    loop {
        match poll_once(&url, &opencode_bin, enabled, cmd, cooldown, &mut last_run_at) {
            Ok(true) => {}
            Ok(false) => eprintln!("teamx worker: nothing actionable; sleeping {interval}s"),
            Err(e) => eprintln!("teamx worker: {e}"),
        }
        if cmd.once {
            break;
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
    Ok(())
}

/// One poll: fetch inbox, decide whether to run a headless turn.
fn poll_once(
    url: &str,
    opencode_bin: &str,
    enabled: bool,
    cmd: &WorkerCmd,
    cooldown: Duration,
    last_run_at: &mut Instant,
) -> Result<bool, String> {
    let mut inbox = rpc(url, "team.inbox", serde_json::json!({ "ack": false }))?;
    // run_rpc returns the `{ok, data}` envelope; unwrap to the payload.
    inbox = inbox.get("data").cloned().unwrap_or(inbox);
    let tasks = inbox.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default();
    let questions = inbox.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
    let nudges = inbox
        .get("nudges")
        .and_then(|n| n.get("unread"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut done = load_done();
    let now = crate::db::now();
    let new_tasks: Vec<Value> = tasks
        .into_iter()
        .filter(|t| !done.items.contains_key(&format!("task:{}", t["id"].as_str().unwrap_or(""))))
        .collect();
    let new_questions: Vec<Value> = questions
        .into_iter()
        .filter(|q| !done.items.contains_key(&format!("q:{}", q["id"].as_str().unwrap_or(""))))
        .collect();

    if new_tasks.is_empty() && new_questions.is_empty() {
        return Ok(false);
    }

    // Throttle: even with unread items, do not run more often than the cooldown
    // (nudges fire periodically while the team is silent; we must not burn
    // tokens by auto-running every nudge).
    if last_run_at.elapsed() < cooldown && !cmd.once {
        eprintln!("teamx worker: cooldown active ({} items pending)", new_tasks.len() + new_questions.len());
        return Ok(true);
    }

    // Build a focused prompt.
    let mut prompt = String::new();
    if !new_tasks.is_empty() {
        prompt.push_str("你有以下待处理的 teamx 任务，请开始执行（检索/产出/提交），完成后用 teamx 工具汇报或 publish：\n");
        for t in new_tasks.iter().take(3) {
            let id = t["id"].as_str().unwrap_or("");
            let title = t["title"].as_str().unwrap_or("");
            prompt.push_str(&format!("- [{id}] {title}\n"));
        }
    }
    if !new_questions.is_empty() {
        prompt.push_str("以下是团队发给你的待答提问，请结合你的实际进展用 teamx 工具答复（respond/汇报进度）：\n");
        for q in new_questions.iter().take(2) {
            prompt.push_str(&format!("- Q: {}\n", q["question"].as_str().unwrap_or("")));
        }
    }
    if let Some(last_nudge) = nudges.last() {
        prompt.push_str(&format!(
            "（另有一条团队提醒：{})\n",
            last_nudge["message"].as_str().unwrap_or("")
        ));
    }
    prompt.push_str("请勿重复已完成的任务；只做与上面相关的事。\n");
    prompt.push_str("执行规则：提交/推送请使用 teamx 工具（如 teamx_git_commit_push），不要在克隆仓库目录内手工运行 teamx/git —— 工具会以工作区目录与你的证书身份执行。");

    eprintln!("teamx worker: would run opencode with {} new task(s), {} new question(s)", new_tasks.len(), new_questions.len());
    eprintln!("  prompt: {}", prompt.chars().take(160).collect::<String>());
    if !enabled {
        eprintln!("  [dry-run] TEAMX_WORKER_ENABLED != 1 — not executing opencode. Items kept unprocessed.");
        return Ok(true);
    }

    // Execute one headless turn.
    let run = Command::new(opencode_bin)
        .arg("run")
        .arg(&prompt)
        .current_dir(std::env::current_dir().map_err(|e| e.to_string())?)
        .output()
        .map_err(|e| format!("opencode run spawn failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&run.stdout);
    let tail: String = stdout.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    eprintln!("teamx worker: opencode run exited={}:\n{}", run.status, tail);
    if !run.status.success() {
        return Err("opencode run failed; items kept for retry".to_string());
    }

    // Mark handled, then acknowledge the inbox (advance the nudge cursor).
    for t in &new_tasks {
        done.items.insert(format!("task:{}", t["id"].as_str().unwrap_or("")), now.clone());
    }
    for q in &new_questions {
        done.items.insert(format!("q:{}", q["id"].as_str().unwrap_or("")), now.clone());
    }
    save_done(&done);
    let _ = rpc(url, "team.inbox", serde_json::json!({ "ack": true }));
    *last_run_at = Instant::now();
    Ok(true)
}
