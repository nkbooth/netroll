// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Pure domain logic for NetRoll by N1CCK.
//!
//! This crate holds business rules only: no I/O, no async runtime.
//! All effects (time, persistence, transport) enter through port traits.

pub mod admin;
pub mod audit;
pub mod auth;
pub mod authz;
pub mod avatar;
pub mod bot_mitigation;
pub mod callsign;
pub mod caps;
pub mod check_in;
pub mod consent;
pub mod deletion;
pub mod egress;
pub mod event;
pub mod export;
pub mod fold;
pub mod lookup;
pub mod model;
pub mod net;
/// The traits the domain needs from the outside world; adapters implement them.
pub mod ports;
pub mod profile;
pub mod qrz;
pub mod session_sm;

use core::time::Duration;

use ports::Clock;

/// Returns how long a session has been running, given its start time
/// as milliseconds since the Unix epoch.
///
/// A start time in the future yields `Duration::ZERO` rather than
/// underflowing.
pub fn session_age(clock: &dyn Clock, started_at_epoch_millis: u64) -> Duration {
    let elapsed_millis = clock
        .now_epoch_millis()
        .saturating_sub(started_at_epoch_millis);
    Duration::from_millis(elapsed_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock {
        now_epoch_millis: u64,
    }

    impl Clock for FixedClock {
        fn now_epoch_millis(&self) -> u64 {
            self.now_epoch_millis
        }
    }

    #[test]
    fn session_age_is_delta_between_fixed_now_and_start() {
        let clock = FixedClock {
            now_epoch_millis: 1_000_000,
        };

        let age = session_age(&clock, 400_000);

        assert_eq!(age, Duration::from_millis(600_000));
    }

    #[test]
    fn session_age_saturates_to_zero_when_start_is_in_the_future() {
        let clock = FixedClock {
            now_epoch_millis: 400_000,
        };

        let age = session_age(&clock, 1_000_000);

        assert_eq!(age, Duration::ZERO);
    }
}
