// In-flight counter and load snapshot shared between handler threads and the health reporter.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use shared::parser::HealthReport;

// load = in_flight / MAX_IN_FLIGHT, clamped to [0.0, 1.0].
pub const MAX_IN_FLIGHT: u32 = 32;

#[derive(Debug)]
pub struct HealthState {
    pub server_id: String,
    pub in_flight: AtomicU32,
    pub available: AtomicBool,
}

impl HealthState {
    // Starts at zero in-flight and available=true.
    pub fn new(server_id: String) -> Arc<Self> {
        Arc::new(Self {
            server_id,
            in_flight: AtomicU32::new(0),
            available: AtomicBool::new(true),
        })
    }

    // Computes load as in_flight / MAX_IN_FLIGHT and returns a report ready to POST.
    pub fn snapshot(&self) -> HealthReport {
        let in_flight = self.in_flight.load(Ordering::Relaxed);
        let load = (in_flight as f32 / MAX_IN_FLIGHT as f32).min(1.0);
        HealthReport {
            server_id: self.server_id.clone(),
            load,
            available: self.available.load(Ordering::Relaxed),
            in_flight,
        }
    }
}

// RAII guard: increments in_flight on creation, decrements on drop (including panics).
pub struct InFlightGuard(Arc<HealthState>);

impl InFlightGuard {
    pub fn new(state: Arc<HealthState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(state)
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}
