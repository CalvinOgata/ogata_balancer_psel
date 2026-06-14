// Backend health: the registry, the ingest endpoint, the sticky-session
// table.
//
// `Registry` is the source of truth for "which backends are live and how
// loaded are they?". It's read by the router every time we have to pick
// where a request should go, and written by the ingest handler every time a
// server pushes a `HealthReport`. Both critical sections are short, so
// straight `std::sync::Mutex` is fine without reaching for parking_lot or
// RwLock.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shared::parser::{HealthReport, Request, Response};

/// If we haven't heard from a server in this long, treat it as unavailable
/// regardless of what its last report said.
pub const STALE_AFTER: Duration = Duration::from_secs(10);

/// How long a sticky-session binding survives. If the chosen server goes away
/// before this expires we evict and re-route immediately; otherwise the
/// client lands on the same server for this window.
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
    // Construct a freshly-seeded backend entry. We start it as `available =
    // false`/`load = 1.0` so a backend that never reports in is treated as
    // down rather than as a perfectly idle target.
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

    // A backend is "live" iff it claimed `available = true` in its last
    // report *and* that report arrived within `STALE_AFTER`. The freshness
    // check defends against backends that crash without notifying us.
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
    // Build a registry pre-populated with the given backends (one per
    // configured host). The sticky-session table starts empty.
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

    // Update the registry entry for the server identified in `report`.
    // Reports for unknown server_ids are dropped on the floor — they would
    // otherwise let a misconfigured peer inject a phantom backend.
    pub fn apply_report(&self, report: HealthReport) {
        let mut g = self.backends.lock().expect("backends lock poisoned");
        if let Some(b) = g.get_mut(&report.server_id) {
            b.load = report.load;
            b.available = report.available;
            b.in_flight = report.in_flight;
            b.last_seen = Some(Instant::now());
        }
    }

    /// Snapshot of all backends, sorted by server_id for stable UIs.
    pub fn snapshot(&self) -> Vec<Backend> {
        let g = self.backends.lock().expect("backends lock poisoned");
        let mut v: Vec<Backend> = g.values().cloned().collect();
        v.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        v
    }

    /// Resolve which backend should handle a request from `client`.
    /// Honors any live sticky binding, otherwise delegates to `pick`.
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

        // Try existing sticky binding first.
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

        // No usable sticky entry — pick fresh among the live ones.
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

// ---------------------------------------------------------------------------
// Health ingest endpoint.
//
// Servers POST `/_health` to the LB on a dedicated TLS port (separate from
// public traffic so we can apply different policy and never confuse one for
// the other). The health-ingest port is only reachable within the Docker
// bridge network, so network isolation handles authentication here.
// ---------------------------------------------------------------------------

pub struct HealthCtx {
    pub registry: Arc<Registry>,
}

// Parse and apply one incoming health report. Rejects wrong method/path and
// malformed bodies; everything else is fed into the registry.
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
