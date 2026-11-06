// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Quantiles read off a histogram by rank, each with its distribution-free
//! 95 % interval, and withheld below a minimum sample count.
//!
//! The point estimate of quantile `q` over `n` samples is the sample of rank
//! `⌈q·n⌉`. Its interval runs from rank `⌊nq − 1.96·√(nq(1−q))⌋` to rank
//! `⌈nq + 1.96·√(nq(1−q))⌉`, the large-sample binomial bound on the ranks
//! (Le Boudec, "Performance Evaluation of Computer and Communication
//! Systems", EPFL Press 2010, §2.2). The ranks are integer arithmetic with
//! an integer square root, so every run computes the same ranks. A quantile
//! with no sample above its rank is withheld: p50 needs 2 samples, p95 20
//! and p99 100. The method follows Veredictum's `perf_result::quantile`
//! (Apache-2.0, credited in `NOTICE`).

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

/// The quantiles a report carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QuantileName {
    /// The median.
    #[serde(rename = "p50")]
    P50,
    /// The 95th percentile.
    #[serde(rename = "p95")]
    P95,
    /// The 99th percentile.
    #[serde(rename = "p99")]
    P99,
}

impl QuantileName {
    /// Every quantile, lowest first.
    pub const ALL: [Self; 3] = [Self::P50, Self::P95, Self::P99];

    /// The quantile as a fraction in hundredths.
    #[must_use]
    pub fn hundredths(self) -> u64 {
        match self {
            Self::P50 => 50,
            Self::P95 => 95,
            Self::P99 => 99,
        }
    }

    /// The name as a report and a baseline write it.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::P50 => "p50",
            Self::P95 => "p95",
            Self::P99 => "p99",
        }
    }
}

/// One quantile of a histogram.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Quantile {
    /// Enough samples lie above the quantile to report it.
    Resolved {
        /// The point estimate, in milliseconds.
        value_ms: f64,
        /// The rank of the point estimate, from one.
        rank: u64,
        /// The lower bound of the 95 % interval, in milliseconds.
        low_ms: f64,
        /// The rank of the lower bound.
        low_rank: u64,
        /// The upper bound of the 95 % interval, in milliseconds.
        high_ms: f64,
        /// The rank of the upper bound.
        high_rank: u64,
    },
    /// Too few samples to report it.
    Withheld {
        /// The samples recorded.
        samples: u64,
        /// The samples it needs.
        needed: u64,
    },
}

impl Quantile {
    /// The point estimate in milliseconds, when resolved.
    #[must_use]
    pub fn value_ms(&self) -> Option<f64> {
        match self {
            Self::Resolved { value_ms, .. } => Some(*value_ms),
            Self::Withheld { .. } => None,
        }
    }
}

/// `value` as `f64`, exact below `2^53`, which every count and microsecond
/// value here stays far beneath.
#[must_use]
pub fn exact_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & 0xffff_ffff).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// The fewest samples quantile `hundredths` needs: one sample must lie above
/// its rank.
#[must_use]
pub fn needed(hundredths: u64) -> u64 {
    // The smallest n with n − ⌈n·h/100⌉ ≥ 1 is ⌈100 / (100 − h)⌉.
    100_u64.div_ceil(100_u64.saturating_sub(hundredths).max(1))
}

/// The rank `⌈n·h/100⌉`, at least one.
fn rank_of(n: u64, hundredths: u64) -> u64 {
    let rank = (u128::from(n) * u128::from(hundredths)).div_ceil(100);
    u64::try_from(rank).unwrap_or(n).clamp(1, n.max(1))
}

/// The interval ranks of quantile `hundredths` over `n` samples.
#[expect(
    clippy::integer_division,
    reason = "the lower rank is the floor of the scaled bound"
)]
fn interval_ranks(n: u64, hundredths: u64) -> (u64, u64) {
    const SCALE: u128 = 1_000_000;
    let (n, h) = (u128::from(n), u128::from(hundredths));
    // Everything is scaled by 100 · SCALE: the centre n·h/100 becomes
    // n·h·SCALE, and 1.96·√(n·(h/100)·(1 − h/100)) becomes
    // 196·√(n·h·(100 − h))·SCALE / 100.
    let centre = n * h * SCALE;
    let root = (n * h * (100 - h.min(100)) * SCALE * SCALE).isqrt();
    let spread = (196 * root).div_ceil(100);
    let unit = 100 * SCALE;
    let low = centre.saturating_sub(spread) / unit;
    let high = (centre + spread).div_ceil(unit);
    let clamp = |rank: u128| u64::try_from(rank.clamp(1, n.max(1))).unwrap_or(u64::MAX);
    (clamp(low), clamp(high))
}

/// The value of the sample of rank `rank`, from one, as the highest value
/// its bucket holds, the `HdrHistogram` convention.
#[must_use]
pub fn value_at_rank(histogram: &Histogram<u64>, rank: u64) -> u64 {
    let mut seen: u64 = 0;
    for step in histogram.iter_recorded() {
        seen = seen.saturating_add(step.count_since_last_iteration());
        if seen >= rank {
            return histogram.highest_equivalent(step.value_iterated_to());
        }
    }
    histogram.highest_equivalent(histogram.max())
}

fn millis(histogram: &Histogram<u64>, rank: u64) -> f64 {
    exact_f64(value_at_rank(histogram, rank)) / 1_000.0
}

/// Reads quantile `name` off `histogram`, whose values are microseconds.
#[must_use]
pub fn quantile(histogram: &Histogram<u64>, name: QuantileName) -> Quantile {
    let n = histogram.len();
    let hundredths = name.hundredths();
    let needed = needed(hundredths);
    if n < needed {
        return Quantile::Withheld { samples: n, needed };
    }
    let rank = rank_of(n, hundredths);
    let (low_rank, high_rank) = interval_ranks(n, hundredths);
    Quantile::Resolved {
        value_ms: millis(histogram, rank),
        rank,
        low_ms: millis(histogram, low_rank),
        low_rank,
        high_ms: millis(histogram, high_rank),
        high_rank,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::histogram;

    fn histogram_of(n: u64) -> Histogram<u64> {
        let mut h = histogram::empty();
        for v in 1..=n {
            h.record(v * 10).unwrap();
        }
        h
    }

    #[test]
    fn the_minimum_samples_are_2_20_and_100() {
        assert_eq!(2, needed(50));
        assert_eq!(20, needed(95));
        assert_eq!(100, needed(99));
        for h in [50, 95, 99] {
            let n = needed(h);
            assert!(n - rank_of(n, h) >= 1, "one sample lies above at {n}");
            assert!(
                n - 1 - rank_of(n - 1, h) < 1,
                "none lies above at {}",
                n - 1
            );
        }
    }

    #[test]
    fn a_p99_below_its_minimum_is_withheld() {
        assert_eq!(
            Quantile::Withheld {
                samples: 99,
                needed: 100
            },
            quantile(&histogram_of(99), QuantileName::P99)
        );
        assert!(
            quantile(&histogram_of(100), QuantileName::P99)
                .value_ms()
                .is_some()
        );
    }

    #[test]
    fn a_p99_over_5000_samples_carries_ranks_4936_to_4964() {
        let Quantile::Resolved {
            rank,
            low_rank,
            high_rank,
            value_ms,
            low_ms,
            high_ms,
        } = quantile(&histogram_of(5_000), QuantileName::P99)
        else {
            panic!("a p99 over 5,000 samples resolves");
        };
        assert_eq!((4_936, 4_950, 4_964), (low_rank, rank, high_rank));
        assert!(low_ms <= value_ms && value_ms <= high_ms, "ordered");
        assert!((value_ms - 49.5).abs() < 0.05, "{value_ms}");
    }

    #[test]
    fn a_median_interval_is_symmetric() {
        // n = 10,000, q = 0.5: 1.96 · √2500 = 98, so ranks 4902 to 5098.
        assert_eq!((4_902, 5_098), interval_ranks(10_000, 50));
    }

    #[test]
    fn an_empty_histogram_resolves_nothing() {
        for name in QuantileName::ALL {
            assert!(quantile(&histogram::empty(), name).value_ms().is_none());
        }
    }

    #[test]
    fn the_tokens_match_the_serialization() {
        for name in QuantileName::ALL {
            assert_eq!(
                format!("\"{}\"", name.token()),
                serde_json::to_string(&name).unwrap()
            );
        }
    }

    #[test]
    fn exact_f64_is_exact_across_the_word_boundary() {
        assert!(
            (exact_f64(1 << 40) - 1_099_511_627_776.0).abs() < 0.5,
            "2^40"
        );
        assert!((exact_f64(7) - 7.0).abs() < f64::EPSILON, "seven");
    }
}
