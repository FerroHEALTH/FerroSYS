// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Where the process is in its life: booting, serving or draining.
//!
//! Readiness is withdrawn in every phase but serving, so an orchestrator
//! sends no request before boot completes, and stops sending them from the
//! moment the process is asked to stop, before the listener closes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::document::Phase;

/// The phase of the process, shared by every clone.
///
/// It moves forward only: booting, then serving, then draining, and a
/// draining process never serves again.
#[derive(Debug, Clone)]
pub struct Lifecycle(Arc<AtomicU8>);

impl Default for Lifecycle {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(code(Phase::Booting))))
    }
}

impl Lifecycle {
    /// Returns the phase.
    #[must_use]
    pub fn phase(&self) -> Phase {
        match self.0.load(Ordering::Acquire) {
            0 => Phase::Booting,
            1 => Phase::Serving,
            _ => Phase::Draining,
        }
    }

    /// Marks boot complete: booting moves to serving, and any other phase
    /// stays.
    pub fn booted(&self) {
        // NOTE: no specification governs this: our own design; a failed exchange
        // means the process already left booting, which is final.
        let _moved: Result<u8, u8> = self.0.compare_exchange(
            code(Phase::Booting),
            code(Phase::Serving),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Marks the process draining, for good.
    pub fn drain(&self) {
        self.0.store(code(Phase::Draining), Ordering::Release);
    }
}

/// The stored code of `phase`.
const fn code(phase: Phase) -> u8 {
    match phase {
        Phase::Booting => 0,
        Phase::Serving => 1,
        Phase::Draining => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::Lifecycle;
    use crate::document::Phase;

    #[test]
    fn the_phase_moves_forward_and_draining_is_final() {
        let lifecycle = Lifecycle::default();
        assert_eq!(Phase::Booting, lifecycle.phase());
        let shared = lifecycle.clone();
        shared.booted();
        assert_eq!(Phase::Serving, lifecycle.phase(), "every clone moves");
        lifecycle.drain();
        assert_eq!(Phase::Draining, shared.phase());
        shared.booted();
        assert_eq!(
            Phase::Draining,
            lifecycle.phase(),
            "a draining process never serves again"
        );
    }

    #[test]
    fn a_process_can_be_asked_to_stop_before_it_booted() {
        let lifecycle = Lifecycle::default();
        lifecycle.drain();
        lifecycle.booted();
        assert_eq!(Phase::Draining, lifecycle.phase());
    }
}
