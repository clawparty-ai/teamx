//! pki.rs — minimal PKI for teamx network mode (mTLS).
//!
//! Generates and stores a per-instance CA (`~/.teamx/ca/`), a server
//! certificate for `teamx serve`, and per-member client certificates bundled
//! into invitation letters. All certificates default to 3650 days (10 years).
//!
//! Layout under the CA dir:
//!   ca.crt        CA certificate (public)
//!   ca.key        CA private key (0600)
//!   server.crt    server certificate (for teamx serve)
//!   server.key    server private key (0600)

use std::fs;
use std::path::{Path, PathBuf};

use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SerialNumber,
};
use time::{Duration, OffsetDateTime};

pub type PkiResult<T> = Result<T, String>;

const CERT_DAYS: i64 = 3650;

/// Directory where this instance's PKI material lives.
pub fn ca_dir(home: &Path) -> PathBuf {
    home.join("ca")
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn read_pem(path: &Path) -> PkiResult<String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn write_pem(path: &Path, pem: &str) -> PkiResult<()> {
    // Create private-key material with mode 0600 directly — a plain
    // `fs::write` + `chmod` leaves a 0644 window on unix.
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        f.write_all(pem.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, pem).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Build CertificateParams with CN, SANs, validity and CA flag.
fn cert_params(cn: &str, sans: &[String], is_ca: bool) -> PkiResult<CertificateParams> {
    let mut params = CertificateParams::new(sans.to_vec())
        .map_err(|e| format!("cert params: {e}"))?;
    params.not_before = now() - Duration::days(1);
    params.not_after = now() + Duration::days(CERT_DAYS);
    params.is_ca = if is_ca {
        IsCa::Ca(rcgen::BasicConstraints::Unconstrained)
    } else {
        IsCa::NoCa
    };
    // A CA must carry keyCertSign/cRLSign; leaf certs sign/encipher instead.
    params.key_usages = if is_ca {
        vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign]
    } else {
        vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment]
    };
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    Ok(params)
}

/// Generate (if absent) the CA + server certificate under `home/ca`.
///
/// The CA is the long-lived trust anchor: it is only (re)generated when either
/// of its two files is missing OR the existing CA is unusable (missing the
/// `keyCertSign` key usage — a bug in CA certs generated before the key usage
/// fix, which makes every client reject the chain). A partially-deleted server
/// cert/key regenerates ONLY the server cert (signed by the existing CA), so
/// losing `server.key` does NOT rotate the CA and invalidate every already-
/// issued member cert.
pub fn ensure_pki(home: &Path) -> PkiResult<PkiPaths> {
    let dir = ca_dir(home);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let ca_cert_path = dir.join("ca.crt");
    let ca_key_path = dir.join("ca.key");
    let server_cert_path = dir.join("server.crt");
    let server_key_path = dir.join("server.key");

    // An existing CA cert from before the keyCertSign fix is unusable: strict
    // verifiers (rustls/OpenSSL/node) reject its chain. Treat it as missing so
    // the fixed `cert_params` regenerates a correct CA. Members must be
    // re-invited because their old certs are signed by the old CA.
    let ca_broken = if ca_cert_path.exists() {
        match rcgen::CertificateParams::from_ca_cert_pem(&read_pem(&ca_cert_path)?) {
            Ok(params) => {
                let has_sign = params.key_usages.iter().any(|u| matches!(u, KeyUsagePurpose::KeyCertSign));
                !has_sign
            }
            Err(_) => true,
        }
    } else {
        false
    };

    let ca_missing = !ca_cert_path.exists() || !ca_key_path.exists() || ca_broken;
    if ca_missing {
        // CA key + self-signed cert.
        let ca_key = KeyPair::generate().map_err(|e| format!("ca keygen: {e}"))?;
        let ca_params = cert_params("teamx-ca", &[], true)?;
        let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| format!("ca self-sign: {e}"))?;
        write_pem(&ca_cert_path, &ca_cert.pem())?;
        write_pem(&ca_key_path, &ca_key.serialize_pem())?;
    }

    // Server cert is cheap to regenerate and derives from the CA. Regenerate it
    // when its files are missing OR the CA was just (re)created.
    if ca_missing || !server_cert_path.exists() || !server_key_path.exists() {
        let ca_cert_pem = read_pem(&ca_cert_path)?;
        let ca_key_pem = read_pem(&ca_key_path)?;
        let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(|e| format!("load ca key: {e}"))?;
        let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)
            .map_err(|e| format!("parse ca cert: {e}"))?;
        let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| format!("reconstruct ca: {e}"))?;

        let server_key = KeyPair::generate().map_err(|e| format!("server keygen: {e}"))?;
        let mut server_params =
            cert_params("teamx-server", &["localhost".to_string(), "127.0.0.1".to_string()], false)?;
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_cert = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .map_err(|e| format!("server sign: {e}"))?;

        write_pem(&server_cert_path, &server_cert.pem())?;
        write_pem(&server_key_path, &server_key.serialize_pem())?;
    }

    Ok(PkiPaths {
        ca_cert: ca_cert_path,
        server_cert: server_cert_path,
        server_key: server_key_path,
        ca_key: ca_key_path,
    })
}

/// Issue a member client certificate signed by the instance CA.
/// The CN carries the member id (and, optionally, a user id) so the server can
/// map a verified cert to a member row and to the owning person; returns PEM
/// strings (cert, key) plus the serial, for the invitation letter and the
/// invitations ledger.
///
/// CN format: `member:<member_id>:<role>` (legacy), or
/// `member:<member_id>:<role>:<user_id>` when a user id is provided. The user
/// id is empty for token-joined members and single-device (legacy) invites.
pub fn issue_member_cert(home: &Path, member_id: &str, role: &str, user_id: Option<&str>) -> PkiResult<IssuedCert> {
    ensure_pki(home)?;
    let dir = ca_dir(home);
    let ca_cert_pem = read_pem(&dir.join("ca.crt"))?;
    let ca_key_pem = read_pem(&dir.join("ca.key"))?;

    let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(|e| format!("load ca key: {e}"))?;
    // Reconstruct the CA certificate from its PEM so it can be the issuer.
    let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)
        .map_err(|e| format!("parse ca cert: {e}"))?;
    let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| format!("reconstruct ca: {e}"))?;

    let cn = match user_id.filter(|u| !u.is_empty()) {
        Some(u) => format!("member:{member_id}:{role}:{u}"),
        None => format!("member:{member_id}:{role}"),
    };
    let mut params = cert_params(&cn, &[], false)?;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    // Assign an explicit random serial so it can be recorded for revocation.
    let serial_bytes: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
    params.serial_number = Some(SerialNumber::from_slice(&serial_bytes));

    let member_key = KeyPair::generate().map_err(|e| format!("member keygen: {e}"))?;
    let member_cert = params
        .signed_by(&member_key, &ca_cert, &ca_key)
        .map_err(|e| format!("member sign: {e}"))?;

    let cert_pem = member_cert.pem();
    let key_pem = member_key.serialize_pem();
    let serial_hex = member_cert
        .params()
        .serial_number
        .as_ref()
        .map(|s| s.to_string())
        .unwrap_or_default();

    Ok(IssuedCert {
        cert_pem,
        key_pem,
        serial_hex,
        cn,
    })
}

/// Ensure the server certificate covers `extra_sans` (e.g. the LAN IP that
/// `teamx serve` binds). Regenerates the server cert when the SAN set changes;
/// the CA is left untouched so previously-issued member certificates keep
/// verifying against the same trust anchor.
pub fn ensure_server_sans(home: &Path, extra_sans: &[String]) -> PkiResult<PkiPaths> {
    let pk = ensure_pki(home)?;
    if extra_sans.is_empty() {
        return Ok(pk);
    }

    let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    for s in extra_sans {
        if !sans.iter().any(|x| x == s) {
            sans.push(s.clone());
        }
    }

    let ca_cert_pem = read_pem(&pk.ca_cert)?;
    let ca_key_pem = read_pem(&pk.ca_key)?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(|e| format!("load ca key: {e}"))?;
    let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)
        .map_err(|e| format!("parse ca cert: {e}"))?;
    let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| format!("reconstruct ca: {e}"))?;

    let server_key = KeyPair::generate().map_err(|e| format!("server keygen: {e}"))?;
    let mut server_params = cert_params("teamx-server", &sans, false)?;
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .map_err(|e| format!("server sign: {e}"))?;

    write_pem(&pk.server_cert, &server_cert.pem())?;
    write_pem(&pk.server_key, &server_key.serialize_pem())?;
    Ok(pk)
}

/// SHA-256 fingerprint of the CA certificate (hex), for letter pinning.
pub fn ca_fingerprint(home: &Path) -> PkiResult<String> {
    let pk = ensure_pki(home)?;
    let ca_pem = read_pem(&pk.ca_cert)?;
    let der = rustls_pemfile::certs(&mut ca_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse ca cert: {e}"))?;
    let der = der
        .first()
        .ok_or_else(|| "empty ca cert".to_string())?;
    let digest = ring::digest::digest(&ring::digest::SHA256, der.as_ref());
    Ok(digest.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// Parse the member identity out of a client certificate CN
/// (`member:<id>:<role>`). A 4-part CN (`member:<id>:<role>:<user>`) still
/// yields the member id + role; the user part is ignored here (see
/// `parse_member_identity` for the user-aware variant).
pub fn parse_member_cn(cn: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = cn.splitn(4, ':').collect();
    if (parts.len() == 3 || parts.len() == 4) && parts[0] == "member" {
        Some((parts[1].to_string(), parts[2].to_string()))
    } else {
        None
    }
}

/// Parse the full member identity, including the optional user id, out of a
/// client certificate CN (`member:<id>:<role>[:<user_id>]`).
///
/// Returns `(member_id, role, user_id)` where `user_id` is `None` for legacy
/// 3-part CNs (unbound members). The user presence drives tunnel access:
/// `None` → team-scoped (legacy), `Some` → user-scoped.
pub fn parse_member_identity(cn: &str) -> Option<(String, String, Option<String>)> {
    let parts: Vec<&str> = cn.splitn(4, ':').collect();
    if (parts.len() == 3 || parts.len() == 4) && parts[0] == "member" {
        let user = (parts.len() == 4 && !parts[3].is_empty()).then(|| parts[3].to_string());
        Some((parts[1].to_string(), parts[2].to_string(), user))
    } else {
        None
    }
}

/// The set of file paths for the instance PKI.
#[derive(Debug, Clone)]
pub struct PkiPaths {
    pub ca_cert: PathBuf,
    pub ca_key: PathBuf,
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
}

/// A member certificate freshly issued by the instance CA (for invitation letters).
#[derive(Debug, Clone)]
pub struct IssuedCert {
    pub cert_pem: String,
    pub key_pem: String,
    /// Colon-separated hex serial (for the invitations ledger / revocation).
    pub serial_hex: String,
    /// Certificate subject CN (`member:<id>:<role>[:<user_id>]`).
    pub cn: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_member_cn_legacy_and_extended() {
        // legacy 3-part CN
        assert_eq!(
            parse_member_cn("member:abc123:contributor"),
            Some(("abc123".to_string(), "contributor".to_string()))
        );
        // extended 4-part CN still returns member + role (user ignored)
        assert_eq!(
            parse_member_cn("member:abc123:contributor:user-1"),
            Some(("abc123".to_string(), "contributor".to_string()))
        );
        // garbage
        assert_eq!(parse_member_cn("server:abc:role"), None);
        assert_eq!(parse_member_cn("member:abc"), None);
    }

    #[test]
    fn parse_member_identity_user_presence() {
        // legacy → user None
        assert_eq!(
            parse_member_identity("member:abc123:contributor"),
            Some(("abc123".to_string(), "contributor".to_string(), None))
        );
        // bound → user Some
        assert_eq!(
            parse_member_identity("member:abc123:contributor:user-1"),
            Some(("abc123".to_string(), "contributor".to_string(), Some("user-1".to_string())))
        );
        // empty user part treated as unbound
        assert_eq!(
            parse_member_identity("member:abc123:contributor:"),
            Some(("abc123".to_string(), "contributor".to_string(), None))
        );
        assert_eq!(parse_member_identity("member:abc"), None);
    }

    #[test]
    fn issue_member_cert_cn_shape() {
        let home = std::env::temp_dir().join(format!("teamx-pki-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();

        let with_user = issue_member_cert(&home, "m1", "contributor", Some("u1")).unwrap();
        assert_eq!(with_user.cn, "member:m1:contributor:u1");

        let no_user = issue_member_cert(&home, "m2", "contributor", None).unwrap();
        assert_eq!(no_user.cn, "member:m2:contributor");

        let empty_user = issue_member_cert(&home, "m3", "contributor", Some("")).unwrap();
        assert_eq!(empty_user.cn, "member:m3:contributor");

        let _ = std::fs::remove_dir_all(&home);
    }
}
