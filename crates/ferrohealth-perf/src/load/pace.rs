// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The precise wait of the open-loop schedule.
//!
//! tokio's timer rounds a deadline up to its next millisecond tick, so an
//! arrival sent after `tokio::time::sleep_until` starts up to a millisecond
//! late and that lag is charged to the server as latency. The schedule's
//! own thread waits instead: it sleeps on the operating system until a
//! margin before the deadline, then yields until the deadline passes. A
//! sleep may overshoot in proportion to its length (macOS lets a timer run
//! late by a share of its interval), so a wait longer than twice the margin
//! sleeps half the way and looks again, and only the last sleep, at most
//! one margin long, is measured. The margin follows that sleep's overshoot,
//! jumping to a larger one at once and easing towards a smaller one, so a
//! host whose sleeps run late spins longer and a quiet host spins only the
//! floor. No specification governs this: our own design (#400).

use std::time::{Duration, Instant};

/// The least margin: below it a sleep that wakes a little late would make
/// the send late.
const FLOOR: Duration = Duration::from_micros(50);

/// The largest margin, bounding the spin of a host whose sleeps run very
/// late to two milliseconds an arrival.
const CEILING: Duration = Duration::from_millis(2);

/// The margin before the first sleep has been measured.
const START: Duration = Duration::from_millis(1);

/// The share of the difference a smaller overshoot takes off the margin,
/// as a divisor: one sixty-fourth a sleep.
const EASE: u32 = 64;

/// The schedule's waiter, with the margin it has learned.
#[derive(Debug)]
pub(super) struct Pacer {
    margin: Duration,
}

impl Default for Pacer {
    fn default() -> Self {
        Self { margin: START }
    }
}

impl Pacer {
    /// Blocks the thread until `deadline` has passed; returns at once when
    /// it already has.
    pub(super) fn wait_until(&mut self, deadline: Instant) {
        loop {
            let now = Instant::now();
            let Some(ahead) = deadline
                .checked_duration_since(now)
                .and_then(|left| left.checked_sub(self.margin))
                .filter(|ahead| !ahead.is_zero())
            else {
                break;
            };
            if ahead > self.margin {
                std::thread::sleep(ahead / 2);
                continue;
            }
            std::thread::sleep(ahead);
            if let Some(wake) = now.checked_add(ahead) {
                self.learn(Instant::now().saturating_duration_since(wake));
            }
            break;
        }
        while Instant::now() < deadline {
            std::thread::yield_now();
        }
    }

    /// Takes one measured overshoot into the margin.
    fn learn(&mut self, overshoot: Duration) {
        let margin = if overshoot >= self.margin {
            overshoot
        } else {
            self.margin
                .saturating_sub(self.margin.saturating_sub(overshoot) / EASE)
        };
        self.margin = margin.clamp(FLOOR, CEILING);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_larger_overshoot_is_taken_at_once_up_to_the_ceiling() {
        let mut pacer = Pacer::default();
        pacer.learn(Duration::from_micros(1_500));
        assert_eq!(Duration::from_micros(1_500), pacer.margin);
        pacer.learn(Duration::from_millis(9));
        assert_eq!(CEILING, pacer.margin);
    }

    #[test]
    fn a_smaller_overshoot_eases_the_margin_down_to_the_floor() {
        let mut pacer = Pacer::default();
        pacer.learn(Duration::ZERO);
        assert_eq!(Duration::from_nanos(984_375), pacer.margin);
        for _ in 0..2_000 {
            pacer.learn(Duration::from_micros(10));
        }
        assert_eq!(FLOOR, pacer.margin);
    }

    #[test]
    fn the_wait_ends_at_the_deadline_and_not_before() {
        let mut pacer = Pacer::default();
        for wait in [0_u64, 300, 2_000, 5_000] {
            let deadline = Instant::now() + Duration::from_micros(wait);
            pacer.wait_until(deadline);
            let late = Instant::now().saturating_duration_since(deadline);
            assert!(Instant::now() >= deadline);
            assert!(late < Duration::from_millis(20), "{late:?} late");
        }
    }

    #[test]
    fn a_deadline_already_past_returns_at_once() {
        let mut pacer = Pacer::default();
        let past = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        let before = Instant::now();
        pacer.wait_until(past);
        assert!(before.elapsed() < Duration::from_millis(20));
        assert_eq!(START, pacer.margin, "no sleep, nothing learned");
    }
}
