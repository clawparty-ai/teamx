//! Small shared utilities used across teamx binaries.

/// Generate a `len`-character access token from an alphabet of digits and
/// upper/lower-case letters (base62). Randomness comes from uuid v4 bytes with
/// rejection sampling so every character is (near-)uniformly likely.
pub(crate) fn gen_access_token(len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    // Largest multiple of 62 that fits in a u8: 62*4 = 248.
    const LIMIT: u8 = 248;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for byte in uuid::Uuid::new_v4().as_bytes() {
            if *byte < LIMIT {
                out.push(ALPHABET[(*byte % 62) as usize] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    out
}
