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
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
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
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
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
        return Ok(PkiPaths { dir, ca_cert: ca_cert_path, server_cert: server_cert_path, server_key: server_key_path });
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

    Ok(PkiPaths { dir, ca_cert: ca_cert_path, server_cert: server_cert_path, server_key: server_key_path })
}

/// Issue a member client certificate signed by the instance CA.
/// The CN carries the member id so the server can map a verified cert to a
/// member row; returns PEM strings (cert, key) for the invitation letter.
pub fn issue_member_cert(home: &Path, member_id: &str, role: &str) -> PkiResult<(String, String)> {
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

    let member_key = KeyPair::generate().map_err(|e| format!("member keygen: {e}"))?;
    let member_cert = params
        .signed_by(&member_key, &ca_cert, &ca_key)
        .map_err(|e| format!("member sign: {e}"))?;

    let cert_pem = member_cert.pem();
    let key_pem = member_key.serialize_pem();

    Ok((cert_pem, key_pem))
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
    pub dir: PathBuf,
    pub ca_cert: PathBuf,
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
}
