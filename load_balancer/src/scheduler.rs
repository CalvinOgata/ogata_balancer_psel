// Picker functions for the router — inputs are pre-filtered to live backends only.

use crate::health::Backend;

// Returns the backend with the lowest load; ties broken by in_flight then server_id.
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

    #[test]
    fn picks_least_loaded() {
        let cs = vec![b("a", 0.8, 5), b("b", 0.2, 1), b("c", 0.5, 3)];
        assert_eq!(lowest_load(&cs).unwrap().server_id, "b");
    }

    #[test]
    fn breaks_ties_with_in_flight() {
        let cs = vec![b("a", 0.5, 9), b("b", 0.5, 1)];
        assert_eq!(lowest_load(&cs).unwrap().server_id, "b");
    }

    #[test]
    fn empty_returns_none() {
        assert!(lowest_load(&[]).is_none());
    }
}
