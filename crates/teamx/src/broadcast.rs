//! broadcast.rs — live WebSocket registry + event fan-out (network mode N1).
//!
//! Each online member connection subscribes to the teams it belongs to. When a
//! command writes a new ledger event, the RPC layer calls [`Hub::publish`] to
//! fan it out to every online member of that team. Delivery is best-effort:
//! a dropped frame is recovered by the member's next `sync` (the ledger stays
//! the single source of truth).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// team_id -> member_id -> sender (one sender per live connection)
type LiveMap = HashMap<String, HashMap<String, UnboundedSender<Value>>>;

#[derive(Clone, Default)]
pub struct Hub {
    live: Arc<Mutex<LiveMap>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a member connection for a set of team ids; returns the receiver
    /// through which events for any of those teams are delivered.
    pub fn subscribe(&self, member_id: &str, teams: &[String]) -> UnboundedReceiver<Value> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut live = self.live.lock().unwrap();
        for team in teams {
            live.entry(team.clone())
                .or_default()
                .insert(member_id.to_string(), tx.clone());
        }
        rx
    }

    /// Remove a member connection (on disconnect).
    pub fn unsubscribe(&self, member_id: &str, teams: &[String]) {
        let mut live = self.live.lock().unwrap();
        for team in teams {
            if let Some(members) = live.get_mut(team) {
                members.remove(member_id);
                if members.is_empty() {
                    live.remove(team);
                }
            }
        }
    }

    /// Fan an event out to every online member of `team_id`.
    pub fn publish(&self, team_id: &str, event: &Value) {
        let live = self.live.lock().unwrap();
        if let Some(members) = live.get(team_id) {
            for tx in members.values() {
                // Best-effort: a full/closed channel is dropped (recovered by sync).
                let _ = tx.send(event.clone());
            }
        }
    }

    /// Actively close every live connection of a member (e.g. on invitation
    /// revocation). Sends a sentinel `close` frame that the WS handler turns
    /// into a connection close.
    pub fn disconnect_member(&self, member_id: &str) {
        let live = self.live.lock().unwrap();
        for members in live.values() {
            if let Some(tx) = members.get(member_id) {
                let _ = tx.send(serde_json::json!({ "type": "close", "code": "revoked" }));
            }
        }
    }

    /// Number of live connections (for `serve status` diagnostics).
    pub fn connection_count(&self) -> usize {
        self.live
            .lock()
            .unwrap()
            .values()
            .map(|m| m.len())
            .sum()
    }
}
