//! broadcast.rs — live WebSocket registry + event fan-out (network mode N1).
//!
//! Each online member connection subscribes to the teams it belongs to. When a
//! command writes a new ledger event, the RPC layer calls [`Hub::publish`] to
//! fan it out to every online member of that team. Delivery is best-effort:
//! a dropped frame is recovered by the member's next `sync` (the ledger stays
//! the single source of truth).
//!
//! A member may hold several live connections at once (e.g. two windows); each
//! `subscribe` call gets its own subscription key (`<member_id>\0<seq>`), so a
//! new connection never silently replaces an earlier one's sender.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// team_id -> subscription_key -> sender (one sender per live connection)
type LiveMap = HashMap<String, HashMap<String, UnboundedSender<Value>>>;

/// Handle identifying one live connection's subscriptions; pass it back to
/// [`Hub::unsubscribe`] on disconnect.
#[derive(Debug, Clone)]
pub struct Subscription {
    key: String,
    teams: Vec<String>,
}

#[derive(Clone, Default)]
pub struct Hub {
    live: Arc<Mutex<LiveMap>>,
    next_conn: Arc<AtomicU64>,
}

fn key_prefix(member_id: &str) -> String {
    // member ids are UUIDs and never contain NUL, so this separator is safe.
    format!("{member_id}\u{0}")
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a member connection for a set of team ids; returns the receiver
    /// through which events for any of those teams are delivered, plus the
    /// subscription handle used to remove it later.
    pub fn subscribe(&self, member_id: &str, teams: &[String]) -> (UnboundedReceiver<Value>, Subscription) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let key = format!("{}{}", key_prefix(member_id), self.next_conn.fetch_add(1, Ordering::Relaxed));
        let mut live = self.live.lock().unwrap();
        for team in teams {
            live.entry(team.clone())
                .or_default()
                .insert(key.clone(), tx.clone());
        }
        let sub = Subscription { key, teams: teams.to_vec() };
        (rx, sub)
    }

    /// Remove one member connection (on disconnect). Only the subscriptions
    /// made by the matching `subscribe` call are removed.
    pub fn unsubscribe(&self, sub: &Subscription) {
        let mut live = self.live.lock().unwrap();
        for team in &sub.teams {
            if let Some(members) = live.get_mut(team) {
                members.remove(&sub.key);
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
        let prefix = key_prefix(member_id);
        for members in live.values() {
            for (key, tx) in members {
                if key.starts_with(&prefix) {
                    let _ = tx.send(serde_json::json!({ "type": "close", "code": "revoked" }));
                }
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
