// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The ways people can reach a net, each carrying only the properties that way
//! actually has: the kind set is closed and each payload is per-variant, so
//! "EchoLink has no frequency" is unrepresentable rather than merely unset.
//! Unrelated to the frontend's WebSocket `ConnectionState` and to the relay
//! tier of [`crate::authz::Role`]; the `kind` column is validated here.

use thiserror::Error;
use uuid::Uuid;

use super::enums::{Band, Mode, ToneMode};
use super::validation::{FrequencyError, check_frequency_hz, check_offset_hz};
use crate::export::{band_to_adif, mode_to_adif};
use crate::profile::{ProfileError, parse_bounded_multiline_text, parse_bounded_text};

/// The reserved `label` namespace a machine-classified `other` connection
/// lives in. An owner can never type a label in this namespace — see
/// [`is_reserved_label`], which is the one predicate that decides — so a count
/// of `other` labels can always separate "the enum is cut wrong, an owner
/// reached for `other`" from "nothing could classify this value".
pub const UNCLASSIFIED_LABEL_PREFIX: &str = "unclassified-";

/// The count-key spelling of the reserved namespace. Every reservation
/// decision — on the read path and on the write path — compares against this,
/// so a capital, a space or a non-breaking hyphen cannot walk a label out of
/// the namespace it belongs to.
const UNCLASSIFIED_COUNT_KEY: &str = "unclassified";

/// The most ways to reach one net a single write may declare.
///
/// A ceiling rather than no ceiling because the list is written by an
/// unbounded `INSERT` loop inside the transaction that holds the definition's
/// row lock: without one, a client decides how long every other writer waits.
/// Well above any plausible net — the largest real case is a handful of RF
/// ways plus one entry per digital network.
pub const MAX_CONNECTIONS: usize = 32;

/// The character ceiling on an `other` connection's prose.
///
/// Its own constant rather than a share of the definition description's:
/// `detail` is serialized on the PUBLIC body and the two fields' bounds move
/// for different reasons.
pub const MAX_CONNECTION_DETAIL_CHARS: usize = 2000;

/// The single-line connection properties' length bound, in characters,
/// post-trim — tone value, node, reflector, network, talkgroup and `other`'s
/// label. One number for all six because a single-line connection property is
/// one size of thing; it was the definition's own repeater-text bound until
/// those fields were retired, and the value was kept so nothing an owner had
/// already stored became too long overnight.
pub const MAX_CONNECTION_TEXT_CHARS: usize = 64;

/// Why a connection or connection set could not be built or changed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NetConnectionError {
    /// A net must declare at least one way to reach it.
    #[error("a net must have at least one connection")]
    Empty,
    /// The set holds exactly one connection and it was asked to be removed.
    #[error("the only remaining connection cannot be removed")]
    LastRemaining,
    /// No connection in the set carries that id.
    #[error("no connection with id {0}")]
    NotFound(Uuid),
    /// A reorder named a different multiset of ids than the set holds.
    #[error("the reorder must name every connection exactly once")]
    ReorderMismatch,
    /// An `other` connection's label was blank after normalization.
    #[error("an `other` connection needs a label")]
    LabelEmpty,
    /// An owner-supplied label reached into the machine-only namespace.
    #[error("`{UNCLASSIFIED_LABEL_PREFIX}…` is reserved and cannot be chosen")]
    LabelReserved,
    /// The `kind` token is not in the closed kind set.
    #[error("unknown connection kind: {0}")]
    UnknownKind(String),
    /// The kind was named but a property it requires was absent.
    #[error("a `{kind}` connection needs `{property}`")]
    MissingProperty {
        /// The kind token that was named.
        kind: &'static str,
        /// The property that was absent.
        property: &'static str,
    },
    /// A property was supplied but is not a value of its enumeration.
    #[error("a `{kind}` connection's `{property}` is not a value it can take")]
    UnknownValue {
        /// The kind token that was named.
        kind: &'static str,
        /// The property whose value is not in the enumeration.
        property: &'static str,
    },
    /// A numeric property was supplied but could not be used — carrying the
    /// parser's own reason, so the message names the fault instead of
    /// reciting the rule.
    #[error("a `{kind}` connection's `{property}` {source}")]
    UnusableProperty {
        /// The kind token that was named.
        kind: &'static str,
        /// The property whose value could not be used.
        property: &'static str,
        /// Why the parser refused it.
        source: FrequencyError,
    },
    /// A text property failed the shared bounded-text guard — too long, or
    /// carrying a control or bidi-control character.
    #[error("a `{kind}` connection's `{property}` {source}")]
    InvalidText {
        /// The kind token that was named.
        kind: &'static str,
        /// The property the guard refused.
        property: &'static str,
        /// Which guard refused it, and the fact its message needs.
        source: ProfileError,
    },
    /// Two entries in one list named the same connection.
    #[error("connection {0} is named twice")]
    DuplicateId(Uuid),
    /// The list declares more ways to reach the net than one write may carry.
    #[error("a net may declare at most {max} connections")]
    TooMany {
        /// The ceiling that was exceeded.
        max: usize,
    },
    /// Which entry in a submitted list failed, and why. The index is the
    /// client's own zero-based array index, so an owner editing a list of ten
    /// is told which one to fix rather than which rule exists.
    #[error("connection {index}: {source}")]
    Entry {
        /// Zero-based index into the submitted list.
        index: usize,
        /// The per-entry failure.
        source: Box<NetConnectionError>,
    },
}

/// One way to reach a net, with exactly the properties that way has.
///
/// Two semantic puns live in this enum and are called out because a reader who
/// meets the wrong one writes a correct-looking wrong value:
///
/// - **`node` serves both EchoLink and AllStar**, and they are *different
///   namespaces*: the variant discriminant — [`Self::EchoLink`] vs
///   [`Self::AllStar`] — is the only disambiguator. An EchoLink node is a
///   numeric node number (`12345`) or a callsign-with-suffix (`N1CCK-R`); an
///   AllStar node is a bare numeric node number (`40000`) from a different,
///   unrelated registry. The same digits mean different stations on the two
///   networks.
/// - **`reflector` serves D-Star, YSF and URF** — three naming conventions in
///   one text field, again disambiguated only by the discriminant. A D-Star
///   reflector is `REF030 C` / `XLX307 B` (a module letter matters); a YSF
///   reflector is a room name or its numeric id (`America Link`, `21493`); a
///   URF reflector is `URF307 B` (M17-era naming, module letter again). DMR is
///   deliberately NOT in this pun — it is addressed by *talkgroup*, a number on
///   a network (`3100` on Brandmeister), which is not a reflector at all.
/// - **`network` is DMR's alone**, and it is a third pun waiting to happen —
///   though not the pun a reader braces for. It names a DMR routing system
///   (Brandmeister, TGIF, FreeDMR) that a talkgroup number lives on. The IP
///   sense of "network" is not the collision to worry about: it is confined to
///   `egress.rs` and never appears near a connection. The confusable
///   neighbour is one field away in THIS enum. The prose around `reflector`
///   uses "network" for *which digital system a reflector belongs to* —
///   `NetConnectionKind::unclassified`'s doc ("a reflector with no network
///   named") and the residue note the editor shows an operator in
///   `ConnectionListEditor.tsx` ("could not tell which network this value
///   belongs to"). **That sense is the DISCRIMINANT** — D-Star vs YSF vs URF —
///   which is closed, structural and never stored as text. **This field is
///   neither**: it is free text the operator types, stored in its own nullable
///   column, and it does not disambiguate anything, because `Dmr` is already
///   one variant. No other variant holds this field, and a kind that later
///   wants one needs its own name for it rather than a share of this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetConnectionKind {
    /// An HF/VHF/UHF simplex or direct frequency.
    Hf {
        /// Exact Hz, never a float.
        planned_frequency_hz: i64,
        /// Band.
        band: Band,
        /// Mode.
        mode: Mode,
    },
    /// A repeater: an RF frequency plus the shift and tone needed to key it.
    Repeater {
        /// The repeater's output frequency in exact Hz.
        planned_frequency_hz: i64,
        /// Band.
        band: Band,
        /// Mode.
        mode: Mode,
        /// Signed input shift in Hz, or `None`.
        offset_hz: Option<i64>,
        /// Tone mode, or `None`.
        tone_mode: Option<ToneMode>,
        /// Tone value, or `None`.
        tone_value: Option<String>,
    },
    /// An EchoLink node — see the enum's own doc for the `node` pun.
    EchoLink {
        /// EchoLink node number or callsign-with-suffix.
        node: String,
    },
    /// An AllStarLink node — see the enum's own doc for the `node` pun.
    AllStar {
        /// AllStarLink node number.
        node: String,
    },
    /// A DMR talkgroup on a network. Not a reflector.
    Dmr {
        /// Talkgroup id, as text (leading zeros survive).
        talkgroup: String,
        /// The DMR network the talkgroup lives on — free text as the operator
        /// wrote it, never a closed set. `None` when nobody recorded one,
        /// which is every connection minted before the column existed.
        network: Option<String>,
    },
    /// A D-Star reflector — see the enum's own doc for the `reflector` pun.
    DStar {
        /// Reflector and module, e.g. `REF030 C`.
        reflector: String,
    },
    /// A System Fusion (YSF) reflector/room — see the `reflector` pun.
    Ysf {
        /// Room name or numeric id.
        reflector: String,
    },
    /// A URF (M17-era) reflector — see the `reflector` pun.
    Urf {
        /// Reflector and module, e.g. `URF307 B`.
        reflector: String,
    },
    /// A way to reach the net the closed set does not name.
    Other {
        /// Short, normalized name — the thing that is counted.
        label: String,
        /// The prose, which is never the thing counted.
        detail: Option<String>,
    },
}

/// WHICH kind a connection is, with none of that kind's properties — the
/// closed vocabulary a kind is named by on the wire, in storage and on a
/// discovery filter.
///
/// [`NetConnectionKind`] carries each kind's payload and so cannot be named
/// without one; a filter that asks for "every EchoLink net" has no node to
/// give. This enum is the payload-free twin, and the one place the nine
/// spellings are written FOR the wire, storage and the filter:
/// `NetConnectionKind::as_str` delegates to [`Self::as_str`] rather than
/// keeping its own copy. A tenth kind added to either enum fails to compile
/// until [`NetConnectionKind::token`], `as_str` and `try_from` all name it, so
/// the filter vocabulary cannot fall behind the domain in silence — the drift a
/// hand-copied `const KINDS` list would allow.
///
/// Three token→payload parsers still match the spellings by hand and are NOT
/// yet routed through this enum: `parse_kind` below, `wire::kind_of` and the
/// adapter's `decode_named_kind`. A tenth kind reaches them as a run-time
/// refusal rather than a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetConnectionKindToken {
    /// `hf` — a simplex or direct RF frequency.
    Hf,
    /// `repeater`.
    Repeater,
    /// `echolink`.
    EchoLink,
    /// `allstar`.
    AllStar,
    /// `dmr`.
    Dmr,
    /// `dstar`.
    DStar,
    /// `ysf`.
    Ysf,
    /// `urf`.
    Urf,
    /// `other` — the owner-supplied kind outside the closed set.
    Other,
}

impl NetConnectionKindToken {
    /// The stable lowercase-kebab storage/wire spelling of this kind — the
    /// one place those nine spellings are written for the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hf => "hf",
            Self::Repeater => "repeater",
            Self::EchoLink => "echolink",
            Self::AllStar => "allstar",
            Self::Dmr => "dmr",
            Self::DStar => "dstar",
            Self::Ysf => "ysf",
            Self::Urf => "urf",
            Self::Other => "other",
        }
    }
}

impl TryFrom<&str> for NetConnectionKindToken {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "hf" => Ok(Self::Hf),
            "repeater" => Ok(Self::Repeater),
            "echolink" => Ok(Self::EchoLink),
            "allstar" => Ok(Self::AllStar),
            "dmr" => Ok(Self::Dmr),
            "dstar" => Ok(Self::DStar),
            "ysf" => Ok(Self::Ysf),
            "urf" => Ok(Self::Urf),
            "other" => Ok(Self::Other),
            _ => Err(()),
        }
    }
}

impl NetConnectionKind {
    /// Which kind this is, without its properties — the token the wire,
    /// storage and a discovery filter name it by.
    pub fn token(&self) -> NetConnectionKindToken {
        match self {
            Self::Hf { .. } => NetConnectionKindToken::Hf,
            Self::Repeater { .. } => NetConnectionKindToken::Repeater,
            Self::EchoLink { .. } => NetConnectionKindToken::EchoLink,
            Self::AllStar { .. } => NetConnectionKindToken::AllStar,
            Self::Dmr { .. } => NetConnectionKindToken::Dmr,
            Self::DStar { .. } => NetConnectionKindToken::DStar,
            Self::Ysf { .. } => NetConnectionKindToken::Ysf,
            Self::Urf { .. } => NetConnectionKindToken::Urf,
            Self::Other { .. } => NetConnectionKindToken::Other,
        }
    }

    /// The stable lowercase-kebab storage/wire token for this kind — spelled
    /// in exactly one place, [`NetConnectionKindToken::as_str`].
    pub fn as_str(&self) -> &'static str {
        self.token().as_str()
    }

    /// Builds an owner-authored `other` connection, normalizing `label` and
    /// refusing the machine-only [`UNCLASSIFIED_LABEL_PREFIX`] namespace.
    ///
    /// Both fields go through the same guards the definition applies to its
    /// own free text: `label` is single-line and bounded, `detail` is prose
    /// and bounded. `detail` is serialized on the
    /// PUBLIC net body, so neither may be unbounded and neither may carry a
    /// bidi control.
    pub fn other(label: &str, detail: Option<String>) -> Result<Self, NetConnectionError> {
        let label = collapse_whitespace(&bounded_text_value(label, "other", "label")?);
        if label.is_empty() {
            return Err(NetConnectionError::LabelEmpty);
        }
        if is_reserved_label(&label) {
            return Err(NetConnectionError::LabelReserved);
        }
        Ok(Self::Other {
            label,
            detail: bounded_prose(detail.as_deref(), "other", "detail")?,
        })
    }

    /// Mints the machine-classified `other` a value the closed set cannot name
    /// degrades into, preserving the original verbatim in `detail`.
    ///
    /// `source` names where the unclassifiable value came from — the flat
    /// column backfill read, or the `kind` a stored row claimed.
    ///
    /// **Two writers reach this, and the reserved namespace covers both:** the
    /// migration's backfill and the read path's row-level degrade. A third —
    /// the scalar edit, when a `reflector` value arrived with no connection
    /// already holding it — retired with the flat request shape;
    /// the connection editor names a reflector's network as a kind, so live
    /// owner input can no longer arrive with the network unknown.
    pub fn unclassified(source: &str, original: String) -> Self {
        Self::Other {
            label: format!("{UNCLASSIFIED_LABEL_PREFIX}{source}"),
            detail: Some(original),
        }
    }

    /// The aggregation key for an `other` connection — case-folded and with
    /// every run of separators flattened, so `Wires-X`, `WIRES-X` and
    /// ` wires x ` all count as one. `None` for every named kind.
    ///
    /// The stored `label` keeps the owner's own spelling, because it is what
    /// the public body renders; the key is what the review aggregate groups by.
    /// That aggregate is SQL — it is recorded in
    /// `20260829100200_backfill_net_connections.sql`'s header and lives on the
    /// database, not in a Rust call site — so this function is the definition
    /// the SQL twin is written against, and
    /// `schema_net_connections::the_review_aggregate_counts_one_row_per_normalised_label`
    /// runs the twin against real Postgres so the two cannot drift.
    pub fn count_key(&self) -> Option<String> {
        match self {
            Self::Other { label, .. } => Some(count_key_of(label)),
            _ => None,
        }
    }
}

/// True when `label` falls in the machine-only namespace.
///
/// Compared on the count key, so the read path and the write path can never
/// disagree about which labels are reserved. A raw `starts_with` on the
/// prefix is side-steppable three ways — case, a space for the hyphen, and
/// U+2011 for the hyphen — each of which mints a label that reads as residue
/// in the review aggregate while being owner-typed.
pub fn is_reserved_label(label: &str) -> bool {
    count_key_of(label)
        .strip_prefix(UNCLASSIFIED_COUNT_KEY)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// Trims and collapses every run of whitespace to a single space.
fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The single-line free-text guard, reported against the kind and property
/// that failed it. Every single-line property — tone value, node, reflector,
/// network, talkgroup and label — shares [`MAX_CONNECTION_TEXT_CHARS`].
fn bounded_text_value(
    value: &str,
    kind: &'static str,
    property: &'static str,
) -> Result<String, NetConnectionError> {
    parse_bounded_text(value, MAX_CONNECTION_TEXT_CHARS).map_err(|source| {
        NetConnectionError::InvalidText {
            kind,
            property,
            source,
        }
    })
}

/// As [`bounded_text_value`], but for an optional wire field: absent or
/// blank-after-trim is `None`, exactly as the definition's own optional text
/// fields are treated.
fn bounded_text(
    value: Option<&str>,
    kind: &'static str,
    property: &'static str,
) -> Result<Option<String>, NetConnectionError> {
    match value {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Ok(None),
        Some(v) => bounded_text_value(v, kind, property).map(Some),
    }
}

/// The prose guard: line breaks are the author's own structure,
/// control and bidi characters are not.
fn bounded_prose(
    value: Option<&str>,
    kind: &'static str,
    property: &'static str,
) -> Result<Option<String>, NetConnectionError> {
    match value {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Ok(None),
        Some(v) => parse_bounded_multiline_text(v, MAX_CONNECTION_DETAIL_CHARS)
            .map(Some)
            .map_err(|source| NetConnectionError::InvalidText {
                kind,
                property,
                source,
            }),
    }
}

/// Case-folds and flattens every run of non-alphanumeric characters to one
/// space, so a hyphen and a space are the same separator. Without that,
/// `Wires-X` and `wires x` would count as two distinct answers to the same
/// question, which is the aggregate this key exists to make answerable.
fn count_key_of(label: &str) -> String {
    label
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One connection on a net definition: an identity, its place in the owner's
/// order, and the way itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConnection {
    /// Stable id — what a check-in's [`Via`] refers to.
    pub id: Uuid,
    /// Zero-based place in the owner's order. Dense across the set, by the
    /// [`NetConnectionSet`] invariant.
    pub position: i32,
    /// The way to reach the net.
    pub kind: NetConnectionKind,
}

/// A net's connections: non-empty, ordered, and densely positioned from zero.
///
/// The invariant is enforced in exactly one place — [`Self::new`] — and every
/// mutation returns a new set rather than mutating in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConnectionSet {
    connections: Vec<NetConnection>,
}

impl NetConnectionSet {
    /// The one constructor. Orders by the supplied `position`, renumbers dense
    /// from zero, and refuses an empty set.
    pub fn new(mut connections: Vec<NetConnection>) -> Result<Self, NetConnectionError> {
        if connections.is_empty() {
            return Err(NetConnectionError::Empty);
        }
        // Stable, so two connections handed the same position keep the caller's
        // order rather than swapping unpredictably between reads.
        connections.sort_by_key(|c| c.position);
        for (index, connection) in connections.iter_mut().enumerate() {
            connection.position = index as i32;
        }
        Ok(Self { connections })
    }

    /// The connections, in order, densely positioned from zero.
    pub fn connections(&self) -> &[NetConnection] {
        &self.connections
    }

    /// The connection carrying `id`, if the set holds one.
    pub fn find(&self, id: Uuid) -> Option<&NetConnection> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// Removes `id` and renumbers the survivors dense from zero, refusing to
    /// remove the last remaining connection.
    pub fn without(&self, id: Uuid) -> Result<Self, NetConnectionError> {
        if self.find(id).is_none() {
            return Err(NetConnectionError::NotFound(id));
        }
        if self.connections.len() == 1 {
            return Err(NetConnectionError::LastRemaining);
        }
        Self::new(
            self.connections
                .iter()
                .filter(|c| c.id != id)
                .cloned()
                .collect(),
        )
    }

    /// Reorders to exactly `order`, which must name every connection once.
    pub fn reordered(&self, order: &[Uuid]) -> Result<Self, NetConnectionError> {
        if order.len() != self.connections.len() {
            return Err(NetConnectionError::ReorderMismatch);
        }
        let mut reordered = Vec::with_capacity(order.len());
        for (position, id) in order.iter().enumerate() {
            let existing = self.find(*id).ok_or(NetConnectionError::ReorderMismatch)?;
            if reordered.iter().any(|c: &NetConnection| c.id == *id) {
                return Err(NetConnectionError::ReorderMismatch);
            }
            reordered.push(NetConnection {
                position: position as i32,
                ..existing.clone()
            });
        }
        Self::new(reordered)
    }
}

/// The raw wire shape of one connection, exactly as the HTTP boundary receives
/// it. Mirrors [`super::validation::RawNetDefinition`]'s posture — parsing and
/// range checks happen here, not at the transport — with one deliberate
/// departure: the two frequency facts arrive TYPED, as exact-Hz integers under
/// the same keys the read serves them on (`plannedFrequencyHz`,
/// `repeaterOffsetHz`), and are range-checked here rather than parsed. One
/// vocabulary for one concept: the decimal-MHz string spelling the write body
/// used to take was justified by the flat definition body carrying the same
/// asymmetry; that body's frequency field is retired, leaving the
/// connection write the only place the API still spoke MHz.
/// Every other field stays text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawNetConnection {
    /// Kind token — required.
    pub kind: Option<String>,
    /// Planned frequency in exact Hz, as served — range-checked, never parsed.
    pub planned_frequency_hz: Option<i64>,
    /// Band token.
    pub band: Option<String>,
    /// Mode token.
    pub mode: Option<String>,
    /// Signed repeater offset in exact Hz, as served.
    pub repeater_offset_hz: Option<i64>,
    /// Tone-mode token.
    pub tone_mode: Option<String>,
    /// Tone value.
    pub tone_value: Option<String>,
    /// EchoLink or AllStar node.
    pub node: Option<String>,
    /// D-Star, YSF or URF reflector.
    pub reflector: Option<String>,
    /// DMR network — free text, optional.
    pub network: Option<String>,
    /// DMR talkgroup.
    pub talkgroup: Option<String>,
    /// `other`'s countable label.
    pub label: Option<String>,
    /// `other`'s prose.
    pub detail: Option<String>,
}

/// Parses a raw connection list into the validated set.
///
/// Ids are supplied by the caller, never minted here: this crate is pure and a
/// UUIDv7 reads the clock. A caller replacing a list passes each connection's
/// existing id through so a `Via` reference survives the edit, and mints only
/// for connections that are new.
///
/// `existing` is the set the definition holds today, and it is what makes a
/// reserved label writable: a machine-minted label cannot be typed, but a
/// client that read a definition and is writing the list back must be able to
/// echo the one it was given, or the definition becomes unwritable.
///
/// Every per-entry failure is wrapped in [`NetConnectionError::Entry`] so the
/// index the client sent comes back with the reason.
pub fn parse_connection_set(
    raw: Vec<(Uuid, RawNetConnection)>,
    existing: Option<&NetConnectionSet>,
) -> Result<NetConnectionSet, NetConnectionError> {
    if raw.len() > MAX_CONNECTIONS {
        return Err(NetConnectionError::TooMany {
            max: MAX_CONNECTIONS,
        });
    }
    let mut connections: Vec<NetConnection> = Vec::with_capacity(raw.len());
    for (position, (id, entry)) in raw.into_iter().enumerate() {
        // Two entries under one id become two rows with one primary key, which
        // the adapter can only report as a constraint violation — a 500 on a
        // request the boundary can name precisely.
        if connections.iter().any(|c| c.id == id) {
            return Err(NetConnectionError::DuplicateId(id));
        }
        let kind = parse_kind(entry, id, existing).map_err(|source| NetConnectionError::Entry {
            index: position,
            source: Box::new(source),
        })?;
        connections.push(NetConnection {
            id,
            position: position as i32,
            kind,
        });
    }
    NetConnectionSet::new(connections)
}

/// Reads one raw entry into its per-variant payload, refusing a kind that names
/// a property it does not carry.
///
/// The three refusals are distinct and stay distinct: a property that is absent
/// is [`NetConnectionError::MissingProperty`], one outside its enumeration is
/// [`NetConnectionError::UnknownValue`], and one the parser could not use is
/// [`NetConnectionError::UnusableProperty`] carrying the parser's own reason.
/// Collapsing the last two into the first tells an owner a field they filled in
/// is empty.
fn parse_kind(
    raw: RawNetConnection,
    id: Uuid,
    existing: Option<&NetConnectionSet>,
) -> Result<NetConnectionKind, NetConnectionError> {
    let token = raw.kind.as_deref().unwrap_or_default().trim().to_owned();
    match token.as_str() {
        "hf" | "repeater" => {
            let kind = if token == "hf" { "hf" } else { "repeater" };
            // The property named in an error is the WIRE key — what the caller
            // has to add or fix.
            let planned_frequency_hz = match raw.planned_frequency_hz {
                None => {
                    return Err(NetConnectionError::MissingProperty {
                        kind,
                        property: "plannedFrequencyHz",
                    });
                }
                Some(hz) => check_frequency_hz(hz).map_err(|source| {
                    NetConnectionError::UnusableProperty {
                        kind,
                        property: "plannedFrequencyHz",
                        source,
                    }
                })?,
            };
            let band = enum_token(raw.band.as_deref(), kind, "band", |v| Band::try_from(v))?;
            let mode = enum_token(raw.mode.as_deref(), kind, "mode", |v| Mode::try_from(v))?;
            if token == "hf" {
                Ok(NetConnectionKind::Hf {
                    planned_frequency_hz,
                    band,
                    mode,
                })
            } else {
                let offset_hz = match raw.repeater_offset_hz {
                    None => None,
                    Some(hz) => Some(check_offset_hz(hz).map_err(|source| {
                        NetConnectionError::UnusableProperty {
                            kind,
                            property: "repeaterOffsetHz",
                            source,
                        }
                    })?),
                };
                let tone_mode = match raw.tone_mode.as_deref().map(str::trim) {
                    None | Some("") => None,
                    Some(v) => Some(ToneMode::try_from(v).map_err(|()| {
                        NetConnectionError::UnknownValue {
                            kind,
                            property: "toneMode",
                        }
                    })?),
                };
                Ok(NetConnectionKind::Repeater {
                    planned_frequency_hz,
                    band,
                    mode,
                    offset_hz,
                    tone_mode,
                    tone_value: bounded_text(raw.tone_value.as_deref(), kind, "toneValue")?,
                })
            }
        }
        "echolink" => Ok(NetConnectionKind::EchoLink {
            node: required_text(raw.node.as_deref(), "echolink", "node")?,
        }),
        "allstar" => Ok(NetConnectionKind::AllStar {
            node: required_text(raw.node.as_deref(), "allstar", "node")?,
        }),
        "dmr" => Ok(NetConnectionKind::Dmr {
            talkgroup: required_text(raw.talkgroup.as_deref(), "dmr", "talkgroup")?,
            // `bounded_text`, never `required_text`: a DMR net whose network
            // nobody recorded must stay saveable by an ordinary title edit.
            network: bounded_text(raw.network.as_deref(), "dmr", "network")?,
        }),
        "dstar" => Ok(NetConnectionKind::DStar {
            reflector: required_text(raw.reflector.as_deref(), "dstar", "reflector")?,
        }),
        "ysf" => Ok(NetConnectionKind::Ysf {
            reflector: required_text(raw.reflector.as_deref(), "ysf", "reflector")?,
        }),
        "urf" => Ok(NetConnectionKind::Urf {
            reflector: required_text(raw.reflector.as_deref(), "urf", "reflector")?,
        }),
        "other" => parse_other(raw, id, existing),
        _ => Err(NetConnectionError::UnknownKind(token)),
    }
}

/// Reads an `other` entry, admitting a reserved label ONLY as the verbatim
/// echo of the one the connection under this id already holds.
///
/// The read path preserves a machine-minted label, so a client that GETs a
/// definition and PUTs its list back to reorder it hands that label straight
/// back. Refusing it there makes every such definition unwritable; accepting
/// it from any id would let an owner mint residue and corrupt the aggregate
/// the aggregate exists to keep answerable.
fn parse_other(
    raw: RawNetConnection,
    id: Uuid,
    existing: Option<&NetConnectionSet>,
) -> Result<NetConnectionKind, NetConnectionError> {
    let submitted = collapse_whitespace(&bounded_text_value(
        raw.label.as_deref().unwrap_or_default(),
        "other",
        "label",
    )?);
    if !is_reserved_label(&submitted) {
        return NetConnectionKind::other(&submitted, raw.detail);
    }
    match existing.and_then(|set| set.find(id)).map(|c| &c.kind) {
        Some(NetConnectionKind::Other { label, .. }) if label == &submitted => {
            Ok(NetConnectionKind::Other {
                label: submitted,
                detail: bounded_prose(raw.detail.as_deref(), "other", "detail")?,
            })
        }
        _ => Err(NetConnectionError::LabelReserved),
    }
}

/// Reads a required enum token, keeping "absent" and "not a value of this
/// enumeration" apart.
fn enum_token<T>(
    value: Option<&str>,
    kind: &'static str,
    property: &'static str,
    parse: impl Fn(&str) -> Result<T, ()>,
) -> Result<T, NetConnectionError> {
    match value.map(str::trim) {
        None | Some("") => Err(NetConnectionError::MissingProperty { kind, property }),
        Some(v) => parse(v).map_err(|()| NetConnectionError::UnknownValue { kind, property }),
    }
}

/// A required text property, through the same guard the optional ones use.
fn required_text(
    value: Option<&str>,
    kind: &'static str,
    property: &'static str,
) -> Result<String, NetConnectionError> {
    bounded_text(value, kind, property)?
        .ok_or(NetConnectionError::MissingProperty { kind, property })
}

/// How a station reached the net for one check-in.
///
/// A reference is a UUID **or** free text — the NCS can name a way the owner
/// never listed, and that must not silently become a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// The UUID of a connection in the session's by-value snapshot.
    Connection(Uuid),
    /// A connection the owner never listed, typed by the NCS mid-net.
    /// Creates no connection and is never written back to the definition.
    Unlisted(String),
}

/// How long a free-text `via` may be, in characters after trimming.
///
/// Deliberately SHORT and deliberately not [`crate::check_in::MAX_NOTE_CHARS`].
/// This value is rendered as a one-line chip beside a callsign on the
/// unauthenticated public roster, so it is a NAME for a way in — "Bob's
/// hotspot", "phone patch" — and not a place to put prose. This bound once
/// reused the note guard, which allowed 2000 multi-line characters on that
/// page; a review of that reuse caught the mismatch.
pub const MAX_VIA_CHARS: usize = 64;

// A way in is a NAME, not the prose a note holds. Asserted at COMPILE time
// rather than in a test: it is a relationship between two constants, so a test
// asserting it is a constant assertion (which clippy rejects, correctly) and
// this is the shape that actually stops someone widening the bound to the note's.
const _: () = assert!(MAX_VIA_CHARS < crate::check_in::MAX_NOTE_CHARS);

/// Parses the operator's own words for a way the owner never listed.
///
/// SINGLE-LINE (the [`crate::profile::parse_bounded_text`] family, alongside
/// name/location/report), never the multi-line prose guard: a `via` is a label,
/// and a newline in it breaks the one-line chip every surface renders it as.
///
/// **Blank-after-trim is an ERROR, not `None`.** Every other optional text field
/// in the tree folds a blank to "absent", and that is right for them — but a
/// caller reaching this function has already written `{"kind":"unlisted"}` on
/// the wire, and folding that to "nobody recorded a way in" is exactly the
/// `Unlisted`→`NotRecorded` collapse [`crate::net::wire::ViaDisplay`]'s own doc
/// forbids. The way to say "not recorded" is to omit `via`, or to send an
/// explicit `null` on the edit path.
pub fn parse_via_text(input: &str) -> Result<String, ViaError> {
    let trimmed = parse_bounded_text(input, MAX_VIA_CHARS)?;
    if trimmed.is_empty() {
        return Err(ViaError::Blank);
    }
    Ok(trimmed)
}

/// Why a free-text `via` was rejected.
///
/// Its own type rather than a bare [`ProfileError`] so the API layer cannot
/// answer a bad `via` with a problem naming some other field: an error names
/// the FAULTING field. This arm once reused the note guard, so a rejected
/// `via` answered `/errors/note-invalid`
/// saying "note" on a request carrying no note at all.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ViaError {
    /// The operator wrote a way in with no words in it.
    #[error("say which way in, or leave it unrecorded")]
    Blank,
    /// Too long, or carrying a control character or a newline.
    #[error(transparent)]
    Text(#[from] ProfileError),
}

/// The ADIF fields one connection contributes to a QSO record.
///
/// Every field is optional and an absent field means **omit the tag**: no QSO
/// field is mandatory in ADIF, and omitting `BAND`/`MODE` rather than guessing
/// one is what this codebase already does for the unmappable band and mode
/// tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdifConnectionFields {
    /// ADIF `BAND` enumeration value, or `None` to omit the tag.
    pub band: Option<&'static str>,
    /// Exact Hz for `FREQ`, or `None` to omit the tag.
    pub freq_hz: Option<i64>,
    /// ADIF `MODE` enumeration value, or `None` to omit the tag.
    pub mode: Option<&'static str>,
    /// ADIF `PROP_MODE` enumeration value, or `None` to omit the tag.
    pub prop_mode: Option<&'static str>,
}

/// Maps one connection to the ADIF fields it contributes.
///
/// The RF kinds reuse [`band_to_adif`] and [`mode_to_adif`] — there is no
/// second table and no copy; `2200m` → `2190m` is a real ADIF quirk that must
/// stay encoded exactly once.
///
/// The internet-carried kinds emit no `BAND`, no `FREQ` and no `MODE` — a net
/// reached only over the internet has none — and instead take ADIF's own
/// propagation-mode value: `ECH` for EchoLink, which ADIF names specifically,
/// and `INTERNET` for the rest. (`INT` is **not** a value in ADIF's
/// `Propagation_Mode` enumeration in 3.1.4 — the version this codebase
/// declares — or in 3.1.7.)
///
/// `RPT` is deliberately not emitted for [`NetConnectionKind::Repeater`]:
/// whether a QSO travelled *through* the repeater is a per-QSO fact NetRoll
/// does not hold, and emitting it would change output that ships today.
pub fn adif_fields_for(connection: &NetConnection) -> AdifConnectionFields {
    match &connection.kind {
        NetConnectionKind::Hf {
            planned_frequency_hz,
            band,
            mode,
        }
        | NetConnectionKind::Repeater {
            planned_frequency_hz,
            band,
            mode,
            ..
        } => AdifConnectionFields {
            band: band_to_adif(band.as_str()),
            freq_hz: Some(*planned_frequency_hz),
            mode: mode_to_adif(mode.as_str()),
            prop_mode: None,
        },
        NetConnectionKind::EchoLink { .. } => AdifConnectionFields {
            prop_mode: Some("ECH"),
            ..AdifConnectionFields::default()
        },
        NetConnectionKind::AllStar { .. }
        | NetConnectionKind::Dmr { .. }
        | NetConnectionKind::DStar { .. }
        | NetConnectionKind::Ysf { .. }
        | NetConnectionKind::Urf { .. } => AdifConnectionFields {
            prop_mode: Some("INTERNET"),
            ..AdifConnectionFields::default()
        },
        // A way to reach the net that ADIF has no vocabulary for. Guessing one
        // would put an unverifiable claim in a file that reaches LoTW and QRZ.
        NetConnectionKind::Other { .. } => AdifConnectionFields::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One value of every payload variant paired with the token and the
    /// spelling it MUST map to. The pairing is the oracle: a test that derived
    /// its expectation from `token()` could not see a mis-mapped arm, because
    /// `as_str` derives from `token()` too. The spellings are the storage
    /// contract — `20260829100200_backfill_net_connections.sql` wrote them.
    fn one_of_every_kind() -> Vec<(NetConnectionKind, NetConnectionKindToken, &'static str)> {
        vec![
            (
                NetConnectionKind::Hf {
                    planned_frequency_hz: 14_230_000,
                    band: Band::TwentyMeters,
                    mode: Mode::Ssb,
                },
                NetConnectionKindToken::Hf,
                "hf",
            ),
            (
                NetConnectionKind::Repeater {
                    planned_frequency_hz: 145_230_000,
                    band: Band::TwoMeters,
                    mode: Mode::Fm,
                    offset_hz: None,
                    tone_mode: None,
                    tone_value: None,
                },
                NetConnectionKindToken::Repeater,
                "repeater",
            ),
            (
                NetConnectionKind::EchoLink { node: "1".into() },
                NetConnectionKindToken::EchoLink,
                "echolink",
            ),
            (
                NetConnectionKind::AllStar {
                    node: "40000".into(),
                },
                NetConnectionKindToken::AllStar,
                "allstar",
            ),
            (
                NetConnectionKind::Dmr {
                    talkgroup: "3100".into(),
                    network: None,
                },
                NetConnectionKindToken::Dmr,
                "dmr",
            ),
            (
                NetConnectionKind::DStar {
                    reflector: "REF030 C".into(),
                },
                NetConnectionKindToken::DStar,
                "dstar",
            ),
            (
                NetConnectionKind::Ysf {
                    reflector: "America Link".into(),
                },
                NetConnectionKindToken::Ysf,
                "ysf",
            ),
            (
                NetConnectionKind::Urf {
                    reflector: "URF307 B".into(),
                },
                NetConnectionKindToken::Urf,
                "urf",
            ),
            (
                NetConnectionKind::Other {
                    label: "Zello".into(),
                    detail: None,
                },
                NetConnectionKindToken::Other,
                "other",
            ),
        ]
    }

    #[test]
    fn every_kind_round_trips_through_its_token() {
        // Each payload variant maps to ITS token, spells
        // itself the way storage does, and parses back from that spelling —
        // the property a discovery filter relies on when it binds a token to
        // `c.kind = $n`. Mutation-proved: swapping one `token()` arm reds here,
        // which the first draft of this test (expectation derived from
        // `token()`) could not see.
        let kinds = one_of_every_kind();
        assert_eq!(kinds.len(), 9, "one value per payload variant");
        for (kind, token, spelling) in kinds {
            assert_eq!(kind.token(), token, "{spelling}");
            assert_eq!(kind.as_str(), spelling);
            assert_eq!(token.as_str(), spelling);
            assert_eq!(NetConnectionKindToken::try_from(spelling), Ok(token));
        }
    }

    #[test]
    fn an_unknown_token_is_not_a_kind() {
        // Case-sensitive kebab, exactly as `Band`/`Mode`: a near miss is
        // a refusal, never a fold onto the closest real kind.
        for wrong in ["echo-link", "EchoLink", "", "  ", "zzz"] {
            assert_eq!(
                NetConnectionKindToken::try_from(wrong),
                Err(()),
                "{wrong:?} is not a connection kind"
            );
        }
    }

    fn at(position: i32, kind: NetConnectionKind) -> NetConnection {
        NetConnection {
            id: Uuid::now_v7(),
            position,
            kind,
        }
    }

    fn hf(band: Band) -> NetConnectionKind {
        NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band,
            mode: Mode::Ssb,
        }
    }

    // --- One fallible constructor, immutable mutations -----------------

    #[test]
    fn a_net_with_no_way_to_reach_it_cannot_be_constructed() {
        assert_eq!(
            NetConnectionSet::new(vec![]),
            Err(NetConnectionError::Empty)
        );
    }

    #[test]
    fn the_constructor_renumbers_dense_from_zero_whatever_positions_it_is_handed() {
        let set = NetConnectionSet::new(vec![
            at(9, hf(Band::TwentyMeters)),
            at(
                4,
                NetConnectionKind::EchoLink {
                    node: "12345".into(),
                },
            ),
        ])
        .expect("two connections are a valid set");

        let positions: Vec<i32> = set.connections().iter().map(|c| c.position).collect();
        assert_eq!(positions, vec![0, 1]);
        // Order follows the supplied positions, not the vec order.
        assert_eq!(set.connections()[0].kind.as_str(), "echolink");
    }

    // --- Delete renumbers, and the last one cannot go ------------------

    #[test]
    fn removing_the_only_connection_is_refused() {
        let only = at(0, hf(Band::TwentyMeters));
        let id = only.id;
        let set = NetConnectionSet::new(vec![only]).expect("one connection is a valid set");

        assert_eq!(set.without(id), Err(NetConnectionError::LastRemaining));
    }

    #[test]
    fn removing_a_middle_connection_leaves_the_survivors_dense_from_zero() {
        let first = at(0, hf(Band::TwentyMeters));
        let middle = at(
            1,
            NetConnectionKind::EchoLink {
                node: "12345".into(),
            },
        );
        let last = at(
            2,
            NetConnectionKind::AllStar {
                node: "40000".into(),
            },
        );
        let (first_id, middle_id, last_id) = (first.id, middle.id, last.id);
        let set = NetConnectionSet::new(vec![first, middle, last]).expect("three is a valid set");

        let after = set
            .without(middle_id)
            .expect("removing the middle is allowed");

        let survivors: Vec<(Uuid, i32)> = after
            .connections()
            .iter()
            .map(|c| (c.id, c.position))
            .collect();
        assert_eq!(survivors, vec![(first_id, 0), (last_id, 1)]);
    }

    #[test]
    fn removing_position_zero_promotes_the_survivor_to_position_zero() {
        let first = at(0, hf(Band::TwentyMeters));
        let second = at(1, hf(Band::FortyMeters));
        let (first_id, second_id) = (first.id, second.id);
        let set = NetConnectionSet::new(vec![first, second]).expect("two is a valid set");

        let after = set
            .without(first_id)
            .expect("removing position zero is allowed");

        let promoted = &after.connections()[0];
        assert_eq!(promoted.id, second_id);
        assert_eq!(promoted.position, 0);
    }

    #[test]
    fn a_reorder_renumbers_the_set_into_the_new_order() {
        let first = at(0, hf(Band::TwentyMeters));
        let second = at(1, hf(Band::FortyMeters));
        let (first_id, second_id) = (first.id, second.id);
        let set = NetConnectionSet::new(vec![first, second]).expect("two is a valid set");

        let after = set
            .reordered(&[second_id, first_id])
            .expect("a reorder naming every id once is allowed");

        let order: Vec<(Uuid, i32)> = after
            .connections()
            .iter()
            .map(|c| (c.id, c.position))
            .collect();
        assert_eq!(order, vec![(second_id, 0), (first_id, 1)]);
    }

    #[test]
    fn a_reorder_that_drops_a_connection_is_refused() {
        let first = at(0, hf(Band::TwentyMeters));
        let second = at(1, hf(Band::FortyMeters));
        let first_id = first.id;
        let set = NetConnectionSet::new(vec![first, second]).expect("two is a valid set");

        assert_eq!(
            set.reordered(&[first_id]),
            Err(NetConnectionError::ReorderMismatch)
        );
    }

    // --- `other`'s free text is stored in a countable shape ----------

    #[test]
    fn three_spellings_of_one_other_label_share_a_count_key() {
        let a = NetConnectionKind::other("Wires-X", None).expect("a plain label is allowed");
        let b = NetConnectionKind::other(" wires  x ", Some("via the club gateway".into()))
            .expect("a plain label is allowed");
        let c = NetConnectionKind::other("WIRES-X", Some("totally different prose".into()))
            .expect("a plain label is allowed");

        assert_eq!(a.count_key(), b.count_key());
        assert_eq!(b.count_key(), c.count_key());
        assert!(a.count_key().is_some());
    }

    #[test]
    fn a_named_kind_has_no_count_key() {
        assert_eq!(hf(Band::TwentyMeters).count_key(), None);
        assert_eq!(
            NetConnectionKind::EchoLink {
                node: "12345".into()
            }
            .count_key(),
            None
        );
    }

    #[test]
    fn a_blank_other_label_is_refused() {
        assert_eq!(
            NetConnectionKind::other("   ", None),
            Err(NetConnectionError::LabelEmpty)
        );
    }

    // --- The machine namespace is not reachable by an owner ------------

    #[test]
    fn an_owner_cannot_choose_a_label_in_the_machine_namespace() {
        assert_eq!(
            NetConnectionKind::other("unclassified-reflector", None),
            Err(NetConnectionError::LabelReserved)
        );
    }

    #[test]
    fn an_unclassifiable_value_keeps_its_original_verbatim_in_detail() {
        let degraded = NetConnectionKind::unclassified("reflector", "REF030 C".into());

        match degraded {
            NetConnectionKind::Other { label, detail } => {
                assert_eq!(label, "unclassified-reflector");
                assert_eq!(detail.as_deref(), Some("REF030 C"));
            }
            other => panic!("expected an `other` connection, got {}", other.as_str()),
        }
    }

    // --- The payload is per-variant, not an `Option` on a flat struct --

    #[test]
    fn no_construction_path_yields_an_echolink_carrying_a_frequency() {
        let kind = NetConnectionKind::EchoLink {
            node: "12345".into(),
        };

        // EXHAUSTIVE destructure, no `..` rest pattern: this is the assertion.
        // The moment `EchoLink` grows a frequency (or any other RF property)
        // this test stops COMPILING, which is the only way to pin "EchoLink has
        // no frequency" as unrepresentable rather than merely unset. A runtime
        // assertion here would prove the opposite — that the shape exists and
        // happens to be `None`.
        let NetConnectionKind::EchoLink { node } = &kind else {
            panic!("constructed an EchoLink");
        };
        assert_eq!(node, "12345");

        let fields = adif_fields_for(&at(0, kind));
        assert_eq!(fields.freq_hz, None);
        assert_eq!(fields.band, None);
    }

    // --- The ADIF mapping ---------------------------------------

    #[test]
    fn every_internet_carried_kind_emits_no_band_no_freq_no_mode() {
        let internet = [
            NetConnectionKind::EchoLink {
                node: "12345".into(),
            },
            NetConnectionKind::AllStar {
                node: "40000".into(),
            },
            NetConnectionKind::Dmr {
                talkgroup: "3100".into(),
                network: None,
            },
            NetConnectionKind::DStar {
                reflector: "REF030 C".into(),
            },
            NetConnectionKind::Ysf {
                reflector: "America Link".into(),
            },
            NetConnectionKind::Urf {
                reflector: "URF307 B".into(),
            },
        ];

        for kind in internet {
            let token = kind.as_str();
            let fields = adif_fields_for(&at(0, kind));
            assert_eq!(fields.band, None, "{token} must emit no BAND");
            assert_eq!(fields.freq_hz, None, "{token} must emit no FREQ");
            assert_eq!(fields.mode, None, "{token} must emit no MODE");
        }
    }

    #[test]
    fn echolink_takes_adifs_own_echolink_propagation_mode_and_the_rest_take_internet() {
        let echolink = adif_fields_for(&at(0, NetConnectionKind::EchoLink { node: "1".into() }));
        assert_eq!(echolink.prop_mode, Some("ECH"));

        for kind in [
            NetConnectionKind::AllStar {
                node: "40000".into(),
            },
            NetConnectionKind::Dmr {
                talkgroup: "3100".into(),
                network: None,
            },
            NetConnectionKind::DStar {
                reflector: "REF030 C".into(),
            },
            NetConnectionKind::Ysf {
                reflector: "America Link".into(),
            },
            NetConnectionKind::Urf {
                reflector: "URF307 B".into(),
            },
        ] {
            let token = kind.as_str();
            let fields = adif_fields_for(&at(0, kind));
            assert_eq!(fields.prop_mode, Some("INTERNET"), "{token}");
        }
    }

    #[test]
    fn the_rf_kinds_go_through_the_existing_band_and_mode_tables() {
        let fields = adif_fields_for(&at(
            0,
            NetConnectionKind::Hf {
                planned_frequency_hz: 137_500,
                band: Band::TwentyTwoHundredMeters,
                mode: Mode::Cw,
            },
        ));

        // `2200m` → `2190m` is an ADIF quirk encoded exactly once, in
        // `band_to_adif`. Reaching the same answer proves reuse, not a copy.
        assert_eq!(fields.band, band_to_adif("2200m"));
        assert_eq!(fields.band, Some("2190m"));
        assert_eq!(fields.mode, mode_to_adif("cw"));
        assert_eq!(fields.freq_hz, Some(137_500));
        assert_eq!(fields.prop_mode, None, "an RF net has no propagation tag");
    }

    #[test]
    fn a_repeater_emits_its_rf_fields_and_no_propagation_tag() {
        let fields = adif_fields_for(&at(
            0,
            NetConnectionKind::Repeater {
                planned_frequency_hz: 146_940_000,
                band: Band::TwoMeters,
                mode: Mode::Fm,
                offset_hz: Some(-600_000),
                tone_mode: Some(ToneMode::Ctcss),
                tone_value: Some("100.0".into()),
            },
        ));

        assert_eq!(fields.band, Some("2m"));
        assert_eq!(fields.mode, Some("FM"));
        assert_eq!(fields.freq_hz, Some(146_940_000));
        assert_eq!(fields.prop_mode, None);
    }

    #[test]
    fn an_other_connection_emits_nothing_at_all() {
        let kind = NetConnectionKind::other("Wires-X", None).expect("a plain label is allowed");
        assert_eq!(
            adif_fields_for(&at(0, kind)),
            AdifConnectionFields::default()
        );
    }

    // --- the free-text `via` guard -------------------------------------------

    #[test]
    fn a_free_text_via_is_bounded_far_below_a_note() {
        let at_bound = "x".repeat(MAX_VIA_CHARS);
        assert_eq!(parse_via_text(&at_bound), Ok(at_bound.clone()));
        assert_eq!(
            parse_via_text(&"x".repeat(MAX_VIA_CHARS + 1)),
            Err(ViaError::Text(ProfileError::TooLong {
                max_chars: MAX_VIA_CHARS
            }))
        );
    }

    #[test]
    fn a_free_text_via_is_one_line() {
        // The note guard COLLAPSES newlines and keeps the text; this refuses it,
        // because a `via` renders as a one-line chip beside a callsign on the
        // unauthenticated public roster.
        assert_eq!(
            parse_via_text("club\nhotspot"),
            Err(ViaError::Text(ProfileError::IllegalCharacter('\n')))
        );
    }

    #[test]
    fn a_blank_free_text_via_is_refused_rather_than_folded_to_not_recorded() {
        // The collapse `ViaDisplay`'s own doc forbids: a caller that wrote
        // `{"kind":"unlisted"}` said a way in EXISTS, and answering 200 with
        // "nobody recorded one" silently rewrites what they said.
        assert_eq!(parse_via_text("   "), Err(ViaError::Blank));
        assert_eq!(parse_via_text(""), Err(ViaError::Blank));
    }

    #[test]
    fn a_free_text_via_keeps_the_operators_own_words_trimmed() {
        assert_eq!(
            parse_via_text("  Bob's hotspot  "),
            Ok("Bob's hotspot".to_owned())
        );
    }

    // --- The write path enforces the bounded-text guards --------------------

    fn raw(kind: &str) -> RawNetConnection {
        RawNetConnection {
            kind: Some(kind.to_owned()),
            ..RawNetConnection::default()
        }
    }

    #[test]
    fn a_bidi_control_character_in_a_connection_node_is_refused() {
        let mut entry = raw("echolink");
        entry.node = Some("12345\u{202E}".to_owned());

        let result = parse_connection_set(vec![(Uuid::now_v7(), entry)], None);

        assert!(
            result.is_err(),
            "a bidi override reorders every rendered string it lands in, and a node is \
             rendered on every surface that names the way in"
        );
    }

    #[test]
    fn a_connection_text_property_past_the_single_line_bound_is_refused() {
        let mut entry = raw("allstar");
        entry.node = Some("4".repeat(65));

        assert!(
            parse_connection_set(vec![(Uuid::now_v7(), entry)], None).is_err(),
            "a single-line connection property is bounded at 64 characters — the bound the \
             definition's own single-line text fields carried before they retired"
        );
    }

    #[test]
    fn an_other_connections_label_and_detail_are_bounded_too() {
        let mut long_label = raw("other");
        long_label.label = Some("x".repeat(65));
        assert!(parse_connection_set(vec![(Uuid::now_v7(), long_label)], None).is_err());

        let mut long_detail = raw("other");
        long_detail.label = Some("Wires-X".to_owned());
        long_detail.detail = Some("x".repeat(2001));
        assert!(
            parse_connection_set(vec![(Uuid::now_v7(), long_detail)], None).is_err(),
            "`detail` is serialized on the PUBLIC body, so it cannot be unbounded"
        );
    }

    #[test]
    fn an_other_connections_detail_may_still_hold_the_paragraphs_its_author_wrote() {
        let mut entry = raw("other");
        entry.label = Some("Wires-X".to_owned());
        entry.detail = Some("First line.\n\nSecond line.".to_owned());

        let set =
            parse_connection_set(vec![(Uuid::now_v7(), entry)], None).expect("prose is allowed");

        assert_eq!(
            set.connections()[0].kind,
            NetConnectionKind::Other {
                label: "Wires-X".to_owned(),
                detail: Some("First line.\n\nSecond line.".to_owned())
            }
        );
    }

    // --- The reserved namespace is one predicate, not two -------------------

    #[test]
    fn the_reserved_namespace_cannot_be_side_stepped_by_case_or_separator() {
        for spelling in [
            "Unclassified-Reflector",
            "unclassified reflector",
            "unclassified\u{2011}reflector",
            "UNCLASSIFIED--REFLECTOR",
        ] {
            assert_eq!(
                NetConnectionKind::other(spelling, None),
                Err(NetConnectionError::LabelReserved),
                "{spelling} counts as the reserved label, so it must not be typable"
            );
        }
    }
    // --- A list has a ceiling ------------------------------------------------

    #[test]
    fn a_connection_list_past_the_ceiling_is_refused() {
        let entries: Vec<(Uuid, RawNetConnection)> = (0..MAX_CONNECTIONS + 1)
            .map(|i| {
                let mut entry = raw("echolink");
                entry.node = Some(i.to_string());
                (Uuid::now_v7(), entry)
            })
            .collect();

        assert_eq!(
            parse_connection_set(entries, None),
            Err(NetConnectionError::TooMany {
                max: MAX_CONNECTIONS
            }),
            "an unbounded list is an unbounded INSERT loop inside the transaction that holds \
             the definition row lock"
        );
    }

    // --- An error names the fault and the entry ------------------------------

    #[test]
    fn an_out_of_range_frequency_is_reported_as_unusable_not_as_absent() {
        let mut entry = raw("hf");
        // ≈1 THz — the older fixture was the MHz string "999999.0".
        entry.planned_frequency_hz = Some(999_999_000_000);
        entry.band = Some("20m".to_owned());
        entry.mode = Some("ssb".to_owned());

        let error =
            parse_connection_set(vec![(Uuid::now_v7(), entry)], None).expect_err("out of range");

        assert_eq!(
            error,
            NetConnectionError::Entry {
                index: 0,
                source: Box::new(NetConnectionError::UnusableProperty {
                    kind: "hf",
                    property: "plannedFrequencyHz",
                    source: FrequencyError::OutOfRange,
                }),
            },
            "the client supplied the property; telling them it is missing sends them looking \
             for a field they already filled in"
        );
    }

    #[test]
    fn a_negative_frequency_is_reported_as_negative_not_merely_out_of_range() {
        // With the frequency arriving as an integer, `Negative`
        // and `OutOfRange` are the only two faults this route can raise, and a
        // negative integer must get the one that names its actual problem.
        let mut entry = raw("hf");
        entry.planned_frequency_hz = Some(-14_230_000);
        entry.band = Some("20m".to_owned());
        entry.mode = Some("ssb".to_owned());

        let error =
            parse_connection_set(vec![(Uuid::now_v7(), entry)], None).expect_err("negative");

        assert_eq!(
            error,
            NetConnectionError::Entry {
                index: 0,
                source: Box::new(NetConnectionError::UnusableProperty {
                    kind: "hf",
                    property: "plannedFrequencyHz",
                    source: FrequencyError::Negative,
                }),
            }
        );
    }

    #[test]
    fn an_unknown_band_token_is_reported_as_unknown_not_as_absent() {
        let mut entry = raw("hf");
        entry.planned_frequency_hz = Some(14_230_000);
        entry.band = Some("19m".to_owned());
        entry.mode = Some("ssb".to_owned());

        let error =
            parse_connection_set(vec![(Uuid::now_v7(), entry)], None).expect_err("unknown band");

        assert_eq!(
            error,
            NetConnectionError::Entry {
                index: 0,
                source: Box::new(NetConnectionError::UnknownValue {
                    kind: "hf",
                    property: "band",
                }),
            }
        );
    }

    #[test]
    fn a_failure_names_which_entry_in_the_list_failed() {
        let mut good = raw("echolink");
        good.node = Some("12345".to_owned());

        let error = parse_connection_set(
            vec![(Uuid::now_v7(), good), (Uuid::now_v7(), raw("allstar"))],
            None,
        )
        .expect_err("the second entry has no node");

        assert!(
            matches!(error, NetConnectionError::Entry { index: 1, .. }),
            "a client editing a list of ten needs to know which one to fix"
        );
    }

    #[test]
    fn two_entries_naming_the_same_connection_are_refused_rather_than_colliding_in_storage() {
        let id = Uuid::now_v7();
        let mut first = raw("echolink");
        first.node = Some("12345".to_owned());
        let mut second = raw("allstar");
        second.node = Some("40000".to_owned());

        assert_eq!(
            parse_connection_set(vec![(id, first), (id, second)], None),
            Err(NetConnectionError::DuplicateId(id)),
            "two rows with one primary key is a 23505 at the adapter, which surfaces as a 500"
        );
    }
}
