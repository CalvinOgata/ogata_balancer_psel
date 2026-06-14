// Request dispatcher: static assets, /api/servers status fragment, or backend proxy.

use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shared::parser::{Request, Response};

use crate::health::Registry;
use crate::proxy::{ProxyCtx, forward};
use crate::scheduler::lowest_load;

pub struct RouterCtx {
    pub registry: Arc<Registry>,
    pub proxy: ProxyCtx,
    pub frontend_dir: PathBuf,
}

// Matches (method, path) and dispatches to the appropriate handler.
pub fn route(ctx: &RouterCtx, client_ip: IpAddr, req: &Request) -> Response {
    match (req.method.as_str(), req.target.as_str()) {
        ("GET", "/") => serve_static(
            &ctx.frontend_dir.join("index.html"),
            "text/html; charset=utf-8",
        ),
        ("GET", path) if path.starts_with("/assets/") => serve_asset(&ctx.frontend_dir, path),
        ("GET", "/api/servers") => render_servers_html(&ctx.registry),
        _ => proxy_to_backend(ctx, client_ip, req),
    }
}

// Routes to a sticky-pinned or freshly picked backend; returns 503/502 on failure.
fn proxy_to_backend(ctx: &RouterCtx, client_ip: IpAddr, req: &Request) -> Response {
    let Some(backend) = ctx.registry.route(client_ip, lowest_load) else {
        return Response::status(503, "Service Unavailable", "no backend currently available");
    };
    match forward(&ctx.proxy, &backend, req, client_ip) {
        Ok(resp) => resp,
        Err(e) => Response::status(
            502,
            "Bad Gateway",
            &format!("backend {} failed: {}", backend.server_id, e),
        ),
    }
}

// Read a file from disk and serve it with the given content type.
fn serve_static(path: &Path, content_type: &str) -> Response {
    match fs::read(path) {
        Ok(body) => Response::ok(body, content_type),
        Err(_) => Response::status(404, "Not Found", "asset missing"),
    }
}

// Serves a file from /assets/ with a canonicalize-based path-traversal guard.
fn serve_asset(frontend_dir: &Path, target: &str) -> Response {
    let rel = target.trim_start_matches('/');
    let candidate = frontend_dir.join(rel);
    let canonical_root = match fs::canonicalize(frontend_dir) {
        Ok(p) => p,
        Err(_) => return Response::status(500, "Internal Server Error", "frontend dir gone"),
    };
    let canonical = match fs::canonicalize(&candidate) {
        Ok(p) => p,
        Err(_) => return Response::status(404, "Not Found", "asset missing"),
    };
    if !canonical.starts_with(&canonical_root) {
        return Response::status(403, "Forbidden", "path traversal");
    }
    let content_type = guess_content_type(&canonical);
    let mut f = match fs::File::open(&canonical) {
        Ok(f) => f,
        Err(_) => return Response::status(404, "Not Found", "asset missing"),
    };
    let mut body = Vec::new();
    if f.read_to_end(&mut body).is_err() {
        return Response::status(500, "Internal Server Error", "read failed");
    }
    Response::ok(body, content_type)
}

// Extension-to-MIME mapping for shipped asset types; falls back to octet-stream.
fn guess_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

// Renders the backend table as an HTMX-swappable HTML fragment.
fn render_servers_html(registry: &Registry) -> Response {
    let mut html = String::new();
    html.push_str("<table class=\"servers\">");
    html.push_str("<thead><tr><th>id</th><th>state</th><th>load</th><th>in-flight</th><th>last seen</th></tr></thead>");
    html.push_str("<tbody>");
    let now = std::time::Instant::now();
    for b in registry.snapshot() {
        let state = if b.is_live(now) {
            "<span class=\"ok\">live</span>"
        } else if b.available {
            "<span class=\"warn\">stale</span>"
        } else {
            "<span class=\"bad\">down</span>"
        };
        let last_seen = match b.last_seen {
            Some(t) => format!("{}s ago", now.duration_since(t).as_secs()),
            None => "never".into(),
        };
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:.0}%</td><td>{}</td><td>{}</td></tr>",
            html_escape(&b.server_id),
            state,
            b.load * 100.0,
            b.in_flight,
            html_escape(&last_seen),
        ));
    }
    html.push_str("</tbody></table>");
    Response::ok(html.into_bytes(), "text/html; charset=utf-8")
}

// Escapes &, <, >, " to prevent markup injection in the status fragment.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
