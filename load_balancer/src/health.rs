// Backend registry, sticky-session table, and health-ingest endpoint.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shared::parser::{HealthReport, Request, Response};

// Backend is considered dead if no report arrives within this window.
pub const STALE_AFTER: Duration = Duration::from_secs(10);

// Sticky-session binding lifetime; re-routed immediately if the pinned backend goes stale.
pub const STICKY_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone)]
pub struct Backend {
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub load: f32,
    pub available: bool,
    pub in_flight: u32,
    pub last_seen: Option<Instant>,
}

impl Backend {
    // Starts as unavailable/load=1.0 so a backend that never reports is treated as down.
    pub fn new(server_id: String, host: String, port: u16) -> Self {
        Self {
            server_id,
            host,
            port,
            load: 1.0,
            available: false,
            in_flight: 0,
            last_seen: None,
        }
    }

    // True only if available=true and last report arrived within STALE_AFTER.
    pub fn is_live(&self, now: Instant) -> bool {
        if !self.available {
            return false;
        }
        match self.last_seen {
            Some(t) => now.duration_since(t) <= STALE_AFTER,
            None => false,
        }
    }
}

#[derive(Debug)]
pub struct Registry {
    backends: Mutex<HashMap<String, Backend>>,
    sticky: Mutex<HashMap<IpAddr, (String, Instant)>>,
}

impl Registry {
    // Initialize the registry with seeded backends; sticky table starts empty.
    pub fn with_seed(seed: Vec<Backend>) -> Self {
        let mut map = HashMap::new();
        for b in seed {
            map.insert(b.server_id.clone(), b);
        }
        Self {
            backends: Mutex::new(map),
            sticky: Mutex::new(HashMap::new()),
        }
    }

    // Apply an incoming report; unknown server_ids are silently dropped to prevent phantom backends.
    pub fn apply_report(&self, report: HealthReport) {
        let mut g = self.backends.lock().expect("backends lock poisoned");
        if let Some(b) = g.get_mut(&report.server_id) {
            b.load = report.load;
            b.available = report.available;
            b.in_flight = report.in_flight;
            b.last_seen = Some(Instant::now());
        }
    }

    /// Returns all backends sorted by server_id.
    pub fn snapshot(&self) -> Vec<Backend> {
        let g = self.backends.lock().expect("backends lock poisoned");
        let mut v: Vec<Backend> = g.values().cloned().collect();
        v.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        v
    }

    /// Route a client to its pinned backend, or pick a fresh one via `pick` if no live binding exists.
    pub fn route(
        &self,
        client: IpAddr,
        pick: impl Fn(&[Backend]) -> Option<&Backend>,
    ) -> Option<Backend> {
        let now = Instant::now();
        let backends_snapshot: Vec<Backend> = {
            let g = self.backends.lock().expect("backends lock poisoned");
            g.values().cloned().collect()
        };

        let sticky_choice = {
            let mut g = self.sticky.lock().expect("sticky lock poisoned");
            g.retain(|_, (_, t)| now.duration_since(*t) <= STICKY_TTL);
            g.get(&client).and_then(|(id, _)| {
                backends_snapshot
                    .iter()
                    .find(|b| &b.server_id == id && b.is_live(now))
                    .cloned()
            })
        };
        if let Some(b) = sticky_choice {
            return Some(b);
        }

        let live: Vec<Backend> = backends_snapshot
            .into_iter()
            .filter(|b| b.is_live(now))
            .collect();
        let chosen = pick(&live)?.clone();

        let mut g = self.sticky.lock().expect("sticky lock poisoned");
        g.insert(client, (chosen.server_id.clone(), now));
        Some(chosen)
    }
}

pub struct HealthCtx {
    pub registry: Arc<Registry>,
}

// Parse and apply one health report POSTed to /_health; rejects wrong method, path, or body.
pub fn handle(ctx: &HealthCtx, req: &Request) -> Response {
    if req.method != "POST" || req.target != "/_health" {
        return Response::status(404, "Not Found", "no such route");
    }
    let body_str = match std::str::from_utf8(&req.body) {
        Ok(s) => s,
        Err(_) => return Response::status(400, "Bad Request", "non-utf8 body"),
    };
    let report = match HealthReport::decode(body_str) {
        Some(r) => r,
        None => return Response::status(400, "Bad Request", "malformed report"),
    };
    ctx.registry.apply_report(report);
    Response::status(200, "OK", "ok")
}
