// Self-monitored health state.
//
// The handler increments `in_flight` while a request is being served. The
// reporter thread periodically reads the state and pushes a `HealthReport` to
// the load balancer. There's no central CPU/mem probe — for a learning
// project we model "load" as a saturation ratio against a fixed capacity,
// which is enough to drive the resource-aware scheduler.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use shared::parser::HealthReport;

/// Notional capacity of a single backend. With `MAX_IN_FLIGHT` concurrent
/// requests the server reports `load == 1.0`.
pub const MAX_IN_FLIGHT: u32 = 32;

#[derive(Debug)]
pub struct HealthState {
    pub server_id: String,
    pub in_flight: AtomicU32,
    pub available: AtomicBool,
}

impl HealthState {
    // Build a fresh shared `HealthState` for this server. Starts at zero
    // in-flight and `available = true`; both fields are mutated atomically
    // by handler threads and read by the health-reporter thread.
    pub fn new(server_id: String) -> Arc<Self> {
        Arc::new(Self {
            server_id,
            in_flight: AtomicU32::new(0),
            available: AtomicBool::new(true),
        })
    }

    // Capture the current state as a `HealthReport` ready to send to the LB.
    // `load` is computed live from in-flight / MAX_IN_FLIGHT and clamped to
    // [0.0, 1.0] so the scheduler always sees a normalized value.
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

/// RAII guard: increment `in_flight` on creation, decrement on drop.
pub struct InFlightGuard(Arc<HealthState>);

impl InFlightGuard {
    // Take a guard at the start of a request: bumps `in_flight` immediately
    // so the next health snapshot reflects this request as ongoing.
    pub fn new(state: Arc<HealthState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(state)
    }
}

impl Drop for InFlightGuard {
    // Decrement `in_flight` when the guard goes out of scope. Runs even on
    // panic, so a crashed handler doesn't leave a phantom request counted.
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}
