// rustls config helpers.
//
// Server roles use `server_config` (plain) or `server_config_mtls` (requires
// client cert). Client roles use `client_config` (plain, for health reports)
// or `client_config_mtls` (presents LB client cert, for backend proxy calls).
// Keeping plain and mTLS variants separate means each call site is explicit
// about what level of authentication it needs.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

/// Load PEM-encoded certs from `path`. Multiple certs in one file (a chain)
/// are returned in order.
pub fn load_certs(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Load a PEM-encoded private key (PKCS#8, RSA, or SEC1).
pub fn load_private_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key in file"))
}

/// Plain TLS server — no client certificate required. Used by the LB for its
/// public-facing ports (clients don't have our cert) and for the health-ingest
/// port (servers authenticate via the rotating token, not a client cert).
pub fn server_config(cert_path: &Path, key_path: &Path) -> io::Result<Arc<ServerConfig>> {
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Arc::new(cfg))
}

/// mTLS server — requires the connecting client to present a certificate
/// trusted by `client_ca_path`. Used by backend servers on port 4443: every
/// inbound connection must prove it is the load balancer by presenting the
/// LB's dedicated client certificate, whose private key never leaves the LB
/// container.
pub fn server_config_mtls(
    cert_path: &Path,
    key_path: &Path,
    client_ca_path: &Path,
) -> io::Result<Arc<ServerConfig>> {
    use rustls::server::WebPkiClientVerifier;

    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    let mut roots = RootCertStore::empty();
    for cert in load_certs(client_ca_path)? {
        roots
            .add(cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    }
    let client_verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let cfg = ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Arc::new(cfg))
}

/// Plain TLS client — trusts `ca_path`, presents no client certificate. Used
/// by servers when pushing health reports to the LB's health-ingest port
/// (token-based auth is sufficient there).
pub fn client_config(ca_path: &Path) -> io::Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    for cert in load_certs(ca_path)? {
        roots
            .add(cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    }
    let verifier = WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let cfg = ClientConfig::builder()
        .with_webpki_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// mTLS client — trusts `ca_path` for server verification AND presents
/// `client_cert_path` / `client_key_path` as a client certificate. Used
/// exclusively by the load balancer when dialling backend servers. The private
/// key at `client_key_path` is the LB's unique credential and must never be
/// mounted into server containers.
pub fn client_config_mtls(
    ca_path: &Path,
    client_cert_path: &Path,
    client_key_path: &Path,
) -> io::Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    for cert in load_certs(ca_path)? {
        roots
            .add(cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    }
    let verifier = WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let client_certs = load_certs(client_cert_path)?;
    let client_key = load_private_key(client_key_path)?;

    let cfg = ClientConfig::builder()
        .with_webpki_verifier(verifier)
        .with_client_auth_cert(client_certs, client_key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Arc::new(cfg))
}
