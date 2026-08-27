//! tls_parse.rs — minimal TLS record/handshake parsing for capture & analysis.
//!
//! Pure functions over byte slices: protocol fingerprinting, ClientHello
//! metadata (SNI + client_random), and record framing helpers used by the
//! decrypt engine. No crypto here.

/// Protocol labels stored in `flows.protocol`.
pub const P_TLS: &str = "tls";
pub const P_HTTP11: &str = "http/1.1";
pub const P_H2: &str = "h2";
#[allow(dead_code)]
pub const P_WS: &str = "ws";
pub const P_DNS: &str = "dns";
pub const P_OTHER: &str = "other";

/// Fingerprint the up-direction leading bytes of a connection.
pub fn detect_protocol(first: &[u8]) -> &'static str {
    if first.is_empty() {
        return P_OTHER;
    }
    // TLS: content_type 0x16 = handshake, then version, length, type 0x01
    if first[0] == 0x16 && first.len() > 5 && first[5] == 0x01 {
        return P_TLS;
    }
    // HTTP/1.x request line
    if starts_with_method(first) {
        return P_HTTP11;
    }
    // HTTP/2 preface
    if first.starts_with(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n") {
        return P_H2;
    }
    // WebSocket upgrade is HTTP/1.1-shaped with Upgrade header; detect later
    // during header parsing — keep ws out of the first-bytes fast path.
    // DNS over TCP: 2-byte length prefix, plausible DNS header
    if first.len() >= 12 && (first[2] & 0x80) != 0 {
        return P_DNS;
    }
    P_OTHER
}

fn starts_with_method(b: &[u8]) -> bool {
    const METHODS: [&[u8]; 8] = [b"GET ", b"POST ", b"PUT ", b"DELETE ", b"HEAD ", b"OPTIONS ", b"PATCH ", b"CONNECT "];
    for m in METHODS {
        if b.starts_with(m) {
            return true;
        }
    }
    false
}

/// Parse the first bytes of a TLS connection and extract SNI + client_random.
/// Returns `(sni, client_random)`; both `None` when the bytes are not a
/// parseable TLS ClientHello.
pub fn client_hello_meta(first: &[u8]) -> (Option<String>, Option<Vec<u8>>) {
    match parse_client_hello(first) {
        Some(h) => (h.sni, h.client_random),
        None => (None, None),
    }
}

struct ClientHelloRaw {
    sni: Option<String>,
    client_random: Option<Vec<u8>>,
}

fn parse_client_hello(buf: &[u8]) -> Option<ClientHelloRaw> {
    // TLS record header: type(1)=0x16 ver(2) len(2)
    if buf.len() < 5 || buf[0] != 0x16 {
        return None;
    }
    let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    let end = (buf.len()).min(5 + rec_len);
    let mut p = 5usize;

    // Handshake header: type(1)=0x01 len(3)
    if p + 4 > end || buf[p] != 0x01 {
        return None;
    }
    let hs_len = ((buf[p + 1] as usize) << 16) | ((buf[p + 2] as usize) << 8) | buf[p + 3] as usize;
    p += 4;
    let hs_end = (p + hs_len).min(end);

    // version(2)
    p += 2;
    // random(32)
    if p + 32 > hs_end {
        return None;
    }
    let random = buf[p..p + 32].to_vec();
    p += 32;

    // session_id (1B len + data)
    if p >= hs_end {
        return None;
    }
    let sid_len = buf[p] as usize;
    p += 1 + sid_len;

    // cipher_suites (2B len + data)
    if p + 2 > hs_end {
        return None;
    }
    let cs_len = u16::from_be_bytes([buf[p], buf[p + 1]]) as usize;
    p += 2 + cs_len;

    // compression methods (1B len + data)
    if p >= hs_end {
        return None;
    }
    let comp_len = buf[p] as usize;
    p += 1 + comp_len;

    // extensions (2B total len, then entries)
    if p + 2 > hs_end {
        return Some(ClientHelloRaw { sni: None, client_random: Some(random) });
    }
    let ext_total = u16::from_be_bytes([buf[p], buf[p + 1]]) as usize;
    p += 2;
    let ext_end = (p + ext_total).min(hs_end);

    let mut sni = None;
    while p + 4 <= ext_end {
        let etype = u16::from_be_bytes([buf[p], buf[p + 1]]);
        let elen = u16::from_be_bytes([buf[p + 2], buf[p + 3]]) as usize;
        p += 4;
        if p + elen > ext_end {
            break;
        }
        let edata = &buf[p..p + elen];
        if etype == 0x0000 && edata.len() >= 5 {
            // server_name list: list_len(2) name_type(1)=0 name_len(2) name
            let nlen = u16::from_be_bytes([edata[3], edata[4]]) as usize;
            if edata.len() >= 5 + nlen {
                if let Ok(s) = std::str::from_utf8(&edata[5..5 + nlen]) {
                    sni = Some(s.to_string());
                }
            }
        }
        p += elen;
    }

    Some(ClientHelloRaw { sni, client_random: Some(random) })
}

/// Split a byte stream into TLS records. Returns (content_type, payload)
/// tuples; trailing partial record is left in the remainder (not returned).
/// `max_records` caps the number of records returned (0 = unlimited); use
/// this to bound memory on very large streams.
pub fn split_records(stream: &[u8], max_records: usize) -> (Vec<(u8, Vec<u8>)>, usize) {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 5 <= stream.len() {
        let ct = stream[p];
        let len = u16::from_be_bytes([stream[p + 3], stream[p + 4]]) as usize;
        if ct > 23 || len > 18_000 {
            return (out, p); // not TLS or corrupt
        }
        if p + 5 + len > stream.len() {
            break; // partial record
        }
        out.push((ct, stream[p + 5..p + 5 + len].to_vec()));
        p += 5 + len;
        if max_records > 0 && out.len() >= max_records {
            break;
        }
    }
    (out, p)
}
