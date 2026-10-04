// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Per-account resource-cap predicates. Pure: given a current count and a
//! configured cap, decide whether one more is admissible; the count comes from
//! storage and the cap from boot config. Both caps share the identical rule —
//! admit strictly below the cap — so there is ONE predicate, not two synonyms.

/// Returns whether adding one more is within the cap: admits only while the
/// current count is strictly below `cap`.
///
/// A `cap` of 0 admits nothing (`0 < 0` is false), and `current == cap` is
/// refused (the count already sits at the ceiling) while `current == cap - 1`
/// is the last admissible slot.
pub fn within_cap(current: usize, cap: usize) -> bool {
    current < cap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_the_cap_is_admitted() {
        assert!(within_cap(0, 7));
        assert!(within_cap(6, 7));
    }

    #[test]
    fn at_the_cap_is_refused() {
        assert!(!within_cap(7, 7));
    }

    #[test]
    fn the_last_slot_boundary_is_admitted_and_the_ceiling_is_not() {
        // current == cap - 1 is the final admissible slot; current == cap is not.
        assert!(within_cap(4, 5), "cap-1 is the last admissible slot");
        assert!(!within_cap(5, 5), "the ceiling itself is refused");
    }

    #[test]
    fn a_cap_of_zero_admits_nothing() {
        assert!(!within_cap(0, 0));
    }

    #[test]
    fn over_the_cap_is_refused() {
        // A count already past the ceiling (e.g. after a config lowered the cap)
        // stays refused rather than wrapping.
        assert!(!within_cap(9, 5));
    }
}
