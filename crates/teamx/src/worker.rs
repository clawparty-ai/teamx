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


fn session_file() -> PathBuf {
    crate::db::teamx_home().join("worker-session.json")
}
fn load_session_id() -> Option<String> {
    std::fs::read_to_string(session_file())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["session_id"].as_str().map(str::to_string))
}
fn save_session_id(id: &str) {
    if let Ok(s) = serde_json::to_string_pretty(&serde_json::json!({ "session_id": id })) {
        let _ = std::fs::write(session_file(), s);
    }
}

/// Fetch `GET {url}/api/session` from the local opencode server (plain HTTP).
fn rpc_http(url: &str) -> Result<Value, String> {
    use std::io::Read;
    let host = url.trim_end_matches('/').trim_start_matches("http://");
    let mut parts = host.splitn(2, ':');
    let addr = parts.next().unwrap_or("127.0.0.1");
    let port: u16 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(80);
    let mut stream = std::net::TcpStream::connect((addr, port)).map_err(|e| format!("connect: {e}"))?;
    let req = format!("GET /api/session HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    use std::io::Write;
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("data").cloned())
        .ok_or_else(|| "no session payload".to_string())
}

fn rpc(url: &str, method: &str, args: Value) -> Result<Value, String> {
    crate::tunnel_client::run_rpc(url, method, args)
}

/// Resolve which local opencode server the worker attaches to.
///
/// Precedence: `--opencode-url` flag, then `TEAMX_OPENCODE_URL`, then a hard
/// error. There is deliberately NO port-4199 fallback: on nodes hosting
/// several workspaces (each opencode serve on its own port) a silent default
/// attaches to whatever happens to listen on 4199 — potentially another
/// workspace's session — and runs that member's tasks in the wrong place.
fn resolve_opencode_url(flag: Option<&str>) -> Result<String, String> {
    if let Some(u) = flag.filter(|u| !u.is_empty()) {
        return Ok(u.to_string());
    }
    if let Some(u) = std::env::var("TEAMX_OPENCODE_URL").ok().filter(|u| !u.is_empty()) {
        return Ok(u);
    }
    Err("teamx worker: refusing to guess the opencode server URL (the old \
         http://127.0.0.1:4199 default attached to the wrong workspace on \
         multi-workspace nodes). Pass --opencode-url <url> (e.g. the port \
         from `opencode serve --port <4199+k>`) or set TEAMX_OPENCODE_URL."
        .to_string())
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
    // No silent default: on a multi-workspace node the 4199 default used to
    // attach to whatever opencode happened to listen there (possibly another
    // workspace's session). Fail fast instead — pass --opencode-url or set
    // TEAMX_OPENCODE_URL.
    let opencode_url = resolve_opencode_url(cmd.opencode_url.as_deref())?;

    eprintln!(
        "teamx worker: member inbox worker (server {url})\n  workspace: {}\n  opencode: {opencode_url}\n  execute={} interval={interval}s cooldown={}s",
        std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
        enabled,
        cooldown.as_secs()
    );

    let mut last_run_at = Instant::now() - cooldown; // allow first run
    loop {
        match poll_once(&url, &opencode_bin, &opencode_url, enabled, cmd, cooldown, &mut last_run_at) {
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
    opencode_url: &str,
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
    let discussions = inbox.get("discussions").and_then(Value::as_array).cloned().unwrap_or_default();
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
    let new_discussions: Vec<Value> = discussions
        .into_iter()
        .filter(|d| !done.items.contains_key(&format!("disc:{}", d["seq"].as_i64().unwrap_or(0))))
        .collect();

    if new_tasks.is_empty() && new_questions.is_empty() && new_discussions.is_empty() {
        return Ok(false);
    }

    // Throttle: even with unread items, do not run more often than the cooldown
    // (nudges fire periodically while the team is silent; we must not burn
    // tokens by auto-running every nudge).
    if last_run_at.elapsed() < cooldown && !cmd.once {
        eprintln!(
            "teamx worker: cooldown active ({} items pending)",
            new_tasks.len() + new_questions.len() + new_discussions.len()
        );
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
    if !new_discussions.is_empty() {
        prompt.push_str("以下是团队讨论中与你相关的新消息（你被 @ / 订阅 / 与你任务相关）：\n");
        for d in new_discussions.iter().take(5) {
            let did = d["discussion_id"].as_str().unwrap_or("");
            let title = d["title"].as_str().unwrap_or("");
            let author = d["author"].as_str().unwrap_or("");
            let body = d["body"].as_str().unwrap_or("");
            let trig = d.get("trigger").cloned().unwrap_or_else(|| serde_json::json!({}));
            let why: Vec<&str> = vec![
                trig.get("mentioned").and_then(|x| x.as_bool()).unwrap_or(false).then_some("@提及"),
                trig.get("subscribed").and_then(|x| x.as_bool()).unwrap_or(false).then_some("订阅"),
                trig.get("my_task").and_then(|x| x.as_bool()).unwrap_or(false).then_some("关联任务"),
            ]
            .into_iter()
            .flatten()
            .collect();
            prompt.push_str(&format!(
                "- [{title}] ({did}) {author} 说：{body}  [触发: {}]\n",
                why.join("/")
            ));
        }
        prompt.push_str(
            "请判断是否参与讨论：若与你的职责/被@/任务相关且有增量信息，用 teamx discussion reply 参与；否则保持安静不要回复（不要无意义消耗配额）。\n",
        );
    }
    if let Some(last_nudge) = nudges.last() {
        prompt.push_str(&format!(
            "（另有一条团队提醒：{})\n",
            last_nudge["message"].as_str().unwrap_or("")
        ));
    }
    prompt.push_str("请勿重复已完成的任务；只做与上面相关的事。\n");
    prompt.push_str("执行规则：提交/推送请使用 teamx 工具（如 teamx_git_commit_push），不要在克隆仓库目录内手工运行 teamx/git —— 工具会以工作区目录与你的证书身份执行。");

    eprintln!(
        "teamx worker: would run opencode with {} new task(s), {} new question(s), {} new discussion(s)",
        new_tasks.len(),
        new_questions.len(),
        new_discussions.len()
    );
    eprintln!("  prompt: {}", prompt.chars().take(160).collect::<String>());
    if !enabled {
        eprintln!("  [dry-run] TEAMX_WORKER_ENABLED != 1 — not executing opencode. Items kept unprocessed.");
        return Ok(true);
    }

    // Execute one headless turn against the node's resident opencode server,
    // continuing this worker's stable session (same context ttyd/web can see).
    let session_id = load_session_id();
    let mut args: Vec<String> = vec!["run".into(), "--attach".into(), opencode_url.to_string()];
    if let Some(sid) = &session_id {
        args.push("-s".into());
        args.push(sid.clone());
    }
    args.push(prompt);
    let run = match Command::new(opencode_bin)
        .args(&args)
        .current_dir(std::env::current_dir().map_err(|e| e.to_string())?)
        .output()
    {
        Ok(r) => r,
        Err(e) => {
            *last_run_at = Instant::now(); // throttle retries even on failure
            return Err(format!("opencode run spawn failed: {e}"));
        }
    };
    let stdout = String::from_utf8_lossy(&run.stdout);
    // 诊断缺口修复：原先**只打印 stdout**，opencode 的错误全在 stderr 上 → 真机排查时"exit 1 且无输出"。
    // 现在失败时同时给出 stdout 与 stderr 的尾部（各 12 行）。
    let tail: String = stdout.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    if run.status.success() {
        eprintln!("teamx worker: opencode run exited={}:\n{}", run.status, tail);
    } else {
        let stderr = String::from_utf8_lossy(&run.stderr);
        let etail: String = stderr.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        eprintln!(
            "teamx worker: opencode run exited={}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            run.status, tail, etail
        );
    }
    if !run.status.success() {
        *last_run_at = Instant::now(); // do not retry every poll after a failure
        return Err("opencode run failed; items kept for retry".to_string());
    }
    // Remember the session id the first time so later turns share its context.
    if session_id.is_none() {
        if let Ok(list) = rpc_http(opencode_url) {
            // Pick the most recently updated session (the one we just created).
            let newest = list.as_array().and_then(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some((
                            s.get("id").and_then(Value::as_str)?.to_string(),
                            s.get("time").and_then(|t| t.get("updated")).and_then(Value::as_i64).unwrap_or(0),
                        ))
                    })
                    .max_by_key(|(_id, ts)| *ts)
            });
            if let Some((id, _ts)) = newest {
                save_session_id(&id);
            }
        }
    }

    // Mark handled, then acknowledge the inbox (advance the nudge cursor).
    for t in &new_tasks {
        done.items.insert(format!("task:{}", t["id"].as_str().unwrap_or("")), now.clone());
    }
    for q in &new_questions {
        done.items.insert(format!("q:{}", q["id"].as_str().unwrap_or("")), now.clone());
    }
    for d in &new_discussions {
        done.items.insert(format!("disc:{}", d["seq"].as_i64().unwrap_or(0)), now.clone());
    }
    save_done(&done);
    let _ = rpc(url, "team.inbox", serde_json::json!({ "ack": true }));
    *last_run_at = Instant::now();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save + clear the env var, returning the previous value for restore.
    fn stash_env() -> Option<String> {
        let prev = std::env::var("TEAMX_OPENCODE_URL").ok();
        std::env::remove_var("TEAMX_OPENCODE_URL");
        prev
    }

    fn restore_env(prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var("TEAMX_OPENCODE_URL", v),
            None => std::env::remove_var("TEAMX_OPENCODE_URL"),
        }
    }

    #[test]
    fn explicit_flag_wins_over_env() {
        let prev = stash_env();
        std::env::set_var("TEAMX_OPENCODE_URL", "http://127.0.0.1:4201");
        let got = resolve_opencode_url(Some("http://127.0.0.1:4301")).unwrap();
        assert_eq!(got, "http://127.0.0.1:4301");
        restore_env(prev);
    }

    #[test]
    fn env_is_used_when_no_flag() {
        let prev = stash_env();
        std::env::set_var("TEAMX_OPENCODE_URL", "http://127.0.0.1:4201");
        let got = resolve_opencode_url(None).unwrap();
        assert_eq!(got, "http://127.0.0.1:4201");
        restore_env(prev);
    }

    #[test]
    fn empty_flag_falls_through_to_env() {
        let prev = stash_env();
        std::env::set_var("TEAMX_OPENCODE_URL", "http://127.0.0.1:4201");
        let got = resolve_opencode_url(Some("")).unwrap();
        assert_eq!(got, "http://127.0.0.1:4201");
        restore_env(prev);
    }

    #[test]
    fn missing_everywhere_is_a_hard_error_not_a_silent_default() {
        let prev = stash_env();
        let err = resolve_opencode_url(None).unwrap_err();
        assert!(err.contains("--opencode-url"), "{err}");
        assert!(err.contains("TEAMX_OPENCODE_URL"), "{err}");
        restore_env(prev);
    }
}
