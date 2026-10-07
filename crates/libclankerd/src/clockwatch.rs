//! Notices that the host slept, so the guest clock can be corrected.
//!
//! A suspended host stops the guest's clock with its own, and the guest has
//! no way to know. The host can: its wall clock jumps ahead of a clock that
//! does not count suspended time.

use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime};

use crate::engine::Inner;
use crate::machine::Machine;
use crate::state::Status;

/// How often the clocks are compared.
const INTERVAL: Duration = Duration::from_secs(1);
/// Wall time beyond the running clock that counts as a sleep.
const THRESHOLD: Duration = Duration::from_secs(3);

/// A reading of both clocks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sample {
    wall: SystemTime,
    /// Time since an arbitrary start that does not advance while the host sleeps.
    awake: Duration,
}

impl Sample {
    pub fn now() -> Self {
        Sample {
            wall: SystemTime::now(),
            awake: awake_clock(),
        }
    }
}

#[cfg(target_os = "macos")]
const AWAKE: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
#[cfg(not(target_os = "macos"))]
const AWAKE: libc::clockid_t = libc::CLOCK_MONOTONIC;

fn awake_clock() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec to a valid pointer.
    unsafe { libc::clock_gettime(AWAKE, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// How long the host slept between two samples, if it did.
pub(crate) fn slept(before: Sample, after: Sample) -> Option<Duration> {
    let wall = after.wall.duration_since(before.wall).ok()?;
    let awake = after.awake.saturating_sub(before.awake);
    wall.checked_sub(awake).filter(|gap| *gap >= THRESHOLD)
}

/// Resyncs the guest clock of every running machine whenever the host wakes.
/// Ends when the engine is gone.
pub(crate) fn spawn(inner: Weak<Inner>) {
    std::thread::spawn(move || {
        let mut last = Sample::now();
        loop {
            std::thread::sleep(INTERVAL);
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let now = Sample::now();
            if slept(last, now).is_some() {
                resync_all(&inner);
            }
            last = now;
        }
    });
}

fn resync_all(inner: &Arc<Inner>) {
    let Ok(ids) = inner.store.ids() else {
        return;
    };
    for id in ids {
        if inner
            .store
            .load_state(&id)
            .is_ok_and(|s| s.status == Status::Running)
        {
            // Best effort: a machine that just ended has nothing to correct.
            let _ = Machine::new(inner.clone(), id).sync_clock();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(wall_secs: u64, awake_secs: u64) -> Sample {
        Sample {
            wall: SystemTime::UNIX_EPOCH + Duration::from_secs(wall_secs),
            awake: Duration::from_secs(awake_secs),
        }
    }

    #[test]
    fn a_wall_clock_that_ran_ahead_of_the_awake_clock_means_the_host_slept() {
        // One second apart on both clocks: no sleep.
        assert_eq!(slept(at(100, 50), at(101, 51)), None);
        // Eight hours of wall time, one second awake: slept for the rest.
        assert_eq!(
            slept(at(100, 50), at(100 + 28_800, 51)),
            Some(Duration::from_secs(28_799))
        );
    }

    #[test]
    fn small_jitter_is_not_a_sleep() {
        assert_eq!(slept(at(100, 50), at(102, 50)), None);
    }

    #[test]
    fn a_wall_clock_set_backwards_is_not_a_sleep() {
        assert_eq!(slept(at(200, 50), at(100, 51)), None);
    }
}
