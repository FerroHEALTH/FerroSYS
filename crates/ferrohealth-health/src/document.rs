// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The readiness document a server writes and a console reads.
//!
//! An indicator's detail is written for an operator: it names what is wrong
//! (a refused connection, a timeout, a full spool) and never carries a
//! credential, a URL with its userinfo, or a value from a request.

use serde::{Deserialize, Serialize};

/// The state of one indicator, or of the whole process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Working.
    Up,
    /// Working with a fault an operator should see, such as a backlog
    /// growing; still ready.
    Degraded,
    /// Not working; the process is not ready while one indicator is down.
    Down,
}

/// Where the process is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Starting: not ready yet.
    Booting,
    /// Serving: ready when no indicator is down.
    Serving,
    /// Asked to stop: never ready again, while the requests in flight
    /// finish.
    Draining,
}

/// The outcome of one indicator's check.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IndicatorState {
    /// The indicator's name, such as `database` or `cdr`.
    pub name: String,
    /// What the check found.
    pub state: State,
    /// Why the indicator is degraded or down, for an operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl IndicatorState {
    /// The indicator `name` is up.
    #[must_use]
    pub fn up(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: State::Up,
            detail: None,
        }
    }

    /// The indicator `name` is degraded, for `detail`.
    #[must_use]
    pub fn degraded(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: State::Degraded,
            detail: Some(detail.into()),
        }
    }

    /// The indicator `name` is down, for `detail`.
    #[must_use]
    pub fn down(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: State::Down,
            detail: Some(detail.into()),
        }
    }
}

/// The readiness document: the overall state, the phase, and every
/// indicator's state in a fixed order.
///
/// # Examples
///
/// ```
/// use ferrohealth_health::document::{IndicatorState, Phase, Readiness, State};
///
/// let ready = Readiness::new(Phase::Serving, vec![IndicatorState::up("database")]);
/// assert_eq!(State::Up, ready.state);
/// assert!(ready.is_ready());
///
/// let draining = Readiness::new(Phase::Draining, vec![IndicatorState::up("database")]);
/// assert_eq!(State::Down, draining.state);
/// assert!(!draining.is_ready());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Readiness {
    /// `up` while the process serves and every indicator is up, `degraded`
    /// while it serves and one is degraded and none is down, `down`
    /// otherwise.
    pub state: State,
    /// Where the process is in its life.
    pub phase: Phase,
    /// Every indicator's state, sorted by name.
    pub indicators: Vec<IndicatorState>,
}

impl Readiness {
    /// The document of a process in `phase` whose indicators report
    /// `indicators`.
    #[must_use]
    pub fn new(phase: Phase, indicators: Vec<IndicatorState>) -> Self {
        Self::with_optional(phase, indicators, Vec::new())
    }

    /// The document of a process in `phase` whose required indicators report
    /// `required` and whose optional ones report `optional`.
    ///
    /// An optional indicator keeps its own state in the document and counts
    /// at most as `degraded` toward the overall state, so a subsystem the
    /// process serves without (a fail-open audit sender, an event broker)
    /// never takes the process out of rotation.
    ///
    /// ```
    /// use ferrohealth_health::document::{IndicatorState, Phase, Readiness, State};
    ///
    /// let required = vec![IndicatorState::up("database")];
    /// let optional = vec![IndicatorState::down("broker", "the connection was refused")];
    /// let readiness = Readiness::with_optional(Phase::Serving, required, optional);
    /// assert_eq!(State::Degraded, readiness.state);
    /// assert!(readiness.is_ready());
    /// assert_eq!(State::Down, readiness.indicators[0].state);
    /// ```
    #[must_use]
    pub fn with_optional(
        phase: Phase,
        required: Vec<IndicatorState>,
        optional: Vec<IndicatorState>,
    ) -> Self {
        let worst = required
            .iter()
            .map(|indicator| indicator.state)
            .chain(
                optional
                    .iter()
                    .map(|indicator| indicator.state.min(State::Degraded)),
            )
            .max()
            .unwrap_or(State::Up);
        let mut indicators = required;
        indicators.extend(optional);
        indicators.sort_by(|a, b| a.name.cmp(&b.name));
        let state = if phase == Phase::Serving {
            worst
        } else {
            State::Down
        };
        Self {
            state,
            phase,
            indicators,
        }
    }

    /// Whether the process takes requests: serving, and no indicator down.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state != State::Down
    }

    /// The document with every indicator's detail removed: the names, the
    /// states and the phase stay.
    ///
    /// A detail can name a dependency, a count or a position in a log, so a
    /// route anyone can reach serves this form and the detail stays behind
    /// authentication.
    #[must_use]
    pub fn without_detail(mut self) -> Self {
        for indicator in &mut self.indicators {
            indicator.detail = None;
        }
        self
    }

    /// The status alone, as `/health` answers it.
    #[must_use]
    pub const fn summary(&self) -> Summary {
        Summary { state: self.state }
    }
}

/// The document of `/health`: the readiness state and nothing else, for a
/// caller that may not see the detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Summary {
    /// The readiness state.
    pub state: State,
}

#[cfg(test)]
mod tests {
    use super::{IndicatorState, Phase, Readiness, State, Summary};

    #[test]
    fn readiness_is_up_only_while_serving_and_every_indicator_is_up() {
        let up = vec![IndicatorState::up("database"), IndicatorState::up("cdr")];
        assert_eq!(State::Up, Readiness::new(Phase::Serving, up.clone()).state);
        assert_eq!(
            State::Down,
            Readiness::new(Phase::Booting, up.clone()).state
        );
        assert_eq!(State::Down, Readiness::new(Phase::Draining, up).state);
        assert_eq!(State::Up, Readiness::new(Phase::Serving, Vec::new()).state);
    }

    #[test]
    fn the_worst_indicator_decides_and_degraded_stays_ready() {
        let degraded = Readiness::new(
            Phase::Serving,
            vec![
                IndicatorState::up("database"),
                IndicatorState::degraded("spool", "the backlog grows"),
            ],
        );
        assert_eq!(State::Degraded, degraded.state);
        assert!(
            degraded.is_ready(),
            "a degraded process still takes requests"
        );
        let down = Readiness::new(
            Phase::Serving,
            vec![
                IndicatorState::degraded("spool", "the backlog grows"),
                IndicatorState::down("database", "refused"),
            ],
        );
        assert_eq!(State::Down, down.state);
        assert!(!down.is_ready(), "one indicator down is not ready");
    }

    #[test]
    fn an_optional_indicator_down_degrades_and_stays_ready() {
        let readiness = Readiness::with_optional(
            Phase::Serving,
            vec![IndicatorState::up("database")],
            vec![IndicatorState::down("audit", "the sender refused")],
        );
        assert_eq!(State::Degraded, readiness.state);
        assert!(
            readiness.is_ready(),
            "an optional indicator never flips readiness"
        );
        assert_eq!(
            vec![
                IndicatorState::down("audit", "the sender refused"),
                IndicatorState::up("database"),
            ],
            readiness.indicators,
            "the optional row keeps its own state, sorted with the others"
        );
        let required_down = Readiness::with_optional(
            Phase::Serving,
            vec![IndicatorState::down("database", "refused")],
            vec![IndicatorState::up("audit")],
        );
        assert_eq!(State::Down, required_down.state);
        let draining = Readiness::with_optional(
            Phase::Draining,
            Vec::new(),
            vec![IndicatorState::up("audit")],
        );
        assert_eq!(State::Down, draining.state, "the phase still decides first");
    }

    #[test]
    fn without_detail_keeps_the_names_states_and_phase() {
        let document = Readiness::new(
            Phase::Serving,
            vec![
                IndicatorState::degraded("spool", "4096 records behind"),
                IndicatorState::up("database"),
            ],
        );
        let public = document.clone().without_detail();
        assert_eq!(
            Readiness {
                state: State::Degraded,
                phase: Phase::Serving,
                indicators: vec![
                    IndicatorState::up("database"),
                    IndicatorState {
                        name: "spool".to_owned(),
                        state: State::Degraded,
                        detail: None,
                    },
                ],
            },
            public
        );
        assert_eq!(
            r#"{"state":"degraded","phase":"serving","indicators":[{"name":"database","state":"up"},{"name":"spool","state":"degraded"}]}"#,
            serde_json::to_string(&public).expect("the document serializes")
        );
    }

    #[test]
    fn the_document_sorts_its_indicators_and_round_trips_as_json() {
        let document = Readiness::new(
            Phase::Serving,
            vec![
                IndicatorState::up("terminology"),
                IndicatorState::down("cdr", "the connection was refused"),
            ],
        );
        let json = serde_json::to_string(&document).expect("the document serializes");
        assert_eq!(
            r#"{"state":"down","phase":"serving","indicators":[{"name":"cdr","state":"down","detail":"the connection was refused"},{"name":"terminology","state":"up"}]}"#,
            json
        );
        let read: Readiness = serde_json::from_str(&json).expect("the document reads");
        assert_eq!(document, read);
        assert_eq!(
            r#"{"state":"down"}"#,
            serde_json::to_string(&document.summary()).expect("the summary serializes")
        );
        assert_eq!(Summary { state: State::Down }, document.summary());
    }
}
