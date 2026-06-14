// mTLS proxy: forwards client requests to a backend and returns the sanitized response.

use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConnection, Stream};
use shared::SERVER_ID_HEADER;
use shared::parser::{Request, Response, read_response, write_request};

use crate::health::Backend;

// RFC 7230 hop-by-hop headers — stripped from both request and response.
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

// Extra request-only strips: Host is rewritten; X-Server-Id must not be client-spoofable.
const REQUEST_STRIP_EXTRA: &[&str] = &["host", SERVER_ID_HEADER];

pub struct ProxyCtx {
    pub tls_client: Arc<rustls::ClientConfig>,
}

// Opens a fresh mTLS connection per call (no pooling); HEAD is rewritten to GET to avoid body-less parse issues.
pub fn forward(
    ctx: &ProxyCtx,
    backend: &Backend,
    client_req: &Request,
    client_ip: std::net::IpAddr,
) -> std::io::Result<Response> {
    let is_head = client_req.method.eq_ignore_ascii_case("HEAD");
    let upstream_req = forge_request(backend, client_req, client_ip, is_head);

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
    if is_head {
        resp.body.clear();
    }
    Ok(resp)
}

// Strips forbidden headers, rewrites Host, appends X-Forwarded-For, and converts HEAD to GET.
fn forge_request(
    backend: &Backend,
    src: &Request,
    client_ip: std::net::IpAddr,
    is_head: bool,
) -> Request {
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
        method: if is_head {
            "GET".into()
        } else {
            src.method.clone()
        },
        target: src.target.clone(),
        version: "HTTP/1.1".into(),
        headers,
        body: src.body.clone(),
    }
}

// Drops hop-by-hop headers, ensures Content-Length is set, forces Connection: close.
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
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
}
