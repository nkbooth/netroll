// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Platform-level administration: the one authorization axis that is NOT
//! per-net-scoped. A platform admin is a pure predicate over a boot-configured
//! email allowlist, never a runtime-mutable role table. An admin holds no
//! standing inside any specific net, only over the surface
//! [`AdminCapability`] enumerates.

use crate::auth::normalize_email;

/// Whether `email` names a configured platform administrator.
///
/// `true` iff the normalized email is present in the boot-configured
/// `allowlist`. Comparison is on the SAME normalized (trimmed,
/// ASCII-lowercased) form the account model stores and the allowlist is
/// resolved into at boot ([`normalize_email`]) — so an operator configuring
/// `Admin@Example.COM` matches the stored `admin@example.com`.
///
/// An empty allowlist denies everyone — the no-admin default (an instance with
/// no `ADMIN_ACCOUNT_EMAILS` configured has no reachable admin surface at all),
/// mirroring `can_manage_definition`'s empty-set-denies-everyone rule. Pure: no
/// I/O, no clock.
pub fn is_admin(email: &str, allowlist: &[String]) -> bool {
    let needle = normalize_email(email);
    allowlist
        .iter()
        .any(|entry| normalize_email(entry) == needle)
}

/// The CLOSED, finite set of platform-admin capabilities.
///
/// The explicit "defined, limited set", and the reason there is no unbounded
/// superuser path: every admin-privileged code
/// path MUST map to exactly one of these variants, and there is deliberately no
/// wildcard/god-mode variant. In particular NONE of these touches a per-user
/// secret (the QRZ credential boundary stays admin-unreadable).
///
/// Kept DISTINCT from the per-net [`crate::authz::Capability`] — platform admin
/// is a separate authorization axis, not a new rank in the per-net containment
/// chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminCapability {
    /// List submitted abuse reports for review (`GET /api/admin/abuse-reports`).
    ViewReports,
    /// Mark an abuse report resolved/actioned.
    ResolveReport,
    /// Disable an account (instance-wide enforcement — distinct from a
    /// self-deletion, and from a per-session moderation block).
    DisableAccount,
    /// Re-enable a previously disabled account (reversibility of a mis-disable).
    ReenableAccount,
    /// Review the consolidated security audit log (`GET /api/admin/audit-log`).
    /// A bounded READ over `audit_log` only — never a per-user
    /// secret; the review itself is also audited (consistent with `ViewReports`).
    ViewAuditLog,
    /// RETIRED — superseded by [`AdminCapability::SearchObjects`]. The surface
    /// can no longer produce this verb, but rows bearing it are in the
    /// historical log forever, so the variant stays: the `?action=` filter and
    /// the UI label map both resolve against `EVERY ∪ RETIRED`, and dropping it
    /// would make those rows unfilterable and unlabelable — losing a record's
    /// legibility is losing the record.
    LookupAccount,
    /// Resolve business objects by identifier or name — accounts, net
    /// definitions, net sessions, and abuse reports
    /// (`GET /api/admin/search?q=`). The read that makes every other capability
    /// operable: an abuse report is free text with no target FK, so without it
    /// an admin has no way to turn "this operator" or "this net" into the id
    /// the action endpoints and the audit filters require.
    ///
    /// SCOPE OF MATCHING (a deliberate narrowing, not an accident): callsign and
    /// display name match by PREFIX — they are public radio data, already shown
    /// on public net pages — while **email and every id match EXACTLY**. That
    /// asymmetry is the whole fence: it keeps this a targeting tool for someone
    /// who already has an identifier, not an address-harvesting sweep. Net and
    /// session titles match by substring; they are public too.
    ///
    /// Reads identity columns only; per-user secrets stay unreachable.
    SearchObjects,
}

/// Rows returned when a caller names no page size.
pub const DEFAULT_PAGE_LIMIT: usize = 50;

/// The largest page an admin read will serve, however large a limit is asked
/// for — an unbounded limit would defeat the point of paging.
pub const MAX_PAGE_LIMIT: usize = 200;

/// Clamps a requested page size into `1..=MAX_PAGE_LIMIT`, defaulting when
/// unset. A zero request becomes one row: a zero-row page would make "load more"
/// loop forever without advancing. Pure: no I/O, no clock.
pub fn clamp_limit(requested: Option<u32>) -> usize {
    match requested {
        None => DEFAULT_PAGE_LIMIT,
        Some(n) => (n as usize).clamp(1, MAX_PAGE_LIMIT),
    }
}

/// The position an admin list read resumes from — the last row of the previous
/// page.
///
/// Both admin reads order by a timestamp with the row id as tiebreak, so the
/// cursor carries BOTH: a timestamp alone is not unique (two rows can share a
/// millisecond) and would skip or repeat rows at a page boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCursor {
    /// The ordering timestamp of the last row served, epoch millis.
    pub at_millis: u64,
    /// That row's id — the tiebreak within a shared timestamp.
    pub id: uuid::Uuid,
}

/// A cursor that did not come from [`encode_cursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorError;

/// Renders a cursor into its opaque wire form (`"{millis}:{uuid}"`).
///
/// The shape is an implementation detail clients must not construct or parse —
/// they echo back whatever `nextCursor` the previous page returned.
pub fn encode_cursor(cursor: PageCursor) -> String {
    format!("{}:{}", cursor.at_millis, cursor.id)
}

/// Parses a cursor previously produced by [`encode_cursor`].
///
/// Anything else is [`CursorError`] — a malformed cursor is refused rather than
/// treated as "start over", which would turn a client bug into an endless first
/// page. That includes a syntactically valid cursor naming an instant no
/// timestamp can represent: the millis are client-supplied and reach an
/// infallible conversion downstream, so refusing here is what keeps a typed-in
/// query string a 400 instead of an aborted request. Pure: no I/O, no clock.
pub fn parse_cursor(raw: &str) -> Result<PageCursor, CursorError> {
    let (at, id) = raw.split_once(':').ok_or(CursorError)?;
    let at_millis: u64 = at.parse().map_err(|_| CursorError)?;
    if !is_representable_instant(at_millis) {
        return Err(CursorError);
    }
    Ok(PageCursor {
        at_millis,
        id: uuid::Uuid::parse_str(id).map_err(|_| CursorError)?,
    })
}

/// Whether epoch `millis` names an instant the app's timestamp type can carry.
///
/// Asked of `chrono` rather than compared against a hardcoded bound so the two
/// can never drift apart.
fn is_representable_instant(millis: u64) -> bool {
    i64::try_from(millis)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .is_some()
}

/// An opaque stamp identifying WHICH filter set a cursor was issued under.
///
/// A keyset cursor is only meaningful against the query that produced it. Replay
/// a page-2 cursor with different filters and the read silently re-anchors:
/// the caller receives a partial slice with no indication that rows were
/// skipped. On an audit surface that reads as "this is everything" — the most
/// damaging way to be wrong. Binding the cursor to its filters converts that
/// into a refusal.
///
/// Not a security boundary and not collision-proof in the cryptographic sense —
/// it is a "these are not the same question" check, and the caller is an
/// authenticated admin, not an adversary.
pub type FilterFingerprint = u64;

/// Fingerprints an audit-filter set. Field-position-sensitive, so the same uuid
/// used as an actor and as an object yields different stamps — they are
/// different questions. Pure: no I/O, no clock.
pub fn filter_fingerprint(
    actor: Option<uuid::Uuid>,
    action: Option<&str>,
    object: Option<uuid::Uuid>,
) -> FilterFingerprint {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Discriminate by position: hash a field tag before each value so a uuid in
    // the actor slot can never fingerprint the same as one in the object slot.
    "actor".hash(&mut hasher);
    actor.hash(&mut hasher);
    "action".hash(&mut hasher);
    action.hash(&mut hasher);
    "object".hash(&mut hasher);
    object.hash(&mut hasher);
    hasher.finish()
}

/// Renders a cursor stamped with the filter set it was issued under.
pub fn encode_cursor_for(cursor: PageCursor, filters: FilterFingerprint) -> String {
    format!("{}:{}:{}", cursor.at_millis, cursor.id, filters)
}

/// Parses a cursor and refuses it unless it was issued under `filters`.
///
/// A mismatch is [`CursorError`], the same refuse-don't-guess posture
/// [`parse_cursor`] takes on a malformed cursor.
pub fn parse_cursor_for(raw: &str, filters: FilterFingerprint) -> Result<PageCursor, CursorError> {
    let (position, stamp) = raw.rsplit_once(':').ok_or(CursorError)?;
    let stamp: FilterFingerprint = stamp.parse().map_err(|_| CursorError)?;
    if stamp != filters {
        return Err(CursorError);
    }
    parse_cursor(position)
}

/// How a lookup term was resolved — the shape decision behind the RETIRED
/// [`AdminCapability::LookupAccount`], and residue of it: the account read this
/// classified is now reached through [`AdminCapability::SearchObjects`].
///
/// Each variant maps to exactly ONE existing exact-match account read, so the
/// lookup can never widen into a scan. Pure classification: the callsign arm
/// carries the term only trimmed, leaving the grammar parse to the caller
/// (`parse_callsign`), matching the convention `AccountRepo::find_by_callsign`
/// already documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupTerm {
    /// A parseable account id — resolved by `find_by_id`.
    AccountId(uuid::Uuid),
    /// A normalized email address — resolved by `find_by_email`.
    Email(String),
    /// A trimmed candidate callsign — parsed, then resolved by `find_by_callsign`.
    Callsign(String),
}

/// Classifies an admin's lookup term by shape, or `None` when it is blank.
///
/// Order is load-bearing: an account id is tried first (a uuid holds no `@`, so
/// the arms cannot both match), then anything containing `@` is treated as an
/// email and normalized with the same [`normalize_email`] rule the account model
/// stores, and everything else is a candidate callsign.
///
/// A blank term yields `None` so the caller refuses it rather than degenerating
/// into an unbounded read. Pure: no I/O, no clock.
pub fn classify_lookup_term(term: &str) -> Option<LookupTerm> {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(id) = uuid::Uuid::parse_str(trimmed) {
        return Some(LookupTerm::AccountId(id));
    }
    if trimmed.contains('@') {
        return Some(LookupTerm::Email(normalize_email(trimmed)));
    }
    Some(LookupTerm::Callsign(trimmed.to_owned()))
}

impl AdminCapability {
    /// Every LIVE admin capability — the finite closed set the surface can
    /// exercise today. The "no unbounded path" test enumerates this to prove
    /// the admin surface is exactly these six actions.
    pub const EVERY: [AdminCapability; 6] = [
        AdminCapability::ViewReports,
        AdminCapability::ResolveReport,
        AdminCapability::DisableAccount,
        AdminCapability::ReenableAccount,
        AdminCapability::ViewAuditLog,
        AdminCapability::SearchObjects,
    ];

    /// Capabilities the surface no longer exercises but whose rows remain in
    /// the append-only log.
    ///
    /// `as_str` is doing two jobs with opposite change policies: it is the
    /// authorization identifier (refactorable) AND the permanent audit verb
    /// ("once written they are the historical record"). Retiring rather than
    /// deleting separates them — the log's vocabulary stays finite and
    /// enumerable (`EVERY ∪ RETIRED`), which is the property the review surface
    /// needs, without pretending the surface can still perform the action.
    pub const RETIRED: [AdminCapability; 1] = [AdminCapability::LookupAccount];

    /// The stable lowercase-kebab wire/audit spelling of this capability — the
    /// value written as the `action` on an [`crate::admin`] audit-log entry.
    pub fn as_str(self) -> &'static str {
        match self {
            AdminCapability::ViewReports => "view-reports",
            AdminCapability::ResolveReport => "resolve-report",
            AdminCapability::DisableAccount => "disable-account",
            AdminCapability::ReenableAccount => "reenable-account",
            AdminCapability::ViewAuditLog => "view-audit-log",
            AdminCapability::LookupAccount => "lookup-account",
            AdminCapability::SearchObjects => "search-objects",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_email_is_an_admin() {
        let allowlist = vec!["admin@example.com".to_owned()];
        assert!(is_admin("admin@example.com", &allowlist));
    }

    #[test]
    fn a_non_configured_email_is_not_an_admin() {
        let allowlist = vec!["admin@example.com".to_owned()];
        assert!(!is_admin("someone@example.com", &allowlist));
    }

    #[test]
    fn an_empty_allowlist_denies_everyone() {
        // The no-admin default — mirrors can_manage_definition's empty-set rule.
        assert!(!is_admin("admin@example.com", &[]));
    }

    #[test]
    fn matching_is_case_and_whitespace_insensitive_via_the_shared_normalization() {
        // Both sides normalize through the SAME rule the account email stores
        // (trim + ASCII-lowercase) — no second normalization contract.
        let allowlist = vec!["admin@example.com".to_owned()];
        assert!(is_admin("  Admin@Example.COM ", &allowlist));

        let messy_allowlist = vec![" Admin@Example.COM ".to_owned()];
        assert!(is_admin("admin@example.com", &messy_allowlist));
    }

    #[test]
    fn any_of_several_configured_admins_matches() {
        let allowlist = vec![
            "a@example.com".to_owned(),
            "b@example.com".to_owned(),
            "c@example.com".to_owned(),
        ];
        assert!(is_admin("b@example.com", &allowlist));
        assert!(!is_admin("d@example.com", &allowlist));
    }

    #[test]
    fn a_retired_capability_keeps_its_verb_so_its_rows_stay_legible() {
        // The surface can no longer perform it, but rows bearing the verb are in
        // the append-only log forever. Dropping the variant would make them
        // unfilterable and unlabelable — the record would still exist and be
        // unreadable, which is the same as losing it.
        assert_eq!(AdminCapability::RETIRED.len(), 1);
        assert!(AdminCapability::RETIRED.contains(&AdminCapability::LookupAccount));
        assert_eq!(AdminCapability::LookupAccount.as_str(), "lookup-account");
        assert!(
            !AdminCapability::EVERY.contains(&AdminCapability::LookupAccount),
            "a retired capability is not one the surface can still exercise"
        );
    }

    #[test]
    fn the_search_capability_has_its_stable_wire_spelling() {
        assert_eq!(AdminCapability::SearchObjects.as_str(), "search-objects");
    }

    #[test]
    fn no_verb_collides_across_the_live_and_retired_sets() {
        // The log's total vocabulary is EVERY ∪ RETIRED; a collision would make
        // one verb ambiguous between a live and a dead capability.
        let all: Vec<&str> = AdminCapability::EVERY
            .iter()
            .chain(AdminCapability::RETIRED.iter())
            .map(|c| c.as_str())
            .collect();
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "{all:?}");
    }

    #[test]
    fn the_admin_capability_set_is_exactly_the_six_bounded_actions() {
        // The crux: the admin surface is finite and closed — no god-mode. It
        // grew four→five with the audit-log review capability; the admin
        // dashboard grew it five→six with the object search that turns a
        // free-text report into an actionable id. The exact-match account
        // lookup that first held that sixth slot is RETIRED, not live — hence
        // `SearchObjects` below and `LookupAccount` in `RETIRED`.
        assert_eq!(AdminCapability::EVERY.len(), 6);
        assert!(AdminCapability::EVERY.contains(&AdminCapability::ViewReports));
        assert!(AdminCapability::EVERY.contains(&AdminCapability::ResolveReport));
        assert!(AdminCapability::EVERY.contains(&AdminCapability::DisableAccount));
        assert!(AdminCapability::EVERY.contains(&AdminCapability::ReenableAccount));
        assert!(AdminCapability::EVERY.contains(&AdminCapability::ViewAuditLog));
        assert!(AdminCapability::EVERY.contains(&AdminCapability::SearchObjects));
    }

    #[test]
    fn the_view_audit_log_capability_has_its_stable_wire_spelling() {
        assert_eq!(AdminCapability::ViewAuditLog.as_str(), "view-audit-log");
    }

    #[test]
    fn a_lookup_term_is_classified_by_its_shape() {
        // The lookup resolves one account EXACTLY — never a prefix/substring
        // sweep — so the term's shape alone decides which existing exact-match
        // read runs. An id wins over the email rule (a uuid holds no '@').
        assert_eq!(
            classify_lookup_term("00000000-0000-7000-8000-000000000000"),
            Some(LookupTerm::AccountId(
                uuid::Uuid::parse_str("00000000-0000-7000-8000-000000000000")
                    .expect("a valid uuid")
            ))
        );
        assert_eq!(
            classify_lookup_term("  Op@Example.COM "),
            Some(LookupTerm::Email("op@example.com".to_owned()))
        );
        assert_eq!(
            classify_lookup_term(" w1abc "),
            Some(LookupTerm::Callsign("w1abc".to_owned()))
        );
    }

    #[test]
    fn a_cursor_round_trips_with_its_filter_fingerprint() {
        let id = uuid::Uuid::parse_str("00000000-0000-7000-8000-0000000000ab").expect("uuid");
        let cursor = PageCursor {
            at_millis: 1_754_000_000_123,
            id,
        };
        let fp = filter_fingerprint(Some(id), Some("disable-account"), None);
        assert_eq!(
            parse_cursor_for(&encode_cursor_for(cursor, fp), fp),
            Ok(cursor)
        );
    }

    #[test]
    fn a_cursor_is_refused_when_the_filters_changed_under_it() {
        // A page-2 cursor replayed against a DIFFERENT filter set would silently
        // re-anchor: the caller gets a partial slice with no signal that rows
        // were skipped. In an audit tool that reads as "this is everything".
        let id = uuid::Uuid::parse_str("00000000-0000-7000-8000-0000000000ab").expect("uuid");
        let cursor = PageCursor {
            at_millis: 1_754_000_000_123,
            id,
        };
        let issued = filter_fingerprint(Some(id), None, None);
        let now_asking = filter_fingerprint(None, None, Some(id));
        assert_ne!(issued, now_asking);
        assert_eq!(
            parse_cursor_for(&encode_cursor_for(cursor, issued), now_asking),
            Err(CursorError)
        );
    }

    #[test]
    fn the_fingerprint_distinguishes_which_field_a_value_came_from() {
        // The same uuid as actor and as object are different questions; a
        // fingerprint that collided would let one page's cursor resume the other.
        let id = uuid::Uuid::parse_str("00000000-0000-7000-8000-0000000000ab").expect("uuid");
        assert_ne!(
            filter_fingerprint(Some(id), None, None),
            filter_fingerprint(None, None, Some(id))
        );
    }

    #[test]
    fn the_unfiltered_fingerprint_is_stable() {
        // The default (unfiltered) view must keep paging across calls.
        assert_eq!(
            filter_fingerprint(None, None, None),
            filter_fingerprint(None, None, None)
        );
    }

    #[test]
    fn a_page_cursor_round_trips_through_its_wire_form() {
        // The cursor is opaque to the client but must resume EXACTLY where the
        // previous page stopped — a lossy round trip would skip or repeat rows.
        let id = uuid::Uuid::parse_str("00000000-0000-7000-8000-0000000000ab").expect("uuid");
        let cursor = PageCursor {
            at_millis: 1_754_000_000_123,
            id,
        };
        assert_eq!(parse_cursor(&encode_cursor(cursor)), Ok(cursor));
    }

    #[test]
    fn a_malformed_cursor_is_rejected_rather_than_silently_reset() {
        // Falling back to "start from the beginning" on a bad cursor would make
        // a client bug look like an endless first page.
        for bad in [
            "",
            "notanumber:00000000-0000-7000-8000-0000000000ab",
            "1754000000123:not-a-uuid",
            "1754000000123",
            "1754000000123:00000000-0000-7000-8000-0000000000ab:extra",
        ] {
            assert_eq!(parse_cursor(bad), Err(CursorError), "rejects {bad:?}");
        }
    }

    #[test]
    fn a_cursor_naming_an_unrepresentable_instant_is_rejected() {
        // Syntactically well-formed but past what a timestamp can hold. The
        // millis are client-supplied and are converted infallibly at the storage
        // boundary, so accepting one here would abort the read rather than
        // refuse the input.
        for bad in [
            "9000000000000000:00000000-0000-7000-8000-0000000000ab",
            "18446744073709551615:00000000-0000-7000-8000-0000000000ab",
        ] {
            assert_eq!(parse_cursor(bad), Err(CursorError), "rejects {bad:?}");
        }
        // The bound is only useful if it still admits every instant the app can
        // actually produce.
        assert!(
            parse_cursor("1754000000123:00000000-0000-7000-8000-0000000000ab").is_ok(),
            "a real timestamp is still a valid cursor"
        );
    }

    #[test]
    fn a_page_limit_is_clamped_to_the_bounded_range() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(0)), 1, "zero would return no progress");
        assert_eq!(
            clamp_limit(Some(10_000)),
            MAX_PAGE_LIMIT,
            "an unbounded limit would defeat paging entirely"
        );
    }

    #[test]
    fn a_blank_lookup_term_is_not_classifiable() {
        // An empty query must not degenerate into "match everything" — the
        // handler turns `None` into a validation refusal, never a table scan.
        assert_eq!(classify_lookup_term(""), None);
        assert_eq!(classify_lookup_term("   "), None);
    }

    #[test]
    fn every_admin_capability_has_a_distinct_stable_wire_spelling() {
        // Each capability serializes to its own kebab `action` string; no two
        // collide (they are the actions this audit log records).
        let spellings: std::collections::HashSet<_> =
            AdminCapability::EVERY.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            spellings.len(),
            AdminCapability::EVERY.len(),
            "capability action strings are distinct"
        );
    }
}
