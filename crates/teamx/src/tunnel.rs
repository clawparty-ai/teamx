//! tunnel.rs — reverse tunnels (frp-style) for exposing member services.
//!
//! A provider member ("server", e.g. a developer) opens a persistent WebSocket
//! to the teamx server and registers a local service. The server allocates a
//! public TCP port and relays bytes between consumers (other team members) and
//! the provider over that WebSocket.
//!
//! Protocol (server ↔ provider, one WS per service):
//!   provider → server (text control frames):
//!     {"type":"register","name":"httpbin","port":8080,"lan_ip":"192.168.1.5"}
//!     {"type":"unregister","name":"httpbin"}
//!     {"type":"close_stream","stream_id":3}
//!   server → provider (text control frames):
//!     {"type":"registered","port":9001,"name":"httpbin"}
//!     {"type":"open_stream","stream_id":1}
//!     {"type":"error","message":"..."}
//!   data (binary frames, both directions): [4-byte BE stream_id][payload]
//!
//! A consumer connects to `tcp://<server>:<port>`; each TCP connection becomes
//! one stream on the provider's WS. The provider dials its local `target_port`
//! and relays bytes. `lan_ip` records the provider's LAN address so the server
//! can help consumers decide whether to connect directly (same subnet) or
//! through the relay.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::ws::Message;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// Minimum / default public port pool for tunnels.
/// Starts at 9100 to avoid the 9000s commonly used by other software
/// (e.g. Docker Desktop proxies) on dev machines.
pub const TUNNEL_PORT_MIN: u16 = 9100;
pub const TUNNEL_PORT_MAX: u16 = 9999;

/// A registered exposed service.
pub struct Tunnel {
    pub name: String,
    pub team_id: String,
    pub provider_member_id: String,
    /// Public port on the server (allocated at register).
    pub port: u16,
    /// Local port on the provider's machine that the tunnel forwards to.
    pub target_port: u16,
    /// Provider's LAN address (for direct-connect hints), if known.
    pub lan_ip: Option<String>,
    /// Sender that delivers WS messages to the provider's connection.
    pub ws_tx: UnboundedSender<Message>,
    /// Per-stream writers: stream_id → channel carrying bytes that the
    /// provider relays back, to be written to the consumer's TCP connection.
    pub streams: Arc<Mutex<HashMap<u64, UnboundedSender<Vec<u8>>>>>,
    /// Signals the relay task to stop listening (sent on close).
    pub shutdown: tokio::sync::watch::Sender<bool>,
}
/// Data-frame helpers: [4-byte BE stream_id][payload].
pub struct TunnelFrame;

impl TunnelFrame {
    /// Encode a data frame: [4-byte BE stream_id][payload].
    pub fn encode_data(stream_id: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(stream_id as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Decode the stream_id + payload of a binary data frame.
    pub fn decode_data(buf: &[u8]) -> Option<(u64, &[u8])> {
        if buf.len() < 4 {
            return None;
        }
        let sid = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        Some((sid, &buf[4..]))
    }
}

/// Shared tunnel registry (server-side).
#[derive(Clone, Default)]
pub struct TunnelRegistry {
    /// Key: `{team_id}/{name}` → Tunnel.
    by_key: Arc<Mutex<HashMap<String, Tunnel>>>,
    /// Occupied public ports (fast allocation).
    ports: Arc<Mutex<Vec<u16>>>,
}

impl TunnelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(team_id: &str, name: &str) -> String {
        format!("{team_id}/{name}")
    }

    /// Allocate the next free public port from the pool.
    fn alloc_port(&self) -> Option<u16> {
        let mut ports = self.ports.lock().unwrap();
        (TUNNEL_PORT_MIN..=TUNNEL_PORT_MAX).find(|p| !ports.contains(p)).inspect(|p| ports.push(*p))
    }

    /// Register a tunnel. Returns the allocated public port, or an error if
    /// the name is already taken / no port is free.
    pub fn register(
        &self,
        team_id: &str,
        provider_member_id: &str,
        name: &str,
        target_port: u16,
        lan_ip: Option<String>,
        ws_tx: UnboundedSender<Message>,
    ) -> Result<u16, String> {
        let key = Self::key(team_id, name);
        let mut by_key = self.by_key.lock().unwrap();
        if by_key.contains_key(&key) {
            return Err(format!("tunnel `{name}` already exists in this team"));
        }
        let port = self.alloc_port().ok_or("tunnel port pool exhausted (9000-9999)")?;
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        by_key.insert(
            key,
            Tunnel {
                name: name.to_string(),
                team_id: team_id.to_string(),
                provider_member_id: provider_member_id.to_string(),
                port,
                target_port,
                lan_ip,
                ws_tx,
                streams: Arc::new(Mutex::new(HashMap::new())),
                shutdown: shutdown_tx,
            },
        );
        Ok(port)
    }

    /// Look up a tunnel by team + name (returns a cheap clone).
    pub fn get(&self, team_id: &str, name: &str) -> Option<Tunnel> {
        let by_key = self.by_key.lock().unwrap();
        by_key.get(&Self::key(team_id, name)).map(|t| Tunnel {
            name: t.name.clone(),
            team_id: t.team_id.clone(),
            provider_member_id: t.provider_member_id.clone(),
            port: t.port,
            target_port: t.target_port,
            lan_ip: t.lan_ip.clone(),
            ws_tx: t.ws_tx.clone(),
            streams: t.streams.clone(),
            shutdown: t.shutdown.clone(),
        })
    }

    /// List tunnels for a team.
    pub fn list(&self, team_id: &str) -> Vec<serde_json::Value> {
        let by_key = self.by_key.lock().unwrap();
        by_key
            .values()
            .filter(|t| t.team_id == team_id)
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "port": t.port,
                    "target_port": t.target_port,
                    "lan_ip": t.lan_ip,
                    "provider_member_id": t.provider_member_id,
                })
            })
            .collect()
    }

    /// Remove a tunnel (by provider disconnect or explicit close). Returns the
    /// freed public port.
    pub fn remove(&self, team_id: &str, name: &str) -> Option<u16> {
        let key = Self::key(team_id, name);
        let mut by_key = self.by_key.lock().unwrap();
        if let Some(t) = by_key.remove(&key) {
            let _ = t.shutdown.send(true);
            let mut ports = self.ports.lock().unwrap();
            ports.retain(|p| *p != t.port);
            Some(t.port)
        } else {
            None
        }
    }

    /// Whether two socket addresses are on the same /24 subnet (used for
    /// direct-connect hints). Compares the first three octets of IPv4.
    pub fn same_subnet(a: &SocketAddr, b: &str) -> bool {
        let a_ip = match a.ip() {
            std::net::IpAddr::V4(v4) => v4.octets(),
            _ => return false,
        };
        let b_ip: std::net::Ipv4Addr = match b.parse() {
            Ok(ip) => ip,
            Err(_) => return false,
        };
        let b_oct = b_ip.octets();
        a_ip[0] == b_oct[0] && a_ip[1] == b_oct[1] && a_ip[2] == b_oct[2]
    }
}

/// Relay data frames coming back from a provider (via its tunnel WS) into the
/// matching consumer TCP connection's writer channel.
pub fn route_provider_data(registry: &TunnelRegistry, team_id: &str, name: &str, buf: &[u8]) {
    let (sid, payload) = match TunnelFrame::decode_data(buf) {
        Some(v) => v,
        None => return,
    };
    if let Some(t) = registry.get(team_id, name) {
        if let Some(tx) = t.streams.lock().unwrap().get(&sid) {
            let _ = tx.send(payload.to_vec());
        }
    }
}

/// Spawn the TCP relay for one tunnel: accept consumer connections and bridge
/// bytes with the provider's WS.
pub async fn run_tcp_relay(
    registry: TunnelRegistry,
    team_id: String,
    name: String,
) -> Result<(), String> {
    let t = registry
        .get(&team_id, &name)
        .ok_or_else(|| format!("tunnel `{name}` disappeared before relay start"))?;
    let port = t.port;
    let bind = format!("0.0.0.0:{port}");
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("teamx tunnel `{name}` bind {bind} failed: {e}");
            return Err(format!("bind tunnel port {port}: {e}"));
        }
    };

    let next_id: Arc<Mutex<u64>> = Arc::new(Mutex::new(1));
    eprintln!("teamx tunnel `{name}` listening on tcp://0.0.0.0:{port}");
    let mut shutdown_rx = t.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                eprintln!("teamx tunnel `{name}` closed; releasing port {port}");
                break;
            }
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(v) => v,
                    Err(_) => break,
                };
        let (read_half, mut write_half) = stream.into_split();
        let sid = {
            let mut n = next_id.lock().unwrap();
            let id = *n;
            *n += 1;
            id
        };
        let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
        if let Some(tunnel) = registry.get(&team_id, &name) {
            tunnel.streams.lock().unwrap().insert(sid, tx.clone());
            let _ = tunnel.ws_tx.send(Message::Text(
                serde_json::json!({ "type": "open_stream", "stream_id": sid })
                    .to_string()
                    .into(),
            ));
        } else {
            eprintln!("teamx tunnel `{name}` relay lost registration for stream {sid}");
        }

        let registry2 = registry.clone();
        let team_id2 = team_id.clone();
        let name2 = name.clone();
        // consumer → provider
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut read_half = tokio::io::BufReader::new(read_half);
            let mut buf = [0u8; 8192];
            loop {
                let n = match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                let frame = TunnelFrame::encode_data(sid, &buf[..n]);
                if let Some(t) = registry2.get(&team_id2, &name2) {
                    let _ = t.ws_tx.send(Message::Binary(frame.into()));
                }
            }
            // consumer disconnected: notify provider
            if let Some(t) = registry2.get(&team_id2, &name2) {
                let _ = t.ws_tx.send(Message::Text(
                    serde_json::json!({ "type": "close_stream", "stream_id": sid })
                        .to_string()
                        .into(),
                ));
            }
            if let Some(t) = registry2.get(&team_id2, &name2) {
                t.streams.lock().unwrap().remove(&sid);
            }
        });

        // provider → consumer
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(bytes) = rx.recv().await {
                if write_half.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let payload = b"hello world";
        let frame = TunnelFrame::encode_data(7, payload);
        let (sid, data) = TunnelFrame::decode_data(&frame).unwrap();
        assert_eq!(sid, 7);
        assert_eq!(data, payload);
        assert!(TunnelFrame::decode_data(&[1, 2]).is_none());
    }

    #[test]
    fn same_subnet_judgment() {
        let a: SocketAddr = "192.168.1.10:9001".parse().unwrap();
        assert!(TunnelRegistry::same_subnet(&a, "192.168.1.5"));
        assert!(!TunnelRegistry::same_subnet(&a, "192.168.2.5"));
        assert!(!TunnelRegistry::same_subnet(&a, "10.0.0.1"));
        // IPv6 peers are never "same /24"
        let v6: SocketAddr = "[::1]:9001".parse().unwrap();
        assert!(!TunnelRegistry::same_subnet(&v6, "192.168.1.5"));
    }

    #[test]
    fn registry_register_list_remove() {
        let reg = TunnelRegistry::new();
        let (tx, _rx) = unbounded_channel();
        let port = reg.register("team1", "member1", "httpbin", 8080, Some("192.168.1.5".into()), tx).unwrap();
        assert!((TUNNEL_PORT_MIN..=TUNNEL_PORT_MAX).contains(&port));
        // duplicate name rejected
        let (tx2, _rx2) = unbounded_channel();
        assert!(reg.register("team1", "member1", "httpbin", 8081, None, tx2).is_err());
        // list only returns this team's tunnels
        let list = reg.list("team1");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "httpbin");
        assert_eq!(reg.list("team2").len(), 0);
        // remove frees the port
        let freed = reg.remove("team1", "httpbin");
        assert_eq!(freed, Some(port));
        assert!(reg.get("team1", "httpbin").is_none());
    }
}
