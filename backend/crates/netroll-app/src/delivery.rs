// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! On-close delivery: the summary email, the signed webhook and the Discord
//! announcement a closed session owes, run for legs `NetSessionRepo::close`
//! already made durable. Spawning is a latency optimisation, never the
//! durability mechanism — an unrun or half-run leg is recovered by
//! [`crate::delivery_sweeper`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use netroll_adapters::pg::accounts::AccountRepo;
use netroll_adapters::pg::delivery_configs::{DeliveryConfig, DeliveryConfigRepo};
use netroll_adapters::pg::delivery_jobs::{
    DESTINATION_DISCORD, DESTINATION_EMAIL, DESTINATION_WEBHOOK, DeliveryJob, DeliveryJobRepo,
    TerminalState,
};
use netroll_adapters::pg::net_sessions::{NetSessionRepo, NetSessionRow};
use netroll_adapters::pg::session_events::SessionEventLog;
use netroll_domain::check_in::CheckInSource;
use netroll_domain::egress::{Egress, EgressError, EgressMethod, EgressRequest};
use netroll_domain::export;
use netroll_domain::fold::{RosterEntry, SessionState, replay};
use netroll_domain::net::delivery::sign_webhook;
use netroll_domain::net::wire::{
    NetConnectionWire, ViaDisplay, ViaWire, connection_label, connection_name, resolve_via,
    via_label, via_of,
};
use netroll_domain::ports::{Clock, MailAttachment, MailError, Mailer, NetSummaryMail};
use serde::Serialize;
use uuid::Uuid;

use crate::delivery_sweeper::{
    DELIVERY_LEASE_MILLIS, DELIVERY_RETRY_MAX_MILLIS, MAX_JOB_ATTEMPTS, retry_delay_millis,
};
use crate::http::rfc3339;

/// Attempts within ONE claim of a leg — the inner retry ladder, for a blip
/// inside a single run. Exhausting it reschedules the durable row on the outer
/// ladder ([`crate::delivery_sweeper`]) rather than losing the delivery.
const MAX_ATTEMPTS: u32 = 3;

/// Backoff between inner-ladder retries. Small enough that an integration test
/// polls for completion rather than sleeping.
const RETRY_BACKOFF: Duration = Duration::from_millis(25);

/// Bounds in-flight deliveries. The caller drops the `JoinHandle`, so without
/// this a burst — the presence sweep auto-closing many stalled sessions in one
/// tick — spawns one task per session, each holding a pool connection and
/// sockets for the whole retry budget. The permit is taken INSIDE the task, so
/// spawning still returns immediately.
pub const MAX_CONCURRENT_DELIVERIES: usize = 16;

/// Net identity on the webhook payload.
///
/// There is no `band`/`mode`: they described one way to reach a net that may
/// have several, and are absent entirely for an internet-only net. The whole
/// connection set rides the payload's own `connections`, so a receiver that
/// wants a band reads `connections[0].band` and learns when there isn't one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookNet {
    title: String,
}

/// One roster station on the outbound webhook contract.
///
/// It drops only the fold-internal fields — the row id, the concurrency version
/// and the correction history — named on the `// Dropped:` arms in
/// [`build_webhook_payload`], whose exhaustive destructure makes omitting a new
/// one a compile error.
///
/// Adding a field is additive, not breaking, and
/// `docs/reference/webhook-payload.md` publishes that promise. Keep
/// `skip_serializing_if` on every optional: it is what makes a payload with the
/// field unset byte-identical to one from before the field existed. There is no
/// payload version in any disguise; `definition_version` is the net
/// definition's.
///
/// The field set matches the CSV header plus `viaLabel`, asserted by
/// `the_webhook_roster_entry_key_set_matches_the_csv_header_field_for_field`.
/// Four renderings differ deliberately: camelCase against `snake_case`, an
/// omitted key against an empty cell, real JSON scalars against stringified
/// ones, and a STRUCTURED `via` with its label beside it against the CSV's
/// label alone.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookRosterEntry {
    callsign: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    /// The Maidenhead locator: machine-meaningful, and deliberately separate
    /// from the free-text `location` a human reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    grid: Option<String>,
    source: String,
    /// The staff logger's callsign; a `self` entry has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    entering_operator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal_report: Option<String>,
    staying: String,
    precedence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    traffic: Option<i64>,
    worked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    /// The PUBLIC note. The webhook is the net's own outbound record, so it
    /// carries BOTH notes; it is not a public surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    public_note: Option<String>,
    /// WHICH way in this station arrived on, omitted when nobody recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<ViaWire>,
    /// The SAME `via`, resolved to the label a person reads.
    ///
    /// The webhook is the one surface carrying both, because it is the only
    /// machine contract with no browser behind it: handed an id alone, every
    /// receiver would re-derive the label and there would be as many answers as
    /// integrations. Present whenever `via` is, including an unresolvable one,
    /// which says so in words.
    ///
    /// The frequency inside the label is the one that connection was on WHEN
    /// THIS STATION CHECKED IN, while the top-level `connections` reports where
    /// each way in ENDED. For a station worked before a mid-session move the two
    /// legitimately differ.
    #[serde(skip_serializing_if = "Option::is_none")]
    via_label: Option<String>,
    /// WHICH STATION passed this check-in's traffic, omitted when nobody did.
    ///
    /// There is deliberately no `relayedByLabel` sibling: a callsign is already
    /// the text a person reads, so a label field would imply a resolution step
    /// that does not exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    relayed_by: Option<String>,
    checked_in_at: String,
}

/// The outbound webhook payload. Built from the SAME fold every other summary
/// uses and serialized ONCE; the HMAC signs those exact bytes. serde's JSON is
/// structurally injection-safe, so the body carries no formula-injection
/// vector; the spreadsheet concern lives in the CSV attachment.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookPayload {
    net: WebhookNet,
    session_id: Uuid,
    definition_id: Uuid,
    definition_version: i32,
    /// Every way the net was reachable at close, in the owner's order: the
    /// frozen snapshot with each mid-session move overlaid. These ids are the
    /// ones each check-in's `via` refers to.
    connections: Vec<NetConnectionWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<i64>,
    participant_count: usize,
    roster: Vec<WebhookRosterEntry>,
}

/// The local part of the summary email's `Message-ID`, derived from the session
/// id so every attempt is the same message to a receiver. Dot-atom characters
/// only — a `:` is not RFC 5322 `atext`, which is why this is not
/// [`webhook_delivery_id`] verbatim.
fn summary_message_id(session_id: Uuid) -> String {
    format!("net-session-close.{session_id}")
}

/// The webhook's idempotency key: identical across every retry of this
/// session's delivery, so a receiver dedupes and a retry never double-delivers.
fn webhook_delivery_id(session_id: Uuid) -> String {
    format!("net-session-close:{session_id}")
}

/// Whether an egress failure is worth retrying. A `BlockedAddress` at delivery
/// time is a DNS rebind caught in the act and must never be retried; the retry
/// would re-resolve to the same denied target.
///
/// Matched EXHAUSTIVELY, no `_` arm: a new `EgressError` variant must fail to
/// compile here until a human classifies it, rather than defaulting into one
/// class silently.
fn is_transient_egress(err: &EgressError) -> bool {
    match err {
        EgressError::Timeout | EgressError::Transport(_) => true,
        EgressError::BlockedAddress
        | EgressError::InvalidUrl
        | EgressError::NotHttps
        | EgressError::TooManyRedirects
        | EgressError::ResponseTooLarge => false,
    }
}

/// How a completed HTTP exchange classifies for retry.
#[derive(Debug, PartialEq, Eq)]
enum StatusClass {
    Success,
    Transient,
    Permanent,
}

/// 2xx is success; 5xx and 429 are transient, because a rate limit is a request
/// to resend slower rather than a refusal; every other 4xx is permanent.
fn classify_status(status: u16) -> StatusClass {
    if (200..300).contains(&status) {
        StatusClass::Success
    } else if status == 429 || (500..600).contains(&status) {
        StatusClass::Transient
    } else {
        StatusClass::Permanent
    }
}

/// Every per-net delivery destination kind — the ONE declaration three
/// mechanisms read.
///
/// [`Self::is_armed`] and [`Self::from_job_destination`] match it exhaustively,
/// and `tests/delivery_destination_kinds.rs` parses this variant list out of
/// the file's text, so a fourth kind cannot be added without a human saying what
/// "configured" means for it, what its stored `destination` parses to, and how
/// it behaves independently and durably.
///
/// It exists because the "is delivery off?" predicate was duplicated, and the
/// inline copy is the one a new destination silently omits: a net whose only
/// target was Discord returned early and delivered nothing, with the only
/// covering test asserting an absence and staying green.
///
/// A fourth kind also needs a leg planned in
/// `netroll_adapters::pg::delivery_jobs::plan_in_tx` — the one site the
/// compiler cannot force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryDestinationKind {
    Email,
    Webhook,
    Discord,
}

impl DeliveryDestinationKind {
    /// Every declared kind. The register asserts this array covers the enum's
    /// full variant list, parsed off the declaration above.
    const ALL: [Self; 3] = [Self::Email, Self::Webhook, Self::Discord];

    /// Whether `config` arms this destination. Matched exhaustively, no `_` arm.
    fn is_armed(self, config: &DeliveryConfig) -> bool {
        match self {
            Self::Email => !config.emails.is_empty(),
            Self::Webhook => config.webhook_url.is_some(),
            Self::Discord => config.discord_webhook_url.is_some(),
        }
    }

    /// The kind a stored `net_delivery_jobs.destination` names, or `None` for a
    /// value this build does not know — a row written by a newer deploy that a
    /// rollback is now reading.
    pub(crate) fn from_job_destination(destination: &str) -> Option<Self> {
        match destination {
            DESTINATION_EMAIL => Some(Self::Email),
            DESTINATION_WEBHOOK => Some(Self::Webhook),
            DESTINATION_DISCORD => Some(Self::Discord),
            _ => None,
        }
    }
}

/// Whether a stored config arms NO destination at all, derived from
/// [`DeliveryDestinationKind::ALL`] rather than hand-spelled per target.
fn is_delivery_off(config: &DeliveryConfig) -> bool {
    !DeliveryDestinationKind::ALL
        .iter()
        .any(|kind| kind.is_armed(config))
}

/// Combines the storage results of the two fallible legs.
///
/// Both have already finished by the time this runs; the only question is which
/// errors REACH the caller. Discarding one with `let (_, webhook_result, _) =
/// tokio::join!(…)` drops the Discord leg's `sqlx::Error`, and a storage fault
/// there then looks indistinguishable from a delivery that was never
/// configured.
///
/// The first error wins, which is the webhook's, so the Discord leg's is
/// dropped whenever both fail. That is acceptable only because the Discord leg
/// logs its own storage failure at its own site — see the `discord_target`
/// error arm in [`DeliveryService::deliver_discord`], which once did not, and
/// then the fault was recorded nowhere at all.
fn combine_leg_storage_results(
    webhook: Result<(), sqlx::Error>,
    discord: Result<(), sqlx::Error>,
) -> Result<(), sqlx::Error> {
    webhook.and(discord)
}

/// The summary duration in whole seconds when both endpoints are known. The
/// close-time clamp guarantees it is never negative.
fn duration_seconds(folded: &SessionState) -> Option<i64> {
    match (folded.started_at, folded.closed_at) {
        (Some(started), Some(closed)) => Some((closed.saturating_sub(started) / 1000) as i64),
        _ => None,
    }
}

/// Projects the folded session onto the outbound webhook payload.
/// `entering_ops` resolves a staff entry's logger id to a callsign; a `self`
/// entry always projects a blank entering operator.
fn build_webhook_payload(
    session_id: Uuid,
    row: &NetSessionRow,
    folded: &SessionState,
    entering_ops: &BTreeMap<Uuid, String>,
) -> WebhookPayload {
    // Where each way in ENDED — deliberately NOT the set each entry's `via`
    // resolves against, which is the snapshot as at that entry's own seq.
    let connections = folded.live_connections(&row.definition_snapshot.connections);
    let snapshot = &row.definition_snapshot.connections;
    let roster = folded
        .roster
        .iter()
        .map(|entry| {
            // EXHAUSTIVE destructure, no `..` rest pattern: a new `RosterEntry`
            // field must fail to compile HERE until a human decides whether this
            // machine-parsed projection carries it.
            let RosterEntry {
                callsign,
                name,
                location,
                grid,
                source,
                added_by,
                signal_report,
                staying,
                precedence,
                traffic,
                worked,
                notes,
                public_note,
                via,
                relayed_by,
                added_at,
                // READ, not emitted: it selects the set `viaLabel` resolves
                // against. A log cursor is not a receiver fact.
                added_seq,
                // Dropped: fold-internal, projected by NO export surface — the
                // roster row's own id, its optimistic-concurrency version, and the
                // derived per-field correction history.
                check_in_id: _,
                version: _,
                corrections: _,
            } = entry;
            // The set as it stood WHEN this station checked in, so the label's
            // frequency is the one it was worked on.
            let as_at = folded.connections_at(*added_seq, snapshot);
            let entering_operator = if *source == CheckInSource::Staff {
                added_by.and_then(|id| entering_ops.get(&id)).cloned()
            } else {
                None
            };
            WebhookRosterEntry {
                callsign: callsign.as_str().to_owned(),
                name: name.as_ref().map(|n| n.as_str().to_owned()),
                location: location.as_ref().map(|l| l.as_str().to_owned()),
                grid: grid.as_ref().map(|g| g.as_str().to_owned()),
                source: source.as_str().to_owned(),
                entering_operator,
                signal_report: signal_report.as_ref().map(|r| r.as_str().to_owned()),
                staying: staying.as_str().to_owned(),
                precedence: precedence.as_str().to_owned(),
                traffic: traffic.map(|t| t.get() as i64),
                worked: *worked,
                notes: notes.as_ref().map(|n| n.as_str().to_owned()),
                public_note: public_note.as_ref().map(|n| n.as_str().to_owned()),
                // BOTH halves, through the one resolver: a `NotRecorded` `via`
                // omits both keys, and is NOT filled in from the ADIF-export
                // connection the way the `.adi` for this entry legitimately is.
                via: via.as_ref().map(via_of),
                via_label: via_label(&resolve_via(via.as_ref(), &as_at)),
                relayed_by: relayed_by.as_ref().map(|c| c.as_str().to_owned()),
                checked_in_at: rfc3339(*added_at),
            }
        })
        .collect::<Vec<_>>();
    WebhookPayload {
        net: WebhookNet {
            title: row.definition_snapshot.title.clone(),
        },
        session_id,
        definition_id: folded.definition_id.unwrap_or(row.definition_id),
        definition_version: folded.definition_version.unwrap_or(row.definition_version),
        connections,
        started_at: folded.started_at.map(rfc3339),
        closed_at: folded.closed_at.map(rfc3339),
        duration_seconds: duration_seconds(folded),
        participant_count: folded.roster.len(),
        roster,
    }
}

// --- The Discord announcement -----------------------------------
//
// Discord's documented embed limits, read 2026-08-27 from
// `https://docs.discord.com/developers/resources/message#embed-object-embed-limits`:
// title 256, description 4096, 25 fields, field.name 256, field.value 1024,
// footer.text 2048, and a COMBINED 6000 characters across every embed in one
// message. Violating any of them returns 400 with `code: 50035`.
//
// The caps below are NetRoll's own, an order of magnitude under those. The
// PRIMARY bound is the SHAPE, not the clamp: nothing in this embed scales with
// roster size, so no net a user can run moves the total. The clamps cover the
// residual case — a stored title that predates a validation change, or a
// hand-edited row.

/// Cap on the embed title: the same number as
/// [`netroll_domain::net::validation::MAX_TITLE_CHARS`], the bound the title was
/// validated against on write. The `const _` assertion below is the link, and
/// it fails the BUILD if either number moves.
const DISCORD_TITLE_CHARS: usize = 120;
/// Cap on the embed description (fixed prose plus the session link).
const DISCORD_DESCRIPTION_CHARS: usize = 512;
/// Bound on the number of embed fields — CHECKED against, never applied as a
/// truncation. Headroom over the five the summary needs, so an adversarial fold
/// that made the list scale with the roster overshoots it and reds instead of
/// being quietly clamped.
const DISCORD_FIELD_COUNT: usize = 8;
/// Cap on an embed field's name. Labels are fixed literals.
const DISCORD_FIELD_NAME_CHARS: usize = 32;
/// Cap on an embed field's value for the FIXED-SHAPE slots — a count, an
/// instant, a duration — every one a scalar this builder formats itself.
///
/// Deliberately NOT the cap on `Ways in`, which is a LIST; see
/// [`DISCORD_WAYS_IN_VALUE_CHARS`]. Clamping that list to a scalar's cap cut it
/// mid-word and dropped ways to reach the net from the one surface whose
/// purpose is naming them.
const DISCORD_FIELD_VALUE_CHARS: usize = 64;
/// Cap on the `Ways in` field's value — the ONE slot whose content scales, and
/// with the owner's connection list rather than with the roster.
///
/// Set to Discord's own documented field-value limit, because that is the real
/// bound and pretending to a smaller one is what dropped content silently. A
/// list too long even for this is cut by WHOLE LINE with the dropped count
/// stated — see [`ways_in_value`] — never with a bare `…`.
const DISCORD_WAYS_IN_VALUE_CHARS: usize = DISCORD_LIMIT_FIELD_VALUE;
/// Cap on the embed footer text. Fixed attribution.
const DISCORD_FOOTER_CHARS: usize = 128;
/// Discord's aggregate limit across every counted slot of every embed in ONE
/// message. Not a cap this builder applies — the const assertion below proves
/// the declared caps cannot reach it.
const DISCORD_AGGREGATE_CHARS: usize = 6000;

/// The largest message this builder can produce as Discord counts it.
///
/// `Ways in` is counted ONCE at its own larger cap because
/// [`build_discord_message`] pushes exactly one; every other slot is counted at
/// the scalar cap. Sizing all eight at the ways-in cap would overshoot the 6000
/// aggregate and fail the build for a shape this builder cannot produce.
const DECLARED_CAP_SUM: usize = DISCORD_TITLE_CHARS
    + DISCORD_DESCRIPTION_CHARS
    + (DISCORD_FIELD_NAME_CHARS + DISCORD_WAYS_IN_VALUE_CHARS)
    + (DISCORD_FIELD_COUNT - 1) * (DISCORD_FIELD_NAME_CHARS + DISCORD_FIELD_VALUE_CHARS)
    + DISCORD_FOOTER_CHARS;

// Discord's own per-slot limits, declared rather than left in prose so every
// cap above can be proven below its counterpart.
const DISCORD_LIMIT_TITLE: usize = 256;
const DISCORD_LIMIT_DESCRIPTION: usize = 4096;
const DISCORD_LIMIT_FIELD_COUNT: usize = 25;
const DISCORD_LIMIT_FIELD_NAME: usize = 256;
const DISCORD_LIMIT_FIELD_VALUE: usize = 1024;
const DISCORD_LIMIT_FOOTER: usize = 2048;

// Checked at COMPILE time in the shipped artifact rather than only in a test:
// widening one cap past its Discord counterpart, or past the aggregate budget,
// fails the BUILD, and holds for a consumer who never runs the tests.
const _: () = assert!(
    DECLARED_CAP_SUM < DISCORD_AGGREGATE_CHARS,
    "the declared per-slot caps must not be able to reach Discord's 6000-character aggregate"
);
const _: () = assert!(DISCORD_TITLE_CHARS <= DISCORD_LIMIT_TITLE);
// Every other cap here is a claim about Discord's documented limits; this one
// is a claim about another NetRoll module, so it needs its own assertion.
const _: () = assert!(
    DISCORD_TITLE_CHARS == netroll_domain::net::validation::MAX_TITLE_CHARS,
    "the embed title cap IS the net title's write-time validation bound"
);
const _: () = assert!(DISCORD_DESCRIPTION_CHARS <= DISCORD_LIMIT_DESCRIPTION);
const _: () = assert!(DISCORD_FIELD_COUNT <= DISCORD_LIMIT_FIELD_COUNT);
const _: () = assert!(DISCORD_FIELD_NAME_CHARS <= DISCORD_LIMIT_FIELD_NAME);
const _: () = assert!(DISCORD_FIELD_VALUE_CHARS <= DISCORD_LIMIT_FIELD_VALUE);
const _: () = assert!(DISCORD_WAYS_IN_VALUE_CHARS <= DISCORD_LIMIT_FIELD_VALUE);
const _: () = assert!(DISCORD_FOOTER_CHARS <= DISCORD_LIMIT_FOOTER);

/// The longest `retry_after` this deliverer will WAIT on a Discord 429 before
/// abandoning the announcement.
///
/// A per-channel Discord rate limit is sub-second, so a multi-second
/// `retry_after` indicates the GLOBAL limit, where waiting is unlikely to help a
/// once-per-close post. The value is also attacker-influenceable — the
/// configured URL is whatever the owner pasted — and a waiting task holds one of
/// [`MAX_CONCURRENT_DELIVERIES`] permits plus a pool connection for its whole
/// lifetime.
const MAX_HONOURED_RETRY_AFTER: Duration = Duration::from_secs(5);

/// Keeps at most `cap` CODE POINTS of `text`, marking a cut with `…`.
///
/// Code points, never `String::len()`: Discord counts characters, and a
/// byte-based cut both rejects valid non-ASCII text and can slice mid-sequence.
fn clamp_chars(text: &str, cap: usize) -> String {
    if cap == 0 {
        return String::new();
    }
    if text.chars().count() <= cap {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(cap - 1).collect();
    out.push('…');
    out
}

/// One field of the announcement embed.
#[derive(Serialize)]
struct DiscordEmbedField {
    name: String,
    value: String,
    inline: bool,
}

/// The announcement embed's footer.
#[derive(Serialize)]
struct DiscordEmbedFooter {
    text: String,
}

/// The single embed of a NetRoll close announcement.
#[derive(Serialize)]
struct DiscordEmbed {
    title: String,
    /// Makes the title clickable. Not counted toward Discord's 6000 aggregate.
    url: String,
    description: String,
    fields: Vec<DiscordEmbedField>,
    footer: DiscordEmbedFooter,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

/// The Discord webhook message body: ONE embed, no `content`.
#[derive(Serialize)]
struct DiscordMessage {
    embeds: Vec<DiscordEmbed>,
}

/// The description. The link is never PARTIALLY cut: it is carried whole at the
/// end, or omitted entirely.
///
/// The prose is the only truncatable region, clamped to whatever the cap leaves
/// after the link. The residual branch is real: `resolve_public_base_url` puts
/// no length bound on `PUBLIC_BASE_URL`, so a ~470-character one leaves no room
/// for the link. Clamping the link there produced a description ending
/// `…/live/00000000-0000-0000-0000…`, and a broken URL in a chat channel is
/// worse than none. The whole link is still on `embed.url`, which is never
/// clamped.
fn discord_description(link: &str) -> String {
    const PROSE: &str = "This net's session has closed. Full log:";
    let reserved = link.chars().count() + 1;
    let prose_cap = DISCORD_DESCRIPTION_CHARS.saturating_sub(reserved);
    if prose_cap == 0 {
        // Clamped despite being a fixed literal, like `embed_field`'s labels: a
        // later-widened source cannot exceed the cap for want of a clamp here.
        return clamp_chars(PROSE, DISCORD_DESCRIPTION_CHARS);
    }
    format!("{} {}", clamp_chars(PROSE, prose_cap), link)
}

/// Builds the Discord announcement for a just-closed session.
///
/// It carries NO per-station data: not a truncated roster, not the first N
/// callsigns, not a `fields` array over `folded.roster` — a count, a window and
/// a link back. A receiving channel READS this message, it does not parse it,
/// and nobody should later "restore parity" by serializing a roster into an
/// embed. `folded.roster.len()` is a count, not a projection, and is the only
/// roster fact here; this function deliberately does not destructure a
/// `RosterEntry`, and if an edit makes it do so,
/// `tests/roster_projection_sites.rs` reds — the fix is to stop destructuring.
///
/// Pure over the fold, the row and the public base URL, so the bounds walker in
/// this module's tests can hammer it with adversarial inputs without any I/O.
fn build_discord_message(
    session_id: Uuid,
    row: &NetSessionRow,
    folded: &SessionState,
    public_base_url: &str,
) -> DiscordMessage {
    // The public, account-less live view. It applies no lifecycle filter, so a
    // CLOSED session still resolves — which matters, since the session is
    // closed by the time this posts — and it projects the redacted view, so
    // pasting the link into a channel exposes that and nothing more.
    let link = format!("{public_base_url}/live/{session_id}");

    // ONE `Ways in` field rather than a frequency and a band/mode pair: a
    // session no longer has one frequency, and band/mode belong to individual
    // connections. A fixed number of FIELDS — nothing here scales with roster
    // size — but this field's VALUE scales with the owner's connection list,
    // which is why it has its own cap and by-the-line shortening.
    let mut fields = vec![
        ways_in_field(&connection_lines(
            &folded.live_connections(&row.definition_snapshot.connections),
        )),
        embed_field("Check-ins", folded.roster.len().to_string()),
    ];
    if let Some(started) = folded.started_at {
        fields.push(embed_field("Started", rfc3339(started)));
    }
    if let Some(secs) = duration_seconds(folded) {
        fields.push(embed_field("Duration", format!("{secs} seconds")));
    }
    // NO `fields.truncate(DISCORD_FIELD_COUNT)` here, deliberately. Every push
    // above is unconditional or guarded by an `Option`, so this list is at most
    // five entries by CONSTRUCTION. A truncate would CLAMP a future edit that
    // did make the list scale with the roster, silencing the one assertion that
    // catches it —
    // `the_announcement_stays_inside_every_declared_cap_on_adversarial_input`
    // builds against a 500-station fold and asserts the field count.

    DiscordMessage {
        embeds: vec![DiscordEmbed {
            title: clamp_chars(&row.definition_snapshot.title, DISCORD_TITLE_CHARS),
            url: link.clone(),
            description: discord_description(&link),
            fields,
            footer: DiscordEmbedFooter {
                text: clamp_chars("Posted by NetRoll by N1CCK", DISCORD_FOOTER_CHARS),
            },
            timestamp: folded.closed_at.map(rfc3339),
        }],
    }
}

/// One line per way the net was reachable, in the owner's order: what kind of
/// way it is, and the identifier or frequency a reader needs to get on it.
///
/// Human prose for two human surfaces — the summary email's meta block and the
/// Discord embed. The machine-readable answer is the webhook's `connections`
/// array; this carries only what someone reading a sentence can act on.
///
/// The name comes from [`netroll_domain::net::wire::connection_label`], the ONE
/// backend answer to "what is this connection called", so the server and the
/// browser cannot disagree about a net owner's own connection.
fn connection_lines(connections: &[NetConnectionWire]) -> Vec<String> {
    connections.iter().map(connection_label).collect()
}

/// How many check-ins came in on each way, in the owner's order, plus a line
/// each for the three facts that are not a listed connection.
///
/// FOUR buckets off ONE exhaustive match on [`ViaDisplay`], because they are
/// four different facts: attributing "not recorded" to the way the net leads
/// with is a lie, and a check-in pointing at a way the session has LOST is a
/// defect an owner can act on, where an operator typing an unlisted way is the
/// feature.
///
/// Every bucket is `> 0`-gated, or a closing net's summary gains a block of
/// zeroes restating the `Reachable on:` lines above it. Those list the ways in
/// whether or not anyone used them; this block is about who did.
///
/// The count line carries NO frequency: `12 on HF — 14.250 MHz` claims something
/// about each of the twelve that a mid-session move makes false for the earlier
/// ones. It goes through `connection_name` instead, while `2 on EchoLink —
/// 12345` keeps its node, which is not a number that changed under the stations
/// it counts. Accepted consequence: a net with two HF ways in emits two
/// `N on HF` lines.
fn connection_counts(folded: &SessionState, connections: &[NetConnectionWire]) -> Vec<String> {
    let mut on_connection: BTreeMap<Uuid, usize> = BTreeMap::new();
    let (mut unresolvable, mut unlisted, mut unrecorded) = (0usize, 0usize, 0usize);
    for entry in &folded.roster {
        // ONE answer to "which bucket is this check-in in", for every bucket:
        // comparing `entry.via` directly for some and resolving for others put
        // two answers to one question three lines apart.
        match resolve_via(entry.via.as_ref(), connections) {
            ViaDisplay::Resolved(connection) => {
                *on_connection.entry(connection.id).or_default() += 1;
            }
            ViaDisplay::Unresolvable => unresolvable += 1,
            ViaDisplay::Unlisted(_) => unlisted += 1,
            ViaDisplay::NotRecorded => unrecorded += 1,
        }
    }
    let mut lines = Vec::new();
    // The owner's order, from the connection list rather than the tally.
    for connection in connections {
        let count = on_connection.get(&connection.id).copied().unwrap_or(0);
        if count > 0 {
            lines.push(format!("{count} on {}", connection_name(connection)));
        }
    }
    if unlisted > 0 {
        lines.push(format!("{unlisted} on a way this net does not list"));
    }
    if unresolvable > 0 {
        lines.push(format!(
            "{unresolvable} on a way in this net no longer lists"
        ));
    }
    if unrecorded > 0 {
        lines.push(format!("{unrecorded} with no way in recorded"));
    }
    lines
}

/// The `Ways in` field, whose value is a LIST and cannot share the scalar
/// slots' clamp.
fn ways_in_field(lines: &[String]) -> DiscordEmbedField {
    DiscordEmbedField {
        name: clamp_chars("Ways in", DISCORD_FIELD_NAME_CHARS),
        value: ways_in_value(lines),
        inline: true,
    }
}

/// Joins the ways-in lines into one embed value, keeping WHOLE lines and saying
/// how many did not fit.
///
/// [`clamp_chars`] is wrong for this slot: a mid-word `…` on a list of ways to
/// reach a net reads as the end of the list, so a reader cannot tell that a way
/// was dropped, let alone how many. The list genuinely can exceed Discord's
/// 1024-character field limit, so some list must be shortened — visibly, by
/// whole lines, with the dropped count stated.
fn ways_in_value(lines: &[String]) -> String {
    let whole = lines.join("\n");
    if whole.chars().count() <= DISCORD_WAYS_IN_VALUE_CHARS {
        return whole;
    }
    // Grow the kept prefix a whole line at a time, always leaving room for the
    // marker. `lines.len()` is the widest the count can print, so reserve
    // against it rather than the count we happen to land on.
    let marker_width = more_ways_marker(lines.len()).chars().count();
    let budget = DISCORD_WAYS_IN_VALUE_CHARS.saturating_sub(marker_width + 1);
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in lines {
        let cost = line.chars().count() + usize::from(!kept.is_empty());
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(line);
    }
    let dropped = lines.len() - kept.len();
    if kept.is_empty() {
        // A single line wider than the budget: nothing can be kept whole, so
        // the field says only how many ways it could not show.
        return more_ways_marker(dropped);
    }
    format!("{}\n{}", kept.join("\n"), more_ways_marker(dropped))
}

/// The line that stands in for the ways this field could not show.
fn more_ways_marker(dropped: usize) -> String {
    format!("+{dropped} more — see the net page")
}

/// One clamped embed field. Every slot goes through [`clamp_chars`], including
/// the fixed literal labels, so a later-widened source cannot exceed its cap
/// for want of a clamp.
fn embed_field(name: &str, value: String) -> DiscordEmbedField {
    DiscordEmbedField {
        name: clamp_chars(name, DISCORD_FIELD_NAME_CHARS),
        value: clamp_chars(&value, DISCORD_FIELD_VALUE_CHARS),
        inline: true,
    }
}

/// What to do after a Discord 429.
#[derive(Debug, PartialEq, Eq)]
enum RetryAfter {
    /// Wait this long, then retry within the existing attempt budget.
    Wait(Duration),
    /// Give up now without sleeping, carrying the wait the destination asked
    /// for so the durable row honours it.
    Abandon(Duration),
}

/// Reads Discord's rate-limit response body and decides how long to wait.
///
/// The JSON body's `retry_after` is authoritative, not the `Retry-After` header:
/// it is the typed value, carries millisecond precision where the header is
/// seconds rounded up, and is the only one reachable, since
/// `netroll_domain::egress::EgressResponse` carries no headers. It is a FLOAT
/// NUMBER OF SECONDS — before API v8 it was an integer of milliseconds, and
/// Discord's own dev-support article still says so, so any snippet claiming
/// milliseconds is pre-v8.
///
/// Four cases, because a 429 need not come from Discord itself — a proxy or CDN
/// can answer with HTML:
///
/// - within [`MAX_HONOURED_RETRY_AFTER`] → wait exactly that long
/// - over the cap, or out of `Duration`'s range → abandon now, never sleep
/// - `<= 0`, `NaN` or infinite → [`RETRY_BACKOFF`]; never sleep zero, because a
///   hot loop against a rate limiter is worse than not retrying
/// - unparseable or absent → [`RETRY_BACKOFF`] and still retry
///
/// The conversion is TOTAL and must stay total. `Duration::from_secs_f64` PANICS
/// rather than erroring on a finite value too large to hold, and any host can
/// answer `{"retry_after":2e19}`. That panic unwound a `tokio::join!` branch and
/// took the email and webhook legs with it.
fn discord_retry_after(body: &[u8]) -> RetryAfter {
    let Some(seconds) = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("retry_after").and_then(serde_json::Value::as_f64))
    else {
        return RetryAfter::Wait(RETRY_BACKOFF);
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return RetryAfter::Wait(RETRY_BACKOFF);
    }
    // Too large for a `Duration` at all, and still a request to stay away:
    // clamp to the outer ladder's ceiling rather than park the leg forever.
    let ceiling = Duration::from_millis(DELIVERY_RETRY_MAX_MILLIS);
    let Ok(wait) = Duration::try_from_secs_f64(seconds) else {
        return RetryAfter::Abandon(ceiling);
    };
    if wait > MAX_HONOURED_RETRY_AFTER {
        RetryAfter::Abandon(wait.min(ceiling))
    } else {
        RetryAfter::Wait(wait)
    }
}

/// The `yyyymmdd` segment of an attachment filename, from the close or start
/// instant; `unknown-date` when neither is present.
fn summary_date_slug(folded: &SessionState) -> String {
    folded
        .closed_at
        .or(folded.started_at)
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
        .map(|dt| dt.format("%Y%m%d").to_string())
        .unwrap_or_else(|| "unknown-date".to_owned())
}

/// The HTML part of the on-close summary mail.
///
/// Rendered HERE rather than in the mail adapter, which never sees the fold
/// types, and because both parts must come from one roster loop: `station_lines`
/// holds the SAME strings the text body carries, so the two cannot describe
/// different sessions.
///
/// An askama `.html` template, so every `{{ }}` is HTML-escaped by the
/// extension-selected escaper — the net title and each station line are
/// user-controlled free text reaching an HTML document. The plain-text body is
/// deliberately not escaped.
#[derive(Template)]
#[template(path = "net_summary.html")]
struct NetSummaryHtml<'a> {
    title: &'a str,
    meta: &'a [String],
    station_lines: &'a [String],
    /// Gates the sentence promising the attached exports. Derived from the
    /// real attachment list, never assumed: the port documents `attachments` as
    /// possibly empty and the adapter has a live no-attachment branch, so an
    /// unconditional promise is one the mail itself can contradict.
    has_attachments: bool,
}

/// Builds the on-close summary email: a text body, its HTML alternative, and
/// the CSV/ADIF attachments. Pure over the fold and snapshot, so it is testable
/// without I/O. The attachments come from `export::to_csv`/`to_adif`, never a
/// hand-rolled CSV, so they are injection-safe by construction.
///
/// Fallible only because the HTML render is: a template failure becomes a
/// [`MailError`] and joins the path an SMTP failure already takes rather than
/// panicking inside the spawned task.
fn build_summary_email(
    row: &NetSessionRow,
    folded: &SessionState,
    entering_ops: &BTreeMap<Uuid, String>,
    now_millis: u64,
) -> Result<NetSummaryMail, MailError> {
    let title = &row.definition_snapshot.title;
    let subject = format!("NetRoll session summary: {title}");

    // Built once and rendered into BOTH parts, so the HTML cannot state a
    // different window or participant count from the text.
    let mut meta: Vec<String> = Vec::new();
    if let Some(started) = folded.started_at {
        meta.push(format!("Started: {}", rfc3339(started)));
    }
    if let Some(closed) = folded.closed_at {
        meta.push(format!("Closed: {}", rfc3339(closed)));
    }
    if let Some(secs) = duration_seconds(folded) {
        meta.push(format!("Duration: {secs} seconds"));
    }
    meta.push(format!("Participants: {}", folded.roster.len()));
    // Every way the net was reachable, in the owner's order, with the frequency
    // each was on at close — a cross-mode net's summary that named one number
    // described none of them. Resolved ONCE for the whole email: the meta
    // block, the per-way counts and the attachments all read this list.
    let connections = folded.live_connections(&row.definition_snapshot.connections);
    for line in connection_lines(&connections) {
        meta.push(format!("Reachable on: {line}"));
    }
    // The stations nobody recorded a way in for get their own named bucket
    // rather than being folded into a connection, which would then report a
    // confident wrong number.
    for line in connection_counts(folded, &connections) {
        meta.push(format!("Check-ins: {line}"));
    }

    // ONE loop feeds both parts. A second for the HTML would add a third
    // `RosterEntry` destructure and red `roster_projection_sites.rs` — or,
    // written as plain field reads, pass that count while leaving the site the
    // guard exists for invisible to it.
    let mut station_lines: Vec<String> = Vec::with_capacity(folded.roster.len());
    for entry in &folded.roster {
        // EXHAUSTIVE destructure, no `..` rest pattern: a new `RosterEntry`
        // field must fail to compile HERE until a human decides whether this
        // projection carries it. It uses `callsign`, `name` and `location` and
        // drops the rest, because a HUMAN SUMMARY has no line for them.
        //
        // The body roster is a DIGEST: its reader wants the COUNTS in the meta
        // block, not a way in repeated beside four hundred callsigns, and the
        // full per-entry answer travels with this email in the attachments. So
        // the default answer for a NEW field is "only if it serves the reader",
        // and a field count is not the test.
        let RosterEntry {
            callsign,
            name,
            location,
            check_in_id: _,
            added_at: _,
            added_by: _,
            // A log cursor; the per-station frequency it selects travels in
            // the attachments.
            added_seq: _,
            source: _,
            signal_report: _,
            staying: _,
            grid: _,
            precedence: _,
            traffic: _,
            notes: _,
            // Not read here, for the same reason the staff note is not: this
            // is the human summary. Both notes travel with this email in the
            // CSV attachment.
            public_note: _,
            via: _,
            relayed_by: _,
            worked: _,
            version: _,
            corrections: _,
        } = entry;
        let mut line = callsign.as_str().to_owned();
        if let Some(name) = name {
            line.push_str(&format!(" — {}", name.as_str()));
        }
        if let Some(location) = location {
            line.push_str(&format!(" ({})", location.as_str()));
        }
        station_lines.push(line);
    }

    let mut body = String::new();
    body.push_str(&format!("Net: {title}\r\n"));
    for line in &meta {
        body.push_str(&format!("{line}\r\n"));
    }
    body.push_str("\r\n");
    body.push_str("Roster:\r\n");
    for line in &station_lines {
        body.push_str(&format!("  {line}\r\n"));
    }

    let date = summary_date_slug(folded);
    // The SAME set the export handler passes, so the emailed `.csv`/`.adi` and
    // the downloaded ones cannot describe one session differently. The FROZEN
    // snapshot, not the live `connections` the meta block read: each export
    // resolves per entry as at that entry's own seq, so a station worked before
    // a move keeps the frequency it was worked on.
    let snapshot = &row.definition_snapshot.connections;
    let csv = export::to_csv(folded, entering_ops, snapshot);
    let adif = export::to_adif(folded, snapshot, now_millis);
    // Built BEFORE the HTML so its attachment sentence is driven by the real
    // list rather than an assumption about it.
    let attachments = vec![
        MailAttachment {
            filename: format!("net-session-{date}.csv"),
            content_type: "text/csv; charset=utf-8".to_owned(),
            content: csv,
        },
        MailAttachment {
            filename: format!("net-session-{date}.adi"),
            content_type: "text/plain; charset=utf-8".to_owned(),
            content: adif,
        },
    ];

    let html_body = NetSummaryHtml {
        title,
        meta: &meta,
        station_lines: &station_lines,
        has_attachments: !attachments.is_empty(),
    }
    .render()
    // STATIC message, no title and no address.
    .map_err(|_| MailError("summary email template failed to render".into()))?;

    Ok(NetSummaryMail {
        subject,
        body,
        html_body,
        attachments,
        message_id: summary_message_id(row.id),
    })
}

/// Maps a summary that could not be rendered onto every email leg's outcome.
///
/// Extracted so the classification is assertable on its own: it decides whether
/// the recipients are still owed or permanently refused, and the two answers
/// differ by ten attempts across two hours.
fn unrenderable_summary_outcomes<'a>(
    legs: &[&'a DeliveryJob],
) -> Vec<(&'a DeliveryJob, LegOutcome)> {
    // Permanent, not transient: a closed session's snapshot and fold are
    // frozen, so a render that failed once fails identically every time. The
    // payload-serialize arms answer the same question the same way.
    legs.iter()
        .map(|leg| (*leg, LegOutcome::Permanent))
        .collect()
}

/// How one destination LEG ended — the INNER ladder's verdict, which the outer
/// ladder acts on in [`DeliveryService::settle_leg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegOutcome {
    /// Sent: a 2xx, or the mailer returned `Ok`.
    Succeeded,
    /// Refused for good: a permanent status or `EgressError`, a webhook URL
    /// with no stored secret, or a payload that failed to serialize.
    Permanent,
    /// The inner ladder was spent on transient failures. Still owed. Every
    /// mail failure is this: `MailError` carries no classification, so a port
    /// that cannot tell a 550 from a timeout is read honestly as "try later".
    Transient,
    /// Transient, and the destination named how long to stay away.
    ///
    /// Only a Discord 429 over [`MAX_HONOURED_RETRY_AFTER`] reaches this: the
    /// attempt is abandoned without sleeping on a permit, but the wait has to
    /// survive into `next_attempt_at` or the outer ladder re-POSTs inside the
    /// window Discord explicitly closed, against the same
    /// 10,000-invalid-requests ban budget that makes a 401 permanent here.
    TransientAfter(Duration),
    /// Deliberately not sent: the target was cleared between the plan and the
    /// send. Neither a success, since nothing was delivered, nor a transient,
    /// since a target that no longer exists would burn the whole budget.
    Skipped,
}

/// How the legs of one claimed set were settled, by row transition. Counts
/// only, never a target, so a caller can log it and a test can assert the
/// executor's own account of its work beside the rows it left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LegTally {
    /// Legs moved to `succeeded`.
    pub succeeded: u64,
    /// Legs moved to `failed`: a permanent refusal or a spent attempt budget.
    pub failed: u64,
    /// Legs moved to `skipped`: a cleared target or an unfoldable session.
    pub skipped: u64,
    /// Legs released back to `pending` with a later `next_attempt_at`.
    pub rescheduled: u64,
    /// Settles that moved NO row: the claim was already gone, its lease lapsed
    /// and recovery or a newer claim had taken the leg. Counted rather than
    /// dropped, or the tick's own numbers disagree with the rows an operator
    /// reads back.
    pub stale: u64,
}

impl LegTally {
    /// Records a settle: `moved` rows means the transition happened, zero means
    /// the claim was already gone and nothing was written.
    fn record(&mut self, moved: u64, transition: impl FnOnce(&mut Self)) {
        if moved == 0 {
            self.stale = 1;
        } else {
            transition(self);
        }
    }

    /// Folds another tally into this one.
    pub(crate) fn add(&mut self, other: LegTally) {
        self.succeeded += other.succeeded;
        self.failed += other.failed;
        self.skipped += other.skipped;
        self.rescheduled += other.rescheduled;
        self.stale += other.stale;
    }
}

/// The on-close deliverer. Cheap to clone, built via
/// [`crate::http::AppState::delivery_service`] and shared by both close paths.
#[derive(Clone)]
pub struct DeliveryService {
    net_sessions: NetSessionRepo,
    session_events: SessionEventLog,
    delivery_configs: DeliveryConfigRepo,
    /// The durable legs this deliverer claims and settles.
    delivery_jobs: DeliveryJobRepo,
    accounts: AccountRepo,
    mailer: Arc<dyn Mailer + Send + Sync>,
    egress: Arc<dyn Egress + Send + Sync>,
    clock: Arc<dyn Clock + Send + Sync>,
    /// The public base URL behind the Discord announcement's link back. Not a
    /// new config knob: the same resolved value that builds the verification
    /// and email-change links.
    public_base_url: String,
    /// Bounds concurrent in-flight deliveries. Shared across every clone,
    /// including the per-request ones, so the bound is process-wide rather than
    /// reset per close.
    concurrency: Arc<tokio::sync::Semaphore>,
}

impl DeliveryService {
    /// Wires the deliverer from the already-built handles. The semaphore is
    /// passed in rather than created here, so it is never accidentally reset.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        net_sessions: NetSessionRepo,
        session_events: SessionEventLog,
        delivery_configs: DeliveryConfigRepo,
        delivery_jobs: DeliveryJobRepo,
        accounts: AccountRepo,
        mailer: Arc<dyn Mailer + Send + Sync>,
        egress: Arc<dyn Egress + Send + Sync>,
        clock: Arc<dyn Clock + Send + Sync>,
        public_base_url: String,
        concurrency: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            net_sessions,
            session_events,
            delivery_configs,
            delivery_jobs,
            accounts,
            mailer,
            egress,
            clock,
            public_base_url,
            concurrency,
        }
    }

    /// Fire-and-forget: spawns the delivery and returns immediately.
    /// Production call sites DROP the handle, so a hung target can never stall
    /// the close response or the sweep tick; tests may await it to observe
    /// completion. The task owns its errors, logging a static line, because
    /// there is no caller to propagate to.
    ///
    /// A latency optimisation, not the durability mechanism: it runs legs
    /// `close()` already planned, and the sweeper picks them up if it never
    /// runs. The permit is taken INSIDE the task, so this returns immediately
    /// even when the pool is saturated, and BEFORE the claim, because a lease
    /// starts at the claim and a leg waiting for a permit past its own lease
    /// reads to the sweeper as an interrupted attempt.
    pub fn spawn_for_closed_session(&self, session_id: Uuid) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let _permit = this
                .concurrency
                .clone()
                .acquire_owned()
                .await
                .expect("delivery concurrency semaphore is never closed");
            let now = this.clock.now_epoch_millis();
            let result: Result<(), sqlx::Error> = async {
                let legs = this
                    .delivery_jobs
                    .claim_session(session_id, now, DELIVERY_LEASE_MILLIS)
                    .await?;
                // Nothing to claim is the common no-op: delivery off, so no
                // legs were planned, or the sweeper got here first.
                if legs.is_empty() {
                    return Ok(());
                }
                this.deliver_claimed(session_id, &legs, now)
                    .await
                    .map(|_| ())
            }
            .await;
            if result.is_err() {
                // Static line — a sqlx error's text can embed row values.
                tracing::error!(
                    net_session_id = %session_id,
                    "on-close delivery aborted on a storage error; its legs stay owed"
                );
            }
        })
    }

    /// Runs already-claimed legs of ONE session on its own task — the sweeper's
    /// per-session runner, which reserves the permit before it claims.
    pub(crate) fn spawn_claimed(
        &self,
        session_id: Uuid,
        legs: Vec<DeliveryJob>,
        now_millis: u64,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> tokio::task::JoinHandle<Result<LegTally, sqlx::Error>> {
        let this = self.clone();
        tokio::spawn(async move {
            // The permit is ALREADY HELD, taken by the caller before it
            // claimed. Acquiring it here instead puts the wait inside the
            // lease's own clock: a burst could hold every permit while a
            // claimed leg queues behind them, and past its lease that leg is
            // recovered as interrupted while still owed. Passing it in makes
            // claiming without one unwriteable.
            let _permit = permit;
            this.deliver_claimed(session_id, &legs, now_millis).await
        })
    }

    /// Takes up to `max` delivery permits WITHOUT waiting, for a caller that
    /// must hold them before it claims. Non-blocking on purpose: a tick that
    /// cannot get a permit claims less rather than leasing rows it cannot start.
    pub(crate) fn reserve_delivery_permits(
        &self,
        max: usize,
    ) -> Vec<tokio::sync::OwnedSemaphorePermit> {
        let mut permits = Vec::new();
        while permits.len() < max {
            match self.concurrency.clone().try_acquire_owned() {
                Ok(permit) => permits.push(permit),
                Err(_) => break,
            }
        }
        permits
    }

    /// Delivers the CLAIMED legs of ONE session and settles their rows: one row
    /// load, one config load, ONE fold, one entering-operator resolve, then the
    /// legs concurrently, so per-target latency isolation survives.
    ///
    /// `now_millis` is the instant the caller claimed at and is what every
    /// transition is stamped against — a caller input, never a clock read. The
    /// sends take real time; that is what the lease is for.
    ///
    /// A `sqlx::Error` on the initial loads is returned for the caller to log,
    /// leaving the legs claimed until the lease expires. The one storage error
    /// handled here is a log that can never be folded: EVERY leg is skipped, not
    /// just the claimed ones, so no later tick hits the same wall.
    pub(crate) async fn deliver_claimed(
        &self,
        session_id: Uuid,
        legs: &[DeliveryJob],
        now_millis: u64,
    ) -> Result<LegTally, sqlx::Error> {
        let loaded = match self.load_for_delivery(session_id).await {
            Ok(loaded) => loaded,
            Err(err) if netroll_adapters::pg::is_unreplayable_log(&err) => {
                // Not retryable and not a fault: the log predates its
                // connection list and fails identically every time. Reached,
                // not hypothetical — the presence monitor auto-closes abandoned
                // old sessions and spawns this — and without it the session is
                // re-claimed on every tick, forever.
                let skipped = self
                    .delivery_jobs
                    .skip_session(session_id, now_millis)
                    .await?;
                tracing::warn!(
                    net_session_id = %session_id,
                    legs = skipped,
                    "on-close delivery skipped: this session's log predates its connection \
                     list and can never be folded. Not retryable and not a fault"
                );
                return Ok(LegTally {
                    skipped,
                    ..LegTally::default()
                });
            }
            Err(err) => return Err(err),
        };
        let Some((row, folded)) = loaded else {
            // The row is gone and `ON DELETE CASCADE` took its legs with it.
            tracing::error!(net_session_id = %session_id, "on-close delivery: session vanished");
            return Ok(LegTally::default());
        };
        let definition_id = row.definition_id;

        let mut tally = LegTally::default();
        let config = self.delivery_configs.get(definition_id).await?;
        // The config was removed, or cleared to arm nothing, between the plan
        // and the send. Every target is gone: skipped, never "succeeded" and
        // never retried. Derived from the destination-kind declaration, so a
        // fourth kind gets its answer from `is_armed` or fails to compile.
        // The binding is discarded on purpose: every destination re-reads its
        // own target at send time, so this snapshot decides delivery-off and
        // nothing else.
        let Some(_armed) = config.filter(|config| !is_delivery_off(config)) else {
            for leg in legs {
                tally.add(
                    self.settle_leg(leg, LegOutcome::Skipped, now_millis)
                        .await?,
                );
            }
            return Ok(tally);
        };
        let entering_ops = self.resolve_entering_operators(&folded).await?;

        // Exhaustive, so a stored value this build cannot name is recorded
        // rather than looped on.
        let mut email_legs: Vec<&DeliveryJob> = Vec::new();
        let mut webhook_leg: Option<&DeliveryJob> = None;
        let mut discord_leg: Option<&DeliveryJob> = None;
        for leg in legs {
            match DeliveryDestinationKind::from_job_destination(&leg.destination) {
                Some(DeliveryDestinationKind::Email) => email_legs.push(leg),
                Some(DeliveryDestinationKind::Webhook) => webhook_leg = Some(leg),
                Some(DeliveryDestinationKind::Discord) => discord_leg = Some(leg),
                None => {
                    tracing::error!(
                        net_session_id = %session_id,
                        net_delivery_job_id = %leg.id,
                        "on-close delivery leg names a destination this build does not know; failed"
                    );
                    tally.add(
                        self.settle_leg(leg, LegOutcome::Permanent, now_millis)
                            .await?,
                    );
                }
            }
        }

        // The three legs run CONCURRENTLY — true per-target latency isolation,
        // not just failure isolation. Each address's retry loop can take
        // MAX_ATTEMPTS times the mailer's own timeout, and running the webhook
        // after the whole email fan-out would let a slow recipient delay a
        // webhook that has nothing to do with it.
        //
        // The addresses are re-read at send time, as each fallible leg re-reads
        // its own target: the owner's CURRENT list decides what is armed, not
        // the snapshot this executor opened with. Hoisted out of the join so a
        // storage failure propagates rather than folding into a leg outcome.
        let current_emails = self.delivery_configs.delivery_emails(definition_id).await?;
        let email_fut = async {
            let mut outcomes: Vec<(&DeliveryJob, LegOutcome)> =
                Vec::with_capacity(email_legs.len());
            if email_legs.is_empty() {
                return outcomes;
            }
            let now = self.clock.now_epoch_millis();
            match build_summary_email(&row, &folded, &entering_ops, now) {
                Ok(mail) => {
                    // CONCURRENTLY, not one address after another: one claim
                    // leases EVERY leg for `DELIVERY_LEASE_MILLIS`, so a
                    // sequential fan-out makes the lease scale with the address
                    // count instead of with one attempt — and a lease shorter
                    // than the work it covers lets recovery re-claim a running
                    // attempt and double-send. Driven here rather than spawned,
                    // so a panic still unwinds into this task.
                    outcomes = futures_util::future::join_all(email_legs.iter().map(|leg| {
                        let mail = &mail;
                        let emails = &current_emails;
                        async move {
                            // An address removed between the plan and the send
                            // is a cleared target: the owner's current list wins.
                            let outcome = if emails.contains(&leg.target) {
                                self.deliver_email(session_id, definition_id, &leg.target, mail)
                                    .await
                            } else {
                                LegOutcome::Skipped
                            };
                            (*leg, outcome)
                        }
                    }))
                    .await;
                }
                // Logged like an exhausted send — ids only, other legs
                // unaffected, no panic — and leaves the legs OWED, not lost.
                Err(_) => {
                    tracing::error!(
                        net_session_id = %session_id,
                        net_definition_id = %definition_id,
                        "on-close summary email could not be rendered; failed"
                    );
                    outcomes = unrenderable_summary_outcomes(&email_legs);
                }
            }
            outcomes
        };
        let webhook_fut = async {
            match webhook_leg {
                Some(leg) => Some((
                    leg,
                    self.deliver_webhook(session_id, definition_id, &row, &folded, &entering_ops)
                        .await,
                )),
                None => None,
            }
        };
        let discord_fut = async {
            match discord_leg {
                Some(leg) => Some((
                    leg,
                    self.deliver_discord(leg, session_id, definition_id, &row, &folded, now_millis)
                        .await,
                )),
                None => None,
            }
        };
        // Every joined leg reports its outcome, so nothing here is `_`;
        // `tests/delivery_destination_kinds.rs` asserts exactly that.
        let (email_outcomes, webhook_result, discord_result) =
            tokio::join!(email_fut, webhook_fut, discord_fut);

        for (leg, outcome) in email_outcomes {
            tally.add(self.settle_leg(leg, outcome, now_millis).await?);
        }
        // A leg that hit a STORAGE error stays claimed — its lease expires and
        // recovery classifies it — while the others still settle. See
        // [`combine_leg_storage_results`] for the shape that dropped one.
        let mut webhook_storage: Result<(), sqlx::Error> = Ok(());
        if let Some((leg, result)) = webhook_result {
            match result {
                Ok(outcome) => tally.add(self.settle_leg(leg, outcome, now_millis).await?),
                Err(err) => webhook_storage = Err(err),
            }
        }
        let mut discord_storage: Result<(), sqlx::Error> = Ok(());
        if let Some((leg, result)) = discord_result {
            match result {
                Ok(outcome) => tally.add(self.settle_leg(leg, outcome, now_millis).await?),
                Err(err) => discord_storage = Err(err),
            }
        }
        combine_leg_storage_results(webhook_storage, discord_storage)?;
        Ok(tally)
    }

    /// Loads the session row and folds its log ONCE — the single fold every
    /// summary uses. `Ok(None)` when the row is gone.
    async fn load_for_delivery(
        &self,
        session_id: Uuid,
    ) -> Result<Option<(NetSessionRow, SessionState)>, sqlx::Error> {
        let Some(row) = self.net_sessions.find(session_id).await? else {
            return Ok(None);
        };
        let events = self.session_events.events_since(session_id, 0).await?;
        Ok(Some((row, replay(&events, 0))))
    }

    /// Moves ONE leg's row per its outcome — the transition table, in one place.
    ///
    /// `attempts` was incremented at the CLAIM, so a leg whose attempt was
    /// killed mid-flight has still burned one. `>=`, never `==`: a row whose
    /// count somehow overshoots the budget must still stop.
    async fn settle_leg(
        &self,
        leg: &DeliveryJob,
        outcome: LegOutcome,
        now_millis: u64,
    ) -> Result<LegTally, sqlx::Error> {
        let mut tally = LegTally::default();
        // Fenced by the claim it belongs to: a run whose send outlived its
        // lease must not settle over a NEWER claim, and a settle that moved
        // nothing must not be counted as a transition that happened.
        let lease = leg.lease_until_millis;
        match outcome {
            LegOutcome::Succeeded => {
                let moved = self
                    .delivery_jobs
                    .mark_terminal(leg.id, TerminalState::Succeeded, now_millis, lease)
                    .await?;
                tally.record(moved, |t| t.succeeded = 1);
            }
            LegOutcome::Permanent => {
                let moved = self
                    .delivery_jobs
                    .mark_terminal(leg.id, TerminalState::Failed, now_millis, lease)
                    .await?;
                tally.record(moved, |t| t.failed = 1);
            }
            LegOutcome::Skipped => {
                let moved = self
                    .delivery_jobs
                    .mark_terminal(leg.id, TerminalState::Skipped, now_millis, lease)
                    .await?;
                tally.record(moved, |t| t.skipped = 1);
            }
            LegOutcome::Transient | LegOutcome::TransientAfter(_)
                if leg.attempts >= MAX_JOB_ATTEMPTS =>
            {
                let moved = self
                    .delivery_jobs
                    .mark_terminal(leg.id, TerminalState::Failed, now_millis, lease)
                    .await?;
                // Ids and a count only — the target may be an address.
                tracing::error!(
                    net_session_id = %leg.session_id,
                    net_delivery_job_id = %leg.id,
                    attempts = leg.attempts,
                    "on-close delivery leg exhausted its attempt budget; failed"
                );
                tally.record(moved, |t| t.failed = 1);
            }
            LegOutcome::Transient | LegOutcome::TransientAfter(_) => {
                // The ladder is a floor, never a ceiling: when the destination
                // named a longer wait, honour ITS number, or the retry re-enters
                // a window the receiver closed.
                //
                // Measured from the CLAIM instant, not from now: reading a clock
                // here would put scheduling back on wall time. The consequence
                // is that an attempt slower than its own rung gets no additional
                // rest — a 45 s timeout leaves the 30 s rung already past, so
                // the leg is due when the attempt ends. Coverage holds: elapsed
                // per rung is max(rung, attempt duration), so the rungs still
                // span their ~2 hours.
                let requested = match outcome {
                    LegOutcome::TransientAfter(wait) => {
                        u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)
                    }
                    _ => 0,
                };
                let delay = retry_delay_millis(leg.attempts).max(requested);
                let next = now_millis.saturating_add(delay);
                let moved = self.delivery_jobs.reschedule(leg.id, next, lease).await?;
                tally.record(moved, |t| t.rescheduled = 1);
            }
        }
        Ok(tally)
    }

    /// Resolves the distinct staff entering-operator callsigns: only STAFF
    /// entries carry a meaningful logger, and an id with no claimed callsign is
    /// skipped. The roster is bounded, so a per-id lookup is fine.
    async fn resolve_entering_operators(
        &self,
        folded: &SessionState,
    ) -> Result<BTreeMap<Uuid, String>, sqlx::Error> {
        let mut distinct: Vec<Uuid> = Vec::new();
        for entry in &folded.roster {
            if entry.source == CheckInSource::Staff
                && let Some(id) = entry.added_by
                && !distinct.contains(&id)
            {
                distinct.push(id);
            }
        }
        let mut resolved = BTreeMap::new();
        for id in distinct {
            if let Some(account) = self.accounts.find_by_id(id).await?
                && let Some(callsign) = account.callsign
            {
                resolved.insert(id, callsign);
            }
        }
        Ok(resolved)
    }

    /// Sends the summary to one address with bounded retry. The message carries
    /// a stable `Message-ID`, so a retry a receiver already accepted is a
    /// duplicate it can recognise — common MUA behaviour, not a guarantee.
    /// Exhausting the bound reports [`LegOutcome::Transient`]: `MailError`
    /// carries no classification, so every failure reads as "try later".
    async fn deliver_email(
        &self,
        session_id: Uuid,
        definition_id: Uuid,
        address: &str,
        mail: &NetSummaryMail,
    ) -> LegOutcome {
        for attempt in 0..MAX_ATTEMPTS {
            match self.mailer.send_net_summary(address, mail).await {
                Ok(()) => return LegOutcome::Succeeded,
                Err(_err) => {
                    if attempt + 1 < MAX_ATTEMPTS {
                        tokio::time::sleep(RETRY_BACKOFF).await;
                    }
                }
            }
        }
        // Ids only, never the address.
        tracing::error!(
            net_session_id = %session_id,
            net_definition_id = %definition_id,
            "on-close summary email failed after bounded retries; this recipient is still owed"
        );
        LegOutcome::Transient
    }

    /// Signs and POSTs the webhook through the SSRF-safe client with bounded
    /// retry. The payload is serialized ONCE, the signature covers those exact
    /// bytes, and the SAME signed request is reused on every retry so a receiver
    /// dedupes. The secret is READ, never regenerated: a URL with no stored
    /// secret is a recorded refusal, not a fabricated key. The two are re-read
    /// TOGETHER, in one query, right before use, so a config edit landing during
    /// the concurrent fan-out cannot pair a fresh secret with a stale URL.
    async fn deliver_webhook(
        &self,
        session_id: Uuid,
        definition_id: Uuid,
        row: &NetSessionRow,
        folded: &SessionState,
        entering_ops: &BTreeMap<Uuid, String>,
    ) -> Result<LegOutcome, sqlx::Error> {
        let Some((url, secret)) = self.delivery_configs.webhook_target(definition_id).await? else {
            // Cleared between the top-level snapshot and this fresh read.
            return Ok(LegOutcome::Skipped);
        };
        let Some(secret) = secret else {
            // A data anomaly: record it, and never mint a key the receiver
            // cannot verify.
            tracing::error!(
                net_session_id = %session_id,
                net_definition_id = %definition_id,
                "on-close webhook has a URL but no stored secret; failed (never regenerated)"
            );
            return Ok(LegOutcome::Permanent);
        };

        let payload = build_webhook_payload(session_id, row, folded, entering_ops);
        // ONCE — the signature must cover the literal wire bytes.
        let body = match serde_json::to_vec(&payload) {
            Ok(bytes) => bytes,
            Err(_err) => {
                tracing::error!(
                    net_session_id = %session_id,
                    net_definition_id = %definition_id,
                    "on-close webhook payload failed to serialize; failed"
                );
                return Ok(LegOutcome::Permanent);
            }
        };
        let signature = sign_webhook(&secret, &body);
        let delivery_id = webhook_delivery_id(session_id);
        let request = EgressRequest {
            method: EgressMethod::Post,
            url,
            headers: vec![
                ("content-type".to_owned(), "application/json".to_owned()),
                ("X-NetRoll-Signature".to_owned(), signature),
                ("X-NetRoll-Delivery-Id".to_owned(), delivery_id),
            ],
            body: Some(body),
        };

        for attempt in 0..MAX_ATTEMPTS {
            // The IDENTICAL signed request on every retry.
            match self.egress.send(request.clone()).await {
                Ok(response) => match classify_status(response.status) {
                    StatusClass::Success => return Ok(LegOutcome::Succeeded),
                    StatusClass::Permanent => {
                        tracing::error!(
                            net_session_id = %session_id,
                            net_definition_id = %definition_id,
                            status = response.status,
                            "on-close webhook rejected (permanent); failed"
                        );
                        return Ok(LegOutcome::Permanent);
                    }
                    StatusClass::Transient => {
                        if attempt + 1 < MAX_ATTEMPTS {
                            tokio::time::sleep(RETRY_BACKOFF).await;
                        }
                    }
                },
                Err(err) => {
                    if is_transient_egress(&err) {
                        if attempt + 1 < MAX_ATTEMPTS {
                            tokio::time::sleep(RETRY_BACKOFF).await;
                        }
                    } else {
                        // A permanent refusal — a rebind caught at delivery
                        // time, NotHttps, InvalidUrl — is never retried. Log
                        // the static category only.
                        tracing::error!(
                            net_session_id = %session_id,
                            net_definition_id = %definition_id,
                            "on-close webhook refused (permanent); failed"
                        );
                        return Ok(LegOutcome::Permanent);
                    }
                }
            }
        }
        tracing::error!(
            net_session_id = %session_id,
            net_definition_id = %definition_id,
            "on-close webhook failed after bounded retries; still owed"
        );
        Ok(LegOutcome::Transient)
    }

    /// POSTs the Discord announcement through the SAME SSRF-safe `Egress` port
    /// the generic webhook uses, with bounded retry. The URL is re-read
    /// immediately before use, because the executor's snapshot may name a
    /// channel the owner has since cleared. No signature and no delivery-id
    /// header: Discord verifies nothing and has no dedupe channel to key.
    ///
    /// NO request is ever logged. `EgressRequest`'s `Debug` redacts the QUERY
    /// STRING, for a `?password=` login — but a Discord token is in the PATH,
    /// which it prints verbatim, so logging one here would leak a channel
    /// takeover. Widening the redaction is deliberately not the fix: it would
    /// blind the generic-webhook diagnostics.
    ///
    /// Retry classification is REUSED rather than forked, which makes two
    /// inherited behaviours right without a branch: a 404 is `Permanent`, as
    /// Discord requires of a webhook that returned one, and so is a 401, whose
    /// retries count against the 10,000-invalid-requests ban budget. A 429 over
    /// the honoured cap abandons the attempt without sleeping but reports
    /// `Transient`; Discord did NOT post, so that retry carries no duplicate
    /// risk.
    async fn deliver_discord(
        &self,
        leg: &DeliveryJob,
        session_id: Uuid,
        definition_id: Uuid,
        row: &NetSessionRow,
        folded: &SessionState,
        now_millis: u64,
    ) -> Result<LegOutcome, sqlx::Error> {
        let url = match self.delivery_configs.discord_target(definition_id).await {
            Ok(Some(url)) => url,
            // Cleared between the executor's snapshot and this fresh read.
            Ok(None) => return Ok(LegOutcome::Skipped),
            Err(err) => {
                // Logged HERE rather than left to the caller, which is what
                // makes [`combine_leg_storage_results`]'s justification true:
                // that returns the FIRST error, so when the webhook leg has also
                // failed this one never reaches the wrapper's line. Before this
                // arm existed, nothing anywhere recorded it.
                tracing::error!(
                    net_session_id = %session_id,
                    net_definition_id = %definition_id,
                    "on-close discord announcement could not read its destination; skipped"
                );
                return Err(err);
            }
        };

        let message = build_discord_message(session_id, row, folded, &self.public_base_url);
        let body = match serde_json::to_vec(&message) {
            Ok(bytes) => bytes,
            Err(_err) => {
                tracing::error!(
                    net_session_id = %session_id,
                    net_definition_id = %definition_id,
                    "on-close discord announcement failed to serialize; failed"
                );
                return Ok(LegOutcome::Permanent);
            }
        };
        let request = EgressRequest {
            method: EgressMethod::Post,
            url,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: Some(body),
        };

        for attempt in 0..MAX_ATTEMPTS {
            // Record the dispatch BEFORE it happens: Discord has no dedupe
            // channel, so recovery must tell a send that may have landed from a
            // claim that never left this process, and only this write answers
            // that after a crash. Per attempt, so the record describes the
            // request actually in flight.
            self.delivery_jobs
                .mark_request_sent(leg.id, now_millis, leg.lease_until_millis)
                .await?;
            match self.egress.send(request.clone()).await {
                Ok(response) => match classify_status(response.status) {
                    StatusClass::Success => return Ok(LegOutcome::Succeeded),
                    StatusClass::Permanent => {
                        tracing::error!(
                            net_session_id = %session_id,
                            net_definition_id = %definition_id,
                            status = response.status,
                            "on-close discord announcement rejected (permanent); failed"
                        );
                        return Ok(LegOutcome::Permanent);
                    }
                    StatusClass::Transient => {
                        // A 429 asks for a specific wait; every other transient
                        // status gets the fixed backoff.
                        let wait = if response.status == 429 {
                            discord_retry_after(&response.body)
                        } else {
                            RetryAfter::Wait(RETRY_BACKOFF)
                        };
                        match wait {
                            RetryAfter::Abandon(requested) => {
                                tracing::error!(
                                    net_session_id = %session_id,
                                    net_definition_id = %definition_id,
                                    "on-close discord announcement rate-limited beyond the \
                                     honoured wait; this attempt abandoned without sleeping"
                                );
                                // Abandoned without sleeping, but the wait
                                // Discord asked for rides out on the outcome so
                                // the durable row honours it.
                                return Ok(LegOutcome::TransientAfter(requested));
                            }
                            RetryAfter::Wait(delay) => {
                                if attempt + 1 < MAX_ATTEMPTS {
                                    tokio::time::sleep(delay).await;
                                }
                            }
                        }
                    }
                },
                Err(err) => {
                    if is_transient_egress(&err) {
                        if attempt + 1 < MAX_ATTEMPTS {
                            tokio::time::sleep(RETRY_BACKOFF).await;
                        }
                    } else {
                        tracing::error!(
                            net_session_id = %session_id,
                            net_definition_id = %definition_id,
                            "on-close discord announcement refused (permanent); failed"
                        );
                        return Ok(LegOutcome::Permanent);
                    }
                }
            }
        }
        tracing::error!(
            net_session_id = %session_id,
            net_definition_id = %definition_id,
            "on-close discord announcement failed after bounded retries; still owed"
        );
        Ok(LegOutcome::Transient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netroll_adapters::pg::net_sessions::DefinitionSnapshot;
    use netroll_domain::callsign::parse_callsign;
    use netroll_domain::check_in::{
        Precedence, StayingStatus, parse_location, parse_name, parse_note, parse_signal_report,
        parse_traffic_count,
    };
    use netroll_domain::fold::{ControlState, RosterEntry, SessionLifecycle};
    use netroll_domain::net::connection::Via;
    use netroll_domain::profile::parse_grid;
    use serde_json::Value;

    /// The HF connection every fixture snapshot's position zero carries.
    const HF_CONNECTION: Uuid = Uuid::from_u128(0x16_04);

    pub(super) fn hf_wire(band: &str, mode: &str) -> NetConnectionWire {
        NetConnectionWire {
            id: HF_CONNECTION,
            position: 0,
            kind: "hf".to_owned(),
            planned_frequency_hz: Some(14_250_000),
            band: Some(band.to_owned()),
            mode: Some(mode.to_owned()),
            repeater_offset_hz: None,
            tone_mode: None,
            tone_value: None,
            node: None,
            reflector: None,
            network: None,
            talkgroup: None,
            label: None,
            detail: None,
        }
    }

    pub(super) fn snapshot(title: &str, band: &str, mode: &str) -> DefinitionSnapshot {
        DefinitionSnapshot {
            title: title.to_owned(),
            description: None,
            connections: vec![hf_wire(band, mode)],
            net_category: "traffic".to_owned(),
            net_type: "directed".to_owned(),
            country: None,
            state: None,
            grid: None,
        }
    }

    pub(super) fn row(snapshot: DefinitionSnapshot) -> NetSessionRow {
        NetSessionRow {
            id: Uuid::from_u128(10),
            definition_id: Uuid::from_u128(7),
            definition_version: 3,
            definition_snapshot: snapshot,
            lifecycle: SessionLifecycle::Closed,
            started_at_millis: Some(1_000),
            closed_at_millis: Some(61_000),
            control_state: ControlState::Active,
            active_ncs_account_id: None,
            stalled_at_millis: None,
            last_seq: 3,
            created_at_millis: 0,
            updated_at_millis: 61_000,
        }
    }

    pub(super) fn roster_entry(
        id: u128,
        call: &str,
        source: CheckInSource,
        added_by: Option<Uuid>,
    ) -> RosterEntry {
        RosterEntry {
            check_in_id: Uuid::from_u128(id),
            callsign: parse_callsign(call).expect("valid callsign"),
            added_at: 2_000,
            // A seq no fixture moves before, so resolution lands on the
            // snapshot's planned value.
            added_seq: 2,
            added_by,
            source,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: None,
            grid: None,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            worked: false,
            version: 1,
            via: None,
            corrections: Vec::new(),
            relayed_by: None,
        }
    }

    pub(super) fn closed_state(roster: Vec<RosterEntry>) -> SessionState {
        SessionState {
            lifecycle: SessionLifecycle::Closed,
            started_at: Some(1_000),
            closed_at: Some(61_000),
            definition_id: Some(Uuid::from_u128(7)),
            definition_version: Some(3),
            roster,
            ..Default::default()
        }
    }

    #[test]
    fn webhook_payload_serializes_camelcase_with_summary_and_roster() {
        let operator = Uuid::from_u128(900);
        let mut staff = roster_entry(1, "W1AW", CheckInSource::Staff, Some(operator));
        staff.name = parse_name("Maria").unwrap();
        let mut ops = BTreeMap::new();
        ops.insert(operator, "K1OP".to_owned());

        let payload = build_webhook_payload(
            Uuid::from_u128(10),
            &row(snapshot("Sunday Traffic", "20m", "ssb")),
            &closed_state(vec![staff]),
            &ops,
        );
        let json = serde_json::to_value(&payload).expect("serialize");

        assert_eq!(json["net"]["title"], "Sunday Traffic");
        assert_eq!(json["definitionVersion"], 3);
        // Receiver-visible: the top-level `operatingFrequencyHz` and
        // `net.band`/`net.mode` are GONE, replaced by the connection set. A
        // receiver that wants a band reads `connections[0].band` and learns
        // when there isn't one.
        assert!(json.get("operatingFrequencyHz").is_none());
        assert!(json["net"].get("band").is_none());
        assert!(json["net"].get("mode").is_none());
        assert_eq!(json["connections"][0]["kind"], "hf");
        assert_eq!(json["connections"][0]["band"], "20m");
        assert_eq!(json["connections"][0]["mode"], "ssb");
        assert_eq!(json["connections"][0]["plannedFrequencyHz"], 14_250_000);
        assert_eq!(json["durationSeconds"], 60);
        assert_eq!(json["participantCount"], 1);
        let entry = &json["roster"][0];
        assert_eq!(entry["callsign"], "W1AW");
        assert_eq!(entry["name"], "Maria");
        assert_eq!(entry["source"], "staff");
        assert_eq!(entry["enteringOperator"], "K1OP");
        assert_eq!(entry["checkedInAt"], "1970-01-01T00:00:02+00:00");
    }

    #[test]
    fn webhook_payload_leaves_entering_operator_absent_for_a_self_entry() {
        let account = Uuid::from_u128(901);
        let mut ops = BTreeMap::new();
        // Even with the id present in the resolved map, a self entry is blank.
        ops.insert(account, "N1CCK".to_owned());
        let payload = build_webhook_payload(
            Uuid::from_u128(10),
            &row(snapshot("Net", "20m", "ssb")),
            &closed_state(vec![roster_entry(
                2,
                "N1CCK",
                CheckInSource::SelfService,
                Some(account),
            )]),
            &ops,
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["roster"][0]["source"], "self");
        assert!(
            json["roster"][0].get("enteringOperator").is_none(),
            "a self entry has no entering operator"
        );
    }

    // The exact serialized key set of one webhook roster entry, in wire order:
    // the CSV header's fields in the same order, plus the webhook-only
    // `viaLabel`.
    // `the_webhook_roster_entry_key_set_matches_the_csv_header_field_for_field`
    // asserts the shared set against `csv_header_camelized` rather than asking
    // a reader to trust it. Widening this list without widening the published
    // contract is the failure mode the pin exists to catch; the four
    // renderings that legitimately differ are named on `WebhookRosterEntry`.
    const WEBHOOK_ROSTER_ENTRY_FIELDS: [&str; 17] = [
        "callsign",
        "name",
        "location",
        "grid",
        "source",
        "enteringOperator",
        "signalReport",
        "staying",
        "precedence",
        "traffic",
        "worked",
        // The staff note and the public note. The webhook is the net's own
        // outbound record, so it carries both.
        "notes",
        "publicNote",
        // The CSV's `via` column is the LABEL; the webhook carries the
        // STRUCTURED value under that key and its label beside it, because it
        // is the one machine contract with no browser to resolve in. `viaLabel`
        // is therefore the ONE key with no CSV column, and the parity assertion
        // names it rather than relaxing.
        "via",
        "viaLabel",
        // The webhook carries the CSV's `relayed_by` under the camelized key.
        // No `relayedByLabel` beside it: a callsign needs no resolution, so
        // this is not a second webhook-only key.
        "relayedBy",
        "checkedInAt",
    ];

    /// The one webhook key that has no CSV column, and why.
    const WEBHOOK_ONLY_FIELDS: [&str; 1] = ["viaLabel"];

    /// A roster entry with EVERY optional populated, so its projection
    /// serializes every key and none is missing merely for being unset.
    ///
    /// ⚠️ `via` MUST be set here. Every optional carries `skip_serializing_if`,
    /// so a field this fixture leaves unset is invisible to all three parts of
    /// the key-set assertion below and the pin silently stops covering it.
    fn fully_populated_entry(operator: Uuid) -> RosterEntry {
        let mut entry = roster_entry(1, "W1AW", CheckInSource::Staff, Some(operator));
        entry.name = parse_name("Maria").unwrap();
        entry.location = parse_location("Newington CT").unwrap();
        entry.grid = Some(parse_grid("fn31PR").unwrap());
        entry.signal_report = parse_signal_report("59").unwrap();
        entry.staying = StayingStatus::StayingForComments;
        entry.precedence = Precedence::Priority;
        entry.traffic = parse_traffic_count(Some(2)).unwrap();
        entry.worked = true;
        entry.notes = parse_note("Two pieces for Hartford").unwrap();
        entry.public_note = parse_note("Relaying for W1BBB").unwrap();
        entry.via = Some(Via::Unlisted("a phone patch".to_owned()));
        // Set for the same reason `via` is: an unset `skip_serializing_if`
        // optional is invisible to all three parts of the key-set assertion.
        entry.relayed_by = Some(parse_callsign("W3REL").expect("valid callsign"));
        entry
    }

    /// One CSV record split on RFC-4180 rules: a comma inside a quoted field is
    /// data, not a separator. Splitting the output on a bare `,` is correct only
    /// by accident of today's column names; this stays correct when one gains a
    /// comma.
    fn split_csv_record(record: &str) -> Vec<String> {
        let mut fields = vec![String::new()];
        let mut quoted = false;
        let mut chars = record.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    chars.next();
                    fields.last_mut().expect("a field").push('"');
                }
                '"' => quoted = !quoted,
                ',' if !quoted => fields.push(String::new()),
                other => fields.last_mut().expect("a field").push(other),
            }
        }
        fields
    }

    /// The CSV header row `to_csv` emits, each column converted to wire
    /// camelCase. Parsed from the OUTPUT rather than the private `CSV_COLUMNS`,
    /// so it pins what ships.
    ///
    /// The rename is hand-rolled because there is nothing to delegate it to, and
    /// deriving the webhook side from serde would make this assertion compare
    /// the struct to itself; the point is to compare it to the CSV.
    fn csv_header_camelized() -> Vec<String> {
        let csv = export::to_csv(&closed_state(Vec::new()), &BTreeMap::new(), &[]);
        let header = csv.lines().next().expect("a header row").to_owned();
        split_csv_record(&header)
            .into_iter()
            .map(|column| {
                let mut out = String::new();
                let mut upper_next = false;
                for ch in column.chars() {
                    if ch == '_' {
                        upper_next = true;
                    } else if upper_next {
                        out.extend(ch.to_uppercase());
                        upper_next = false;
                    } else {
                        out.push(ch);
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn webhook_roster_entry_carries_the_per_check_in_grid() {
        let operator = Uuid::from_u128(900);
        let mut ops = BTreeMap::new();
        ops.insert(operator, "K1OP".to_owned());
        let payload = build_webhook_payload(
            Uuid::from_u128(10),
            &row(snapshot("Net", "20m", "ssb")),
            &closed_state(vec![fully_populated_entry(operator)]),
            &ops,
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(
            json["roster"][0]["grid"], "FN31pr",
            "the canonical Maidenhead locator rides the webhook roster entry"
        );
    }

    #[test]
    fn webhook_roster_entry_omits_the_grid_key_entirely_when_there_is_no_grid() {
        // The load-bearing half: a grid-less station must be byte-identical to a
        // older payload, so the key is ABSENT rather than `"grid": null`.
        let payload = build_webhook_payload(
            Uuid::from_u128(10),
            &row(snapshot("Net", "20m", "ssb")),
            &closed_state(vec![roster_entry(
                2,
                "N1CCK",
                CheckInSource::SelfService,
                None,
            )]),
            &BTreeMap::new(),
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        // ANCHOR FIRST. `Value::get` on the `Null` an out-of-range index
        // returns is also `None`, so a bare absence assertion passes on an empty
        // roster, a renamed `roster` key or a non-object entry. Prove the entry
        // exists and carries its other keys, then prove `grid` is absent.
        let entry = json["roster"][0]
            .as_object()
            .expect("a roster entry object to assert the absence against");
        assert_eq!(
            entry.get("callsign").and_then(Value::as_str),
            Some("N1CCK"),
            "the entry under test is the grid-less station, not an empty slot"
        );
        assert!(
            entry.contains_key("source") && entry.contains_key("checkedInAt"),
            "the entry serialized its always-present keys, so absence below means absence"
        );
        assert!(
            !entry.contains_key("grid"),
            "an absent grid omits the key; it is never serialized as null"
        );
    }

    #[test]
    fn the_webhook_roster_entry_key_set_matches_the_csv_header_field_for_field() {
        let operator = Uuid::from_u128(900);
        let mut ops = BTreeMap::new();
        ops.insert(operator, "K1OP".to_owned());
        let payload = build_webhook_payload(
            Uuid::from_u128(10),
            &row(snapshot("Net", "20m", "ssb")),
            &closed_state(vec![fully_populated_entry(operator)]),
            &ops,
        );

        // 1. The declared list IS the CSV header, camelized, plus the named
        // webhook-only keys, so parity reds if either side gains, loses or
        // reorders a field and a NEW webhook-only key must be declared with its
        // reason rather than slipping through.
        let shared: Vec<&str> = WEBHOOK_ROSTER_ENTRY_FIELDS
            .iter()
            .copied()
            .filter(|field| !WEBHOOK_ONLY_FIELDS.contains(field))
            .collect();
        assert_eq!(
            csv_header_camelized(),
            shared,
            "the webhook entry's declared fields are the CSV header's, camelized"
        );
        // `viaLabel` sits immediately after `via`: the label belongs beside the
        // value it is a label OF, not at the end of the object.
        let via_at = WEBHOOK_ROSTER_ENTRY_FIELDS
            .iter()
            .position(|f| *f == "via")
            .expect("via is declared");
        assert_eq!(
            WEBHOOK_ROSTER_ENTRY_FIELDS.get(via_at + 1),
            Some(&"viaLabel"),
            "`viaLabel` follows `via`; indexing here would panic unreadably if `via` moved last"
        );

        // 2. The serialized entry carries exactly that set, no more and no less.
        let json = serde_json::to_value(&payload).expect("serialize");
        let entry = json["roster"][0].as_object().expect("an entry object");
        let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected: Vec<&str> = WEBHOOK_ROSTER_ENTRY_FIELDS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);

        // 3. And in that order on the wire, which `serde_json::Value` cannot
        // show because its map is sorted — so assert against the serialized
        // bytes, scoped to the ENTRY's own: a top-level key or a free-text
        // `notes` containing `"grid":` would anchor a needle outside the object
        // under test.
        let wire = serde_json::to_string(&payload.roster[0]).expect("serialize");
        let mut previous: Option<usize> = None;
        for field in WEBHOOK_ROSTER_ENTRY_FIELDS {
            let needle = format!("\"{field}\":");
            let at = wire.find(&needle).unwrap_or_else(|| {
                panic!("{field} is missing from the wire entry: {wire}");
            });
            match previous {
                // The first field is anchored to the object's OPENING BRACE rather
                // than to `0`, which any index trivially beats.
                None => assert_eq!(at, 1, "{field} is not the first key on the wire entry"),
                Some(previous) => {
                    assert!(
                        at > previous,
                        "{field} is out of CSV order on the wire entry"
                    )
                }
            }
            previous = Some(at);
        }
    }

    #[test]
    fn summary_email_carries_the_title_participant_count_and_way_in_breakdown() {
        let staff = roster_entry(1, "W1AW", CheckInSource::Staff, None);
        let folded = closed_state(vec![staff]);
        let session_row = row(snapshot("Sunday Traffic", "20m", "ssb"));
        let ops = BTreeMap::new();
        let mail = build_summary_email(&session_row, &folded, &ops, 61_000).expect("render");
        // The exports take the FROZEN snapshot and resolve per entry.
        let snapshot = &session_row.definition_snapshot.connections;

        assert!(mail.subject.contains("Sunday Traffic"));
        assert!(mail.body.contains("Sunday Traffic"));
        assert!(
            mail.body.contains("Participants: 1"),
            "the body states the participant count"
        );
        // The per-connection breakdown. A cross-mode net's
        // summary that named ONE frequency described none of its ways in.
        assert!(
            mail.body.contains("Reachable on: HF — 14.250 MHz"),
            "the body breaks the net down by way in: {}",
            mail.body
        );
        assert!(
            mail.html_body.contains("Reachable on: HF — 14.250 MHz"),
            "the html part carries the same breakdown as the text part"
        );
        // The HTML alternative is a first-class part, not a stub —
        // it carries the same title, the same count and the same station line.
        assert!(mail.html_body.contains("Sunday Traffic"));
        assert!(
            mail.html_body.contains("Participants: 1"),
            "the html part states the same participant count as the text part"
        );
        assert!(
            mail.html_body.contains("W1AW"),
            "the html part carries the same station lines as the text part"
        );
        // The HTML's attachment sentence is GATED on the real list, not written
        // as an unconditional promise: the port documents `attachments` as
        // possibly empty and the adapter has a live no-attachment branch.
        // Asserted as the two halves TOGETHER, because that is the invariant.
        assert_eq!(
            mail.html_body.contains("attached CSV and ADIF exports"),
            !mail.attachments.is_empty(),
            "the html part promises attachments exactly when it has them"
        );
        assert_eq!(mail.attachments.len(), 2);
        let csv = mail
            .attachments
            .iter()
            .find(|a| a.filename.ends_with(".csv"))
            .expect("csv attachment");
        // Byte-identical to export::to_csv over the same fold (reuse, not reimpl).
        assert_eq!(csv.content, export::to_csv(&folded, &ops, snapshot));
        let adif = mail
            .attachments
            .iter()
            .find(|a| a.filename.ends_with(".adi"))
            .expect("adif attachment");
        assert_eq!(adif.content, export::to_adif(&folded, snapshot, 61_000));
    }

    #[test]
    fn the_html_part_escapes_user_text_and_the_text_part_leaves_it_raw() {
        // A net title and a station name/location are arbitrary user free text
        // — control and bidi characters are rejected, but `<`, `&` and `"` are
        // not — and the HTML part is where they reach an HTML document.
        let hostile = r#"<script>alert("x")&</script>"#;
        let mut station = roster_entry(1, "W1AW", CheckInSource::Staff, None);
        station.name = parse_name(hostile).expect("valid name");
        station.location = parse_location(hostile).expect("valid location");
        let folded = closed_state(vec![station]);
        let ops = BTreeMap::new();
        let mail =
            build_summary_email(&row(snapshot(hostile, "20m", "ssb")), &folded, &ops, 61_000)
                .expect("render");

        // The HTML part carries no attacker-controlled markup: the raw form is
        // absent and the escaped form is present, for the title AND the roster.
        assert!(
            !mail.html_body.contains("<script>"),
            "the html part must carry no attacker-controlled markup, got: {}",
            mail.html_body
        );
        assert!(
            !mail.html_body.contains("alert(\"x\")"),
            "an unescaped double quote would break out of an attribute"
        );
        // askama's HTML escaper emits NUMERIC character references
        // (`&#60;`), not the named `&lt;` form — assert what it actually emits.
        assert_eq!(
            mail.html_body.matches("&#60;script&#62;").count(),
            3,
            "the escaped form appears once in the title and twice in the one \
             station line (its name and its location)"
        );
        assert!(
            mail.html_body.contains("&#38;"),
            "a bare `&` must be escaped in the html part"
        );
        assert!(
            mail.html_body.contains("&#34;"),
            "a double quote must be escaped in the html part"
        );

        // The plain-text part is NOT HTML-escaped — `&`, `<` and `\"` appear as
        // themselves. Escaping here would be the failure, not a hardening.
        assert!(
            mail.body.contains(hostile),
            "the text part must carry the raw bytes, got: {}",
            mail.body
        );
        assert!(
            !mail.body.contains("&#60;") && !mail.body.contains("&#38;"),
            "the text part must never be HTML-escaped"
        );
    }

    #[test]
    fn webhook_delivery_id_is_stable_per_session_and_distinct_across_sessions() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        assert_eq!(
            webhook_delivery_id(a),
            webhook_delivery_id(a),
            "same session → same idempotency key across retries"
        );
        assert_ne!(
            webhook_delivery_id(a),
            webhook_delivery_id(b),
            "different session → different key"
        );
    }

    #[test]
    fn egress_timeout_and_transport_are_transient_refusals_are_permanent() {
        assert!(is_transient_egress(&EgressError::Timeout));
        assert!(is_transient_egress(&EgressError::Transport(
            "connection failed".into()
        )));
        assert!(!is_transient_egress(&EgressError::BlockedAddress));
        assert!(!is_transient_egress(&EgressError::NotHttps));
        assert!(!is_transient_egress(&EgressError::InvalidUrl));
        assert!(!is_transient_egress(&EgressError::TooManyRedirects));
        assert!(!is_transient_egress(&EgressError::ResponseTooLarge));
    }

    #[test]
    fn http_status_classifies_2xx_success_5xx_transient_4xx_permanent() {
        assert_eq!(classify_status(200), StatusClass::Success);
        assert_eq!(classify_status(204), StatusClass::Success);
        assert_eq!(classify_status(500), StatusClass::Transient);
        assert_eq!(classify_status(503), StatusClass::Transient);
        assert_eq!(classify_status(400), StatusClass::Permanent);
        assert_eq!(classify_status(404), StatusClass::Permanent);
        assert_eq!(classify_status(301), StatusClass::Permanent);
    }

    #[test]
    fn http_status_429_classifies_as_transient_not_permanent() {
        // A rate limit is the receiver asking for a slower resend, not a
        // refusal, so retrying with backoff is correct where a real 4xx is not.
        assert_eq!(classify_status(429), StatusClass::Transient);
    }
}

#[cfg(test)]
mod discord_tests {
    use super::*;
    use netroll_adapters::pg::net_sessions::DefinitionSnapshot;
    use netroll_domain::fold::{ControlState, SessionLifecycle};
    use serde_json::Value;

    const BASE_URL: &str = "https://netroll.example.radio";

    fn snapshot(title: &str) -> DefinitionSnapshot {
        DefinitionSnapshot {
            title: title.to_owned(),
            description: None,
            connections: vec![super::tests::hf_wire("20m", "ssb")],
            net_category: "traffic".to_owned(),
            net_type: "directed".to_owned(),
            country: None,
            state: None,
            grid: None,
        }
    }

    fn row(snapshot: DefinitionSnapshot) -> NetSessionRow {
        NetSessionRow {
            id: Uuid::from_u128(10),
            definition_id: Uuid::from_u128(7),
            definition_version: 3,
            definition_snapshot: snapshot,
            lifecycle: SessionLifecycle::Closed,
            started_at_millis: Some(1_000),
            closed_at_millis: Some(61_000),
            control_state: ControlState::Active,
            active_ncs_account_id: None,
            stalled_at_millis: None,
            last_seq: 3,
            created_at_millis: 0,
            updated_at_millis: 61_000,
        }
    }

    fn closed_state(participants: usize) -> SessionState {
        SessionState {
            lifecycle: SessionLifecycle::Closed,
            started_at: Some(1_000),
            closed_at: Some(61_000),
            definition_id: Some(Uuid::from_u128(7)),
            definition_version: Some(3),
            roster: (0..participants)
                .map(|i| {
                    use netroll_domain::callsign::parse_callsign;
                    use netroll_domain::check_in::{
                        Precedence, StayingStatus, parse_location, parse_name,
                    };
                    RosterEntry {
                        check_in_id: Uuid::from_u128(100 + i as u128),
                        // Distinctive strings, so "no per-station data"
                        // asserts against real roster values rather than tokens
                        // that could never have appeared.
                        callsign: parse_callsign("W1AW").expect("valid callsign"),
                        added_at: 2_000,
                        added_seq: 2,
                        added_by: None,
                        source: CheckInSource::SelfService,
                        signal_report: None,
                        staying: StayingStatus::InAndOut,
                        name: parse_name("Marigold").expect("valid name"),
                        location: parse_location("Zzyzx").expect("valid location"),
                        grid: None,
                        precedence: Precedence::Routine,
                        traffic: None,
                        notes: None,
                        public_note: None,
                        worked: false,
                        version: 1,
                        via: None,
                        corrections: Vec::new(),
                        relayed_by: None,
                    }
                })
                .collect(),
            ..Default::default()
        }
    }

    // --- The truncation contract ------------------------------------

    /// Every JSON key the announcement may carry and, when Discord counts its
    /// value toward the 6000 aggregate, the cap that governs it. `None` is a
    /// container or a slot Discord does not count.
    ///
    /// The walker treats ANY key not in this table as a contract violation,
    /// which is what makes the bound DERIVED rather than restated: a new embed
    /// slot appears as an unknown key and reds with NO test edit, instead of
    /// shipping unmeasured against Discord's limits.
    const COUNTED_SLOTS: [(&str, Option<usize>); 11] = [
        ("embeds", None),
        ("title", Some(DISCORD_TITLE_CHARS)),
        ("url", None),
        ("description", Some(DISCORD_DESCRIPTION_CHARS)),
        ("fields", None),
        ("name", Some(DISCORD_FIELD_NAME_CHARS)),
        ("value", Some(DISCORD_FIELD_VALUE_CHARS)),
        ("inline", None),
        ("footer", None),
        ("text", Some(DISCORD_FOOTER_CHARS)),
        ("timestamp", None),
    ];

    /// Walks a serialized announcement, asserting every slot against the
    /// contract and returning the total characters Discord would count.
    fn walk_counted_chars(node: &Value, path: &str, total: &mut usize) {
        match node {
            Value::Object(map) => {
                for (key, child) in map {
                    let Some((_, cap)) = COUNTED_SLOTS.iter().find(|(name, _)| name == key) else {
                        panic!(
                            "the announcement carries a slot the truncation contract does not \
                             cover: `{path}.{key}`. Add it to the contract (with its cap and \
                             its place in the truncation order) before shipping it."
                        );
                    };
                    if let (Some(cap), Value::String(text)) = (cap, child) {
                        let len = text.chars().count();
                        assert!(
                            len <= *cap,
                            "`{path}.{key}` is {len} code points, over its cap of {cap}: {text}"
                        );
                        *total += len;
                    }
                    walk_counted_chars(child, &format!("{path}.{key}"), total);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk_counted_chars(item, &format!("{path}[{index}]"), total);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn the_announcement_stays_inside_every_declared_cap_on_adversarial_input() {
        // A title far past its cap in MULTIBYTE text — an ASCII fixture cannot
        // tell a code-point clamp from a byte clamp — a huge roster, and a long
        // base URL, none of which any slot may scale with.
        let long_title: String = "Ωμέγα ".repeat(200);
        let long_base = format!("https://{}.example.radio", "x".repeat(200));
        let message = build_discord_message(
            Uuid::from_u128(10),
            &row(snapshot(&long_title)),
            &closed_state(500),
            &long_base,
        );
        let json = serde_json::to_value(&message).expect("serialize");

        let embeds = json["embeds"].as_array().expect("embeds");
        assert_eq!(
            embeds.len(),
            1,
            "one embed, always (the 6000 is per MESSAGE)"
        );
        assert!(
            embeds[0]["fields"].as_array().expect("fields").len() <= DISCORD_FIELD_COUNT,
            "the field count is a compile-time constant, never a function of roster size"
        );

        let mut total = 0usize;
        walk_counted_chars(&json, "message", &mut total);
        assert!(
            total < DISCORD_AGGREGATE_CHARS,
            "the counted characters ({total}) must stay under Discord's aggregate limit"
        );
        assert!(
            total <= DECLARED_CAP_SUM,
            "the counted characters ({total}) cannot exceed the declared cap budget"
        );
    }

    // The per-slot and cap-sum arithmetic is deliberately NOT a test here. It is
    // `const _: () = assert!(…)` beside the constants, so widening a cap past
    // its Discord counterpart or past the 6000 aggregate fails the BUILD rather
    // than one test run. Written as tests, every one of those assertions is a
    // constant expression and clippy's `assertions_on_constants` rejects them
    // under `-D warnings` — the lint saying to put them where they belong.

    #[test]
    fn the_clamp_counts_code_points_and_marks_a_cut() {
        // Six code points, twelve bytes: a byte-based clamp cuts this and a
        // code-point one does not. This is the fixture that distinguishes them.
        let multibyte = "Ωμέγαξ";
        assert_eq!(multibyte.chars().count(), 6);
        assert_eq!(multibyte.len(), 12, "twice as many bytes as code points");
        assert_eq!(
            clamp_chars(multibyte, 6),
            multibyte,
            "exactly at the cap is not truncated"
        );
        let cut = clamp_chars(multibyte, 4);
        assert_eq!(
            cut.chars().count(),
            4,
            "the result is at most `cap` code points"
        );
        assert!(cut.ends_with('…'), "a cut is visible to the reader");
        assert_eq!(cut, "Ωμέ…");
        assert_eq!(clamp_chars(multibyte, 0), "");
    }

    /// Builds an announcement against `base_url` and returns
    /// `(description, embed_url, expected_link)`.
    fn description_for(base_url: &str) -> (String, String, String) {
        let message = build_discord_message(
            Uuid::from_u128(10),
            &row(snapshot("Sunday Traffic")),
            &closed_state(3),
            base_url,
        );
        (
            message.embeds[0].description.clone(),
            message.embeds[0].url.clone(),
            format!("{base_url}/live/{}", Uuid::from_u128(10)),
        )
    }

    #[test]
    fn the_description_ends_with_the_whole_session_link_even_when_the_prose_is_clamped() {
        // Three fixtures, because the first ALONE was this test's own bug: with
        // the short `BASE_URL` the description is nowhere near its cap, so it
        // contains no `…` and the clamping half was never exercised. Cases 2
        // and 3 reach the two clamping branches.

        // 1 — the ordinary case.
        let (description, embed_url, link) = description_for(BASE_URL);
        assert!(
            description.ends_with(&link),
            "the link is carried whole at the end: {description}"
        );
        assert!(
            !description.contains('…'),
            "and nothing is clamped at this length"
        );
        assert_eq!(embed_url, link, "and the title is clickable");

        // 2 — the PROSE is clamped and the link survives WHOLE: a 463-character
        // base URL makes a 505-character link, leaving six code points of prose.
        let long_base = format!("https://{}.example.radio", "x".repeat(441));
        let (description, embed_url, link) = description_for(&long_base);
        assert_eq!(link.chars().count(), 505, "the fixture reaches the branch");
        assert!(
            description.contains('…'),
            "the prose IS clamped here — that is the branch under test: {description}"
        );
        assert!(
            description.ends_with(&link),
            "and the clamp fell on the prose, never on the link: {description}"
        );
        assert!(
            description.chars().count() <= DISCORD_DESCRIPTION_CHARS,
            "still inside the cap"
        );
        assert_eq!(embed_url, link);

        // 3 — the link CANNOT fit the cap at all, so it is omitted whole rather
        // than cut: a broken URL in a chat channel is worse than none, and
        // `embed.url` still carries it.
        let huge_base = format!("https://{}.example.radio", "x".repeat(600));
        let (description, embed_url, link) = description_for(&huge_base);
        assert!(
            link.chars().count() > DISCORD_DESCRIPTION_CHARS,
            "the fixture reaches the branch"
        );
        // The first 32 code points of the link, not `/live/`: with a base URL
        // this long the clamp cuts inside the HOST, so a `/live/` check passes
        // vacuously under the mutation this asserts against.
        let link_head: String = link.chars().take(32).collect();
        assert!(
            !description.contains(&link_head),
            "no FRAGMENT of the link is left in the description: {description}"
        );
        assert!(
            !description.ends_with('…'),
            "and the description never ends in a cut URL: {description}"
        );
        assert!(
            description.chars().count() <= DISCORD_DESCRIPTION_CHARS,
            "still inside the cap"
        );
        assert_eq!(
            embed_url, link,
            "and the whole link is still reachable on `embed.url`, which is never clamped"
        );
    }

    #[test]
    fn the_announcement_carries_a_count_and_never_a_station() {
        let message = build_discord_message(
            Uuid::from_u128(10),
            &row(snapshot("Sunday Traffic")),
            &closed_state(3),
            BASE_URL,
        );
        let rendered = serde_json::to_string(&message).expect("serialize");
        assert!(
            rendered.contains("\"value\":\"3\""),
            "the check-in COUNT is the only roster fact carried: {rendered}"
        );
        for station_value in ["W1AW", "Marigold", "Zzyzx"] {
            assert!(
                !rendered.contains(station_value),
                "no per-station data is serialized, but `{station_value}` \
                 appears: {rendered}"
            );
        }
    }

    // --- The honoured `retry_after` ---------------------------------

    #[test]
    fn a_retry_after_within_the_cap_is_honoured_verbatim() {
        assert_eq!(
            discord_retry_after(br#"{"message":"rate limited","retry_after":1.25}"#),
            RetryAfter::Wait(Duration::from_millis(1_250)),
            "float SECONDS with millisecond precision (API v8+), not milliseconds"
        );
        assert_eq!(
            discord_retry_after(br#"{"retry_after":5.0}"#),
            RetryAfter::Wait(MAX_HONOURED_RETRY_AFTER),
            "exactly at the cap is still honoured"
        );
    }

    #[test]
    fn a_retry_after_above_the_cap_abandons_rather_than_sleeping() {
        // Discord's own documented examples are 64.57 and 1336.57, both
        // global-limit waits far past anything worth holding a permit for. The
        // wait is CARRIED, not discarded: the attempt is abandoned without
        // sleeping and the durable row reschedules for the time Discord named.
        assert_eq!(
            discord_retry_after(br#"{"retry_after":5.001}"#),
            RetryAfter::Abandon(Duration::from_secs_f64(5.001))
        );
        assert_eq!(
            discord_retry_after(br#"{"retry_after":64.57}"#),
            RetryAfter::Abandon(Duration::from_secs_f64(64.57))
        );
        assert_eq!(
            discord_retry_after(br#"{"retry_after":1336.57}"#),
            RetryAfter::Abandon(Duration::from_secs_f64(1336.57))
        );
        // The class that PANICKED: finite, positive, and too large for
        // `Duration`. `is_finite()` and `<= 0.0` both pass it through, and
        // `Duration::from_secs_f64` panics rather than erroring — inside a
        // `tokio::join!` branch, so the unwind took the email and webhook legs
        // with it. Attacker-influenceable: any host can answer a 429 with this
        // body.
        for body in [
            &br#"{"retry_after":2e19}"#[..],
            &br#"{"retry_after":1e30}"#[..],
            &br#"{"retry_after":1e300}"#[..],
            &br#"{"retry_after":1.7976931348623157e308}"#[..],
        ] {
            assert_eq!(
                discord_retry_after(body),
                RetryAfter::Abandon(Duration::from_millis(DELIVERY_RETRY_MAX_MILLIS)),
                "a `retry_after` past `Duration`'s own range is an over-cap wait like any \
                 other — never a panic: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn a_hostile_or_absent_retry_after_falls_back_to_the_fixed_backoff_and_still_retries() {
        for body in [
            &br#"{"retry_after":0}"#[..],
            &br#"{"retry_after":-5}"#[..],
            &br#"{"retry_after":null}"#[..],
            &br#"{"retry_after":"soon"}"#[..],
            &br#"{"message":"rate limited"}"#[..],
            &br#"{}"#[..],
            &b"<html>429 Too Many Requests</html>"[..],
            &b""[..],
        ] {
            assert_eq!(
                discord_retry_after(body),
                RetryAfter::Wait(RETRY_BACKOFF),
                "a 429 with an unusable body is still TRANSIENT — never an abandon, \
                 and never a zero-length sleep: {}",
                String::from_utf8_lossy(body)
            );
        }
        // `NaN`/`Infinity` are not valid JSON numbers, so the parse itself
        // rejects them — the same fallback, reached one step earlier.
        assert_eq!(
            discord_retry_after(br#"{"retry_after":NaN}"#),
            RetryAfter::Wait(RETRY_BACKOFF)
        );
    }

    // --- The destination-kind declaration and the leg results --------

    fn config(emails: Vec<String>, webhook: Option<&str>, discord: Option<&str>) -> DeliveryConfig {
        DeliveryConfig {
            emails,
            webhook_url: webhook.map(str::to_owned),
            webhook_secret_set: webhook.is_some(),
            discord_webhook_url: discord.map(str::to_owned),
        }
    }

    #[test]
    fn every_declared_destination_kind_can_arm_delivery_on_its_own() {
        // Derived from the declaration: each kind must be armable ALONE, which
        // is the property the deliverer's early return kept getting wrong.
        let armed_alone = [
            config(vec!["a@example.com".to_owned()], None, None),
            config(Vec::new(), Some("https://hooks.example.com/net"), None),
            config(
                Vec::new(),
                None,
                Some("https://discord.com/api/webhooks/1/t"),
            ),
        ];
        assert_eq!(
            armed_alone.len(),
            DeliveryDestinationKind::ALL.len(),
            "one single-target config per declared kind"
        );
        for (kind, config) in DeliveryDestinationKind::ALL.iter().zip(&armed_alone) {
            assert!(
                kind.is_armed(config),
                "{kind:?} should be armed by its own config"
            );
            assert!(
                !is_delivery_off(config),
                "a net whose ONLY target is {kind:?} has delivery ON"
            );
        }
        assert!(
            is_delivery_off(&config(Vec::new(), None, None)),
            "a config row arming nothing is still delivery-off"
        );
    }

    #[test]
    fn a_storage_error_in_either_fallible_leg_reaches_the_caller() {
        // `tokio::join!` already runs the legs to completion, but returning ONE
        // leg's Result discards the other's storage error, so it never reaches
        // the wrapper's log and a real fault looks like "not configured".
        assert!(combine_leg_storage_results(Ok(()), Ok(())).is_ok());
        assert!(
            combine_leg_storage_results(Ok(()), Err(sqlx::Error::RowNotFound)).is_err(),
            "the DISCORD leg's storage error must not be swallowed"
        );
        assert!(
            combine_leg_storage_results(Err(sqlx::Error::RowNotFound), Ok(())).is_err(),
            "nor the webhook leg's"
        );
        assert!(
            combine_leg_storage_results(
                Err(sqlx::Error::RowNotFound),
                Err(sqlx::Error::PoolClosed)
            )
            .is_err()
        );
    }

    #[test]
    fn the_summary_message_id_is_stable_per_session_distinct_across_sessions_and_a_dot_atom() {
        // The email's dedupe basis: same session, same id on every attempt;
        // different session, different id; and only RFC 5322 `atext`/`.`
        // characters, so the adapter's `<local@domain>` is a valid addr-spec.
        let a = Uuid::from_u128(0x17_07_01);
        let b = Uuid::from_u128(0x17_07_02);
        assert_eq!(summary_message_id(a), summary_message_id(a));
        assert_ne!(summary_message_id(a), summary_message_id(b));
        let atext = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c);
        for id in [summary_message_id(a), summary_message_id(b)] {
            assert!(id.chars().all(atext), "not a dot-atom: {id}");
            assert!(!id.starts_with('.') && !id.ends_with('.') && !id.contains(".."));
        }
    }
}

#[cfg(test)]
mod render_failure_tests {
    use super::*;

    fn leg(target: &str) -> DeliveryJob {
        DeliveryJob {
            id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            destination: "email".to_owned(),
            target: target.to_owned(),
            attempts: 1,
            request_sent: false,
            lease_until_millis: 0,
        }
    }

    #[test]
    fn a_summary_that_cannot_be_rendered_is_a_permanent_refusal_for_every_recipient() {
        // The session is CLOSED: the snapshot and fold are frozen, so a render
        // that failed once fails identically every time. Classifying it
        // `Transient` makes each recipient burn all ten claims over roughly two
        // hours re-rendering the same failure, and disagrees with the
        // payload-serialize arms, which are both `Permanent`.
        let a = leg("a@example.com");
        let b = leg("b@example.com");
        let legs = vec![&a, &b];

        let outcomes = unrenderable_summary_outcomes(&legs);

        assert_eq!(
            outcomes.len(),
            2,
            "every recipient is answered, not just one"
        );
        for (_, outcome) in &outcomes {
            assert_eq!(*outcome, LegOutcome::Permanent);
        }
    }
}

#[cfg(test)]
mod via_delivery_tests {
    //! WHICH way in each check-in came on, through the delivery
    //! surfaces — and the one place a fallback is allowed.

    use super::tests::{closed_state, roster_entry, row, snapshot};
    use super::*;
    use netroll_domain::check_in::CheckInSource;
    use netroll_domain::fold::RosterEntry;
    use netroll_domain::net::connection::Via;
    use netroll_domain::net::wire::UNRESOLVABLE_VIA_LABEL;

    // --- `via` on the delivery surfaces -------------------------

    fn cross_mode_row() -> NetSessionRow {
        let mut snap = snapshot("Cross-mode Net", "20m", "ssb");
        let hf = snap.connections[0].clone();
        snap.connections = vec![
            hf,
            NetConnectionWire {
                id: Uuid::from_u128(0x16_05),
                position: 1,
                kind: "echolink".to_owned(),
                planned_frequency_hz: None,
                band: None,
                mode: None,
                repeater_offset_hz: None,
                tone_mode: None,
                tone_value: None,
                node: Some("12345".to_owned()),
                reflector: None,
                network: None,
                talkgroup: None,
                label: None,
                detail: None,
            },
        ];
        row(snap)
    }

    fn entry_via(id: u128, call: &str, via: Option<Via>) -> RosterEntry {
        let mut entry = roster_entry(id, call, CheckInSource::Staff, None);
        entry.via = via;
        entry
    }

    #[test]
    fn the_webhook_roster_entry_carries_both_the_structured_via_and_its_label() {
        let session_row = cross_mode_row();
        let echolink = session_row.definition_snapshot.connections[1].id;
        let folded = closed_state(vec![entry_via(1, "W1AW", Some(Via::Connection(echolink)))]);
        let payload =
            build_webhook_payload(Uuid::from_u128(10), &session_row, &folded, &BTreeMap::new());
        let json = serde_json::to_value(&payload).expect("serialize");
        let entry = json["roster"][0].as_object().expect("an entry object");
        assert_eq!(entry["via"]["kind"], "connection");
        assert_eq!(entry["via"]["connectionId"], echolink.to_string());
        assert_eq!(entry["viaLabel"], "EchoLink — 12345");
    }

    #[test]
    fn a_free_text_via_reaches_the_webhook_as_the_operators_own_words() {
        let session_row = cross_mode_row();
        let folded = closed_state(vec![entry_via(
            1,
            "W1AW",
            Some(Via::Unlisted("a phone patch".to_owned())),
        )]);
        let payload =
            build_webhook_payload(Uuid::from_u128(10), &session_row, &folded, &BTreeMap::new());
        let json = serde_json::to_value(&payload).expect("serialize");
        let entry = json["roster"][0].as_object().expect("an entry object");
        assert_eq!(entry["via"]["kind"], "unlisted");
        assert_eq!(entry["via"]["text"], "a phone patch");
        assert_eq!(entry["viaLabel"], "a phone patch");
    }

    #[test]
    fn an_unresolvable_via_reaches_the_webhook_naming_the_fault_not_the_export_connection() {
        let session_row = cross_mode_row();
        let folded = closed_state(vec![entry_via(
            1,
            "W1AW",
            Some(Via::Connection(Uuid::from_u128(0xDEAD))),
        )]);
        let payload =
            build_webhook_payload(Uuid::from_u128(10), &session_row, &folded, &BTreeMap::new());
        let json = serde_json::to_value(&payload).expect("serialize");
        let entry = json["roster"][0].as_object().expect("an entry object");
        assert_eq!(entry["viaLabel"], UNRESOLVABLE_VIA_LABEL);
        assert_ne!(
            entry["viaLabel"].as_str(),
            Some(connection_label(&session_row.definition_snapshot.connections[0]).as_str()),
            "never the ADIF-export connection's label"
        );
    }

    #[test]
    fn the_adif_falls_back_for_an_unrecorded_via_and_the_webhook_for_the_same_entry_does_not() {
        // BOTH halves in ONE run. `serde_json`'s index returns `Null` for a
        // MISSING key and an explicit null alike, so the absence assertions go
        // through `contains_key` on the entry object.
        let session_row = cross_mode_row();
        let folded = closed_state(vec![entry_via(1, "W1AW", None)]);

        // The exports take the FROZEN snapshot, never a live set.
        let snapshot = &session_row.definition_snapshot.connections;
        let adif = export::to_adif(&folded, snapshot, 61_000);
        assert!(
            adif.contains("<BAND:3>20m"),
            "the .adi falls back to the session's ADIF-export connection: {adif}"
        );

        let payload =
            build_webhook_payload(Uuid::from_u128(10), &session_row, &folded, &BTreeMap::new());
        let json = serde_json::to_value(&payload).expect("serialize");
        let entry = json["roster"][0].as_object().expect("an entry object");
        assert!(
            entry.contains_key("callsign") && entry.contains_key("checkedInAt"),
            "the entry serialized its always-present keys, so absence below means absence"
        );
        assert!(
            !entry.contains_key("via") && !entry.contains_key("viaLabel"),
            "the export fallback is never laundered into the log as data"
        );

        let csv = export::to_csv(&folded, &BTreeMap::new(), snapshot);
        let cell = csv.lines().nth(1).expect("a data row").split(',').nth(13);
        assert_eq!(
            cell,
            Some(""),
            "and the CSV cell for the same entry is empty"
        );
    }

    #[test]
    fn the_summary_email_counts_the_check_ins_on_each_way_in() {
        // ⚠️ PROSE assertions, against the standing rule that a test asserts
        // behaviour and never text. They stand because the summary email has no
        // structured form to assert against — the body IS the product. The
        // limitation is real: a re-wording reds this test with no behaviour
        // change. What is pinned is the ARITHMETIC and the BUCKETING, which is
        // what the four distinct counts are chosen to separate.
        let session_row = cross_mode_row();
        let hf = session_row.definition_snapshot.connections[0].id;
        let echolink = session_row.definition_snapshot.connections[1].id;
        let folded = closed_state(vec![
            entry_via(1, "W1AW", Some(Via::Connection(hf))),
            entry_via(2, "W1ABC", Some(Via::Connection(echolink))),
            entry_via(3, "W1DEF", Some(Via::Connection(echolink))),
            entry_via(4, "W1GHI", None),
            entry_via(5, "W1JKL", Some(Via::Unlisted("a phone patch".to_owned()))),
            // A way in this session no longer lists — a DIFFERENT fact from the
            // free-text one above, and the reason the two have separate lines.
            entry_via(6, "W1MNO", Some(Via::Connection(Uuid::from_u128(0xDEAD)))),
        ]);
        let mail =
            build_summary_email(&session_row, &folded, &BTreeMap::new(), 61_000).expect("render");
        for expected in [
            // The HF count line names the way in and NO frequency; see
            // `the_count_lines_name_the_way_in_without_a_frequency_once_the_net_has_moved`
            // for the absence on a moved fixture. Internet ways in keep their
            // identifying fact.
            "Check-ins: 1 on HF",
            "Check-ins: 2 on EchoLink — 12345",
            "Check-ins: 1 on a way this net does not list",
            "Check-ins: 1 on a way in this net no longer lists",
            "Check-ins: 1 with no way in recorded",
        ] {
            // A whole LINE, not a substring: `Check-ins: 1 on HF` is a prefix
            // of `Check-ins: 1 on HF — 14.250 MHz`, so a `contains` stays green
            // across the change this exists to pin.
            assert!(
                mail.body.lines().any(|line| line == expected),
                "missing line {expected}: {}",
                mail.body
            );
            // A sibling test asserts BOTH parts: the HTML renders the same
            // `meta` block and a reader of either must see the same counts.
            assert!(
                mail.html_body.contains(expected),
                "the HTML alternative is missing {expected}: {}",
                mail.html_body
            );
        }
        assert!(
            !mail.html_body.contains("Check-ins: 1 on HF —"),
            "the HTML alternative's count line carries no frequency either: {}",
            mail.html_body
        );
    }

    #[test]
    fn the_count_lines_name_the_way_in_without_a_frequency_once_the_net_has_moved() {
        // A count under a frequency is a claim about EACH station counted, and
        // after a move it is false for the pre-move ones. The count line
        // therefore names the way in without a number, while the `Reachable on:`
        // lines describe the NET at close and keep the final frequency.
        // Asserted on the frequency's ABSENCE, not on the sentence.
        use netroll_domain::net::validation::format_frequency_mhz;

        let session_row = cross_mode_row();
        let hf = session_row.definition_snapshot.connections[0].id;
        let planned_hz = session_row.definition_snapshot.connections[0]
            .planned_frequency_hz
            .expect("the HF way in has a planned frequency");
        let moved_to_hz = 14_300_000;
        assert_ne!(planned_hz, moved_to_hz);
        let mut folded = closed_state(vec![
            entry_via(1, "W1AW", Some(Via::Connection(hf))),
            entry_via(2, "W1ABC", Some(Via::Connection(hf))),
        ]);
        folded.connection_frequencies.insert(hf, moved_to_hz);

        let mail =
            build_summary_email(&session_row, &folded, &BTreeMap::new(), 61_000).expect("render");
        let count_lines: Vec<&str> = mail
            .body
            .lines()
            .filter(|line| line.starts_with("Check-ins:"))
            .collect();
        assert!(!count_lines.is_empty(), "{}", mail.body);
        for hz in [planned_hz, moved_to_hz] {
            let mhz = format_frequency_mhz(hz);
            assert!(
                count_lines.iter().all(|line| !line.contains(&mhz)),
                "a count line attaches {mhz} to stations that were not all on it: {count_lines:?}"
            );
        }
        assert!(
            mail.body
                .lines()
                .filter(|line| line.starts_with("Reachable on:"))
                .any(|line| line.contains(&format_frequency_mhz(moved_to_hz))),
            "the net's own way-in line still names where it ended: {}",
            mail.body
        );
    }

    #[test]
    fn an_unresolvable_way_in_is_not_folded_in_with_the_free_text_ones() {
        // Two different facts, and only one is something an owner can act on: a
        // check-in pointing at a way the session has LOST is a defect, where an
        // operator typing a way the net never listed is the feature.
        let session_row = cross_mode_row();
        let folded = closed_state(vec![entry_via(
            1,
            "W1AW",
            Some(Via::Connection(Uuid::from_u128(0xDEAD))),
        )]);
        let mail =
            build_summary_email(&session_row, &folded, &BTreeMap::new(), 61_000).expect("render");
        assert!(
            mail.body
                .contains("Check-ins: 1 on a way in this net no longer lists"),
            "{}",
            mail.body
        );
        assert!(
            !mail.body.contains("does not list"),
            "the free-text bucket is not where an unresolvable via is reported: {}",
            mail.body
        );
    }

    #[test]
    fn the_unrecorded_bucket_is_named_rather_than_folded_into_the_lead_connection() {
        let session_row = cross_mode_row();
        let folded = closed_state(vec![
            entry_via(1, "W1AW", None),
            entry_via(2, "W1ABC", None),
        ]);
        let mail =
            build_summary_email(&session_row, &folded, &BTreeMap::new(), 61_000).expect("render");
        assert!(mail.body.contains("Check-ins: 2 with no way in recorded"));
        // A zero-count line per connection restates the `Reachable on:` block
        // directly above it and turns every closing net's summary into a block
        // of zeroes. The ways in are listed whether or not anyone used them;
        // this block is about who did.
        assert!(
            !mail.body.contains("Check-ins: 0 on"),
            "a way in nobody used gets no count line: {}",
            mail.body
        );
        assert!(
            mail.body.contains("Reachable on: HF — 14.250 MHz"),
            "the way in is still listed, just not counted: {}",
            mail.body
        );
    }

    #[test]
    fn the_discord_embed_carries_no_per_check_in_way_in() {
        // A stated NO: `Ways in` is a digest of the NET, not of the roster, and
        // per-check-in text in a 1024-character embed slot re-opens the
        // shortening path.
        //
        // Asserting only `!contains("W1AW")` proves the embed has no roster ROWS
        // and says nothing about a per-check-in `via`. THREE check-ins share one
        // way in here, so a projection carrying each entry's `via` would repeat
        // that label three more times than the digest does.
        let session_row = cross_mode_row();
        let echolink = session_row.definition_snapshot.connections[1].id;
        let folded = closed_state(vec![
            entry_via(1, "W1AW", Some(Via::Connection(echolink))),
            entry_via(2, "W1ABC", Some(Via::Connection(echolink))),
            entry_via(3, "W1DEF", Some(Via::Unlisted("a phone patch".to_owned()))),
        ]);
        let message = build_discord_message(
            Uuid::from_u128(10),
            &session_row,
            &folded,
            "https://example.test/s",
        );
        let wire = serde_json::to_string(&message).expect("serialize");
        assert_eq!(
            wire.matches("EchoLink — 12345").count(),
            1,
            "the label appears once, in the net's digest, and not once per check-in: {wire}"
        );
        assert!(
            !wire.contains("a phone patch"),
            "an operator's free-text way in never reaches the embed: {wire}"
        );
        assert!(
            !wire.contains("W1AW"),
            "and the embed still carries no roster rows at all: {wire}"
        );
    }
}

#[cfg(test)]
mod connection_surface_tests {
    //! The three delivery surfaces describe every way in, each asserted on a
    //! net with THREE different kinds — a same-kind fixture cannot tell a
    //! builder that carries every connection from one that carries the first.

    use super::tests::{closed_state, roster_entry, row, snapshot};
    use super::*;
    use netroll_domain::check_in::CheckInSource;
    use netroll_domain::net::connection::MAX_CONNECTIONS;

    fn three_ways() -> NetSessionRow {
        let mut snap = snapshot("Cross-mode Net", "20m", "ssb");
        let hf = snap.connections[0].clone();
        snap.connections = vec![
            hf,
            NetConnectionWire {
                id: Uuid::from_u128(0x16_05),
                position: 1,
                kind: "echolink".to_owned(),
                node: Some("12345".to_owned()),
                ..blank(1)
            },
            NetConnectionWire {
                id: Uuid::from_u128(0x16_06),
                position: 2,
                kind: "dmr".to_owned(),
                talkgroup: Some("31337".to_owned()),
                network: Some("Brandmeister".to_owned()),
                ..blank(2)
            },
        ];
        row(snap)
    }

    fn blank(position: i32) -> NetConnectionWire {
        NetConnectionWire {
            id: Uuid::nil(),
            position,
            kind: String::new(),
            planned_frequency_hz: None,
            band: None,
            mode: None,
            repeater_offset_hz: None,
            tone_mode: None,
            tone_value: None,
            node: None,
            reflector: None,
            network: None,
            talkgroup: None,
            label: None,
            detail: None,
        }
    }

    #[test]
    fn the_webhook_carries_every_connection_with_its_own_id() {
        // The ids are the ones each check-in's `via` refers to, so a receiver
        // joins the two without guessing.
        let payload = build_webhook_payload(
            Uuid::from_u128(10),
            &three_ways(),
            &closed_state(vec![roster_entry(1, "W1AW", CheckInSource::Staff, None)]),
            &BTreeMap::new(),
        );
        let json = serde_json::to_value(&payload).expect("serialize");
        let connections = json["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 3);
        assert_eq!(
            connections
                .iter()
                .map(|c| c["kind"].as_str().expect("kind"))
                .collect::<Vec<_>>(),
            vec!["hf", "echolink", "dmr"],
            "all three ways in, in the owner's order — not just the first"
        );
        assert_eq!(
            connections[2]["id"].as_str().expect("id"),
            Uuid::from_u128(0x16_06).to_string()
        );
    }

    #[test]
    fn the_summary_email_breaks_the_net_down_by_way_in() {
        // Per-connection CHECK-IN COUNTS need `via` on each roster entry, so
        // the breakdown is by way; inventing a split would report a confident
        // wrong number.
        let mail = build_summary_email(
            &three_ways(),
            &closed_state(vec![roster_entry(1, "W1AW", CheckInSource::Staff, None)]),
            &BTreeMap::new(),
            61_000,
        )
        .expect("render");
        for expected in [
            "Reachable on: HF — 14.250 MHz",
            "Reachable on: EchoLink — 12345",
            "Reachable on: DMR — TG 31337 on Brandmeister",
        ] {
            assert!(
                mail.body.contains(expected),
                "missing {expected}: {}",
                mail.body
            );
            assert!(
                mail.html_body.contains(expected),
                "missing {expected} in html"
            );
        }
    }

    #[test]
    fn the_discord_announcement_names_every_way_in_and_no_dead_frequency_field() {
        // Both embed fields lost their sources when the session-level frequency
        // and the flat band/mode retired; ONE field replaces them, and the old
        // labels must not linger empty.
        let message = build_discord_message(
            Uuid::from_u128(10),
            &three_ways(),
            &closed_state(vec![roster_entry(1, "W1AW", CheckInSource::Staff, None)]),
            "https://netroll.example.radio",
        );
        let json = serde_json::to_value(&message).expect("serialize");
        let fields = json["embeds"][0]["fields"]
            .as_array()
            .expect("embed fields");
        let names: Vec<&str> = fields
            .iter()
            .map(|f| f["name"].as_str().expect("name"))
            .collect();
        assert!(!names.contains(&"Frequency"), "the dead field is gone");
        assert!(!names.contains(&"Band / mode"), "the dead field is gone");
        let ways = fields
            .iter()
            .find(|f| f["name"] == "Ways in")
            .expect("the ways-in field");
        let value = ways["value"].as_str().expect("value");
        for expected in [
            "HF — 14.250 MHz",
            "EchoLink — 12345",
            "DMR — TG 31337 on Brandmeister",
        ] {
            assert!(value.contains(expected), "missing {expected} in {value}");
        }
    }

    #[test]
    fn the_ways_in_field_never_silently_drops_a_way_to_reach_the_net() {
        // `Ways in` was once clamped by `DISCORD_FIELD_VALUE_CHARS`, a cap
        // written for a scalar, while the list it holds is bounded by
        // `MAX_CONNECTIONS`. A three-connection fixture rendered 63 characters
        // against a cap of 64, so ONE more way in cut the list with `…` and
        // dropped ways to reach the net from the one surface whose purpose is
        // naming them. `DECLARED_CAP_SUM` cannot catch that: it proves the caps
        // fit Discord's limits, not that the content fits the caps.
        let mut row = three_ways();
        row.definition_snapshot.connections.push(NetConnectionWire {
            id: Uuid::from_u128(0x16_07),
            position: 3,
            kind: "allstar".to_owned(),
            node: Some("54321".to_owned()),
            ..blank(3)
        });
        let message = build_discord_message(
            Uuid::from_u128(10),
            &row,
            &closed_state(vec![roster_entry(1, "W1AW", CheckInSource::Staff, None)]),
            "https://netroll.example.radio",
        );
        let json = serde_json::to_value(&message).expect("serialize");
        let value = json["embeds"][0]["fields"]
            .as_array()
            .expect("embed fields")
            .iter()
            .find(|f| f["name"] == "Ways in")
            .expect("the ways-in field")["value"]
            .as_str()
            .expect("value")
            .to_owned();
        for expected in [
            "HF — 14.250 MHz",
            "EchoLink — 12345",
            "DMR — TG 31337 on Brandmeister",
            "AllStar — 54321",
        ] {
            assert!(value.contains(expected), "missing {expected} in {value}");
        }
    }

    #[test]
    fn an_overlong_ways_in_list_says_how_many_it_could_not_show() {
        // The list IS bounded — `MAX_CONNECTIONS` (32) entries, each carrying up
        // to `MAX_REPEATER_TEXT_CHARS` of owner text — and that bound is far
        // above Discord's own 1024-character field limit, so SOME list cannot be
        // shown whole. What must never happen is showing a shortened list that
        // reads as complete: the cut is by WHOLE LINE and it states the count it
        // dropped.
        let mut row = three_ways();
        row.definition_snapshot.connections = (0..MAX_CONNECTIONS as i32)
            .map(|position| NetConnectionWire {
                id: Uuid::from_u128(0x2000 + position as u128),
                position,
                kind: "dmr".to_owned(),
                talkgroup: Some("31337".to_owned()),
                network: Some("N".repeat(64)),
                ..blank(position)
            })
            .collect();
        let message = build_discord_message(
            Uuid::from_u128(10),
            &row,
            &closed_state(vec![]),
            "https://netroll.example.radio",
        );
        let json = serde_json::to_value(&message).expect("serialize");
        let value = json["embeds"][0]["fields"]
            .as_array()
            .expect("embed fields")
            .iter()
            .find(|f| f["name"] == "Ways in")
            .expect("the ways-in field")["value"]
            .as_str()
            .expect("value")
            .to_owned();
        assert!(
            value.chars().count() <= DISCORD_WAYS_IN_VALUE_CHARS,
            "still inside its own declared cap: {}",
            value.chars().count()
        );
        assert!(
            !value.contains('…'),
            "the cut is by whole line, never a mid-word ellipsis: {value}"
        );
        let shown = value.lines().filter(|l| l.starts_with("DMR")).count();
        assert!(shown > 0, "some ways in are still shown: {value}");
        assert!(
            value.contains(&format!("+{} more", MAX_CONNECTIONS - shown)),
            "the reader is told exactly how many ways were not shown: {value}"
        );
    }
}
