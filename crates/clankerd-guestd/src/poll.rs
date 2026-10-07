//! Polling for things the kernel creates asynchronously.

use std::time::{Duration, Instant};

/// Calls `ready` every `interval` until it returns true or `limit` passed
/// (it is always called at least once). Returns whether it became ready.
pub fn poll_until(limit: Duration, interval: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_true_once_ready_and_false_at_the_limit() {
        let mut calls = 0;
        assert!(poll_until(
            Duration::from_secs(5),
            Duration::from_millis(1),
            || {
                calls += 1;
                calls == 3
            }
        ));
        assert_eq!(calls, 3);
        assert!(!poll_until(
            Duration::from_millis(10),
            Duration::from_millis(1),
            || false
        ));
    }
}
