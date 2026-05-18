// Pure picker functions used by the router.
//
// Resource-based: pick the live backend with the lowest reported load, with
// in-flight count as a tiebreaker. Inputs are pre-filtered to only contain
// live (available + non-stale) backends.

use crate::health::Backend;

// Pick the candidate with the smallest reported `load`. Ties on `load` fall
// back to the smallest `in_flight`, then to `server_id` for a totally
// deterministic choice (handy for tests and reproducible behaviour at
// startup when every backend reports the same numbers).
pub fn lowest_load(candidates: &[Backend]) -> Option<&Backend> {
    candidates.iter().min_by(|a, b| {
        a.load
            .partial_cmp(&b.load)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.in_flight.cmp(&b.in_flight))
            .then(a.server_id.cmp(&b.server_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test helper: build a fully-formed Backend with the few fields the
    // scheduler actually inspects (load, in_flight, server_id) and sane
    // defaults for everything else.
    fn b(id: &str, load: f32, in_flight: u32) -> Backend {
        Backend {
            server_id: id.into(),
            host: id.into(),
            port: 4443,
            load,
            available: true,
            in_flight,
            last_seen: Some(std::time::Instant::now()),
        }
    }

    // Among a mixed set, the backend with the lowest `load` wins regardless
    // of `in_flight`.
    #[test]
    fn picks_least_loaded() {
        let cs = vec![b("a", 0.8, 5), b("b", 0.2, 1), b("c", 0.5, 3)];
        assert_eq!(lowest_load(&cs).unwrap().server_id, "b");
    }

    // When two backends report identical `load`, the one with fewer
    // in-flight requests wins — keeps balance smooth at idle.
    #[test]
    fn breaks_ties_with_in_flight() {
        let cs = vec![b("a", 0.5, 9), b("b", 0.5, 1)];
        assert_eq!(lowest_load(&cs).unwrap().server_id, "b");
    }

    // With no candidates at all we must return None — the caller turns
    // that into a 503 rather than panicking.
    #[test]
    fn empty_returns_none() {
        assert!(lowest_load(&[]).is_none());
    }
}
