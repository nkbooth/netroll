// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! RFC 9457 `application/problem+json` error mapping. Every API error
//! carries a stable relative `type` slug (`/errors/<kebab>`) the frontend
//! maps to user-facing copy.

use std::borrow::Cow;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use netroll_adapters::pg::UnreplayableLog;
use serde::Serialize;
use thiserror::Error;

/// Wire shape of a problem response.
#[derive(Debug, Serialize)]
pub struct Problem {
    /// Stable relative slug, e.g. `/errors/validation`.
    pub r#type: &'static str,
    /// Short human-readable summary (not the frontend's copy source).
    pub title: &'static str,
    /// HTTP status, duplicated in the body per RFC 9457.
    pub status: u16,
    /// Optional occurrence-specific explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// RFC 9457 extension member naming WHICH entry of a submitted connection
    /// list was refused, zero-based, as the client numbered it.
    ///
    /// An extension member rather than a second problem type: the failure is
    /// the same `/errors/net-definition-invalid` a set-level refusal is, and
    /// the only new fact is which row carries it. Absent — not null — on every
    /// response that does not set it, so no existing problem body changes
    /// shape. A set-level refusal deliberately leaves it absent: pointing an
    /// owner at a row would name a connection the failure is not about.
    #[serde(rename = "connectionIndex", skip_serializing_if = "Option::is_none")]
    pub connection_index: Option<usize>,
}

/// Application errors; each maps to exactly one problem shape.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Request payload failed validation.
    ///
    /// `Cow` rather than `&'static str`: most of this variant's copy is a fixed
    /// sentence, but the profile and roster field rejections have to name the
    /// bound exceeded or the character that offended, which are only known at
    /// runtime. Construct it through [`ApiError::validation`].
    #[error("validation failed: {0}")]
    Validation(Cow<'static, str>),
    /// Magic link past its TTL.
    #[error("magic link expired")]
    MagicLinkExpired,
    /// Magic link already used once.
    #[error("magic link consumed")]
    MagicLinkConsumed,
    /// No such magic link.
    #[error("magic link invalid")]
    MagicLinkInvalid,
    /// Missing, revoked, or expired session.
    #[error("unauthenticated")]
    Unauthenticated,
    /// Authenticated but unconsented account hitting a gated action.
    #[error("consent required")]
    ConsentRequired,
    /// Consent submitted for a version the server did not ask for (stale
    /// gate page after a terms bump).
    #[error("consent version mismatch")]
    ConsentVersionMismatch,
    /// Callsign failed the domain grammar. Carries the domain
    /// error's `Display` as the problem `detail` — each rejection reason
    /// gets its own specific message.
    #[error("callsign invalid: {0}")]
    CallsignInvalid(String),
    /// Callsign already held by another account.
    #[error("callsign taken")]
    CallsignTaken,
    /// Maidenhead grid failed the domain grammar. Carries
    /// the domain error's `Display` as the problem `detail` — each
    /// rejection reason gets its own specific message (the
    /// `CallsignInvalid` pattern).
    #[error("grid invalid: {0}")]
    GridInvalid(String),
    /// Callsign claim refused because the account's email is not verified.
    /// Defensive: unreachable through the magic-link flow today, which always
    /// verifies on account creation.
    #[error("email unverified")]
    EmailUnverified,
    /// Email-change confirmation link past its TTL.
    #[error("email change expired")]
    EmailChangeExpired,
    /// Email-change confirmation link already used once.
    #[error("email change consumed")]
    EmailChangeConsumed,
    /// No such email-change confirmation link.
    #[error("email change invalid")]
    EmailChangeInvalid,
    /// Target email already identifies another account.
    #[error("email taken")]
    EmailTaken,
    /// A submitted net definition failed field validation.
    /// Carries the domain error's `Display` (`"<field>: <reason>"`) as the
    /// problem `detail` — field-level, the `CallsignInvalid`/`GridInvalid`
    /// pattern.
    #[error("net definition invalid: {0}")]
    NetDefinitionInvalid(String),
    /// A connection-list write the domain refused, carrying the failing
    /// entry's zero-based index when the refusal was about one entry rather
    /// than about the set. Shares
    /// `/errors/net-definition-invalid` with [`ApiError::NetDefinitionInvalid`]
    /// deliberately: the client's new fact is the index, not a new error class.
    #[error("net connections invalid: {1}")]
    NetConnectionsInvalid(Option<usize>, String),
    /// A submitted schedule failed field validation. Carries
    /// the domain error's `Display` (`"<field>: <reason>"`) as the problem
    /// `detail` — field-level, the `NetDefinitionInvalid` pattern.
    #[error("schedule invalid: {0}")]
    ScheduleInvalid(String),
    /// A discovery filter or sort query carried an out-of-vocabulary token.
    /// Carries the domain error's `Display` as the problem `detail`:
    /// field-level, the `ScheduleInvalid` pattern.
    #[error("discovery query invalid: {0}")]
    DiscoveryQueryInvalid(String),
    /// A submitted per-net delivery config failed field validation. Carries the
    /// domain error's `Display` as the problem `detail`: field-level, the
    /// `ScheduleInvalid` pattern. Those messages are fixed and never echo the
    /// submitted URL or email.
    #[error("delivery config invalid: {0}")]
    DeliveryConfigInvalid(String),
    /// A callsign-gated action attempted without a reserved callsign. Inlined
    /// here rather than made an extractor, until a second consumer needs one.
    #[error("callsign required")]
    CallsignRequired,
    /// Object-level authorization denied: the acting account is not an owner
    /// of the target definition. Generic and detail-free —
    /// it must not leak owner identities.
    #[error("forbidden")]
    Forbidden,
    /// No net definition exists for the given id.
    #[error("net definition not found")]
    NetDefinitionNotFound,
    /// A session command named a connection this session did not freeze at
    /// start. Detail-free; maps to a 404.
    #[error("net connection not found")]
    NetConnectionNotFound,
    /// A retune named a way in that carries no frequency at all — an EchoLink
    /// node, a DMR talkgroup, a reflector. The
    /// connection EXISTS on the session, so this is not a 404; the operation
    /// simply does not apply to it, which is a 422.
    #[error("that connection carries no frequency")]
    ConnectionCarriesNoFrequency,
    /// A session lifecycle command (start/close) was rejected by the pure
    /// guard (`session_sm`) — each variant maps to its own 409
    /// slug. The mapping table lives only here.
    #[error("session transition rejected: {0}")]
    SessionTransition(#[from] netroll_domain::session_sm::SessionTransitionError),
    /// No net session exists for the given id.
    #[error("net session not found")]
    NetSessionNotFound,
    /// Owner-management target does not resolve: a callsign no
    /// account holds, or a remove of an account not in the owner set.
    /// Detail-free — it must not confirm which callsigns/accounts exist beyond
    /// the not-found signal.
    #[error("owner not found")]
    OwnerNotFound,
    /// Removing the sole owner via the API is refused: a net
    /// can never be voluntarily orphaned — add another owner first.
    #[error("last owner")]
    LastOwner,
    /// A role-grant request named a string that is not a known role. Carries the
    /// domain parse error's `Display` as the problem `detail`.
    #[error("role invalid: {0}")]
    RoleInvalid(String),
    /// A role-revoke named a `(session, account)` pair with no grant.
    /// Detail-free: it must not confirm which accounts hold a grant beyond the
    /// not-found signal, mirroring `OwnerNotFound`.
    #[error("role grant not found")]
    RoleGrantNotFound,
    /// A submitted signal report failed the bounded-string guard. Carries the
    /// domain error's `Display` as the problem `detail`.
    #[error("signal report invalid: {0}")]
    SignalReportInvalid(String),
    /// A check-in named a `staying` value that is not a known status token.
    /// Carries the offending token as `detail`, the posture `RoleInvalid` uses.
    #[error("staying invalid: {0}")]
    StayingInvalid(String),
    /// A check-in edit named a `precedence` value that is not a known token.
    /// Carries the sanitized offending token as `detail`.
    #[error("precedence invalid: {0}")]
    PrecedenceInvalid(String),
    /// A check-in edit carried a traffic count outside the representable range.
    /// Carries the domain error's `Display` as `detail`.
    #[error("traffic invalid: {0}")]
    TrafficInvalid(String),
    /// A per-station or net-level note failed the bounded-text guard. Carries the
    /// domain error's `Display` as `detail`.
    #[error("note invalid: {0}")]
    NoteInvalid(String),
    /// A free-text `via` failed the bounded single-line guard, or was blank. Its
    /// OWN problem type rather than `NoteInvalid`'s, because an error must name
    /// the faulting field: a `via` rejection answering `/errors/note-invalid` on
    /// a request with no `notes` key sends the operator to the wrong control.
    /// Carries the domain error's `Display` as `detail`.
    #[error("via invalid: {0}")]
    ViaInvalid(String),
    /// A `relayedBy` callsign failed the domain grammar. Its OWN problem type
    /// rather than `CallsignInvalid`'s, because reusing a borrowed guard
    /// compiles, validates correctly, and then tells the operator their CALLSIGN
    /// is wrong on a request whose callsign was fine. An error names the
    /// FAULTING field. Carries the domain error's `Display` as `detail`.
    #[error("relayed by invalid: {0}")]
    RelayedByInvalid(String),
    /// A check-in edit/remove carried an `expectedVersion` that no longer
    /// matches the entry's current fold-derived version — the
    /// optimistic-concurrency CAS lost. Detail-free: the loser reconciles
    /// against the authoritative version it already holds from the live delta.
    #[error("stale version")]
    StaleVersion,
    /// A competing soft-lock acquire while another account holds a valid lease.
    /// Carries the holder's callsign as `detail` so the client can say who is
    /// editing.
    #[error("lock held: {0}")]
    LockHeld(String),
    /// A `claim-control` request targeted a session that is NOT stalled, so
    /// there is nothing to claim. Detail-free; maps to a 409.
    #[error("control not stalled")]
    ControlNotStalled,
    /// A voluntary `handoff` named a target account whose resolved role is below
    /// the NetControl tier — an unqualified target cannot
    /// receive control. Detail-free; maps to a 422.
    #[error("handoff target unqualified")]
    HandoffTargetUnqualified,
    /// A blocked account attempted to self-check-in to a session it was moderated
    /// out of. Detail-free (never echoes PII or the slug's
    /// internals): the frontend maps it to a friendly message. Maps to a 403.
    #[error("account blocked")]
    AccountBlocked,
    /// A moderation `block` was requested against an account-less staff-logged
    /// entry — there is no account to key the block on. The
    /// removal is NOT performed (all-or-nothing). Detail-free; maps to a 422.
    #[error("nothing to block")]
    NothingToBlock,
    /// A submitted QRZ credential field failed domain validation: empty,
    /// whitespace-only, or over the bounded length. Carries the domain error's
    /// `Display` as `detail`. The message is fixed and never echoes the
    /// submitted credential.
    #[error("qrz credentials invalid: {0}")]
    QrzCredentialsInvalid(String),
    /// A QRZ credential write on an instance with no KEK. The cipher is
    /// fail-closed, so sealing is impossible until an operator supplies one.
    /// Detail-free; a 503, so the client can distinguish "not configured here"
    /// from a validation or auth failure.
    #[error("crypto unavailable")]
    CryptoUnavailable,
    /// An uploaded avatar failed the domain's byte-level validation (empty,
    /// oversize, or not an accepted image type). Carries the domain error's
    /// `Display` as the problem `detail` — each rejection reason gets its own
    /// specific message (the `CallsignInvalid` pattern), because "PNG, JPEG,
    /// WebP, or GIF" and "larger than 1 MB" are different user actions.
    #[error("avatar invalid: {0}")]
    AvatarInvalid(String),
    /// Avatar storage is unavailable on this instance — no store installed, or
    /// the write itself failed. Detail-free on purpose: a filesystem path is an
    /// operator diagnostic (it goes to the log), never a caller's business.
    /// 503 mirrors `CryptoUnavailable`: not configured here, not your request's
    /// fault.
    #[error("avatar storage unavailable")]
    AvatarStorageUnavailable,
    /// The acting account already owns the configured maximum number of active
    /// nets. A business-rule rejection — the 409 posture
    /// of `LastOwner`/`CallsignTaken`. Detail-free (the cap is instance config,
    /// not per-request data).
    #[error("max nets per user reached")]
    MaxNetsPerUserReached,
    /// The target net already has the configured maximum number of owners. A
    /// business-rule rejection, the 409 posture. Detail-free.
    #[error("max owners per net reached")]
    MaxOwnersPerNetReached,
    /// A disabled account attempted to authenticate or act. A
    /// DISTINCT refusal from `Unauthenticated`: returned at the sign-in/verify
    /// step and by `require_session` for a surviving session, so a disabled
    /// account is not silently signed into a broken state. Detail-free and
    /// neutral (no "you are banned" oracle beyond the slug); maps to a 403.
    #[error("account disabled")]
    AccountDisabled,
    /// An admin action named an abuse report id that does not exist.
    /// Detail-free; maps to a 404.
    #[error("abuse report not found")]
    AbuseReportNotFound,
    /// An admin action named an account id that does not exist.
    /// Detail-free; maps to a 404.
    #[error("account not found")]
    AccountNotFound,
    /// An admin targeted their OWN account id with `disable`. Admin status is a
    /// boot-configured email allowlist, not a DB-editable role, so a
    /// self-disable would revoke the acting admin's live sessions and refuse
    /// them from signing back in, with no in-app path to `reenable` themselves.
    /// On a single-admin instance that is an unrecoverable lockout of the whole
    /// admin surface short of a manual DB edit.
    #[error("cannot disable self")]
    CannotDisableSelf,
    /// Too many magic-link requests.
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the window resets, surfaced as `Retry-After`.
        retry_after_secs: u64,
    },
    /// Storage failure — the upstream detail is never logged or sent: sqlx
    /// error text can embed row values such as email addresses.
    ///
    /// **No `#[from]`.** The conversion is hand-written below so a
    /// [`UnreplayableLog`] cannot be swallowed into this variant, and through
    /// it into the generic `/errors/internal` 500.
    #[error("database error")]
    Database(sqlx::Error),
    /// The session's stored log is too old for this version to read.
    ///
    /// Its own variant, its own slug and a populated `detail`, because this is
    /// the one failure an operator can do NOTHING about and will meet with no
    /// warning — opening a closed net's log they attended months ago. Telling
    /// them "something went wrong on our side, try again" would be an
    /// instruction to repeat an action that will never work.
    #[error("unreplayable log")]
    UnreplayableLog,
    /// Unexpected infrastructure failure.
    #[error("internal error: {0}")]
    Internal(&'static str),
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        // The ONE storage fault that is not interchangeable with the others: a
        // record this version can no longer read is permanent, and the reader
        // must be told so rather than invited to retry. Everything else keeps
        // the opaque 500 that stops sqlx error text — which can embed row
        // values such as email addresses — reaching a client.
        if let sqlx::Error::Decode(inner) = &error
            && inner.downcast_ref::<UnreplayableLog>().is_some()
        {
            return ApiError::UnreplayableLog;
        }
        ApiError::Database(error)
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        // Extractor rejections join the problem+json contract instead of axum's
        // default plain-text responses. Each arm is a neutral sentence ending in
        // the only action that helps: naming the wire format would state a fact
        // about the transport rather than anything the reader can change.
        ApiError::validation(match rejection {
            JsonRejection::MissingJsonContentType(_) => {
                "That request wasn't something the server could read — reload the page and try again."
            }
            JsonRejection::JsonDataError(_) => {
                "Some of that request's details weren't what the server expected — reload the page and try again."
            }
            JsonRejection::JsonSyntaxError(_) => {
                "That request arrived damaged — reload the page and try again."
            }
            _ => "The server couldn't read that request — reload the page and try again.",
        })
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        // Query-string rejections join the problem+json contract instead of
        // axum's default plain-text responses. Reached ONLY through the
        // `AppQuery` extractor, never by axum itself.
        //
        // The wording lands on the same neutral sentence
        // [`strict_query_rejection`] ships: same slug, same class of fault, one
        // answer. Enumerating the causes at a reader who can act on none of
        // them would read as an implementation note. The two functions stay
        // separate because the strict path can additionally be reached for an
        // unrecognised key.
        ApiError::validation(match rejection {
            QueryRejection::FailedToDeserializeQueryString(_) => {
                "That request wasn't something the server could read — reload the page and try again."
            }
            // `QueryRejection` is `#[non_exhaustive]` with one variant today; a
            // future axum minor must land in this contract rather than falling
            // back out to a plain-text response.
            _ => "Something about that request couldn't be read — reload the page and try again.",
        })
    }
}

/// The strict-read counterpart of [`From<QueryRejection>`].
///
/// Reached ONLY through [`crate::http::AppStrictQuery`], whose query types carry
/// `#[serde(deny_unknown_fields)]`, so this detail covers one cause the lenient
/// reads cannot produce: an UNRECOGNISED parameter. A read is "filtered" when a
/// silently-dropped key changes the MEANING of the answer, and on the audit log
/// that meaning is "no restriction applied".
///
/// A separate string rather than a widening of the sibling above. The lenient
/// reads' detail is a shipped answer on eight call sites, and this one is
/// USER-FACING COPY: the check-in history surface renders `problem.detail`
/// verbatim, and [`ApiError::Validation`] always populates it.
///
/// Two registers apply on this surface:
///
/// - WHOLE-REQUEST SENTENCE, capitalised, ending in an action. For errors with
///   no field to point at, or whose `detail` renders in a section-level alert
///   covering several fields at once: this function, the two `From` rejections
///   above, the profile PUT, the roster edit, the frequency refusals.
/// - FIELD FRAGMENT, `"<field>: <what would make it acceptable>"`, lowercase,
///   no terminal period. For errors the client can attach to a specific input:
///   the composed domain enums and `QrzCredentialError`. The prefix is
///   load-bearing UI data, not styling — the form pages render `detail` beside
///   the named field.
///
/// The split is whether there is a field to point at on the surface that
/// renders it, not which crate the error came from: the same `ProfileError`
/// takes the fragment register through `NetDefinitionError` and the sentence
/// register through the profile PUT.
///
/// Deliberately CAUSE-NEUTRAL. Leading with "a query parameter is unrecognised"
/// would be a lowercase implementation fragment with no remedy, where every
/// other user-facing string here is a capitalised sentence ending in an action;
/// it would also re-word a pre-existing failure mode, a mis-typed `?limit=abc`,
/// into an accusation of being unrecognised when that parameter is recognised.
/// All three reachable causes — an unrecognised key, a repeated single-valued
/// key, a wrong-typed value — are covered by one neutral sentence. The
/// per-cause contract lives in `docs/reference/http-api.md`, where a developer
/// reads it; this string is what a person reads.
///
/// "Missing" is not worth wording around: every field on every strict query
/// type is an `Option`, so a missing parameter is not reachable here.
pub(crate) fn strict_query_rejection(rejection: QueryRejection) -> ApiError {
    ApiError::validation(match rejection {
        QueryRejection::FailedToDeserializeQueryString(_) => {
            "That request wasn't something the server could read — reload the page and try again."
        }
        // `QueryRejection` is `#[non_exhaustive]` with one variant today; a
        // future axum minor must land in this contract rather than falling back
        // out to a plain-text response.
        _ => "Something about that request couldn't be read — reload the page and try again.",
    })
}

impl ApiError {
    /// Builds a [`ApiError::Validation`] from either fixed copy or a message
    /// composed at the call site.
    pub(crate) fn validation(detail: impl Into<Cow<'static, str>>) -> Self {
        ApiError::Validation(detail.into())
    }

    fn problem(&self) -> Problem {
        let (r#type, title, status, detail) = match self {
            ApiError::Validation(detail) => (
                "/errors/validation",
                "Request failed validation",
                StatusCode::BAD_REQUEST,
                Some(detail.clone().into_owned()),
            ),
            ApiError::MagicLinkExpired => (
                "/errors/magic-link-expired",
                "Magic link expired",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::MagicLinkConsumed => (
                "/errors/magic-link-consumed",
                "Magic link already used",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::MagicLinkInvalid => (
                "/errors/magic-link-invalid",
                "Magic link not recognized",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::Unauthenticated => (
                "/errors/unauthenticated",
                "Sign-in required",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::ConsentRequired => (
                "/errors/consent-required",
                "Consent required",
                StatusCode::FORBIDDEN,
                None,
            ),
            ApiError::ConsentVersionMismatch => (
                "/errors/consent-version-mismatch",
                "Terms version out of date",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::CallsignInvalid(detail) => (
                "/errors/callsign-invalid",
                "Callsign is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::CallsignTaken => (
                "/errors/callsign-taken",
                "Callsign already reserved",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::GridInvalid(detail) => (
                "/errors/grid-invalid",
                "Grid square is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::EmailUnverified => (
                "/errors/email-unverified",
                "Email must be verified first",
                StatusCode::FORBIDDEN,
                None,
            ),
            ApiError::EmailChangeExpired => (
                "/errors/email-change-expired",
                "Email-change link expired",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::EmailChangeConsumed => (
                "/errors/email-change-consumed",
                "Email-change link already used",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::EmailChangeInvalid => (
                "/errors/email-change-invalid",
                "Email-change link not recognized",
                StatusCode::UNAUTHORIZED,
                None,
            ),
            ApiError::EmailTaken => (
                "/errors/email-taken",
                "Email already in use",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::NetDefinitionInvalid(detail) | ApiError::NetConnectionsInvalid(_, detail) => {
                (
                    "/errors/net-definition-invalid",
                    "Net definition is not valid",
                    StatusCode::BAD_REQUEST,
                    Some(detail.clone()),
                )
            }
            ApiError::ScheduleInvalid(detail) => (
                "/errors/schedule-invalid",
                "Schedule is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::DiscoveryQueryInvalid(detail) => (
                "/errors/discovery-query-invalid",
                "Discovery query is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::DeliveryConfigInvalid(detail) => (
                "/errors/delivery-config-invalid",
                "Delivery config is not valid",
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(detail.clone()),
            ),
            ApiError::CallsignRequired => (
                "/errors/callsign-required",
                "Callsign required",
                StatusCode::FORBIDDEN,
                None,
            ),
            ApiError::Forbidden => (
                "/errors/forbidden",
                "Not allowed",
                StatusCode::FORBIDDEN,
                None,
            ),
            ApiError::NetDefinitionNotFound => (
                "/errors/net-definition-not-found",
                "Net definition not found",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::NetConnectionNotFound => (
                "/errors/net-connection-not-found",
                "That way of reaching the net isn't on this session",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::ConnectionCarriesNoFrequency => (
                "/errors/connection-has-no-frequency",
                "That way in has no frequency to change",
                StatusCode::UNPROCESSABLE_ENTITY,
                // voice: name the fault, do not recite the rule.
                // The operator picked a way people reach the net by NAME, and
                // there is nothing on it to retune.
                Some(
                    "That way in is reached by name, not by frequency — there's nothing on it to \
                     retune. Pick the radio way in instead."
                        .to_owned(),
                ),
            ),
            ApiError::SessionTransition(err) => {
                use netroll_domain::session_sm::SessionTransitionError as E;
                let (slug, title) = match err {
                    E::AlreadyLive => ("/errors/session-already-live", "Session is already live"),
                    E::StartAfterClose => (
                        "/errors/session-start-after-close",
                        "Session is already closed",
                    ),
                    E::CloseBeforeLive => (
                        "/errors/session-close-before-live",
                        "Session is not yet live",
                    ),
                    E::AlreadyClosed => (
                        "/errors/session-already-closed",
                        "Session is already closed",
                    ),
                    E::NotYetLive => ("/errors/session-not-yet-live", "Session is not yet live"),
                    // A roster/frequency mutation refused because
                    // the session is stalled (roster frozen, writes suspended).
                    E::SessionPaused => (
                        "/errors/session-paused",
                        "Session is paused (net control stalled)",
                    ),
                };
                (slug, title, StatusCode::CONFLICT, None)
            }
            ApiError::NetSessionNotFound => (
                "/errors/net-session-not-found",
                "Net session not found",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::ControlNotStalled => (
                "/errors/control-not-stalled",
                "Session is not stalled",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::HandoffTargetUnqualified => (
                "/errors/handoff-target-unqualified",
                "Handoff target is not a qualified net control station",
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
            ),
            ApiError::AccountBlocked => (
                "/errors/account-blocked",
                "You have been blocked from this session",
                StatusCode::FORBIDDEN,
                None,
            ),
            ApiError::NothingToBlock => (
                "/errors/nothing-to-block",
                "There is no account to block on this entry",
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
            ),
            ApiError::OwnerNotFound => (
                "/errors/owner-not-found",
                "Owner not found",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::LastOwner => (
                "/errors/last-owner",
                "A net must keep at least one owner",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::RoleInvalid(detail) => (
                "/errors/role-invalid",
                "Role is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::RoleGrantNotFound => (
                "/errors/role-grant-not-found",
                "Role grant not found",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::SignalReportInvalid(detail) => (
                "/errors/signal-report-invalid",
                "Signal report is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::StayingInvalid(detail) => (
                "/errors/staying-invalid",
                "Staying status is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::PrecedenceInvalid(detail) => (
                "/errors/precedence-invalid",
                "Precedence is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::TrafficInvalid(detail) => (
                "/errors/traffic-invalid",
                "Traffic count is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::NoteInvalid(detail) => (
                "/errors/note-invalid",
                "Note is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::ViaInvalid(detail) => (
                "/errors/via-invalid",
                "Way in is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::RelayedByInvalid(detail) => (
                "/errors/relayed-by-invalid",
                "Relaying station is not valid",
                StatusCode::BAD_REQUEST,
                Some(detail.clone()),
            ),
            ApiError::StaleVersion => (
                "/errors/stale-version",
                "This entry changed",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::LockHeld(callsign) => (
                "/errors/lock-held",
                "Another operator is editing this entry",
                StatusCode::CONFLICT,
                Some(callsign.clone()),
            ),
            ApiError::QrzCredentialsInvalid(detail) => (
                "/errors/qrz-credentials-invalid",
                "QRZ credentials are not valid",
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(detail.clone()),
            ),
            ApiError::CryptoUnavailable => (
                "/errors/crypto-unavailable",
                "Credential encryption is not configured on this instance",
                StatusCode::SERVICE_UNAVAILABLE,
                None,
            ),
            ApiError::AvatarInvalid(detail) => (
                "/errors/avatar-invalid",
                "Avatar image is not acceptable",
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(detail.clone()),
            ),
            ApiError::AvatarStorageUnavailable => (
                "/errors/avatar-storage-unavailable",
                "Avatar uploads are not configured on this instance",
                StatusCode::SERVICE_UNAVAILABLE,
                None,
            ),
            ApiError::MaxNetsPerUserReached => (
                "/errors/max-nets-per-user-reached",
                "Net limit reached",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::MaxOwnersPerNetReached => (
                "/errors/max-owners-per-net-reached",
                "Owner limit reached",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::AccountDisabled => (
                "/errors/account-disabled",
                "Account disabled",
                StatusCode::FORBIDDEN,
                None,
            ),
            ApiError::AbuseReportNotFound => (
                "/errors/abuse-report-not-found",
                "Abuse report not found",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::AccountNotFound => (
                "/errors/account-not-found",
                "Account not found",
                StatusCode::NOT_FOUND,
                None,
            ),
            ApiError::CannotDisableSelf => (
                "/errors/cannot-disable-self",
                "An admin cannot disable their own account",
                StatusCode::CONFLICT,
                None,
            ),
            ApiError::RateLimited { .. } => (
                "/errors/rate-limited",
                "Too many requests",
                StatusCode::TOO_MANY_REQUESTS,
                None,
            ),
            // 410 Gone, not 500 and not 404: the log existed, the operator is
            // remembering it correctly, and it is permanently unreadable. A 500
            // would say "try again"; a 404 would say "you misremembered".
            ApiError::UnreplayableLog => (
                "/errors/unreplayable-log",
                "This log can no longer be opened",
                StatusCode::GONE,
                // Name the FAULT, not the rule. No "decode error",
                // no "schema version", no "migration" — none of which is a fact
                // about anything the reader can see or change.
                Some(
                    "This net was run before a net could list more than one way to reach it, \
                     so its log can't be opened any more. Nothing you do will bring it back."
                        .to_owned(),
                ),
            ),
            ApiError::Database(_) | ApiError::Internal(_) => (
                "/errors/internal",
                "Something went wrong on our side",
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
            ),
        };
        Problem {
            r#type,
            title,
            status: status.as_u16(),
            detail,
            // Set from the variant rather than from the match arm above so
            // adding a member here can never silently change another error's
            // body: every arm that does not name an entry leaves it absent.
            connection_index: match self {
                ApiError::NetConnectionsInvalid(index, _) => *index,
                _ => None,
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Display, not Debug: the Display strings here are static, while
        // Debug would echo the upstream sqlx detail — which can carry row
        // values like email addresses into the logs.
        if let ApiError::Database(_) | ApiError::Internal(_) = &self {
            tracing::error!("request failed: {self}");
        }

        let problem = self.problem();
        let status = StatusCode::from_u16(problem.status).expect("status built from StatusCode");
        let mut response = (status, Json(problem)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/problem+json"),
        );
        if let ApiError::RateLimited { retry_after_secs } = self {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                header::HeaderValue::from_str(&retry_after_secs.to_string())
                    .expect("integer is a valid header value"),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_transition_errors_map_to_distinct_409_slugs() {
        use netroll_domain::session_sm::SessionTransitionError as E;
        let cases = [
            (E::AlreadyLive, "/errors/session-already-live"),
            (E::StartAfterClose, "/errors/session-start-after-close"),
            (E::CloseBeforeLive, "/errors/session-close-before-live"),
            (E::AlreadyClosed, "/errors/session-already-closed"),
            (E::NotYetLive, "/errors/session-not-yet-live"),
        ];
        for (err, slug) in cases {
            let problem = ApiError::from(err).problem();
            assert_eq!(problem.status, 409, "every transition error is a 409");
            assert_eq!(problem.r#type, slug);
        }
        // Every variant has its OWN slug — no two collide.
        let slugs: std::collections::HashSet<_> = cases.iter().map(|(_, s)| *s).collect();
        assert_eq!(slugs.len(), cases.len(), "slugs are distinct per variant");
    }

    #[test]
    fn net_session_not_found_is_a_404() {
        let problem = ApiError::NetSessionNotFound.problem();
        assert_eq!(problem.status, 404);
        assert_eq!(problem.r#type, "/errors/net-session-not-found");
    }

    #[test]
    fn an_invalid_role_is_a_400_carrying_its_detail() {
        let problem = ApiError::RoleInvalid("bogus".to_owned()).problem();
        assert_eq!(problem.status, 400);
        assert_eq!(problem.r#type, "/errors/role-invalid");
        assert_eq!(problem.detail.as_deref(), Some("bogus"));
    }

    #[test]
    fn an_invalid_signal_report_is_a_400_carrying_its_detail() {
        let problem = ApiError::SignalReportInvalid("value is too long".to_owned()).problem();
        assert_eq!(problem.status, 400);
        assert_eq!(problem.r#type, "/errors/signal-report-invalid");
        assert_eq!(problem.detail.as_deref(), Some("value is too long"));
    }

    #[test]
    fn an_invalid_staying_token_is_a_400_carrying_its_detail() {
        let problem = ApiError::StayingInvalid("maybe".to_owned()).problem();
        assert_eq!(problem.status, 400);
        assert_eq!(problem.r#type, "/errors/staying-invalid");
        assert_eq!(problem.detail.as_deref(), Some("maybe"));
    }

    #[test]
    fn an_invalid_note_is_a_400_carrying_its_detail() {
        let problem = ApiError::NoteInvalid("value is too long".to_owned()).problem();
        assert_eq!(problem.status, 400);
        assert_eq!(problem.r#type, "/errors/note-invalid");
        assert_eq!(problem.detail.as_deref(), Some("value is too long"));
    }

    #[test]
    fn a_stale_version_is_a_409_with_its_slug() {
        let problem = ApiError::StaleVersion.problem();
        assert_eq!(problem.status, 409);
        assert_eq!(problem.r#type, "/errors/stale-version");
    }

    #[test]
    fn a_held_lock_is_a_409_naming_the_holder_in_detail() {
        let problem = ApiError::LockHeld("W1AW".to_owned()).problem();
        assert_eq!(problem.status, 409);
        assert_eq!(problem.r#type, "/errors/lock-held");
        assert_eq!(problem.detail.as_deref(), Some("W1AW"));
    }

    #[test]
    fn an_invalid_delivery_config_is_a_422_carrying_its_field_named_detail() {
        let problem =
            ApiError::DeliveryConfigInvalid("webhook url: must use https".to_owned()).problem();
        assert_eq!(problem.status, 422);
        assert_eq!(problem.r#type, "/errors/delivery-config-invalid");
        assert_eq!(
            problem.detail.as_deref(),
            Some("webhook url: must use https")
        );
    }

    #[test]
    fn a_missing_role_grant_is_a_404() {
        let problem = ApiError::RoleGrantNotFound.problem();
        assert_eq!(problem.status, 404);
        assert_eq!(problem.r#type, "/errors/role-grant-not-found");
    }

    #[test]
    fn invalid_qrz_credentials_are_a_422_carrying_their_detail() {
        let problem = ApiError::QrzCredentialsInvalid("value is too long".to_owned()).problem();
        assert_eq!(problem.status, 422);
        assert_eq!(problem.r#type, "/errors/qrz-credentials-invalid");
        assert_eq!(problem.detail.as_deref(), Some("value is too long"));
    }

    #[test]
    fn max_nets_per_user_reached_is_a_409_with_its_slug_and_no_detail() {
        let problem = ApiError::MaxNetsPerUserReached.problem();
        assert_eq!(problem.status, 409);
        assert_eq!(problem.r#type, "/errors/max-nets-per-user-reached");
        assert_eq!(problem.detail, None);
    }

    #[test]
    fn max_owners_per_net_reached_is_a_409_with_its_slug_and_no_detail() {
        let problem = ApiError::MaxOwnersPerNetReached.problem();
        assert_eq!(problem.status, 409);
        assert_eq!(problem.r#type, "/errors/max-owners-per-net-reached");
        assert_eq!(problem.detail, None);
    }

    #[test]
    fn crypto_unavailable_is_a_503_with_its_slug_and_no_detail() {
        let problem = ApiError::CryptoUnavailable.problem();
        assert_eq!(problem.status, 503);
        assert_eq!(problem.r#type, "/errors/crypto-unavailable");
        assert_eq!(problem.detail, None);
    }

    #[test]
    fn a_disabled_account_is_a_403_with_its_slug_and_no_detail() {
        let problem = ApiError::AccountDisabled.problem();
        assert_eq!(problem.status, 403);
        assert_eq!(problem.r#type, "/errors/account-disabled");
        assert_eq!(problem.detail, None);
    }

    #[test]
    fn admin_not_found_errors_are_404s_with_their_slugs() {
        let report = ApiError::AbuseReportNotFound.problem();
        assert_eq!(report.status, 404);
        assert_eq!(report.r#type, "/errors/abuse-report-not-found");
        let account = ApiError::AccountNotFound.problem();
        assert_eq!(account.status, 404);
        assert_eq!(account.r#type, "/errors/account-not-found");
    }

    #[test]
    fn cannot_disable_self_is_a_409_with_its_slug_and_no_detail() {
        // Mirrors `LastOwner`'s "keep at least one" 409 shape.
        let problem = ApiError::CannotDisableSelf.problem();
        assert_eq!(problem.status, 409);
        assert_eq!(problem.r#type, "/errors/cannot-disable-self");
        assert_eq!(problem.detail, None);
    }

    #[test]
    fn the_logged_form_of_a_database_error_carries_no_upstream_detail() {
        // IntoResponse logs `{self}` (Display); the upstream message — which
        // here stands in for sqlx detail embedding a row's email — must not
        // survive into that form.
        let err = ApiError::Database(sqlx::Error::Configuration(
            "connection blew up near op@example.com".into(),
        ));
        assert_eq!(err.to_string(), "database error");
    }
}
