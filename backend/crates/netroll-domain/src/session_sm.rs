// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The command-side lifecycle GUARD, the complement of the [`crate::fold`]
//! projection: it decides whether a command is legal BEFORE any event is
//! minted, never projects state, and `fold` never guards. Pure and
//! clock-injected; every illegal transition is a typed
//! [`SessionTransitionError`], never a panic or a bare bool.

use thiserror::Error;
use uuid::Uuid;

use crate::fold::{ControlState, SessionLifecycle};
use crate::ports::Clock;

/// A rejected session lifecycle command — the typed refusal the guard returns
/// instead of panicking, silently no-op'ing, or returning a bare bool. The
/// HTTP layer maps these to RFC 9457 problem+json at the HTTP boundary (out of
/// scope here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SessionTransitionError {
    /// `start` attempted while already `Live` — a running session cannot be
    /// started again.
    #[error("session is already live")]
    AlreadyLive,
    /// `start` attempted while `Closed` — a closed session is terminal and
    /// cannot be restarted.
    #[error("session is closed and cannot be started again")]
    StartAfterClose,
    /// `close` attempted while still `Scheduled` — a session that never went
    /// live cannot be closed.
    #[error("session cannot be closed before it goes live")]
    CloseBeforeLive,
    /// `close` (or a mutation on) an already-`Closed` session — terminal.
    #[error("session is already closed")]
    AlreadyClosed,
    /// A roster/frequency mutation attempted while still `Scheduled` — there is
    /// no live session to mutate yet.
    #[error("session is not yet live")]
    NotYetLive,
    /// A roster/frequency mutation attempted while the session is `Stalled`
    /// — the roster is frozen and writes are suspended until the
    /// net resumes, is claimed, or auto-closes. Maps to a 409 `session-paused`.
    #[error("session is paused (stalled): roster is frozen")]
    SessionPaused,
}

/// A rejected control-status transition — the typed refusal the
/// orthogonal control machine returns, the exact complement of
/// [`SessionTransitionError`] for the `Active`/`Stalled` axis. The HTTP layer maps these
/// to RFC 9457 problem+json at the HTTP boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ControlTransitionError {
    /// A control transition attempted on a session that never went live.
    #[error("session is not yet live")]
    NotYetLive,
    /// A control transition attempted on a closed (terminal) session.
    #[error("session is already closed")]
    AlreadyClosed,
    /// `stall` attempted on an already-`Stalled` session — the idempotency guard
    /// the presence sweep relies on (a second stall tick is a no-op refusal).
    #[error("session is already stalled")]
    AlreadyStalled,
    /// `resume`/`claim_control` attempted on a non-`Stalled` (`Active`) session —
    /// there is nothing to resume or claim. Maps to a 409 `control-not-stalled`.
    #[error("session is not stalled")]
    NotStalled,
    /// A voluntary `hand_off` attempted by an account that is NOT the current
    /// active NCS (or with no active NCS recorded / on a stalled session). Maps
    /// to a 403 — only the current active NCS may voluntarily hand off.
    #[error("caller is not the active net control station")]
    NotActiveNcs,
}

/// A legal control-status transition the guard decided: the target
/// [`ControlState`] and the clock-drawn instant to stamp on the resulting
/// control event's envelope. Mirrors [`SessionTransition`]; the
/// guard does NOT mint the event (that needs `seq` from the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlTransition {
    /// The decided target control state.
    pub to: ControlState,
    /// The transition instant, epoch millis, drawn from the injected clock.
    pub at: u64,
}

/// A legal lifecycle transition the guard decided: the target lifecycle and the
/// clock-drawn instant to stamp on the resulting event's envelope. The
/// guard does NOT mint the event (that needs `seq` from the log and the
/// frequency payload); it returns exactly the decision plus its instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTransition {
    /// The decided target lifecycle (`Live` for start, `Closed` for close).
    pub to: SessionLifecycle,
    /// The transition instant, epoch millis, drawn from the injected clock.
    pub at: u64,
}

/// Guards a `start` command: `Scheduled → Live` is legal (stamping `at` from the
/// injected clock); `Live` and `Closed` are rejected as typed errors.
///
/// The clock is read ONLY on the legal arm — an illegal start has no side
/// effect and is independent of the clock value.
pub fn start(
    current: SessionLifecycle,
    clock: &dyn Clock,
) -> Result<SessionTransition, SessionTransitionError> {
    match current {
        SessionLifecycle::Scheduled => Ok(SessionTransition {
            to: SessionLifecycle::Live,
            at: clock.now_epoch_millis(),
        }),
        SessionLifecycle::Live => Err(SessionTransitionError::AlreadyLive),
        SessionLifecycle::Closed => Err(SessionTransitionError::StartAfterClose),
    }
}

/// Guards a `close` command: `Live → Closed` is legal (stamping `at` from the
/// injected clock); `Scheduled` and `Closed` are rejected as typed errors.
///
/// The clock is read ONLY on the legal arm.
pub fn close(
    current: SessionLifecycle,
    clock: &dyn Clock,
) -> Result<SessionTransition, SessionTransitionError> {
    match current {
        SessionLifecycle::Live => Ok(SessionTransition {
            to: SessionLifecycle::Closed,
            at: clock.now_epoch_millis(),
        }),
        SessionLifecycle::Scheduled => Err(SessionTransitionError::CloseBeforeLive),
        SessionLifecycle::Closed => Err(SessionTransitionError::AlreadyClosed),
    }
}

/// The SINGLE command-side mutation-admission barrier: roster/frequency
/// mutations are admitted only while the session is `Live`. This is the
/// intended sole, physically-single place every caller routes mutation
/// commands through once they land, so "closed ⇒ roster frozen" becomes one
/// tested guarantee rather than an `if` each consumer might forget.
/// No clock — admission is a pure lifecycle predicate, producing no instant.
pub fn ensure_mutable(current: SessionLifecycle) -> Result<(), SessionTransitionError> {
    match current {
        SessionLifecycle::Live => Ok(()),
        SessionLifecycle::Scheduled => Err(SessionTransitionError::NotYetLive),
        SessionLifecycle::Closed => Err(SessionTransitionError::AlreadyClosed),
    }
}

/// The mutation-admission barrier EXTENDED for the orthogonal control axis
/// A roster/frequency mutation is admitted only while the
/// session is `Live` AND `Active`. The lifecycle refusals (`NotYetLive`/
/// `AlreadyClosed`) still dominate — a scheduled/closed session is not writable
/// regardless of control state; a `Live` but `Stalled` session refuses with
/// `SessionPaused` (the 409 `session-paused`, roster frozen). This is the
/// fast-path early-refusal complement of the atomic `WHERE lifecycle='live' AND
/// control_state='active'` guarded write (the DB is the sole TOCTOU authority);
/// no clock — admission is a pure predicate. The shipped [`ensure_mutable`] is
/// UNCHANGED — this is an additive, control-aware sibling.
pub fn ensure_writable(
    lifecycle: SessionLifecycle,
    control_state: ControlState,
) -> Result<(), SessionTransitionError> {
    ensure_mutable(lifecycle)?;
    match control_state {
        ControlState::Active => Ok(()),
        ControlState::Stalled => Err(SessionTransitionError::SessionPaused),
    }
}

/// Guards a `stall` command: legal only from a `Live` +
/// `Active` session (stamping `at` from the injected clock); an already-`Stalled`
/// session is rejected `AlreadyStalled` (the sweep's idempotency guard), a
/// non-live session with the lifecycle refusals. The clock is read ONLY on the
/// legal arm.
pub fn stall(
    lifecycle: SessionLifecycle,
    control_state: ControlState,
    clock: &dyn Clock,
) -> Result<ControlTransition, ControlTransitionError> {
    ensure_live_for_control(lifecycle)?;
    match control_state {
        ControlState::Active => Ok(ControlTransition {
            to: ControlState::Stalled,
            at: clock.now_epoch_millis(),
        }),
        ControlState::Stalled => Err(ControlTransitionError::AlreadyStalled),
    }
}

/// Guards a `resume` command: legal only from a `Live` +
/// `Stalled` session (the active NCS's presence returned); an `Active` session is
/// rejected `NotStalled`. The clock is read ONLY on the legal arm.
pub fn resume(
    lifecycle: SessionLifecycle,
    control_state: ControlState,
    clock: &dyn Clock,
) -> Result<ControlTransition, ControlTransitionError> {
    ensure_stalled_for_control(lifecycle, control_state)?;
    Ok(ControlTransition {
        to: ControlState::Active,
        at: clock.now_epoch_millis(),
    })
}

/// Guards an involuntary `claim_control` command: legal only
/// from a `Live` + `Stalled` session (a claimer takes over an abandoned net); an
/// `Active` session is rejected `NotStalled` (the 409 `control-not-stalled`). The
/// caller's `ClaimControl` capability is enforced at the handler; this guard owns
/// the `stalled` precondition. The clock is read ONLY on the legal arm.
pub fn claim_control(
    lifecycle: SessionLifecycle,
    control_state: ControlState,
    clock: &dyn Clock,
) -> Result<ControlTransition, ControlTransitionError> {
    ensure_stalled_for_control(lifecycle, control_state)?;
    Ok(ControlTransition {
        to: ControlState::Active,
        at: clock.now_epoch_millis(),
    })
}

/// Guards a voluntary `hand_off` command: legal only from a
/// `Live` + `Active` session where `caller` IS the current `active_ncs` (the
/// stream continues uninterrupted, so control stays `Active`). Any other caller,
/// a stalled session, or no recorded active NCS → `NotActiveNcs` (the 403). The
/// target's NetControl-tier qualification is enforced at the handler; this guard
/// owns the active-NCS/healthy-session precondition. The clock is read ONLY on
/// the legal arm.
pub fn hand_off(
    lifecycle: SessionLifecycle,
    control_state: ControlState,
    active_ncs: Option<Uuid>,
    caller: Uuid,
    clock: &dyn Clock,
) -> Result<ControlTransition, ControlTransitionError> {
    ensure_live_for_control(lifecycle)?;
    // Voluntary handoff is the HEALTHY-session path: it requires an Active
    // session whose active NCS is the caller. A stalled session is the claim
    // path, not the handoff path.
    if control_state != ControlState::Active || active_ncs != Some(caller) {
        return Err(ControlTransitionError::NotActiveNcs);
    }
    Ok(ControlTransition {
        to: ControlState::Active,
        at: clock.now_epoch_millis(),
    })
}

/// Shared lifecycle precondition for the control guards: a control transition
/// needs a `Live` session. Maps the lifecycle refusals to their control-axis
/// twins so each guard reads uniformly.
fn ensure_live_for_control(lifecycle: SessionLifecycle) -> Result<(), ControlTransitionError> {
    match lifecycle {
        SessionLifecycle::Live => Ok(()),
        SessionLifecycle::Scheduled => Err(ControlTransitionError::NotYetLive),
        SessionLifecycle::Closed => Err(ControlTransitionError::AlreadyClosed),
    }
}

/// Shared precondition for `resume`/`claim_control`: a `Live` + `Stalled` session.
fn ensure_stalled_for_control(
    lifecycle: SessionLifecycle,
    control_state: ControlState,
) -> Result<(), ControlTransitionError> {
    ensure_live_for_control(lifecycle)?;
    match control_state {
        ControlState::Stalled => Ok(()),
        ControlState::Active => Err(ControlTransitionError::NotStalled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test clock — mirrors the injected-`now` pattern in
    /// `lib.rs::tests::FixedClock` / `deletion.rs`. No `sleep`, no real clock.
    struct FixedClock {
        now_epoch_millis: u64,
    }

    impl crate::ports::Clock for FixedClock {
        fn now_epoch_millis(&self) -> u64 {
            self.now_epoch_millis
        }
    }

    #[test]
    fn start_from_scheduled_goes_live_stamped_from_the_injected_clock() {
        let clock = FixedClock {
            now_epoch_millis: 1_000,
        };
        let transition =
            start(SessionLifecycle::Scheduled, &clock).expect("scheduled -> live is a legal start");
        assert_eq!(
            transition,
            SessionTransition {
                to: SessionLifecycle::Live,
                at: 1_000,
            }
        );
    }

    #[test]
    fn start_while_live_is_rejected_as_already_live() {
        let clock = FixedClock {
            now_epoch_millis: 1_000,
        };
        assert_eq!(
            start(SessionLifecycle::Live, &clock),
            Err(SessionTransitionError::AlreadyLive)
        );
    }

    #[test]
    fn start_after_close_is_rejected_terminal() {
        let clock = FixedClock {
            now_epoch_millis: 1_000,
        };
        assert_eq!(
            start(SessionLifecycle::Closed, &clock),
            Err(SessionTransitionError::StartAfterClose)
        );
    }

    #[test]
    fn close_from_live_goes_closed_stamped_from_the_injected_clock() {
        let clock = FixedClock {
            now_epoch_millis: 5_000,
        };
        let transition =
            close(SessionLifecycle::Live, &clock).expect("live -> closed is a legal close");
        assert_eq!(
            transition,
            SessionTransition {
                to: SessionLifecycle::Closed,
                at: 5_000,
            }
        );
    }

    #[test]
    fn close_before_live_is_rejected() {
        let clock = FixedClock {
            now_epoch_millis: 5_000,
        };
        assert_eq!(
            close(SessionLifecycle::Scheduled, &clock),
            Err(SessionTransitionError::CloseBeforeLive)
        );
    }

    #[test]
    fn close_while_closed_is_rejected_as_already_closed() {
        let clock = FixedClock {
            now_epoch_millis: 5_000,
        };
        assert_eq!(
            close(SessionLifecycle::Closed, &clock),
            Err(SessionTransitionError::AlreadyClosed)
        );
    }

    #[test]
    fn ensure_mutable_admits_a_live_session() {
        assert_eq!(ensure_mutable(SessionLifecycle::Live), Ok(()));
    }

    #[test]
    fn ensure_mutable_rejects_a_not_yet_live_session() {
        assert_eq!(
            ensure_mutable(SessionLifecycle::Scheduled),
            Err(SessionTransitionError::NotYetLive)
        );
    }

    /// `fold` has zero
    /// defense against a `checkin.added` arriving after `session.closed`, so
    /// the roster would grow forever. `ensure_mutable` is the SINGLE
    /// command-side barrier function that will freeze a closed session's
    /// roster before any mutation event is minted, once the callers each
    /// route their mutation commands through it.
    #[test]
    fn mutation_on_a_closed_session_is_rejected_at_the_command_layer() {
        assert_eq!(
            ensure_mutable(SessionLifecycle::Closed),
            Err(SessionTransitionError::AlreadyClosed)
        );
    }

    // --- Orthogonal control-status guards -------------------------

    use crate::fold::ControlState;
    use uuid::Uuid;

    #[test]
    fn stall_is_legal_only_from_live_and_active() {
        let clock = FixedClock {
            now_epoch_millis: 90_000,
        };
        assert_eq!(
            stall(SessionLifecycle::Live, ControlState::Active, &clock),
            Ok(ControlTransition {
                to: ControlState::Stalled,
                at: 90_000,
            })
        );
        // Already stalled → typed refusal (idempotent guard for the sweep).
        assert_eq!(
            stall(SessionLifecycle::Live, ControlState::Stalled, &clock),
            Err(ControlTransitionError::AlreadyStalled)
        );
        // Not live → the lifecycle refusals.
        assert_eq!(
            stall(SessionLifecycle::Scheduled, ControlState::Active, &clock),
            Err(ControlTransitionError::NotYetLive)
        );
        assert_eq!(
            stall(SessionLifecycle::Closed, ControlState::Active, &clock),
            Err(ControlTransitionError::AlreadyClosed)
        );
    }

    #[test]
    fn resume_is_legal_only_from_live_and_stalled() {
        let clock = FixedClock {
            now_epoch_millis: 120_000,
        };
        assert_eq!(
            resume(SessionLifecycle::Live, ControlState::Stalled, &clock),
            Ok(ControlTransition {
                to: ControlState::Active,
                at: 120_000,
            })
        );
        // Not stalled (already active) → NotStalled.
        assert_eq!(
            resume(SessionLifecycle::Live, ControlState::Active, &clock),
            Err(ControlTransitionError::NotStalled)
        );
        assert_eq!(
            resume(SessionLifecycle::Closed, ControlState::Stalled, &clock),
            Err(ControlTransitionError::AlreadyClosed)
        );
    }

    #[test]
    fn claim_control_is_legal_only_from_live_and_stalled() {
        let clock = FixedClock {
            now_epoch_millis: 100_000,
        };
        assert_eq!(
            claim_control(SessionLifecycle::Live, ControlState::Stalled, &clock),
            Ok(ControlTransition {
                to: ControlState::Active,
                at: 100_000,
            })
        );
        // A claim on a NON-stalled session → NotStalled (maps to 409
        // control-not-stalled).
        assert_eq!(
            claim_control(SessionLifecycle::Live, ControlState::Active, &clock),
            Err(ControlTransitionError::NotStalled)
        );
        assert_eq!(
            claim_control(SessionLifecycle::Scheduled, ControlState::Stalled, &clock),
            Err(ControlTransitionError::NotYetLive)
        );
    }

    #[test]
    fn hand_off_is_legal_only_from_live_active_by_the_active_ncs() {
        let clock = FixedClock {
            now_epoch_millis: 2_000,
        };
        let ncs = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        // The active NCS hands off a healthy (Active) live session → legal,
        // stays Active (voluntary path — stream uninterrupted).
        assert_eq!(
            hand_off(
                SessionLifecycle::Live,
                ControlState::Active,
                Some(ncs),
                ncs,
                &clock
            ),
            Ok(ControlTransition {
                to: ControlState::Active,
                at: 2_000,
            })
        );
        // A non-active-NCS caller → NotActiveNcs (maps to 403).
        assert_eq!(
            hand_off(
                SessionLifecycle::Live,
                ControlState::Active,
                Some(ncs),
                other,
                &clock
            ),
            Err(ControlTransitionError::NotActiveNcs)
        );
        // A voluntary handoff on a STALLED session is not the voluntary path —
        // the active NCS's presence is gone; that's the claim path.
        assert_eq!(
            hand_off(
                SessionLifecycle::Live,
                ControlState::Stalled,
                Some(ncs),
                ncs,
                &clock
            ),
            Err(ControlTransitionError::NotActiveNcs)
        );
        // No active NCS recorded (legacy/None) → nobody can voluntarily hand off.
        assert_eq!(
            hand_off(
                SessionLifecycle::Live,
                ControlState::Active,
                None,
                ncs,
                &clock
            ),
            Err(ControlTransitionError::NotActiveNcs)
        );
    }

    #[test]
    fn ensure_writable_refuses_a_stalled_session_but_admits_an_active_live_one() {
        assert_eq!(
            ensure_writable(SessionLifecycle::Live, ControlState::Active),
            Ok(())
        );
        // Frozen-while-stalled: roster/frequency mutations are refused.
        assert_eq!(
            ensure_writable(SessionLifecycle::Live, ControlState::Stalled),
            Err(SessionTransitionError::SessionPaused)
        );
        // The lifecycle refusals still dominate (a scheduled/closed session is
        // not writable regardless of control state).
        assert_eq!(
            ensure_writable(SessionLifecycle::Scheduled, ControlState::Active),
            Err(SessionTransitionError::NotYetLive)
        );
        assert_eq!(
            ensure_writable(SessionLifecycle::Closed, ControlState::Stalled),
            Err(SessionTransitionError::AlreadyClosed)
        );
    }

    #[test]
    fn an_illegal_control_transition_is_independent_of_the_clock_value() {
        let early = FixedClock {
            now_epoch_millis: 1,
        };
        let late = FixedClock {
            now_epoch_millis: u64::MAX,
        };
        assert_eq!(
            stall(SessionLifecycle::Closed, ControlState::Active, &early),
            stall(SessionLifecycle::Closed, ControlState::Active, &late)
        );
    }

    #[test]
    fn an_illegal_transition_is_independent_of_the_clock_value() {
        // Legality short-circuits before the clock is consulted, so the
        // rejection is identical regardless of what the clock would report.
        let early = FixedClock {
            now_epoch_millis: 1,
        };
        let late = FixedClock {
            now_epoch_millis: u64::MAX,
        };
        assert_eq!(
            start(SessionLifecycle::Live, &early),
            start(SessionLifecycle::Live, &late)
        );
    }
}
