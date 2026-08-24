//! metrics.rs — per-member network quality metrics (live, in-memory).
//!
//! Tracks, per member, the last measured RTT (ping) and the current receive /
//! transmit byte rates (bps) over a sliding window. Values are computed on
//! demand from `snapshot()`, so there is no background thread: every
//! `record_rx`/`record_tx` bumps a byte counter and a timestamp; `snapshot`
//! divides deltas by elapsed time to produce bps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One member's live counters.
#[derive(Default, Clone)]
struct Counters {
    rx_bytes: u64,
    tx_bytes: u64,
    last_rx: Option<Instant>,
    last_tx: Option<Instant>,
    /// Most recent measured RTT (ms); None until first ping/pong.
    rtt_ms: Option<f64>,
    /// Last time an RTT was measured.
    last_rtt: Option<Instant>,
}

/// A snapshot of a member's current metrics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemberMetrics {
    pub ping_ms: Option<f64>,
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub online: bool,
}

#[derive(Default)]
pub struct MetricsRegistry {
    map: Mutex<HashMap<String, Counters>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        MetricsRegistry { map: Mutex::new(HashMap::new()) }
    }

    /// Record `n` bytes received (member -> server).
    pub fn record_rx(&self, member: &str, n: u64) {
        let mut m = self.map.lock().unwrap();
        let c = m.entry(member.to_string()).or_default();
        c.rx_bytes = c.rx_bytes.saturating_add(n);
        c.last_rx = Some(Instant::now());
    }

    /// Record `n` bytes sent (server -> member).
    pub fn record_tx(&self, member: &str, n: u64) {
        let mut m = self.map.lock().unwrap();
        let c = m.entry(member.to_string()).or_default();
        c.tx_bytes = c.tx_bytes.saturating_add(n);
        c.last_tx = Some(Instant::now());
    }

    /// Record a measured RTT in milliseconds.
    pub fn record_rtt(&self, member: &str, ms: f64) {
        let mut m = self.map.lock().unwrap();
        let c = m.entry(member.to_string()).or_default();
        c.rtt_ms = Some(ms);
        c.last_rtt = Some(Instant::now());
    }

    /// Compute current rates for one member (bytes/sec over the sliding
    /// window since the last recording, falling back to total elapsed).
    pub fn snapshot(&self, member: &str) -> Option<MemberMetrics> {
        let mut m = self.map.lock().unwrap();
        let c = m.get_mut(member)?;

        // Base window: since the first recorded activity (or now if none).
        let now = Instant::now();
        let start = c.last_rx.or(c.last_tx).unwrap_or(now);
        let elapsed = now.duration_since(start).as_secs_f64().max(0.001);
        // To keep rates responsive, use the min(elapsed, 2s) window but at
        // least 0.2s so a single burst doesn't report absurd bps.
        let window = elapsed.min(2.0).max(0.2);
        let rx_bps = (c.rx_bytes as f64 / window).round() as u64;
        let tx_bps = (c.tx_bytes as f64 / window).round() as u64;

        // Reset the byte counters but keep the window anchor moving.
        c.rx_bytes = 0;
        c.tx_bytes = 0;

        let online = c.last_rx.or(c.last_tx).or(c.last_rtt)
            .map(|t| now.duration_since(t).as_secs() < 10)
            .unwrap_or(false);

        Some(MemberMetrics {
            ping_ms: c.rtt_ms,
            rx_bps,
            tx_bps,
            online,
        })
    }

    /// Snapshot for all known members.
    pub fn snapshot_all(&self) -> HashMap<String, MemberMetrics> {
        let members: Vec<String> = self.map.lock().unwrap().keys().cloned().collect();
        members.iter().filter_map(|m| self.snapshot(m).map(|v| (m.clone(), v))).collect()
    }
}

/// Convenience: shared registry type.
pub type SharedMetrics = Arc<MetricsRegistry>;
