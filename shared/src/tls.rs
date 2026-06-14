// rustls config builders shared by the load balancer and backend servers.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

/// Load all PEM certificates from `path` (supports cert chains).
pub fn load_certs(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Load a PEM private key (PKCS#8, RSA, or SEC1).
pub fn load_private_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key in file"))
}

/// TLS server without client authentication — used for the LB's public and health-ingest ports.
pub fn server_config(cert_path: &Path, key_path: &Path) -> io::Result<Arc<ServerConfig>> {
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Arc::new(cfg))
}

/// mTLS server — requires the client to present a cert trusted by `client_ca_path`.
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

/// TLS client that verifies the server against `ca_path` but presents no client cert.
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

/// mTLS client — verifies the server and presents the LB client cert to authenticate.
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
