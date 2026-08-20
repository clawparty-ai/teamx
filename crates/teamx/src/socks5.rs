//! socks5.rs — SOCKS5 protocol parsing for the `teamx proxy start` consumer.
//!
//! member-a runs a local SOCKS5 proxy port; applications (curl, firefox)
//! configured to use it send their traffic here. We parse the SOCKS5
//! handshake (NO AUTH) and the CONNECT request to learn the target
//! host:port, then tunnel the connection to the team's proxy exit through
//! the teamx server (see tunnel_client::run_socks5_proxy).
//!
//! Only CONNECT is supported (HTTP/HTTPS/arbitrary TCP). UDP ASSOCIATE is
//! out of scope for v1. Parsing is side-effect free so it is unit-testable.

/// Target address extracted from a SOCKS5 CONNECT request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocksTarget {
    pub host: String,
    pub port: u16,
}

/// Parse a SOCKS5 greeting: `VER NMETHODS METHODS...`.
/// Returns the selected auth method (0x00 = NO AUTH) or an error.
///
/// v1 only supports NO AUTH; if the client does not offer it we refuse.
pub fn parse_greeting(buf: &[u8]) -> Result<u8, String> {
    if buf.len() < 2 {
        return Err("SOCKS5 greeting too short".to_string());
    }
    if buf[0] != 0x05 {
        return Err(format!("unsupported SOCKS version {}", buf[0]));
    }
    let nmethods = buf[1] as usize;
    if buf.len() < 2 + nmethods {
        return Err("SOCKS5 greeting methods truncated".to_string());
    }
    let methods = &buf[2..2 + nmethods];
    if methods.contains(&0x00) {
        Ok(0x00) // NO AUTH
    } else {
        Err("client offers no NO-AUTH method (auth not supported in v1)".to_string())
    }
}

/// Parse a SOCKS5 CONNECT request:
/// `VER CMD RSV ATYP ADDR PORT`.
///
/// Returns `(consumed_bytes, target)`.
/// - ATYP 0x01: IPv4 (4 bytes)
/// - ATYP 0x03: domain (1-byte length + name)
/// - ATYP 0x04: IPv6 (16 bytes)
pub fn parse_connect_request(buf: &[u8]) -> Result<(usize, SocksTarget), String> {
    if buf.len() < 4 {
        return Err("SOCKS5 request too short".to_string());
    }
    if buf[0] != 0x05 {
        return Err(format!("unsupported SOCKS version {}", buf[0]));
    }
    let cmd = buf[1];
    if cmd != 0x01 {
        return Err(format!("unsupported SOCKS5 command 0x{cmd:02x} (only CONNECT supported)"));
    }
    let atyp = buf[3];
    let (addr_len, host) = match atyp {
        0x01 => {
            if buf.len() < 4 + 4 + 2 {
                return Err("SOCKS5 IPv4 address truncated".to_string());
            }
            let ip = std::net::Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]);
            (4 + 4, ip.to_string())
        }
        0x03 => {
            if buf.len() < 5 {
                return Err("SOCKS5 domain truncated".to_string());
            }
            let n = buf[4] as usize;
            if buf.len() < 5 + n + 2 {
                return Err("SOCKS5 domain truncated".to_string());
            }
            let name = String::from_utf8_lossy(&buf[5..5 + n]).to_string();
            if name.is_empty() {
                return Err("SOCKS5 empty domain".to_string());
            }
            (4 + 1 + n, name)
        }
        0x04 => {
            if buf.len() < 4 + 16 + 2 {
                return Err("SOCKS5 IPv6 address truncated".to_string());
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            let ip = std::net::Ipv6Addr::from(octets);
            (4 + 16, ip.to_string())
        }
        _ => return Err(format!("unsupported SOCKS5 ATYP 0x{atyp:02x}")),
    };
    let port = u16::from_be_bytes([buf[addr_len], buf[addr_len + 1]]);
    let consumed = addr_len + 2;
    if buf.len() < consumed {
        return Err("SOCKS5 port truncated".to_string());
    }
    Ok((consumed, SocksTarget { host, port }))
}

/// The 10-byte SOCKS5 success reply (CONNECT granted, 0.0.0.0:0).
pub const SOCKS5_SUCCESS_REPLY: [u8; 10] = [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// The 10-byte SOCKS5 failure reply (CONNECT refused).
pub const SOCKS5_FAIL_REPLY: [u8; 10] = [0x05, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_no_auth() {
        assert_eq!(parse_greeting(&[0x05, 0x01, 0x00]).unwrap(), 0x00);
    }

    #[test]
    fn greeting_multiple_methods_picks_no_auth() {
        assert_eq!(parse_greeting(&[0x05, 0x03, 0x00, 0x01, 0x02]).unwrap(), 0x00);
    }

    #[test]
    fn greeting_requires_no_auth_offer() {
        assert!(parse_greeting(&[0x05, 0x01, 0x02]).is_err());
    }

    #[test]
    fn greeting_bad_version() {
        assert!(parse_greeting(&[0x04, 0x01, 0x00]).is_err());
    }

    #[test]
    fn greeting_truncated() {
        assert!(parse_greeting(&[0x05]).is_err());
    }

    #[test]
    fn connect_ipv4() {
        let (n, t) = parse_connect_request(&[0x05, 0x01, 0x00, 0x01, 0x7f, 0x00, 0x00, 0x01, 0x1f, 0x90]).unwrap();
        assert_eq!(n, 10);
        assert_eq!(t.host, "127.0.0.1");
        assert_eq!(t.port, 8080);
    }

    #[test]
    fn connect_domain() {
        // "example.com" = 11 chars -> length byte 0x0b
        let (n, t) = parse_connect_request(&[0x05, 0x01, 0x00, 0x03, 0x0b, b'e', b'x', b'a', b'm', b'p', b'l', b'e', b'.', b'c', b'o', b'm', 0x00, 0x50]).unwrap();
        assert_eq!(n, 4 + 1 + 11 + 2);
        assert_eq!(t.host, "example.com");
        assert_eq!(t.port, 80);
    }

    #[test]
    fn connect_ipv6() {
        let mut buf = vec![0x05, 0x01, 0x00, 0x04];
        buf.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01]);
        buf.extend_from_slice(&[0x00, 0x50]);
        let (n, t) = parse_connect_request(&buf).unwrap();
        assert_eq!(n, 4 + 16 + 2);
        assert_eq!(t.host, "2001:db8::1");
        assert_eq!(t.port, 80);
    }

    #[test]
    fn connect_rejects_bind() {
        assert!(parse_connect_request(&[0x05, 0x02, 0x00, 0x01, 0x7f, 0x00, 0x00, 0x01, 0x00, 0x50]).is_err());
    }

    #[test]
    fn connect_truncated() {
        assert!(parse_connect_request(&[0x05, 0x01, 0x00, 0x01, 0x7f]).is_err());
        assert!(parse_connect_request(&[0x05]).is_err());
    }

    #[test]
    fn connect_rejects_bad_atyp() {
        assert!(parse_connect_request(&[0x05, 0x01, 0x00, 0x05, 0x7f, 0x00, 0x00, 0x01, 0x00, 0x50]).is_err());
    }

    #[test]
    fn connect_rejects_empty_domain() {
        assert!(parse_connect_request(&[0x05, 0x01, 0x00, 0x03, 0x00, 0x00, 0x50]).is_err());
    }
}