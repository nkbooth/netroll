// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Object-level authorization: capability-based, server-enforced and PURE.
//!
//! The owner and capability sets come from the adapter; the DECISION is made
//! here, with no client-trusted ids and no scattered `if role == X`. Keep every
//! addition a pure function over a set, never a hardcoded role string.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use uuid::Uuid;

use crate::check_in::CheckInSource;

/// A per-net role in the strict containment hierarchy `Owner ⊃ NetControl ⊃
/// Logger ⊃ Relay ⊃ Participant`. Because the chain is a strict
/// *containment*, a higher role holds every capability of every lower one —
/// authorization is a single [`rank`] comparison, never a per-role capability
/// table. Wire form is lowercase-kebab: `owner`, `net-control`,
/// `logger`, `relay`, `participant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// The net's definition owner — the apex; holds every capability. Derived
    /// from the definition owner set, never stored as a grant.
    Owner,
    /// Net Control Station (NCS): may run the session and manage lower roles.
    NetControl,
    /// May log check-ins on behalf of stations.
    Logger,
    /// May log check-ins (a relay of off-air traffic); the lowest staff tier.
    Relay,
    /// An authenticated, ungranted account: the server-side floor that
    /// holds no staff capability.
    Participant,
}

/// A server-enforced capability. Additive-only: a new one is introduced by the
/// surface that needs it, never by repurposing an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Read the staff session console (summary + events + WS).
    ViewConsole,
    /// Run the session: change frequency, close it.
    RunSession,
    /// Add a check-in to the roster.
    LogCheckIn,
    /// Grant/revoke per-net roles.
    ManageRoles,
    /// Set staff-only check-in fields — the signal report.
    /// The first capability at the `Logger` threshold: a `Relay` may log a
    /// callsign-only check-in (holds `LogCheckIn`) but may NOT set a report.
    EditStaffFields,
    /// Edit or remove an already-logged check-in: the PUT edit, the DELETE
    /// remove, and the lock acquire. At the `Logger` threshold, so a `Relay` is
    /// add-only. Distinct from `EditStaffFields` (set-a-field at ADD time) even
    /// though they share that floor today, so a later divergence is one line.
    EditCheckIn,
    /// Reorder the SHARED roster by precedence. At the `NetControl` threshold,
    /// HIGHER than `EditCheckIn`'s Logger floor: reordering the shared view for
    /// every viewer is a bigger act than editing one entry. Kept distinct from
    /// `RunSession` so a later divergence is one line.
    ReorderRoster,
    /// Designate the single station currently being worked. At the `NetControl`
    /// threshold, kept DISTINCT from its neighbours so a later divergence is
    /// one line.
    SetWorkedStation,
    /// Set the session's standing ROSTER ORDERING MODE, the worked-sink toggle.
    /// At the `NetControl` threshold, kept DISTINCT from its neighbours so a
    /// later divergence is one line. No role gained or lost anything by it.
    SetRosterOrderMode,
    /// Set the net-level session note. At the `Logger` threshold: whoever
    /// conducts rounds captures the net-level notes.
    AnnotateSession,
    /// Check ONESELF in, and toggle or check out one's OWN entry. At the
    /// `Participant` (rank 0) threshold, so every authenticated role holds it.
    /// This does NOT loosen the no-staff-capability floor: it is a distinct,
    /// self-scoped axis. Holding it is NECESSARY but not SUFFICIENT — the self
    /// path also requires a verified email and a claimed callsign to add, and
    /// object-level ownership via [`owns_check_in`] to edit or check out, so a
    /// rank check alone cannot express a self-action over one's own entry.
    SelfCheckIn,
    /// Claim control of a STALLED session: the involuntary rescue path. At the
    /// `Logger` threshold, DELIBERATELY LOWER than `RunSession`'s NetControl
    /// floor, so a Logger may rescue a net whose NCS dropped. Holding it is
    /// NECESSARY but not SUFFICIENT — the endpoint also requires the session to
    /// actually be stalled, so this never lets a Logger seize a healthy net.
    ClaimControl,
    /// Discipline a disruptive station: remove it, and optionally block its
    /// account from re-checking-in for the rest of the session. At the
    /// `NetControl` threshold. DISTINCT from [`Capability::EditCheckIn`], the
    /// Logger-floor logging CORRECTION: moderation is a disciplinary act at a
    /// materially higher bar, gating a separate endpoint. Both may emit
    /// `checkin.removed`; they are never merged.
    Moderate,
    /// Export the session as a CSV/ADIF download. At the `NetControl` threshold:
    /// the export surfaces the full roster plus the entering operator's
    /// callsign, which is redacted from every public surface. Kept DISTINCT from
    /// its neighbours so a later divergence is one line.
    ExportSession,
}

/// The numeric rank of a role in the containment chain: `Owner` = 4 down to
/// `Participant` = 0. Higher strictly contains lower, so every authorization
/// decision reduces to a comparison of ranks.
pub fn rank(role: Role) -> u8 {
    match role {
        Role::Owner => 4,
        Role::NetControl => 3,
        Role::Logger => 2,
        Role::Relay => 1,
        Role::Participant => 0,
    }
}

/// The minimum role rank that holds `capability`.
fn min_rank(capability: Capability) -> u8 {
    match capability {
        // SelfCheckIn is the one capability at the Participant floor — every
        // authenticated role holds it.
        Capability::SelfCheckIn => rank(Role::Participant),
        Capability::ViewConsole | Capability::LogCheckIn => rank(Role::Relay),
        Capability::EditStaffFields
        | Capability::EditCheckIn
        | Capability::AnnotateSession
        | Capability::ClaimControl => rank(Role::Logger),
        Capability::RunSession
        | Capability::ManageRoles
        | Capability::ReorderRoster
        | Capability::SetWorkedStation
        | Capability::SetRosterOrderMode
        | Capability::Moderate
        | Capability::ExportSession => rank(Role::NetControl),
    }
}

/// Whether `role` holds `capability`: true iff its rank meets the capability's
/// threshold. The containment hierarchy as ONE comparison, so no `if role == X`
/// is ever scattered across a call site.
pub fn role_has(role: Role, capability: Capability) -> bool {
    rank(role) >= min_rank(capability)
}

/// Whether `actor` may grant or revoke the `target` role: true iff `actor`
/// outranks `target` STRICTLY. Owner is never manageable through the session
/// role surface, because no role outranks it.
pub fn can_manage_role(actor: Role, target: Role) -> bool {
    rank(actor) > rank(target)
}

impl Role {
    /// The lowercase-kebab wire/storage form. The single source of
    /// truth the adapter persists and [`FromStr`] parses back.
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::NetControl => "net-control",
            Role::Logger => "logger",
            Role::Relay => "relay",
            Role::Participant => "participant",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The longest offending string [`RoleParseError`] echoes back verbatim. Every
/// real role token is under 16 characters, so this is headroom rather than a
/// taxonomy assumption. Longer input is truncated before it reaches the 400, so
/// an arbitrarily large client-supplied `role` cannot be reflected back at full
/// size in the problem+json `detail`.
const MAX_ECHOED_ROLE_LEN: usize = 64;

/// The offending string when a wire value does not name a known [`Role`];
/// carried into the 400 `/errors/role-invalid` detail at the API boundary.
/// Truncated to [`MAX_ECHOED_ROLE_LEN`] by [`FromStr::from_str`] before
/// construction — never store an untruncated client string here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleParseError(pub String);

impl fmt::Display for RoleParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a role (expected one of: owner, net-control, logger, relay, participant)",
            self.0
        )
    }
}

impl std::error::Error for RoleParseError {}

impl FromStr for Role {
    type Err = RoleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owner" => Ok(Role::Owner),
            "net-control" => Ok(Role::NetControl),
            "logger" => Ok(Role::Logger),
            "relay" => Ok(Role::Relay),
            "participant" => Ok(Role::Participant),
            other => {
                let truncated: String = other.chars().take(MAX_ECHOED_ROLE_LEN).collect();
                Err(RoleParseError(truncated))
            }
        }
    }
}

/// Whether `acting_account_id` may manage (edit/delete) a net definition
/// owned by `owner_account_ids`.
///
/// `true` iff the acting account is a member of the owner set. An empty owner
/// set denies everyone: a zero-owner net, which the ownership lifecycle can
/// transiently produce, is manageable by no one until it is archived.
pub fn can_manage_definition(owner_account_ids: &[Uuid], acting_account_id: Uuid) -> bool {
    owner_account_ids.contains(&acting_account_id)
}

/// Whether `acting` may edit or check out the roster entry described by
/// `entry_source`/`entry_added_by`: the object gate that scopes a Participant's
/// self-edit and self-checkout to their OWN entry.
///
/// `true` iff the entry is self-sourced AND was added by the acting account.
/// [`Capability::SelfCheckIn`] is the rank-tier half of the same decision; THIS
/// predicate is what stops a participant touching a staff-entered entry or
/// another participant's. Pure over its inputs: the caller folds the roster and
/// hands the entry's `source`/`added_by` here, never a client-trusted id.
pub fn owns_check_in(
    entry_source: CheckInSource,
    entry_added_by: Option<Uuid>,
    acting: Uuid,
) -> bool {
    entry_source == CheckInSource::SelfService && entry_added_by == Some(acting)
}

/// Whether removing `removing` from `owner_account_ids` would leave the net
/// with zero owners.
///
/// `true` iff `removing` is currently the SOLE member of the owner set; `false`
/// when other owners remain, and `false` when `removing` is not a member at all,
/// because then there is nothing to orphan.
///
/// `NetDefinitionRepo::remove_owner` must call this INSIDE its row-locked
/// transaction, not beforehand: a caller-side check leaves a window where two
/// concurrent removals of different owners of the same two-owner net each pass
/// a stale guard and jointly orphan it. The only remaining zero-owner path is
/// the account-finalize cascade.
pub fn would_orphan(owner_account_ids: &[Uuid], removing: Uuid) -> bool {
    owner_account_ids.len() == 1 && owner_account_ids[0] == removing
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn removing_the_sole_owner_would_orphan_the_net() {
        let sole = Uuid::now_v7();
        assert!(would_orphan(&[sole], sole));
    }

    #[test]
    fn removing_one_of_several_owners_does_not_orphan() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        // Removing self when a co-owner remains.
        assert!(!would_orphan(&[a, b], a));
        // Removing a co-owner while ≥1 owner remains.
        assert!(!would_orphan(&[a, b], b));
    }

    #[test]
    fn removing_a_non_member_never_orphans() {
        let owner = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        // Nothing to orphan — the target isn't even in the set.
        assert!(!would_orphan(&[owner], stranger));
    }

    #[test]
    fn removing_from_an_empty_set_never_orphans() {
        let anyone = Uuid::now_v7();
        assert!(!would_orphan(&[], anyone));
    }

    #[test]
    fn an_owner_may_manage_the_definition() {
        let owner = Uuid::now_v7();
        let other = Uuid::now_v7();
        assert!(can_manage_definition(&[other, owner], owner));
        assert!(can_manage_definition(&[owner], owner));
    }

    #[test]
    fn a_non_owner_may_not_manage_the_definition() {
        let owner = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        assert!(!can_manage_definition(&[owner], stranger));
    }

    #[test]
    fn an_empty_owner_set_denies_everyone() {
        let anyone = Uuid::now_v7();
        assert!(!can_manage_definition(&[], anyone));
    }

    const EVERY_ROLE: [Role; 5] = [
        Role::Owner,
        Role::NetControl,
        Role::Logger,
        Role::Relay,
        Role::Participant,
    ];

    const EVERY_CAPABILITY: [Capability; 14] = [
        Capability::ViewConsole,
        Capability::RunSession,
        Capability::LogCheckIn,
        Capability::ManageRoles,
        Capability::EditStaffFields,
        Capability::EditCheckIn,
        Capability::ReorderRoster,
        Capability::SetWorkedStation,
        Capability::SetRosterOrderMode,
        Capability::AnnotateSession,
        Capability::SelfCheckIn,
        Capability::ClaimControl,
        Capability::Moderate,
        Capability::ExportSession,
    ];

    /// The staff capabilities a Participant must NOT hold: every capability
    /// except the one self-scoped one.
    const STAFF_CAPABILITIES: [Capability; 13] = [
        Capability::ViewConsole,
        Capability::RunSession,
        Capability::LogCheckIn,
        Capability::ManageRoles,
        Capability::EditStaffFields,
        Capability::EditCheckIn,
        Capability::ReorderRoster,
        Capability::SetWorkedStation,
        Capability::SetRosterOrderMode,
        Capability::AnnotateSession,
        Capability::ClaimControl,
        Capability::Moderate,
        Capability::ExportSession,
    ];

    #[test]
    fn rank_expresses_the_strict_containment_chain() {
        // Owner ⊃ NetControl ⊃ Logger ⊃ Relay ⊃ Participant.
        assert!(rank(Role::Owner) > rank(Role::NetControl));
        assert!(rank(Role::NetControl) > rank(Role::Logger));
        assert!(rank(Role::Logger) > rank(Role::Relay));
        assert!(rank(Role::Relay) > rank(Role::Participant));
        assert_eq!(rank(Role::Owner), 4);
        assert_eq!(rank(Role::Participant), 0);
    }

    #[test]
    fn view_console_and_log_check_in_need_at_least_relay() {
        for cap in [Capability::ViewConsole, Capability::LogCheckIn] {
            assert!(role_has(Role::Owner, cap));
            assert!(role_has(Role::NetControl, cap));
            assert!(role_has(Role::Logger, cap));
            assert!(role_has(Role::Relay, cap));
            assert!(!role_has(Role::Participant, cap));
        }
    }

    #[test]
    fn edit_staff_fields_needs_at_least_logger() {
        // The staff-only signal report is the first Logger-threshold
        // capability. A Relay holds LogCheckIn but NOT EditStaffFields.
        assert!(role_has(Role::Owner, Capability::EditStaffFields));
        assert!(role_has(Role::NetControl, Capability::EditStaffFields));
        assert!(role_has(Role::Logger, Capability::EditStaffFields));
        assert!(!role_has(Role::Relay, Capability::EditStaffFields));
        assert!(!role_has(Role::Participant, Capability::EditStaffFields));
    }

    #[test]
    fn edit_check_in_needs_at_least_logger() {
        // Editing/removing an existing check-in is Logger+ — the
        // same floor as EditStaffFields. A Relay is add-only.
        assert!(role_has(Role::Owner, Capability::EditCheckIn));
        assert!(role_has(Role::NetControl, Capability::EditCheckIn));
        assert!(role_has(Role::Logger, Capability::EditCheckIn));
        assert!(!role_has(Role::Relay, Capability::EditCheckIn));
        assert!(!role_has(Role::Participant, Capability::EditCheckIn));
    }

    #[test]
    fn reorder_roster_needs_at_least_net_control() {
        // Reordering the SHARED roll-call for everyone is a
        // run-the-session act at the NetControl threshold — higher than
        // EditCheckIn's Logger floor. Owner/NCS hold it; Logger/Relay/Participant
        // do not.
        assert!(role_has(Role::Owner, Capability::ReorderRoster));
        assert!(role_has(Role::NetControl, Capability::ReorderRoster));
        assert!(!role_has(Role::Logger, Capability::ReorderRoster));
        assert!(!role_has(Role::Relay, Capability::ReorderRoster));
        assert!(!role_has(Role::Participant, Capability::ReorderRoster));
    }

    #[test]
    fn set_worked_station_needs_at_least_net_control() {
        // Designating the worked station is an NCS act at the
        // NetControl threshold (same floor as ReorderRoster). Owner/NCS hold it;
        // Logger/Relay/Participant do not.
        assert!(role_has(Role::Owner, Capability::SetWorkedStation));
        assert!(role_has(Role::NetControl, Capability::SetWorkedStation));
        assert!(!role_has(Role::Logger, Capability::SetWorkedStation));
        assert!(!role_has(Role::Relay, Capability::SetWorkedStation));
        assert!(!role_has(Role::Participant, Capability::SetWorkedStation));
    }

    #[test]
    fn set_roster_order_mode_needs_at_least_net_control() {
        // Toggling the SHARED roster ordering mode changes what every
        // viewer sees, so it sits at the NetControl threshold alongside
        // ReorderRoster/SetWorkedStation. Owner/NCS hold it; Logger/Relay/
        // Participant do not.
        assert!(role_has(Role::Owner, Capability::SetRosterOrderMode));
        assert!(role_has(Role::NetControl, Capability::SetRosterOrderMode));
        assert!(!role_has(Role::Logger, Capability::SetRosterOrderMode));
        assert!(!role_has(Role::Relay, Capability::SetRosterOrderMode));
        assert!(!role_has(Role::Participant, Capability::SetRosterOrderMode));
    }

    #[test]
    fn annotate_session_needs_at_least_logger() {
        // Setting the net-level note is a Logger+ act (same
        // floor as EditCheckIn). Owner/NCS/Logger hold it; Relay/Participant do not.
        assert!(role_has(Role::Owner, Capability::AnnotateSession));
        assert!(role_has(Role::NetControl, Capability::AnnotateSession));
        assert!(role_has(Role::Logger, Capability::AnnotateSession));
        assert!(!role_has(Role::Relay, Capability::AnnotateSession));
        assert!(!role_has(Role::Participant, Capability::AnnotateSession));
    }

    #[test]
    fn claim_control_needs_at_least_logger() {
        // Claiming control of a STALLED net is the rescue
        // capability at the deliberately-LOWER Logger floor (below RunSession's
        // NetControl floor) — a Logger may rescue a stalled net even though only
        // NetControl+ normally runs one. Owner/NCS/Logger hold it; Relay/
        // Participant do not. The capability is necessary but NOT sufficient: the
        // claim-control endpoint additionally requires the session to be stalled.
        assert!(role_has(Role::Owner, Capability::ClaimControl));
        assert!(role_has(Role::NetControl, Capability::ClaimControl));
        assert!(role_has(Role::Logger, Capability::ClaimControl));
        assert!(!role_has(Role::Relay, Capability::ClaimControl));
        assert!(!role_has(Role::Participant, Capability::ClaimControl));
        assert_eq!(min_rank(Capability::ClaimControl), rank(Role::Logger));
    }

    #[test]
    fn moderate_needs_at_least_net_control() {
        // Disciplinary remove/block of a disruptive
        // station is an NCS act at the NetControl threshold — a materially higher
        // bar than EditCheckIn's Logger floor (a logging correction). Owner/NCS
        // hold it; Logger/Relay/Participant do NOT.
        assert!(role_has(Role::Owner, Capability::Moderate));
        assert!(role_has(Role::NetControl, Capability::Moderate));
        assert!(!role_has(Role::Logger, Capability::Moderate));
        assert!(!role_has(Role::Relay, Capability::Moderate));
        assert!(!role_has(Role::Participant, Capability::Moderate));
        assert_eq!(min_rank(Capability::Moderate), rank(Role::NetControl));
    }

    #[test]
    fn export_session_needs_at_least_net_control() {
        // Export is scoped to "NCS/owners" — the NetControl
        // threshold. Owner/NCS hold it; Logger/Relay/Participant do NOT. A Logger
        // may edit check-ins but may never export the net-control-perspective log.
        assert!(role_has(Role::Owner, Capability::ExportSession));
        assert!(role_has(Role::NetControl, Capability::ExportSession));
        assert!(!role_has(Role::Logger, Capability::ExportSession));
        assert!(!role_has(Role::Relay, Capability::ExportSession));
        assert!(!role_has(Role::Participant, Capability::ExportSession));
        assert_eq!(min_rank(Capability::ExportSession), rank(Role::NetControl));
    }

    #[test]
    fn run_session_and_manage_roles_need_at_least_net_control() {
        for cap in [Capability::RunSession, Capability::ManageRoles] {
            assert!(role_has(Role::Owner, cap));
            assert!(role_has(Role::NetControl, cap));
            assert!(!role_has(Role::Logger, cap));
            assert!(!role_has(Role::Relay, cap));
            assert!(!role_has(Role::Participant, cap));
        }
    }

    #[test]
    fn self_check_in_is_held_by_every_role_at_the_participant_floor() {
        // SelfCheckIn sits at the Participant (rank 0) threshold, so
        // EVERY authenticated role holds it — anyone signed in with a callsign may
        // check THEMSELVES in (over their own entry only, scoped by owns_check_in).
        for role in EVERY_ROLE {
            assert!(role_has(role, Capability::SelfCheckIn));
        }
        assert_eq!(
            min_rank(Capability::SelfCheckIn),
            rank(Role::Participant),
            "SelfCheckIn's threshold is the Participant floor"
        );
    }

    #[test]
    fn participant_holds_exactly_self_check_in_and_no_staff_capability() {
        // The Participant floor is UNCHANGED for every STAFF
        // capability — the one capability added is SelfCheckIn, held by
        // every role including Participant. Assert both halves so a future
        // threshold slip on either side is caught.
        assert!(role_has(Role::Participant, Capability::SelfCheckIn));
        for cap in STAFF_CAPABILITIES {
            assert!(
                !role_has(Role::Participant, cap),
                "a Participant must hold no staff capability"
            );
        }
    }

    #[test]
    fn owns_check_in_is_true_only_for_the_callers_own_self_entry() {
        // The object-level ownership predicate scoping self-edit/
        // self-checkout. True IFF the entry is self-sourced AND was added by the
        // acting account — mirrors can_manage_definition's pure-over-inputs shape.
        let me = Uuid::now_v7();
        let someone_else = Uuid::now_v7();

        // My own self-entry: owned.
        assert!(owns_check_in(CheckInSource::SelfService, Some(me), me));
        // Another participant's self-entry: NOT owned — never touch others'.
        assert!(!owns_check_in(
            CheckInSource::SelfService,
            Some(someone_else),
            me
        ));
        // A staff-entered entry, even one an operator added under my id, is not a
        // self entry — a Participant never reaches the staff add path, but the
        // predicate refuses it structurally regardless.
        assert!(!owns_check_in(CheckInSource::Staff, Some(me), me));
        // A self entry with no adder id (defensive) is unowned by anyone.
        assert!(!owns_check_in(CheckInSource::SelfService, None, me));
    }

    #[test]
    fn participant_holds_no_capability() {
        // The server-side floor for the STAFF capabilities, expressed once. The
        // one self-scoped capability (SelfCheckIn) is asserted separately above.
        for cap in STAFF_CAPABILITIES {
            assert!(!role_has(Role::Participant, cap));
        }
    }

    #[test]
    fn owner_holds_every_capability() {
        // The apex of the containment chain is a pure superset.
        for cap in EVERY_CAPABILITY {
            assert!(role_has(Role::Owner, cap));
        }
    }

    #[test]
    fn a_role_may_manage_only_strictly_lower_roles() {
        // Owner manages everyone below; nobody manages an equal or a superior.
        assert!(can_manage_role(Role::Owner, Role::NetControl));
        assert!(can_manage_role(Role::Owner, Role::Logger));
        assert!(can_manage_role(Role::Owner, Role::Relay));
        assert!(can_manage_role(Role::Owner, Role::Participant));
        assert!(!can_manage_role(Role::Owner, Role::Owner));

        assert!(can_manage_role(Role::NetControl, Role::Logger));
        assert!(can_manage_role(Role::NetControl, Role::Relay));
        assert!(can_manage_role(Role::NetControl, Role::Participant));
        // NCS may NOT manage a peer or a superior.
        assert!(!can_manage_role(Role::NetControl, Role::NetControl));
        assert!(!can_manage_role(Role::NetControl, Role::Owner));
    }

    #[test]
    fn equal_ranks_never_manage_each_other() {
        for role in EVERY_ROLE {
            assert!(!can_manage_role(role, role));
        }
    }

    #[test]
    fn roles_round_trip_through_their_kebab_wire_form() {
        let cases = [
            ("owner", Role::Owner),
            ("net-control", Role::NetControl),
            ("logger", Role::Logger),
            ("relay", Role::Relay),
            ("participant", Role::Participant),
        ];
        for (wire, role) in cases {
            assert_eq!(wire.parse::<Role>().expect("known role parses"), role);
            assert_eq!(role.as_str(), wire);
        }
    }

    #[test]
    fn an_unknown_role_string_fails_to_parse() {
        assert!("net-controller".parse::<Role>().is_err());
        assert!("".parse::<Role>().is_err());
        assert!("Owner".parse::<Role>().is_err());
    }

    #[test]
    fn an_oversized_unknown_role_string_is_truncated_before_being_echoed() {
        // A client-supplied `role` this large must never be reflected back at
        // full size in the 400 detail (review finding: unbounded echo).
        let oversized = "x".repeat(10_000);
        let err = oversized.parse::<Role>().expect_err("not a known role");
        assert_eq!(err.0.len(), MAX_ECHOED_ROLE_LEN);
        assert!(oversized.len() > MAX_ECHOED_ROLE_LEN);
    }

    #[test]
    fn role_serializes_to_the_same_kebab_string_as_as_str() {
        // The response body serializes `role`; it must match the storage/parse
        // wire form so the round-trip never drifts.
        for role in EVERY_ROLE {
            let json = serde_json::to_string(&role).expect("role serializes");
            assert_eq!(json, format!("\"{}\"", role.as_str()));
        }
    }
}
