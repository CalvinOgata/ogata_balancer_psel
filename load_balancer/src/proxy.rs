// Forward a request to a backend over mTLS and return the response.
//
// What we do at this layer:
//   * regenerate hop-by-hop headers — the client's `Connection: keep-alive`
//     etc. don't apply between us and the backend;
//   * rewrite `Host` to the backend's hostname so it matches the cert SAN;
//   * append the client IP to `X-Forwarded-For`.
//
// Authentication is handled entirely by mTLS: the server verifies the LB's
// client certificate during the handshake. No application-layer token needed.
//
// What we *don't* do: connection pooling, streaming. Each forward opens a
// fresh TLS connection and buffers the response in full. For a learning
// project this is the right tradeoff — adding a pool would require either
// async or a small connection-manager thread, neither of which sheds light on
// load balancing.

use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConnection, Stream};
use shared::protocol::{Request, Response, read_response, write_request};
use shared::SERVER_ID_HEADER;

use crate::health::Backend;

// RFC 7230 hop-by-hop headers: stripped in *both* directions because they
// describe a single connection and don't apply to the next hop.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "proxy-authenticate",
    "proxy-authorization",
];

// Request-only strips: `Host` is rewritten to the backend hostname;
// `X-Server-Id` is LB-controlled and must not be spoofed by the client.
const REQUEST_STRIP_EXTRA: &[&str] = &["host", SERVER_ID_HEADER];

pub struct ProxyCtx {
    pub tls_client: Arc<rustls::ClientConfig>,
}

// Open a fresh mTLS connection to `backend`, write a sanitized version of
// the client request, read the full response back, sanitize it, and return
// it. One connection per upstream call — no pooling.
pub fn forward(
    ctx: &ProxyCtx,
    backend: &Backend,
    client_req: &Request,
    client_ip: std::net::IpAddr,
) -> std::io::Result<Response> {
    let upstream_req = forge_request(backend, client_req, client_ip);

    let server_name = ServerName::try_from(backend.host.clone())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut tls = ClientConnection::new(ctx.tls_client.clone(), server_name)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let addr = format!("{}:{}", backend.host, backend.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no addrs"))?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    sock.set_read_timeout(Some(Duration::from_secs(30)))?;
    sock.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut stream = Stream::new(&mut tls, &mut sock);

    write_request(&mut stream, &upstream_req)?;

    let mut reader = BufReader::new(&mut stream);
    let mut resp = read_response(&mut reader)?;
    sanitize_response(&mut resp);
    Ok(resp)
}

// Build the upstream request: strip hop-by-hop and LB-controlled headers,
// set `Host` to the backend hostname, and append the client IP to
// `X-Forwarded-For`.
fn forge_request(backend: &Backend, src: &Request, client_ip: std::net::IpAddr) -> Request {
    let mut headers: Vec<(String, String)> = src
        .headers
        .iter()
        .filter(|(k, _)| !strip_for_request(k))
        .cloned()
        .collect();

    headers.push(("Host".into(), backend.host.clone()));
    headers.push(("Connection".into(), "close".into()));

    let mut xff_set = false;
    for (k, v) in headers.iter_mut() {
        if k.eq_ignore_ascii_case("X-Forwarded-For") {
            *v = format!("{v}, {client_ip}");
            xff_set = true;
            break;
        }
    }
    if !xff_set {
        headers.push(("X-Forwarded-For".into(), client_ip.to_string()));
    }

    Request {
        method: src.method.clone(),
        target: src.target.clone(),
        version: "HTTP/1.1".into(),
        headers,
        body: src.body.clone(),
    }
}

// Clean up an upstream response before sending it to the client: drop
// hop-by-hop headers, ensure `Content-Length` is present, force
// `Connection: close`.
fn sanitize_response(resp: &mut Response) {
    resp.headers.retain(|(k, _)| !strip_for_response(k));
    let has_len = resp
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("Content-Length"));
    if !has_len {
        resp.headers
            .push(("Content-Length".into(), resp.body.len().to_string()));
    }
    resp.headers.push(("Connection".into(), "close".into()));
}

fn strip_for_request(name: &str) -> bool {
    HOP_BY_HOP
        .iter()
        .chain(REQUEST_STRIP_EXTRA.iter())
        .any(|h| h.eq_ignore_ascii_case(name))
}

fn strip_for_response(name: &str) -> bool {
    HOP_BY_HOP
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}
