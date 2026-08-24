//! tun_dev.rs — cross-platform TUN device wrapper.
//!
//! Creates and manages the `tun0` virtual network interface:
//!   - macOS: `utunN` (auto-allocated), configured via ifconfig + route by the
//!     underlying `tun` crate (needs root).
//!   - Linux: `tunN` via /dev/net/tun + ioctl (needs root / CAP_NET_ADMIN).
//!
//! The wrapper exposes raw packet read/write (L3 IP packets, no Ethernet
//! header) plus the raw fd for smoltcp's `phy::wait` poll loop.

use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use tun::AbstractDevice;

/// A configured, up and running TUN device.
pub struct TunDevice {
    dev: tun::Device,
    /// Actual interface name (`utun3`, `tun0`, ...) — may differ from the
    /// requested name when the OS auto-allocates (macOS).
    pub name: String,
    /// The device's own IP on the fake-ip subnet.
    pub ip: Ipv4Addr,
    pub mtu: u16,
}

impl TunDevice {
    /// Create and bring up a TUN device.
    ///
    /// - `name`: optional requested name (`tun0` / `utun0`); macOS may allocate
    ///   a different `utunN`.
    /// - `ip` / `netmask`: the interface address + netmask.
    /// - `mtu`: interface MTU (recommend 1280 for overlay links).
    ///
    /// Requires root (euid 0) on both platforms; the caller should check
    /// privileges before calling (see `require_root`).
    pub fn create(name: Option<&str>, ip: Ipv4Addr, netmask: Ipv4Addr, mtu: u16) -> Result<TunDevice, String> {
        let mut config = tun::Configuration::default();
        if let Some(n) = name {
            config.tun_name(n);
        }
        config
            .address(ip)
            .netmask(netmask)
            .destination(ip) // peer = self for point-to-point
            .mtu(mtu)
            .up();

        #[cfg(target_os = "linux")]
        config.platform_config(|c| {
            c.ensure_root_privileges(true);
        });

        let dev = tun::create(&config).map_err(|e| format!("tun create: {e}"))?;
        // Critical: set the fd non-blocking so read_packet() returns None
        // (instead of blocking the poll loop) when no packet is queued.
        dev.set_nonblock().map_err(|e| format!("tun set_nonblock: {e}"))?;
        let actual = dev.tun_name().unwrap_or_else(|_| name.unwrap_or("tun").to_string());
        Ok(TunDevice { dev, name: actual, ip, mtu })
    }

    /// Read one IP packet (non-blocking). Returns `None` when no packet is
    /// currently available (EWOULDBLOCK / EAGAIN).
    pub fn read_packet(&mut self, buf: &mut [u8]) -> Option<usize> {
        match self.dev.read(buf) {
            Ok(n) if n > 0 => Some(n),
            Ok(_) | Err(_) => None,
        }
    }

    /// Write one IP packet into the device (delivered to the host stack, i.e.
    /// the application side). Public API for direct packet injection.
    #[allow(dead_code)] // used by smoltcp via TunPhy; kept as a direct primitive
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        self.dev.write(packet).map(|_| ()).map_err(|e| format!("tun write: {e}"))
    }

    /// Raw fd for poll loops (smoltcp `phy::wait`).
    #[allow(dead_code)] // kept as a low-level primitive; the poll loop now sleeps asynchronously
    pub fn as_raw_fd(&self) -> i32 {
        self.dev.as_raw_fd()
    }
}

/// Check that the process is running with root privileges (required to create
/// a TUN device and to inject routes).
pub fn require_root() -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid is safe.
        let uid = unsafe { libc::geteuid() };
        if uid != 0 {
            return Err(
                "tun0 requires root privileges — run with `sudo teamx tun0 start`".to_string(),
            );
        }
    }
    #[cfg(not(unix))]
    {
        return Err("tun0 is only supported on Linux and macOS".to_string());
    }
    Ok(())
}

/// Add a route pushing `net/cidr` through the TUN device.
pub fn add_route(net: Ipv4Addr, cidr: u8, dev: &str) -> Result<(), String> {
    let prefix = format!("{net}/{cidr}");
    #[cfg(target_os = "macos")]
    {
        let st = std::process::Command::new("route")
            .args(["-n", "add", "-net", &prefix, "-interface", dev])
            .status()
            .map_err(|e| format!("route add (macos): {e}"))?;
        if !st.success() {
            return Err(format!("route add failed: {prefix} -> {dev} (exit {:?})", st.code()));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let st = std::process::Command::new("ip")
            .args(["route", "add", &prefix, "dev", dev])
            .status()
            .map_err(|e| format!("route add (linux): {e}"))?;
        if !st.success() {
            return Err(format!("route add failed: {prefix} -> {dev} (exit {:?})", st.code()));
        }
    }
    Ok(())
}

/// Remove the route for `net/cidr` through `dev`. Best-effort (ignores
/// "does not exist" errors).
pub fn del_route(net: Ipv4Addr, cidr: u8, dev: &str) -> Result<(), String> {
    let prefix = format!("{net}/{cidr}");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("route")
            .args(["-n", "delete", "-net", &prefix, "-interface", dev])
            .status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("ip")
            .args(["route", "del", &prefix, "dev", dev])
            .status();
    }
    Ok(())
}

/// Add a host route (a single /32 IP) through `dev`. Best-effort: returns
/// `Ok` even if the route already exists (idempotent).
pub fn add_ip_route(ip: Ipv4Addr, dev: &str) -> Result<(), String> {
    match add_route(ip, 32, dev) {
        Ok(()) => Ok(()),
        // "File exists" is fine — the route is already there.
        Err(_) => Ok(()),
    }
}

/// Remove a host route (a single /32 IP). Best-effort.
pub fn del_ip_route(ip: Ipv4Addr, dev: &str) {
    let _ = del_route(ip, 32, dev);
}

/// Point every enabled network service's DNS at the tun fake-ip gateway so
/// apps resolve domains to fake IPs that route through tun0 (transparent
/// proxying). The original DNS servers are kept as fallbacks (2nd, 3rd, …) so
/// the system keeps resolving even if the tun DNS stops answering, and are
/// saved to a backup file so `restore_system_dns` can undo the change.
/// macOS only (networksetup); a no-op elsewhere.
pub fn set_system_dns(ip: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let fallback = current_dns_macos();
        let mut backup = DnsBackup {
            fallback: fallback.clone(),
            services: std::collections::HashMap::new(),
        };
        let services = list_services_macos()?;
        for svc in &services {
            let original = get_dns_macos(svc);
            // New DNS = [fake-ip gateway] + original/fallback (deduped, gateway first).
            let mut new = vec![ip.to_string()];
            for d in fallback.iter().chain(original.iter()) {
                if d != ip && !new.contains(d) {
                    new.push(d.clone());
                }
            }
            set_dns_macos(svc, &new)?;
            backup.services.insert(svc.clone(), original);
        }
        save_backup(&backup)?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ip;
        Ok(())
    }
}

/// Restore the DNS servers saved by `set_system_dns` (back to automatic/DHCP
/// where they were). macOS only; no-op elsewhere.
pub fn restore_system_dns() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(backup) = load_backup() {
            for (svc, original) in &backup.services {
                let _ = set_dns_macos(svc, original);
            }
        }
        let _ = std::fs::remove_file(dns_backup_path());
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// Set system DNS to a single server with no fallback.
/// NOTE: superseded by `set_system_dns` (keeps the original DNS as fallback).
/// macOS only; no-op elsewhere.

/// Per-service DNS snapshot saved across a tun0 session.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct DnsBackup {
    #[allow(dead_code)] // recorded for diagnostics; services is authoritative for restore
    fallback: Vec<String>,
    services: std::collections::HashMap<String, Vec<String>>,
}

#[cfg(target_os = "macos")]
fn dns_backup_path() -> std::path::PathBuf {
    crate::db::teamx_home().join("dns-backup.json")
}

#[cfg(target_os = "macos")]
fn save_backup(b: &DnsBackup) -> Result<(), String> {
    let data = serde_json::to_vec(b).map_err(|e| format!("dns backup serialize: {e}"))?;
    std::fs::write(dns_backup_path(), data).map_err(|e| format!("dns backup write: {e}"))
}

#[cfg(target_os = "macos")]
fn load_backup() -> Option<DnsBackup> {
    let data = std::fs::read(dns_backup_path()).ok()?;
    serde_json::from_slice(&data).ok()
}

/// List enabled network services (skip header + disabled `*` entries).
#[cfg(target_os = "macos")]
fn list_services_macos() -> Result<Vec<String>, String> {
    let out = std::process::Command::new("networksetup")
        .args(["-listallnetworkservices"])
        .output()
        .map_err(|e| format!("networksetup list: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .skip(1)
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('*'))
        .collect())
}

/// Current manually-configured DNS for a service (empty = automatic/DHCP).
#[cfg(target_os = "macos")]
fn get_dns_macos(svc: &str) -> Vec<String> {
    let Ok(out) = std::process::Command::new("networksetup")
        .args(["-getdnsservers", svc])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .collect()
}

/// Set a service's DNS servers; empty list restores automatic ("Empty").
#[cfg(target_os = "macos")]
fn set_dns_macos(svc: &str, list: &[String]) -> Result<(), String> {
    let mut cmd = std::process::Command::new("networksetup");
    cmd.arg("-setdnsservers").arg(svc);
    if list.is_empty() {
        cmd.arg("Empty");
    } else {
        for d in list {
            cmd.arg(d);
        }
    }
    let st = cmd.status().map_err(|e| format!("networksetup setdns {svc}: {e}"))?;
    if !st.success() {
        return Err(format!("networksetup setdns {svc} failed (exit {:?})", st.code()));
    }
    Ok(())
}

/// All currently-active DNS servers (from `scutil --dns`), deduplicated. This
/// captures DHCP-assigned servers that `networksetup -getdnsservers` misses.
/// Loopback / unspecified / fake-ip-gateway addresses are excluded so they are
/// never picked up as fallback DNS.
#[cfg(target_os = "macos")]
fn current_dns_macos() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(o) = std::process::Command::new("scutil").args(["--dns"]).output() {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines() {
            let t = line.trim();
            if let Some(idx) = t.find("nameserver[") {
                if let Some(colon) = t[idx..].find(':') {
                    let ip = t[idx + colon + 1..].trim().to_string();
                    let skip = ip.is_empty()
                        || ip == "127.0.0.1"
                        || ip == "0.0.0.0"
                        || ip == "::1"
                        || ip.starts_with("198.18.") // tun fake-ip gateway range
                        || out.contains(&ip);
                    if !skip {
                        out.push(ip);
                    }
                }
            }
        }
    }
    out
}

/// The system's current DNS servers as IPv4 addresses (for forwarding
/// non-proxied domains from the local DNS proxy). Empty on unsupported
/// platforms or when none can be discovered.
pub fn system_dns_servers() -> Vec<Ipv4Addr> {
    #[cfg(target_os = "macos")]
    {
        current_dns_macos()
            .iter()
            .filter_map(|s| s.parse::<Ipv4Addr>().ok())
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}
