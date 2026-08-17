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

/// Ensure a file has restrictive permissions (0600) — best effort on unix.
fn chmod_0600(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

fn read_pem(path: &Path) -> PkiResult<String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn write_pem(path: &Path, pem: &str) -> PkiResult<()> {
    fs::write(path, pem).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    chmod_0600(path);
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
pub fn ensure_pki(home: &Path) -> PkiResult<PkiPaths> {
    let dir = ca_dir(home);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let ca_cert_path = dir.join("ca.crt");
    let ca_key_path = dir.join("ca.key");
    let server_cert_path = dir.join("server.crt");
    let server_key_path = dir.join("server.key");

    if ca_cert_path.exists() && ca_key_path.exists() && server_cert_path.exists() && server_key_path.exists() {
        return Ok(PkiPaths { ca_cert: ca_cert_path, server_cert: server_cert_path, server_key: server_key_path, ca_key: ca_key_path });
    }

    // CA key + self-signed cert.
    let ca_key = KeyPair::generate().map_err(|e| format!("ca keygen: {e}"))?;
    let ca_params = cert_params("teamx-ca", &[], true)?;
    let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| format!("ca self-sign: {e}"))?;

    write_pem(&ca_cert_path, &ca_cert.pem())?;
    write_pem(&ca_key_path, &ca_key.serialize_pem())?;

    // Server key + cert signed by CA (localhost + loopback IP).
    let server_key = KeyPair::generate().map_err(|e| format!("server keygen: {e}"))?;
    let mut server_params = cert_params("teamx-server", &["localhost".to_string(), "127.0.0.1".to_string()], false)?;
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .map_err(|e| format!("server sign: {e}"))?;

    write_pem(&server_cert_path, &server_cert.pem())?;
    write_pem(&server_key_path, &server_key.serialize_pem())?;

    Ok(PkiPaths { ca_cert: ca_cert_path, server_cert: server_cert_path, server_key: server_key_path, ca_key: ca_key_path })
}

/// Issue a member client certificate signed by the instance CA.
/// The CN carries the member id so the server can map a verified cert to a
/// member row; returns PEM strings (cert, key) plus the serial, for the
/// invitation letter and the invitations ledger.
pub fn issue_member_cert(home: &Path, member_id: &str, role: &str) -> PkiResult<IssuedCert> {
    ensure_pki(home)?;
    let dir = ca_dir(home);
    let ca_cert_pem = read_pem(&dir.join("ca.crt"))?;
    let ca_key_pem = read_pem(&dir.join("ca.key"))?;

    let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(|e| format!("load ca key: {e}"))?;
    // Reconstruct the CA certificate from its PEM so it can be the issuer.
    let ca_params = CertificateParams::from_ca_cert_pem(&ca_cert_pem)
        .map_err(|e| format!("parse ca cert: {e}"))?;
    let ca_cert = ca_params.self_signed(&ca_key).map_err(|e| format!("reconstruct ca: {e}"))?;

    let cn = format!("member:{member_id}:{role}");
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

/// Parse the member identity out of a client certificate CN (`member:<id>:<role>`).
pub fn parse_member_cn(cn: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = cn.splitn(3, ':').collect();
    if parts.len() == 3 && parts[0] == "member" {
        Some((parts[1].to_string(), parts[2].to_string()))
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
    /// Certificate subject CN (`member:<id>:<role>`).
    pub cn: String,
}
