//! opencode agent client — the IM sidecar's link to a **co-located**
//! `opencode serve` HTTP API (方案 1, docs/TODO-im-agent.cn.md).
//!
//! The gateway and the member's agent run on the same host, so the whole
//! conversation travels over `127.0.0.1`: no NAT, no public endpoint, no
//! wake-up latency. One session per member keeps the multi-turn context; the
//! id is persisted next to the workspace so a restart resumes the same thread.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

/// Agent calls can take a while (the model reasons over tools); allow a
/// generous ceiling and let the caller run this off the event loop.
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(300);
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// A co-located opencode agent.
#[derive(Clone)]
pub struct AgentClient {
    base: String,
    session_file: PathBuf,
}

impl AgentClient {
    /// `base` is the opencode serve URL (e.g. `http://127.0.0.1:42630`);
    /// `session_file` persists the session id between restarts.
    pub fn new(base: &str, session_file: PathBuf) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            session_file,
        }
    }

    /// Send one user message and return the assistant's text reply. Blocking —
    /// call it from `spawn_blocking`.
    pub fn chat(&self, text: &str) -> Result<String, String> {
        let session = self.ensure_session()?;
        let url = format!("{}/session/{}/message", self.base, session);
        let body = json!({ "parts": [{ "type": "text", "text": text }] });
        let resp = ureq::post(&url)
            .timeout(MESSAGE_TIMEOUT)
            .send_json(body)
            .map_err(|e| format!("agent message failed: {e}"))?;
        let v: Value = resp
            .into_json()
            .map_err(|e| format!("agent message decode failed: {e}"))?;
        let reply = extract_text(&v);
        if reply.is_empty() {
            return Err("agent returned no text".to_string());
        }
        Ok(reply)
    }

    /// Forget the current session; the next `chat` starts a fresh thread.
    pub fn reset_session(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.session_file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("reset session: {e}")),
        }
    }

    /// Reuse the persisted session id, creating one on first use.
    fn ensure_session(&self) -> Result<String, String> {
        if let Ok(raw) = std::fs::read_to_string(&self.session_file) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(id) = v.get("session_id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        return Ok(id.to_string());
                    }
                }
            }
        }
        let url = format!("{}/session", self.base);
        let resp = ureq::post(&url)
            .timeout(SESSION_TIMEOUT)
            .send_json(json!({}))
            .map_err(|e| format!("agent session create failed: {e}"))?;
        let v: Value = resp
            .into_json()
            .map_err(|e| format!("agent session decode failed: {e}"))?;
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .ok_or("agent session response missing `id`")?
            .to_string();
        if let Some(parent) = self.session_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &self.session_file,
            serde_json::to_string(&json!({ "session_id": id })).unwrap_or_default(),
        );
        Ok(id)
    }
}

/// Concatenate the assistant's text parts from a message response.
fn extract_text(v: &Value) -> String {
    let parts = v
        .get("parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = String::new();
    for p in parts {
        if p.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(t) = p.get("text").and_then(Value::as_str) {
            if t.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_concatenates_text_parts_only() {
        let v = json!({
            "info": { "id": "msg_1" },
            "parts": [
                { "type": "reasoning", "text": "secret chain" },
                { "type": "text", "text": "你好" },
                { "type": "tool", "tool": "bash" },
                { "type": "text", "text": "世界" },
            ]
        });
        assert_eq!(extract_text(&v), "你好\n世界");
        // Nothing to say → empty (the caller treats that as an error).
        assert_eq!(extract_text(&json!({"parts": []})), "");
        assert_eq!(extract_text(&json!({})), "");
    }

    #[test]
    fn session_id_is_reused_from_disk() {
        let dir = std::env::temp_dir().join(format!("teamx-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("session.json");
        std::fs::write(&file, r#"{"session_id":"ses_existing"}"#).unwrap();
        let c = AgentClient::new("http://127.0.0.1:1", file.clone());
        // No HTTP call is made: the persisted id short-circuits ensure_session.
        assert_eq!(c.ensure_session().unwrap(), "ses_existing");
        // reset clears it so the next call would mint a fresh one.
        c.reset_session().unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_url_trailing_slash_is_tolerated() {
        let c = AgentClient::new("http://127.0.0.1:42630/", PathBuf::from("/tmp/x.json"));
        assert_eq!(c.base, "http://127.0.0.1:42630");
    }

    /// End-to-end against a mock `opencode serve`: the first `chat` mints a
    /// session, the second reuses it, and the assistant text comes back.
    #[test]
    fn chat_creates_a_session_once_then_reuses_it() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let sessions = Arc::new(AtomicUsize::new(0));
        let messages = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s2, m2, stop2) = (sessions.clone(), messages.clone(), stop.clone());

        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !stop2.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        sock.set_nonblocking(false).ok();
                        let mut buf = [0u8; 16384];
                        let n = sock.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).to_string();
                        let path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("")
                            .to_string();
                        let body = if path == "/session" {
                            s2.fetch_add(1, Ordering::SeqCst);
                            r#"{"id":"ses_mock1"}"#
                        } else {
                            m2.fetch_add(1, Ordering::SeqCst);
                            r#"{"info":{"id":"msg_1"},"parts":[{"type":"reasoning","text":"x"},{"type":"text","text":"pong"}]}"#
                        };
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = sock.write_all(resp.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        let dir = std::env::temp_dir().join(format!("teamx-agent-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = AgentClient::new(&format!("http://{addr}"), dir.join("session.json"));

        assert_eq!(c.chat("hello").unwrap(), "pong");
        assert_eq!(c.chat("again").unwrap(), "pong");
        assert_eq!(sessions.load(Ordering::SeqCst), 1, "session created exactly once");
        assert_eq!(messages.load(Ordering::SeqCst), 2, "both turns reached the agent");
        stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = server.join();
    }
}
