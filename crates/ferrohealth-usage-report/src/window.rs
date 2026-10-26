// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The metrics window of the daily report.
//!
//! The request log records each finished request: its method, the route
//! TEMPLATE it matched (never the path a client sent), its status and its
//! latency. The product's grouping turns the template into a request group
//! [`Key`]; a request it groups under none, such as a health probe, is not
//! counted. A product counts its own events under a counter key. Nothing of
//! a request's parameters, body or caller reaches the window, so nothing of
//! them can reach a report.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use http::{Method, StatusCode};

use crate::envelope::{
    BUCKET_BOUNDS_MS, BUCKETS, GroupStats, Key, MAX_COUNT, MAX_COUNTERS, MAX_GROUPS, MAX_MS,
    Metrics, RequestsBucket, Window24h,
};

/// The counter of client refusals every window keeps.
pub const RESPONSES_4XX: &str = "responses_4xx";

/// A product's grouping: the request group of a method and a matched route
/// template (`None` when no route matched), or `None` to leave the request
/// out.
pub type Grouping = fn(&Method, Option<&str>) -> Option<Key>;

/// The latency histogram and server errors of one group.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GroupCounts {
    /// The requests per latency bucket.
    pub buckets: [u64; BUCKETS],
    /// The `5xx` responses.
    pub errors_5xx: u64,
    /// The slowest request, in milliseconds.
    pub max_ms: f64,
}

impl GroupCounts {
    /// The requests in the group.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.buckets
            .iter()
            .fold(0_u64, |sum, count| sum.saturating_add(*count))
    }

    /// The latency at quantile `q`, interpolated inside its bucket, never
    /// above the slowest request seen, and rounded to a microsecond.
    #[must_use]
    pub fn quantile_ms(&self, q: f64) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        let rank = q.clamp(0.0, 1.0) * to_f64(total);
        let mut cumulative = 0_u64;
        for (index, count) in self.buckets.iter().copied().enumerate() {
            if count == 0 {
                continue;
            }
            let before = cumulative;
            cumulative = cumulative.saturating_add(count);
            if to_f64(cumulative) >= rank {
                let lower = lower_bound_ms(index);
                let upper = upper_bound_ms(index, self.max_ms)
                    .min(self.max_ms)
                    .max(lower);
                let fraction = ((rank - to_f64(before)) / to_f64(count)).clamp(0.0, 1.0);
                return report_ms((upper - lower).mul_add(fraction, lower));
            }
        }
        report_ms(self.max_ms)
    }

    /// The group's entry in the report, or `None` for a group with no
    /// requests.
    #[must_use]
    pub fn stats(&self) -> Option<GroupStats> {
        (self.total() > 0).then(|| GroupStats {
            p50_ms: self.quantile_ms(0.50),
            p95_ms: self.quantile_ms(0.95),
            p99_ms: self.quantile_ms(0.99),
            errors_5xx: self.errors_5xx.min(MAX_COUNT),
            histogram: self.buckets.map(|count| count.min(MAX_COUNT)),
        })
    }

    fn time(&mut self, elapsed: Duration) {
        let ms = elapsed.as_secs_f64() * 1000.0;
        if let Some(slot) = self.buckets.get_mut(bucket_of(ms)) {
            *slot = slot.saturating_add(1);
        }
        self.max_ms = self.max_ms.max(ms);
    }

    fn absorb(&mut self, other: &Self) {
        for (mine, theirs) in self.buckets.iter_mut().zip(other.buckets) {
            *mine = mine.saturating_add(theirs);
        }
        self.errors_5xx = self.errors_5xx.saturating_add(other.errors_5xx);
        self.max_ms = self.max_ms.max(other.max_ms);
    }
}

/// The counts the window holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counts {
    /// Per request group.
    pub groups: BTreeMap<Key, GroupCounts>,
    /// Per counter.
    pub counters: BTreeMap<Key, u64>,
}

impl Counts {
    /// The requests in every group.
    #[must_use]
    pub fn requests(&self) -> u64 {
        self.groups
            .values()
            .fold(0_u64, |sum, group| sum.saturating_add(group.total()))
    }

    /// The daily report's metrics: at most [`MAX_GROUPS`] groups and
    /// [`MAX_COUNTERS`] counters, in key order, every count capped.
    #[must_use]
    pub fn to_metrics(&self) -> Metrics {
        Metrics {
            window_24h: Window24h {
                requests_bucket: RequestsBucket::of(self.requests()),
                groups: self
                    .groups
                    .iter()
                    .filter_map(|(key, counts)| counts.stats().map(|stats| (key.clone(), stats)))
                    .take(MAX_GROUPS)
                    .collect(),
                counters: self
                    .counters
                    .iter()
                    .take(MAX_COUNTERS)
                    .map(|(key, count)| (key.clone(), (*count).min(MAX_COUNT)))
                    .collect(),
            },
        }
    }

    fn absorb(&mut self, other: &Self) {
        for (key, counts) in &other.groups {
            self.groups.entry(key.clone()).or_default().absorb(counts);
        }
        for (key, count) in &other.counters {
            let slot = self.counters.entry(key.clone()).or_default();
            *slot = slot.saturating_add(*count);
        }
    }
}

/// The window: the counts since the last daily report.
#[derive(Debug)]
pub struct Window {
    grouping: Grouping,
    counts: Mutex<Counts>,
}

impl Window {
    /// A window that groups requests with `grouping`.
    #[must_use]
    pub fn new(grouping: Grouping) -> Self {
        Self {
            grouping,
            counts: Mutex::new(Counts::default()),
        }
    }

    /// Records one finished request.
    pub fn record(
        &self,
        method: &Method,
        route: Option<&str>,
        status: StatusCode,
        elapsed: Duration,
    ) {
        let Some(group) = (self.grouping)(method, route) else {
            return;
        };
        let mut counts = self.lock();
        if status.is_client_error()
            && let Some(key) = Key::new(RESPONSES_4XX)
        {
            let slot = counts.counters.entry(key).or_default();
            *slot = slot.saturating_add(1);
        }
        let entry = counts.groups.entry(group).or_default();
        entry.time(elapsed);
        if status.is_server_error() {
            entry.errors_5xx = entry.errors_5xx.saturating_add(1);
        }
    }

    /// Records the latency of one unit of work outside HTTP, such as an
    /// HL7 v2 message, in `group`.
    pub fn record_work(&self, group: Key, elapsed: Duration) {
        self.lock().groups.entry(group).or_default().time(elapsed);
    }

    /// Counts one event under `counter`.
    pub fn count(&self, counter: Key) {
        let mut counts = self.lock();
        let slot = counts.counters.entry(counter).or_default();
        *slot = slot.saturating_add(1);
    }

    /// Takes the counts, leaving the window empty.
    #[must_use]
    pub fn take(&self) -> Counts {
        std::mem::take(&mut *self.lock())
    }

    /// Puts `counts` back, after a daily report that was not sent.
    pub fn restore(&self, counts: &Counts) {
        self.lock().absorb(counts);
    }

    fn lock(&self) -> MutexGuard<'_, Counts> {
        // NOTE: no specification governs this: our own design; every update leaves
        // the counts consistent, so a poisoned lock still holds them.
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn bucket_of(ms: f64) -> usize {
    BUCKET_BOUNDS_MS
        .iter()
        .position(|bound| ms <= f64::from(*bound))
        .unwrap_or(BUCKETS - 1)
}

fn lower_bound_ms(index: usize) -> f64 {
    index
        .checked_sub(1)
        .and_then(|previous| BUCKET_BOUNDS_MS.get(previous))
        .map_or(0.0, |bound| f64::from(*bound))
}

fn upper_bound_ms(index: usize, max_ms: f64) -> f64 {
    BUCKET_BOUNDS_MS
        .get(index)
        .map_or(max_ms, |bound| f64::from(*bound))
}

fn report_ms(ms: f64) -> f64 {
    (ms.clamp(0.0, MAX_MS) * 1000.0).round() / 1000.0
}

fn to_f64(count: u64) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use http::{Method, StatusCode};

    use super::{GroupCounts, RESPONSES_4XX, Window, bucket_of};
    use crate::envelope::{Key, MAX_GROUPS, RequestsBucket};

    fn grouping(method: &Method, route: Option<&str>) -> Option<Key> {
        match (method, route) {
            (_, Some(route)) if route.starts_with("/health") => None,
            (&Method::GET, Some("/fhir/{type}/{id}")) => Key::new("read"),
            _ => Key::new("other"),
        }
    }

    fn key(name: &str) -> Key {
        Key::new(name).expect("a valid key")
    }

    #[test]
    fn a_request_is_counted_in_its_group_and_a_probe_is_not() {
        let window = Window::new(grouping);
        window.record(
            &Method::GET,
            Some("/fhir/{type}/{id}"),
            StatusCode::OK,
            Duration::from_millis(7),
        );
        window.record(
            &Method::GET,
            Some("/fhir/{type}/{id}"),
            StatusCode::NOT_FOUND,
            Duration::from_millis(3),
        );
        window.record(
            &Method::POST,
            None,
            StatusCode::BAD_GATEWAY,
            Duration::from_millis(400),
        );
        window.record(
            &Method::GET,
            Some("/health/readiness"),
            StatusCode::OK,
            Duration::from_millis(1),
        );
        window.count(key("mapping_failures"));
        let counts = window.take();
        assert_eq!(3, counts.requests());
        let read = counts.groups.get(&key("read")).expect("the read group");
        assert_eq!(2, read.total());
        assert_eq!(
            1,
            counts
                .groups
                .get(&key("other"))
                .expect("the other group")
                .errors_5xx
        );
        assert_eq!(Some(&1), counts.counters.get(&key(RESPONSES_4XX)));
        assert_eq!(Some(&1), counts.counters.get(&key("mapping_failures")));
        assert_eq!(0, window.take().requests(), "take empties the window");
    }

    #[test]
    fn restore_puts_unsent_counts_back() {
        let window = Window::new(grouping);
        window.record(
            &Method::GET,
            Some("/fhir/{type}/{id}"),
            StatusCode::OK,
            Duration::from_millis(7),
        );
        let counts = window.take();
        window.record(
            &Method::GET,
            Some("/fhir/{type}/{id}"),
            StatusCode::OK,
            Duration::from_millis(7),
        );
        window.restore(&counts);
        assert_eq!(2, window.take().requests());
    }

    #[test]
    fn the_quantiles_interpolate_inside_their_bucket() {
        let mut counts = GroupCounts::default();
        for ms in [1, 2, 3, 4, 20, 40, 80, 200, 700, 3000] {
            counts.time(Duration::from_millis(ms));
        }
        let stats = counts.stats().expect("requests were timed");
        assert_eq!(10, stats.histogram.iter().sum::<u64>());
        assert!(
            stats.p50_ms > 10.0 && stats.p50_ms <= 50.0,
            "{}",
            stats.p50_ms
        );
        assert!(
            stats.p99_ms > 2500.0 && stats.p99_ms <= 3000.0,
            "{}",
            stats.p99_ms
        );
        assert!(
            GroupCounts::default().stats().is_none(),
            "an empty group reports nothing"
        );
        assert_eq!(0, bucket_of(5.0));
        assert_eq!(1, bucket_of(5.1));
        assert_eq!(10, bucket_of(90_000.0));
    }

    #[test]
    fn a_window_never_reports_more_groups_than_the_envelope_takes() {
        let window = Window::new(|_method, route| route.and_then(Key::new));
        for index in 0..40 {
            window.record(
                &Method::GET,
                Some(&format!("g{index:02}")),
                StatusCode::OK,
                Duration::from_millis(1),
            );
        }
        let metrics = window.take().to_metrics();
        assert_eq!(MAX_GROUPS, metrics.window_24h.groups.len());
        assert_eq!(RequestsBucket::Below100, metrics.window_24h.requests_bucket);
    }
}
