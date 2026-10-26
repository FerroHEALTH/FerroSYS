// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The indicators readiness runs, and the registry that runs them.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::document::{IndicatorState, Readiness, State};
use crate::lifecycle::Lifecycle;

/// How long one check may take before it counts as down, unless the
/// registry is given another bound.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// The future one check returns.
pub type Check<'a> = Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;

/// What one check found: the state and, when it is not up, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    state: State,
    detail: Option<String>,
}

impl Outcome {
    /// Working.
    #[must_use]
    pub const fn up() -> Self {
        Self {
            state: State::Up,
            detail: None,
        }
    }

    /// Working with a fault an operator should see, for `detail`.
    #[must_use]
    pub fn degraded(detail: impl Into<String>) -> Self {
        Self {
            state: State::Degraded,
            detail: Some(detail.into()),
        }
    }

    /// Not working, for `detail`.
    #[must_use]
    pub fn down(detail: impl Into<String>) -> Self {
        Self {
            state: State::Down,
            detail: Some(detail.into()),
        }
    }

    /// The indicator state of the indicator `name`.
    fn named(self, name: &str) -> IndicatorState {
        IndicatorState {
            name: name.to_owned(),
            state: self.state,
            detail: self.detail,
        }
    }
}

/// A named, bounded check of one subsystem a product needs to serve: its
/// database, an upstream, a spool's headroom, a listener.
///
/// A check names what is wrong and never carries a credential or a value
/// from a request in its detail.
pub trait HealthIndicator: fmt::Debug + Send + Sync + 'static {
    /// The indicator's name in the readiness document.
    fn name(&self) -> &'static str;

    /// Checks the subsystem.
    fn check(&self) -> Check<'_>;
}

/// The indicators readiness runs.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use ferrohealth_health::document::{Phase, State};
/// use ferrohealth_health::lifecycle::Lifecycle;
/// use ferrohealth_health::registry::{Check, HealthIndicator, Outcome, Registry};
///
/// #[derive(Debug)]
/// struct Database;
///
/// impl HealthIndicator for Database {
///     fn name(&self) -> &'static str {
///         "database"
///     }
///
///     fn check(&self) -> Check<'_> {
///         Box::pin(async { Outcome::up() })
///     }
/// }
///
/// # tokio::runtime::Builder::new_current_thread().enable_time().build()?.block_on(async {
/// let lifecycle = Lifecycle::default();
/// let registry = Registry::new(vec![Arc::new(Database)]);
/// assert_eq!(State::Down, registry.readiness(&lifecycle).await.state, "booting is not ready");
/// lifecycle.booted();
/// let readiness = registry.readiness(&lifecycle).await;
/// assert_eq!((Phase::Serving, State::Up), (readiness.phase, readiness.state));
/// # });
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Registry {
    indicators: Arc<[Arc<dyn HealthIndicator>]>,
    timeout: Duration,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Registry {
    /// A registry of `indicators`, each bounded by [`CHECK_TIMEOUT`].
    #[must_use]
    pub fn new(indicators: Vec<Arc<dyn HealthIndicator>>) -> Self {
        Self {
            indicators: indicators.into(),
            timeout: CHECK_TIMEOUT,
        }
    }

    /// Adds `indicator`.
    #[must_use]
    pub fn and(self, indicator: Arc<dyn HealthIndicator>) -> Self {
        let mut indicators = self.indicators.to_vec();
        indicators.push(indicator);
        Self {
            indicators: indicators.into(),
            timeout: self.timeout,
        }
    }

    /// Bounds every check by `timeout` instead.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns the indicators' names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self
            .indicators
            .iter()
            .map(|indicator| indicator.name())
            .collect();
        names.sort_unstable();
        names
    }

    /// Runs every indicator at once, each bounded by the timeout, and
    /// returns the readiness document of the process in `lifecycle`'s phase.
    ///
    /// A check that does not answer inside the timeout counts as down, and so
    /// does one that panics; the others are unaffected. This runs inside a
    /// Tokio runtime with its timer enabled.
    pub async fn readiness(&self, lifecycle: &Lifecycle) -> Readiness {
        let mut checks = tokio::task::JoinSet::new();
        for indicator in self.indicators.iter() {
            let indicator = Arc::clone(indicator);
            let timeout = self.timeout;
            checks.spawn(async move {
                let outcome = match tokio::time::timeout(timeout, indicator.check()).await {
                    Ok(outcome) => outcome,
                    Err(_elapsed) => Outcome::down(format!(
                        "the check did not answer inside {} ms",
                        timeout.as_millis()
                    )),
                };
                outcome.named(indicator.name())
            });
        }
        let mut states = Vec::with_capacity(self.indicators.len());
        let mut lost = self.names();
        while let Some(joined) = checks.join_next().await {
            // NOTE: no specification governs this: our own design; a check that
            // panicked or was cancelled is reported down under its name below.
            if let Ok(state) = joined {
                if let Some(at) = lost.iter().position(|name| *name == state.name) {
                    lost.remove(at);
                }
                states.push(state);
            }
        }
        states.extend(
            lost.into_iter()
                .map(|name| IndicatorState::down(name, "the check failed to complete")),
        );
        Readiness::new(lifecycle.phase(), states)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Check, HealthIndicator, Outcome, Registry};
    use crate::document::{IndicatorState, Phase, State};
    use crate::lifecycle::Lifecycle;

    #[derive(Debug)]
    struct Fixed(&'static str, Outcome);

    impl HealthIndicator for Fixed {
        fn name(&self) -> &'static str {
            self.0
        }

        fn check(&self) -> Check<'_> {
            Box::pin(std::future::ready(self.1.clone()))
        }
    }

    #[derive(Debug)]
    struct Slow(&'static str, Duration);

    impl HealthIndicator for Slow {
        fn name(&self) -> &'static str {
            self.0
        }

        fn check(&self) -> Check<'_> {
            Box::pin(async move {
                tokio::time::sleep(self.1).await;
                Outcome::up()
            })
        }
    }

    #[derive(Debug)]
    struct Panics;

    impl HealthIndicator for Panics {
        fn name(&self) -> &'static str {
            "panics"
        }

        fn check(&self) -> Check<'_> {
            Box::pin(async { panic!("the check fails loudly") })
        }
    }

    fn serving() -> Lifecycle {
        let lifecycle = Lifecycle::default();
        lifecycle.booted();
        lifecycle
    }

    #[tokio::test]
    async fn every_indicator_reports_under_its_name_in_order() {
        let registry = Registry::new(vec![Arc::new(Fixed("terminology", Outcome::up()))]).and(
            Arc::new(Fixed("cdr", Outcome::down("the connection was refused"))),
        );
        assert_eq!(vec!["cdr", "terminology"], registry.names());
        let readiness = registry.readiness(&serving()).await;
        assert_eq!(State::Down, readiness.state);
        assert_eq!(
            vec![
                IndicatorState::down("cdr", "the connection was refused"),
                IndicatorState::up("terminology"),
            ],
            readiness.indicators
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_checks_run_at_once_and_a_slow_one_times_out() {
        let registry = Registry::new(vec![
            Arc::new(Slow("one", Duration::from_millis(900))),
            Arc::new(Slow("two", Duration::from_millis(900))),
            Arc::new(Slow("wedged", Duration::from_secs(60))),
        ])
        .with_timeout(Duration::from_secs(1));
        let started = tokio::time::Instant::now();
        let readiness = registry.readiness(&serving()).await;
        assert!(
            started.elapsed() < Duration::from_millis(1100),
            "the checks ran concurrently"
        );
        assert_eq!(State::Down, readiness.state);
        let wedged = readiness
            .indicators
            .iter()
            .find(|state| state.name == "wedged")
            .expect("the wedged check");
        assert_eq!(
            Some("the check did not answer inside 1000 ms"),
            wedged.detail.as_deref()
        );
        assert!(
            readiness
                .indicators
                .iter()
                .filter(|state| state.state == State::Up)
                .count()
                == 2,
            "the two quick checks are up"
        );
    }

    #[tokio::test]
    async fn a_check_that_panics_is_down_and_the_others_still_report() {
        let registry = Registry::new(vec![
            Arc::new(Panics),
            Arc::new(Fixed("database", Outcome::up())),
        ]);
        let readiness = registry.readiness(&serving()).await;
        assert_eq!(
            vec![
                IndicatorState::up("database"),
                IndicatorState::down("panics", "the check failed to complete"),
            ],
            readiness.indicators
        );
    }

    #[tokio::test]
    async fn readiness_follows_the_lifecycle_and_degraded_stays_ready() {
        let lifecycle = Lifecycle::default();
        let registry = Registry::new(vec![Arc::new(Fixed(
            "spool",
            Outcome::degraded("the backlog grows"),
        ))]);
        assert_eq!(Phase::Booting, registry.readiness(&lifecycle).await.phase);
        lifecycle.booted();
        let readiness = registry.readiness(&lifecycle).await;
        assert_eq!(State::Degraded, readiness.state);
        assert!(readiness.is_ready(), "degraded takes requests");
        lifecycle.drain();
        assert!(
            !registry.readiness(&lifecycle).await.is_ready(),
            "draining never does"
        );
        assert_eq!(
            State::Up,
            Registry::default().readiness(&serving()).await.state
        );
    }
}
