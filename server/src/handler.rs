// Per-connection request handler.
//
// Every connection arrives over mTLS from the load balancer — the TLS
// handshake verifies the caller's identity before a single byte of HTTP is
// read. We therefore skip any application-layer auth token and go straight
// to routing. The IP allowlist in main.rs rejects non-LB connections before
// TLS even starts.
//
//   1. Parse the HTTP request from the TLS stream.
//   2. Route to the appropriate handler (random PDF or ping).
//   3. Tag the response with `X-Server-Id` so the UI knows which backend served.
//   4. Emit a structured access log line regardless of outcome.

use std::fs;
use std::io::{BufReader, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rand::seq::SliceRandom;
use rustls::{ServerConnection, Stream};
use shared::parser::{Request, Response, read_request, write_response};
use shared::SERVER_ID_HEADER;

use crate::health::{HealthState, InFlightGuard};

pub struct HandlerCtx {
    pub files_dir: PathBuf,
    pub health: Arc<HealthState>,
}

pub fn handle_connection(
    ctx: &HandlerCtx,
    tls: &mut ServerConnection,
    sock: &mut std::net::TcpStream,
    peer_ip: IpAddr,
) {
    let started = Instant::now();
    let mut stream = Stream::new(tls, sock);
    let _guard = InFlightGuard::new(ctx.health.clone());

    let mut reader = BufReader::new(&mut stream);
    let req = match read_request(&mut reader) {
        Ok(r) => r,
        Err(e) => {
            access_log(
                &ctx.health.server_id,
                peer_ip,
                "-",
                "-",
                0,
                started.elapsed().as_millis(),
            );
            eprintln!("[{}] parse error from {}: {}", ctx.health.server_id, peer_ip, e);
            return;
        }
    };
    drop(reader);

    let method = req.method.clone();
    let target = req.target.clone();
    let resp = route(ctx, &req);
    let status = resp.status;

    if let Err(e) = write_response(&mut stream, &resp) {
        eprintln!("[{}] write error to {}: {}", ctx.health.server_id, peer_ip, e);
    }

    access_log(
        &ctx.health.server_id,
        peer_ip,
        &method,
        &target,
        status,
        started.elapsed().as_millis(),
    );
}

// Emit one access log line to stderr. Format:
//   [server_id] ACCESS unix_ts peer method target → status (elapsed_ms ms)
fn access_log(
    server_id: &str,
    peer: IpAddr,
    method: &str,
    target: &str,
    status: u16,
    elapsed_ms: u128,
) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    eprintln!(
        "[{}] ACCESS t={} peer={} \"{} {}\" → {} ({}ms)",
        server_id, ts, peer, method, target, status, elapsed_ms,
    );
}

// Dispatch on (method, path). Tags every outgoing response with `X-Server-Id`
// so the LB and the HTMX UI can show which backend served the request.
fn route(ctx: &HandlerCtx, req: &Request) -> Response {
    let mut resp = match (req.method.as_str(), req.target.as_str()) {
        ("GET", "/file") => serve_random_pdf(&ctx.files_dir),
        ("HEAD", "/file") => {
            let mut r = serve_random_pdf(&ctx.files_dir);
            r.body.clear(); // headers (including Content-Length) stay intact
            r
        }
        ("GET", "/_ping") => Response::status(200, "OK", "pong"),
        _ => Response::status(404, "Not Found", "no such route"),
    };
    resp.add_header(SERVER_ID_HEADER, &ctx.health.server_id);
    resp
}

fn serve_random_pdf(files_dir: &Path) -> Response {
    let entries: Vec<std::path::PathBuf> = match fs::read_dir(files_dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|e| e.eq_ignore_ascii_case("pdf"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => return Response::status(500, "Internal Server Error", "files dir missing"),
    };
    if entries.is_empty() {
        return Response::status(503, "Service Unavailable", "no files available");
    }
    let mut rng = rand::thread_rng();
    let chosen = entries
        .choose(&mut rng)
        .expect("entries non-empty checked above");
    let body = match fs::read(chosen) {
        Ok(b) => b,
        Err(e) => {
            return Response::status(500, "Internal Server Error", &format!("read failed: {e}"));
        }
    };
    let filename = chosen
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("document.pdf");
    let mut resp = Response::ok(body, "application/pdf");
    resp.add_header(
        "Content-Disposition",
        &format!("inline; filename=\"{filename}\""),
    );
    resp
}

pub fn close_tls(tls: &mut ServerConnection, sock: &mut std::net::TcpStream) {
    tls.send_close_notify();
    let mut stream = Stream::new(tls, sock);
    let _ = stream.flush();
}
