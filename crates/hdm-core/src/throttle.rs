//! Bandwidth limiting, shared across every thread of every download.
//!
//! A token bucket rather than a fixed sleep per chunk: bursts are what make a
//! throttled download still feel responsive, and averaging over a window is
//! what makes the limit actually hold.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The largest burst allowed, as a multiple of the per-second rate.
///
/// One second of credit lets a stalled connection catch up immediately without
/// letting an idle download bank minutes of unlimited transfer.
const BURST_SECONDS: f64 = 1.0;

struct Bucket {
    /// Bytes per second. Zero means unlimited.
    rate: u64,
    tokens: f64,
    last_refill: Instant,
    /// Bumped whenever the rate changes or a shutdown is requested, so waiters
    /// re-evaluate instead of sleeping out a stale deadline.
    generation: u64,
    closed: bool,
}

pub struct Throttle {
    bucket: Mutex<Bucket>,
    wakeup: Condvar,
}

impl Throttle {
    /// Creates a limiter. A `rate` of zero means unlimited.
    pub fn new(rate: u64) -> Throttle {
        Throttle {
            bucket: Mutex::new(Bucket {
                rate,
                tokens: rate as f64,
                last_refill: Instant::now(),
                generation: 0,
                closed: false,
            }),
            wakeup: Condvar::new(),
        }
    }

    pub fn unlimited() -> Throttle {
        Throttle::new(0)
    }

    pub fn rate(&self) -> u64 {
        self.bucket.lock().unwrap().rate
    }

    /// Changes the limit, waking anyone currently waiting.
    ///
    /// Raising the rate must take effect at once, or a user who lifts the limit
    /// keeps waiting out a deadline computed under the old one.
    pub fn set_rate(&self, rate: u64) {
        let mut bucket = self.bucket.lock().unwrap();
        bucket.rate = rate;
        bucket.tokens = bucket.tokens.min(rate as f64 * BURST_SECONDS);
        bucket.generation += 1;
        drop(bucket);
        self.wakeup.notify_all();
    }

    /// Releases every waiter, for shutdown.
    pub fn close(&self) {
        let mut bucket = self.bucket.lock().unwrap();
        bucket.closed = true;
        bucket.generation += 1;
        drop(bucket);
        self.wakeup.notify_all();
    }

    /// Returns the number of bytes the caller may read now, up to `wanted`,
    /// blocking until at least one is available.
    ///
    /// Returning a partial allowance rather than waiting for the whole request
    /// keeps a large read from stalling behind a small budget, and keeps every
    /// connection progressing evenly when several share one limit.
    pub fn take(&self, wanted: usize) -> usize {
        if wanted == 0 {
            return 0;
        }
        let mut bucket = self.bucket.lock().unwrap();
        loop {
            if bucket.closed {
                return wanted;
            }
            if bucket.rate == 0 {
                return wanted;
            }
            refill(&mut bucket);

            if bucket.tokens >= 1.0 {
                let granted = (bucket.tokens.floor() as usize).min(wanted);
                bucket.tokens -= granted as f64;
                return granted;
            }

            // Wait just long enough for one token, but re-check on any change.
            let needed = 1.0 - bucket.tokens;
            let wait = Duration::from_secs_f64((needed / bucket.rate as f64).clamp(0.001, 0.25));
            let generation = bucket.generation;
            let (guard, _) = self.wakeup.wait_timeout(bucket, wait).unwrap();
            bucket = guard;
            if bucket.generation != generation {
                continue;
            }
        }
    }
}

fn refill(bucket: &mut Bucket) {
    let now = Instant::now();
    let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
    bucket.last_refill = now;
    let capacity = bucket.rate as f64 * BURST_SECONDS;
    bucket.tokens = (bucket.tokens + elapsed * bucket.rate as f64).min(capacity);
}

impl Default for Throttle {
    fn default() -> Self {
        Throttle::unlimited()
    }
}
