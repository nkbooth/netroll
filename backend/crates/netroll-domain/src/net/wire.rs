// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The ONE round-trippable connection shape. It lives here, in the crate both
//! the HTTP body and the stored session snapshot depend on, so neither read
//! path can grow a second parallel shape, and it is `Deserialize` as well as
//! `Serialize` because a stored snapshot has to be read back. Flat with
//! `null`s, `camelCase`, enum-ish fields as their stable string tokens.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::connection::{
    AdifConnectionFields, NetConnection, NetConnectionKind, NetConnectionSet, Via, adif_fields_for,
    is_reserved_label,
};
use super::enums::{Band, Mode, ToneMode};
use super::validation::format_frequency_mhz;

/// One connection on the wire and in the session snapshot.
///
/// The per-variant guarantee ("EchoLink has no frequency") is the Rust enum's;
/// this is its flattened projection, and a reader keys on `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetConnectionWire {
    /// Stable id — what a check-in's `Via` refers to.
    pub id: Uuid,
    /// Zero-based place in the owner's order.
    pub position: i32,
    /// The stable lowercase-kebab kind token.
    pub kind: String,
    /// Exact Hz, on the two RF kinds only.
    ///
    /// On a SESSION body's top-level `connections` array — the one
    /// `live_connections` builds — this is the frequency in force: the current
    /// one while the net is live, the final one at close, not the one the
    /// definition planned. `live_connections` overlays every
    /// `frequency.changed` onto this key. As of 2026-09-03, every session
    /// surface's top-level `connections` array — the close webhook included —
    /// serves the frequency in force, never a planned one.
    ///
    /// The planned value is not lost; it simply is not what THIS array means.
    /// It is still readable from the SAME session body, one level down:
    /// `definition.connections` (the frozen `DefinitionSnapshot`)
    /// carries this identical key untouched by any move, and the moves
    /// themselves are in the event log. A reader wanting the planned
    /// value reads `definition.connections[n].plannedFrequencyHz` instead of
    /// this one.
    ///
    /// The name keeps saying "planned" because there is ONE shape serving
    /// the definition read (where the value IS
    /// planned) and both connections arrays on a session body — the top-level
    /// one, where it is not, and the nested `definition.connections`, where it
    /// still is. A rename or an `operatingFrequencyHz` sibling re-creates the
    /// second, parallel serializer both criteria exist to forbid.
    pub planned_frequency_hz: Option<i64>,
    /// Band token, on the two RF kinds only.
    pub band: Option<String>,
    /// Mode token, on the two RF kinds only.
    pub mode: Option<String>,
    /// Signed repeater shift in Hz.
    pub repeater_offset_hz: Option<i64>,
    /// Tone mode token.
    pub tone_mode: Option<String>,
    /// Tone value.
    pub tone_value: Option<String>,
    /// EchoLink OR AllStar node — which one is decided by `kind` alone.
    pub node: Option<String>,
    /// D-Star, YSF or URF reflector — again disambiguated only by `kind`.
    pub reflector: Option<String>,
    /// The DMR network a talkgroup lives on. `null` on every other kind, and
    /// on a DMR connection whose network nobody recorded.
    pub network: Option<String>,
    /// DMR talkgroup id, as text.
    pub talkgroup: Option<String>,
    /// An `other` connection's short normalized name.
    pub label: Option<String>,
    /// An `other` connection's prose.
    pub detail: Option<String>,
}

/// Why a stored/received wire connection could not be read back.
///
/// Deliberately NOT a "degrade to `other`" path, unlike the storage row
/// decoder: a wire connection was written by this same shape, so a value it
/// cannot express means the record predates the shape or was hand-edited —
/// and the answer to such a record is refusal, not translation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("connection {position} is not a way to reach a net this version can read")]
pub struct NetConnectionWireError {
    /// The offending entry's place in the list.
    pub position: i32,
}

/// Whether a connection of this KIND carries an operating frequency.
///
/// The two RF kinds do and nothing else does — an EchoLink node, a DMR
/// talkgroup or a reflector is reached by name, not by tuning. Lives here, on
/// the wire shape, because `kind` is a stable string token by the time a stored
/// record or a request body is being judged, and because the answer has to be
/// the same for the renderer that hides the field and the guard that refuses
/// the write. An unknown token answers `false`: a kind this version cannot name
/// is not one it can retune.
pub fn kind_carries_frequency(kind: &str) -> bool {
    matches!(kind, "hf" | "repeater")
}

/// A check-in's `via` on the wire and in a stored event payload.
///
/// A `kind`-discriminated object, not two sibling nullable keys: `{"kind":
/// "connection","connectionId":"…"}` or `{"kind":"unlisted","text":"…"}`. The
/// flat alternative (`viaConnectionId` + `viaText`) can represent "both set" and
/// "neither set", and every reader would then have to re-decide which wins —
/// so the invalid states are made unwritable here instead.
///
/// Matches [`NetConnectionWire`]'s own `kind`-discriminant posture and the
/// snapshot storage posture `pg/net_sessions.rs` declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ViaWire {
    /// A connection in the session's frozen snapshot, named by its stable id.
    Connection {
        /// The snapshot connection's id.
        #[serde(rename = "connectionId")]
        connection_id: Uuid,
    },
    /// A way in the owner never listed, typed by the NCS mid-net.
    Unlisted {
        /// The operator's own words, carried verbatim.
        text: String,
    },
}

/// Projects a domain [`Via`] onto the wire shape.
pub fn via_of(via: &Via) -> ViaWire {
    match via {
        Via::Connection(id) => ViaWire::Connection { connection_id: *id },
        Via::Unlisted(text) => ViaWire::Unlisted { text: text.clone() },
    }
}

/// Reads a wire `via` back into the domain.
///
/// Infallible: both variants are total over their payloads, and a free-text
/// `via` is deliberately un-validated against the connection list — that is what
/// "unlisted" means.
pub fn via_from_wire(wire: &ViaWire) -> Via {
    match wire {
        ViaWire::Connection { connection_id } => Via::Connection(*connection_id),
        ViaWire::Unlisted { text } => Via::Unlisted(text.clone()),
    }
}

/// The fixed phrase a `via` naming a connection the session's snapshot does not
/// hold renders as.
///
/// It NAMES the fault rather than reciting the mechanism, and it is
/// deliberately none of: blank, the UUID's string form, or the ADIF-export
/// connection's label. A session replays from its own frozen snapshot, so this
/// is a reachable state — a row written by a newer deploy during a rollback, or
/// a hand-edited payload — not a defensive branch.
pub const UNRESOLVABLE_VIA_LABEL: &str = "A way in this net no longer lists";

/// How one check-in's `via` reads on a surface, resolved against the session's
/// connection set.
///
/// FOUR variants with NO default and no `unwrap_or` shape, on purpose. The
/// cheapest wrong implementation of the unresolvable case is
/// `via_label(..).unwrap_or_else(|| export_label())`: one line, it compiles, and
/// it puts a lie in a file that reaches LoTW. An `Option` invites that; this
/// refuses it, because every consumer has to write the arm out where a reviewer
/// can see it.
///
/// [`ViaDisplay::Unresolvable`] and [`ViaDisplay::NotRecorded`] are DIFFERENT
/// FACTS and must never collapse into each other — "the operator said it came in
/// on something this net does not list" is not "nobody recorded it".
/// [`adif_fields_for_via_wire`] already encodes the same distinction on the
/// ADIF side; this keeps the display side in step with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViaDisplay<'a> {
    /// Nobody recorded how this station got in.
    NotRecorded,
    /// The connection the check-in arrived on.
    Resolved(&'a NetConnectionWire),
    /// A connection id the session's snapshot does not hold.
    Unresolvable,
    /// A way the owner never listed, in the operator's own words.
    Unlisted(&'a str),
}

/// Resolves a check-in's `via` against the session's connection set.
///
/// Resolution is BY ID against the set handed in — never by position,
/// `.first()` or `[0]`. The label is a projection; the id is the key.
///
/// WHICH set the caller hands in decides what frequency a resolved label
/// carries, because [`connection_label`] embeds it for the RF kinds. A live
/// surface hands in `live_connections` (where the net is NOW); every historical
/// surface — the exports, the webhook roster, the check-in history — hands in
/// the snapshot as it stood at THAT check-in's own `seq`
/// (`SessionState::connections_at`), so a station
/// worked before a mid-session move keeps the frequency it was worked on.
pub fn resolve_via<'a>(
    via: Option<&'a Via>,
    connections: &'a [NetConnectionWire],
) -> ViaDisplay<'a> {
    match via {
        None => ViaDisplay::NotRecorded,
        Some(Via::Unlisted(text)) => ViaDisplay::Unlisted(text.as_str()),
        Some(Via::Connection(id)) => match connections.iter().find(|c| c.id == *id) {
            Some(connection) => ViaDisplay::Resolved(connection),
            None => ViaDisplay::Unresolvable,
        },
    }
}

/// The ONE backend answer to "what does this `via` say to a person" — the CSV
/// cell, the webhook's resolved label, a correction's
/// `from`/`to`, and the check-in-history row all read it.
///
/// `None` means [`ViaDisplay::NotRecorded`] and NOTHING ELSE: an unresolvable
/// `via` and a free-text one both answer `Some`, so the two facts cannot collapse
/// into "absent". It is deliberately NOT a fallback hook — the single permitted
/// export fallback lives in [`adif_fields_for_via_wire`]'s `NotRecorded` arm,
/// which takes a `Via` and never a label, so there is no label path that can
/// reach it.
pub fn via_label(display: &ViaDisplay<'_>) -> Option<String> {
    match display {
        ViaDisplay::NotRecorded => None,
        ViaDisplay::Resolved(connection) => Some(connection_label(connection)),
        ViaDisplay::Unresolvable => Some(UNRESOLVABLE_VIA_LABEL.to_owned()),
        ViaDisplay::Unlisted(text) => Some((*text).to_owned()),
    }
}

/// What each connection kind is CALLED wherever one is named to a person.
///
/// The stable wire token is what a machine keys on; no net owner knows what
/// `urf` is. The frontend's `KIND_LABELS` is the hand-written twin of this map
/// in the other language — the two are not shared, and a disagreement between
/// them is a defect in whichever was edited alone.
fn kind_label(kind: &str) -> &str {
    match kind {
        "hf" => "HF",
        "repeater" => "Repeater",
        "echolink" => "EchoLink",
        "allstar" => "AllStar",
        "dmr" => "DMR",
        "dstar" => "D-Star",
        "ysf" => "System Fusion",
        "urf" => "URF",
        "other" => "Other",
        // A kind this build has never heard of names itself rather than
        // pretending to be one it knows.
        unknown => unknown,
    }
}

/// What ONE connection is called on a human surface: the kind's name plus the
/// fact that identifies THAT connection.
///
/// The identifying fact is load-bearing and not decoration. A net may list two
/// repeaters, or HF plus a second HF; a label of `"Repeater"` alone would make
/// the roster say the same thing about two different ways in, which is a quieter
/// version of the failure `via` exists to fix. So the frequency identifies an RF
/// way, the node an EchoLink/AllStar one, the talkgroup-on-network a DMR one,
/// the reflector the three reflector kinds, and the authored prose an `other`.
///
/// This is the ONE home for that knowledge on the server:
/// `delivery::connection_lines` used to hold a second copy of it, which rendered
/// `dstar — REF030C` where the browser rendered `D-Star`.
///
/// For the two RF kinds the identifying fact IS the frequency, so this is
/// [`connection_name`] plus the MHz detail; for every other kind the two
/// functions agree byte for byte.
pub fn connection_label(connection: &NetConnectionWire) -> String {
    match connection.kind.as_str() {
        // INTEGER arithmetic. `hz as f64 / 1e6` formatted to three places
        // renders `145_512_500` as `145.513 MHz` — a different, wrong frequency,
        // and one that collides with every other way in within 500 Hz of it.
        "hf" | "repeater" => match connection.planned_frequency_hz {
            Some(hz) => format!(
                "{} — {} MHz",
                connection_name(connection),
                format_frequency_mhz(hz)
            ),
            None => connection_name(connection),
        },
        _ => connection_name(connection),
    }
}

/// The half of [`connection_label`] that carries NO frequency: the kind's name
/// plus the fact that identifies the connection, for
/// every kind whose identifying fact is not a frequency — the node, the
/// talkgroup-on-network, the reflector, an `other`'s authored label. For `hf`
/// and `repeater` it is the kind name alone.
///
/// It exists for a sentence that COUNTS stations under a way in. A count under
/// a frequency is a claim about each station counted, and after a mid-session
/// QSY that claim is false for the ones worked before the move — so the
/// summary email's `12 on HF` names the way in without a number it cannot
/// honestly attach to all twelve. The accepted, deliberate consequence: a net
/// listing two HF ways in reads as two `N on HF` lines, in the owner's order.
/// Everything that names ONE connection to a person still goes through
/// [`connection_label`].
pub fn connection_name(connection: &NetConnectionWire) -> String {
    // An `other` whose label is machine residue is named by its kind, not by a
    // NetRoll internal published as if it were a fact about the net.
    let authored_label = match (connection.kind.as_str(), connection.label.as_deref()) {
        ("other", Some(label)) if !is_reserved_label(label) => Some(label),
        _ => None,
    };
    let name = authored_label.unwrap_or_else(|| kind_label(&connection.kind));
    let detail = match connection.kind.as_str() {
        "hf" | "repeater" => None,
        "echolink" | "allstar" => connection.node.clone(),
        "dmr" => match (&connection.talkgroup, &connection.network) {
            (Some(tg), Some(network)) => Some(format!("TG {tg} on {network}")),
            (Some(tg), None) => Some(format!("TG {tg}")),
            _ => None,
        },
        "dstar" | "ysf" | "urf" => connection.reflector.clone(),
        _ => connection.detail.clone(),
    };
    match detail {
        Some(detail) => format!("{name} — {detail}"),
        None => name.to_owned(),
    }
}

/// The ADIF fields one WIRE connection contributes to a QSO record.
///
/// Total: a stored connection this version cannot read back contributes nothing
/// rather than raising, because an export must not fail on one unreadable row.
///
/// **`FREQ` survives an unreadable connection, and that is deliberate.**
/// [`connection_of`] refuses an `hf`/`repeater` whose band or mode token this
/// build cannot parse, so a whole RF way in can become unreadable over a single
/// unknown band string — and the export once emitted `<FREQ>`
/// unconditionally as the anchor a logbook uses when it cannot map the band.
/// Dropping it would silently regress that. The frequency is a stored integer
/// that needs no vocabulary to read, so it is carried through from the wire
/// while `BAND`/`MODE`/`PROP_MODE` are correctly withheld.
pub fn adif_fields_for_wire(wire: &NetConnectionWire) -> AdifConnectionFields {
    match connection_of(wire) {
        Ok(connection) => adif_fields_for(&connection),
        Err(_) => AdifConnectionFields {
            freq_hz: wire
                .planned_frequency_hz
                .filter(|_| kind_carries_frequency(&wire.kind)),
            ..AdifConnectionFields::default()
        },
    }
}

/// The ADIF fields a check-in's `via` contributes, resolved against the
/// connection set the caller hands in.
///
/// **The ONE production resolver for that question in the whole tree.** A second
/// one against `NetConnectionSet` once existed with no production caller at all:
/// it resolved against the stored snapshot rather than this wire set, so it
/// silently bypassed the
/// mid-session frequency overlay, and its two clauses had already drifted from
/// this one's. It was deleted rather than gated. A caller reaching for a
/// domain-set twin of this function is reintroducing that defect.
///
/// The export path hands in the frozen snapshot as it stood at THIS record's own
/// `seq` — every move up to that check-in overlaid, none after it
/// (`SessionState::connections_at`) — so `<FREQ>` is the
/// frequency this QSO was worked on. The four answers must not collapse:
///
/// - `NotRecorded` — **the ONE permitted fallback in the whole tree**, to
///   the connection an ADIF export describes this session's QSOs with. It is a
///   display/export decision and is never laundered back into the log.
/// - `Resolved` — that connection's own fields.
/// - `Unresolvable` and `Unlisted` — **every tag omitted**. Stamping the export
///   connection's band on either would be a lie in the one place it matters.
pub fn adif_fields_for_via_wire(
    via: Option<&Via>,
    connections: &[NetConnectionWire],
) -> AdifConnectionFields {
    match resolve_via(via, connections) {
        ViaDisplay::NotRecorded => adif_export_connection_wire(connections)
            .map(adif_fields_for_wire)
            .unwrap_or_default(),
        ViaDisplay::Resolved(connection) => adif_fields_for_wire(connection),
        ViaDisplay::Unresolvable | ViaDisplay::Unlisted(_) => AdifConnectionFields::default(),
    }
}

/// The connection an ADIF export describes a session's QSOs with when the
/// check-in itself recorded none: the FIRST way in that carries a frequency,
/// else the one at position zero.
///
/// **Not `.first()`, and not position zero unconditionally.** Position zero is
/// the way the net's owner leads with, and it need not be a radio way at all —
/// a net that leads with EchoLink and lists HF second really is on 20m, and
/// describing its QSOs with the EchoLink entry discards an RF connection that
/// genuinely exists while still answering 200 with a `.adi` most logbook
/// software rejects.
///
/// This is the ONLY resolver for that question. Callers must never reach for
/// `.first()`, `[0]` or a hand-rolled `position == 0` search: three call sites
/// grow three behaviours on the same broken row.
fn adif_export_connection_wire(connections: &[NetConnectionWire]) -> Option<&NetConnectionWire> {
    connections
        .iter()
        .find(|c| kind_carries_frequency(&c.kind) && c.planned_frequency_hz.is_some())
        .or_else(|| connections.iter().min_by_key(|c| c.position))
}

/// Projects a whole set in the owner's order.
pub fn wire_connections(connections: &NetConnectionSet) -> Vec<NetConnectionWire> {
    connections.connections().iter().map(wire_of).collect()
}

/// Reads a whole list back into the domain set, refusing anything this version
/// cannot represent.
pub fn connection_set_from_wire(
    wires: &[NetConnectionWire],
) -> Result<NetConnectionSet, NetConnectionWireError> {
    let connections = wires
        .iter()
        .map(connection_of)
        .collect::<Result<Vec<_>, _>>()?;
    // `NetConnectionSet::new` refuses an empty set and renumbers dense from
    // zero; an empty stored list therefore refuses here rather than becoming a
    // net with no way to reach it.
    NetConnectionSet::new(connections).map_err(|_| NetConnectionWireError { position: 0 })
}

/// Projects one connection.
pub fn wire_of(connection: &NetConnection) -> NetConnectionWire {
    let base = NetConnectionWire {
        id: connection.id,
        position: connection.position,
        kind: connection.kind.as_str().to_owned(),
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
    };
    match &connection.kind {
        NetConnectionKind::Hf {
            planned_frequency_hz,
            band,
            mode,
        } => NetConnectionWire {
            planned_frequency_hz: Some(*planned_frequency_hz),
            band: Some(band.as_str().to_owned()),
            mode: Some(mode.as_str().to_owned()),
            ..base
        },
        NetConnectionKind::Repeater {
            planned_frequency_hz,
            band,
            mode,
            offset_hz,
            tone_mode,
            tone_value,
        } => NetConnectionWire {
            planned_frequency_hz: Some(*planned_frequency_hz),
            band: Some(band.as_str().to_owned()),
            mode: Some(mode.as_str().to_owned()),
            repeater_offset_hz: *offset_hz,
            tone_mode: tone_mode.map(|t| t.as_str().to_owned()),
            tone_value: tone_value.clone(),
            ..base
        },
        NetConnectionKind::EchoLink { node } | NetConnectionKind::AllStar { node } => {
            NetConnectionWire {
                node: Some(node.clone()),
                ..base
            }
        }
        NetConnectionKind::Dmr { talkgroup, network } => NetConnectionWire {
            talkgroup: Some(talkgroup.clone()),
            network: network.clone(),
            ..base
        },
        NetConnectionKind::DStar { reflector }
        | NetConnectionKind::Ysf { reflector }
        | NetConnectionKind::Urf { reflector } => NetConnectionWire {
            reflector: Some(reflector.clone()),
            ..base
        },
        NetConnectionKind::Other { label, detail } => NetConnectionWire {
            label: Some(label.clone()),
            detail: detail.clone(),
            ..base
        },
    }
}

/// Reads one connection back.
pub fn connection_of(wire: &NetConnectionWire) -> Result<NetConnection, NetConnectionWireError> {
    let kind = kind_of(wire).ok_or(NetConnectionWireError {
        position: wire.position,
    })?;
    Ok(NetConnection {
        id: wire.id,
        position: wire.position,
        kind,
    })
}

fn kind_of(wire: &NetConnectionWire) -> Option<NetConnectionKind> {
    let rf = || -> Option<(i64, Band, Mode)> {
        Some((
            wire.planned_frequency_hz?,
            Band::try_from(wire.band.as_deref()?).ok()?,
            Mode::try_from(wire.mode.as_deref()?).ok()?,
        ))
    };
    match wire.kind.as_str() {
        "hf" => {
            let (planned_frequency_hz, band, mode) = rf()?;
            Some(NetConnectionKind::Hf {
                planned_frequency_hz,
                band,
                mode,
            })
        }
        "repeater" => {
            let (planned_frequency_hz, band, mode) = rf()?;
            let tone_mode = match wire.tone_mode.as_deref() {
                None => None,
                Some(token) => Some(ToneMode::try_from(token).ok()?),
            };
            Some(NetConnectionKind::Repeater {
                planned_frequency_hz,
                band,
                mode,
                offset_hz: wire.repeater_offset_hz,
                tone_mode,
                tone_value: wire.tone_value.clone(),
            })
        }
        "echolink" => Some(NetConnectionKind::EchoLink {
            node: wire.node.clone()?,
        }),
        "allstar" => Some(NetConnectionKind::AllStar {
            node: wire.node.clone()?,
        }),
        // NO `?` on `network`: a DMR connection whose network nobody recorded
        // is a legitimate row, and an older DMR connection is one.
        "dmr" => Some(NetConnectionKind::Dmr {
            talkgroup: wire.talkgroup.clone()?,
            network: wire.network.clone(),
        }),
        "dstar" => Some(NetConnectionKind::DStar {
            reflector: wire.reflector.clone()?,
        }),
        "ysf" => Some(NetConnectionKind::Ysf {
            reflector: wire.reflector.clone()?,
        }),
        "urf" => Some(NetConnectionKind::Urf {
            reflector: wire.reflector.clone()?,
        }),
        "other" => {
            let label = wire.label.clone()?;
            // A reserved label is machine-minted and must survive a round trip
            // verbatim; the owner-facing constructor would refuse it.
            if is_reserved_label(&label) {
                Some(NetConnectionKind::Other {
                    label,
                    detail: wire.detail.clone(),
                })
            } else {
                NetConnectionKind::other(&label, wire.detail.clone()).ok()
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::connection::UNCLASSIFIED_LABEL_PREFIX;

    fn set_of(kinds: Vec<NetConnectionKind>) -> NetConnectionSet {
        NetConnectionSet::new(
            kinds
                .into_iter()
                .enumerate()
                .map(|(i, kind)| NetConnection {
                    id: Uuid::from_u128(i as u128 + 1),
                    position: i as i32,
                    kind,
                })
                .collect(),
        )
        .expect("non-empty set")
    }

    #[test]
    fn every_kind_survives_a_json_round_trip_through_the_one_shape() {
        let set = set_of(vec![
            NetConnectionKind::Hf {
                planned_frequency_hz: 14_230_000,
                band: Band::TwentyMeters,
                mode: Mode::Ssb,
            },
            NetConnectionKind::Repeater {
                planned_frequency_hz: 146_940_000,
                band: Band::TwoMeters,
                mode: Mode::Fm,
                offset_hz: Some(-600_000),
                tone_mode: Some(ToneMode::Ctcss),
                tone_value: Some("100.0".to_owned()),
            },
            NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
            NetConnectionKind::AllStar {
                node: "55555".to_owned(),
            },
            NetConnectionKind::Dmr {
                talkgroup: "31337".to_owned(),
                network: Some("Brandmeister".to_owned()),
            },
            NetConnectionKind::Dmr {
                talkgroup: "9".to_owned(),
                network: None,
            },
            NetConnectionKind::DStar {
                reflector: "REF030 C".to_owned(),
            },
            NetConnectionKind::Ysf {
                reflector: "America Link".to_owned(),
            },
            NetConnectionKind::Urf {
                reflector: "URF307 B".to_owned(),
            },
            NetConnectionKind::other("Zello", Some("channel netroll".to_owned()))
                .expect("owner-authored other"),
            NetConnectionKind::unclassified("reflector", "XLX950 D".to_owned()),
        ]);

        let json = serde_json::to_string(&wire_connections(&set)).expect("serializes");
        let read: Vec<NetConnectionWire> = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(
            connection_set_from_wire(&read).expect("round trips"),
            set,
            "every kind survives storage and read-back unchanged"
        );
        assert!(
            json.contains(UNCLASSIFIED_LABEL_PREFIX),
            "a machine-minted reserved label round trips verbatim rather than being refused"
        );
    }

    #[test]
    fn a_kind_this_version_cannot_read_is_refused_rather_than_degraded() {
        let mut wire = wire_connections(&set_of(vec![NetConnectionKind::EchoLink {
            node: "12345".to_owned(),
        }]));
        wire[0].kind = "carrier-pigeon".to_owned();
        assert_eq!(
            connection_set_from_wire(&wire),
            Err(NetConnectionWireError { position: 0 })
        );
    }

    #[test]
    fn an_empty_stored_list_is_refused_rather_than_read_as_a_net_with_no_way_in() {
        assert!(connection_set_from_wire(&[]).is_err());
    }

    // --- The label resolver and `via`'s display ------------------

    fn two_of_one_kind() -> Vec<NetConnectionWire> {
        wire_connections(&set_of(vec![
            NetConnectionKind::Hf {
                planned_frequency_hz: 14_230_000,
                band: Band::TwentyMeters,
                mode: Mode::Ssb,
            },
            NetConnectionKind::Hf {
                planned_frequency_hz: 7_185_000,
                band: Band::FortyMeters,
                mode: Mode::Ssb,
            },
        ]))
    }

    #[test]
    fn two_connections_of_the_same_kind_get_different_labels() {
        let wires = two_of_one_kind();
        assert_ne!(
            connection_label(&wires[0]),
            connection_label(&wires[1]),
            "a label of the bare kind name would say the same thing about two different ways in"
        );
    }

    #[test]
    fn the_frequency_free_name_drops_only_the_frequency_and_only_for_the_rf_kinds() {
        // For `hf`/`repeater` the identifying fact IS the
        // frequency, so the name is the kind alone — and the label is the name
        // plus the MHz detail, byte for byte. For every other kind the two
        // functions agree, because nothing there is a number that moves.
        let rf = two_of_one_kind();
        assert_eq!(connection_name(&rf[0]), "HF");
        assert_eq!(connection_name(&rf[0]), connection_name(&rf[1]));
        assert_eq!(
            connection_label(&rf[0]),
            format!("{} — 14.230 MHz", connection_name(&rf[0]))
        );
        let internet = wire_connections(&set_of(vec![
            NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
            NetConnectionKind::Dmr {
                talkgroup: "91".to_owned(),
                network: Some("Brandmeister".to_owned()),
            },
        ]));
        for wire in &internet {
            assert_eq!(connection_name(wire), connection_label(wire));
        }
        assert_eq!(connection_name(&internet[0]), "EchoLink — 12345");
    }

    #[test]
    fn a_sub_khz_way_in_is_labelled_with_the_frequency_it_actually_carries() {
        // The TS twin `connectionLabel` asserts this SAME literal, in
        // `connectionPresentation.test.ts`. The two resolvers are hand-written
        // twins and this fixture is where they are held to each other.
        let wires = wire_connections(&set_of(vec![NetConnectionKind::Repeater {
            planned_frequency_hz: 145_512_500,
            band: Band::TwoMeters,
            mode: Mode::Fm,
            offset_hz: None,
            tone_mode: None,
            tone_value: None,
        }]));
        assert_eq!(connection_label(&wires[0]), "Repeater — 145.512.5000 MHz");
    }

    #[test]
    fn two_ways_in_one_raster_step_apart_are_told_apart_by_their_labels() {
        let wires = wire_connections(&set_of(vec![
            NetConnectionKind::Repeater {
                planned_frequency_hz: 145_512_100,
                band: Band::TwoMeters,
                mode: Mode::Fm,
                offset_hz: None,
                tone_mode: None,
                tone_value: None,
            },
            NetConnectionKind::Repeater {
                planned_frequency_hz: 145_512_300,
                band: Band::TwoMeters,
                mode: Mode::Fm,
                offset_hz: None,
                tone_mode: None,
                tone_value: None,
            },
        ]));
        assert_ne!(connection_label(&wires[0]), connection_label(&wires[1]));
    }

    #[test]
    fn a_label_names_the_kind_the_way_a_person_says_it_not_the_wire_token() {
        let wires = wire_connections(&set_of(vec![NetConnectionKind::DStar {
            reflector: "REF030 C".to_owned(),
        }]));
        assert_eq!(connection_label(&wires[0]), "D-Star — REF030 C");
    }

    #[test]
    fn a_machine_minted_other_label_is_never_published_as_the_connections_name() {
        let wires = wire_connections(&set_of(vec![NetConnectionKind::unclassified(
            "reflector",
            "XLX950 D".to_owned(),
        )]));
        let label = connection_label(&wires[0]);
        assert!(
            !label.contains(UNCLASSIFIED_LABEL_PREFIX),
            "publishing `{UNCLASSIFIED_LABEL_PREFIX}…` states a NetRoll internal as a fact about the net, got {label:?}"
        );
        assert_eq!(label, "Other — XLX950 D");
    }

    #[test]
    fn an_owner_authored_other_is_named_by_the_owners_own_words() {
        let wires = wire_connections(&set_of(vec![
            NetConnectionKind::other("Zello", Some("channel netroll".to_owned()))
                .expect("owner-authored other"),
        ]));
        assert_eq!(connection_label(&wires[0]), "Zello — channel netroll");
    }

    #[test]
    fn an_unrecorded_via_and_an_unresolvable_one_are_different_facts() {
        let wires = two_of_one_kind();
        let stranger = Via::Connection(Uuid::from_u128(999));
        assert_eq!(resolve_via(None, &wires), ViaDisplay::NotRecorded);
        assert_eq!(
            resolve_via(Some(&stranger), &wires),
            ViaDisplay::Unresolvable,
            "a via naming an id the snapshot does not hold must not read as `not recorded`"
        );
        assert_eq!(via_label(&ViaDisplay::NotRecorded), None);
        assert_eq!(
            via_label(&ViaDisplay::Unresolvable),
            Some(UNRESOLVABLE_VIA_LABEL.to_owned())
        );
    }

    #[test]
    fn an_unresolvable_via_renders_neither_a_blank_nor_the_uuid_nor_the_export_connections_label() {
        let wires = two_of_one_kind();
        let stranger_id = Uuid::from_u128(999);
        let rendered =
            via_label(&resolve_via(Some(&Via::Connection(stranger_id)), &wires)).expect("renders");
        assert!(!rendered.trim().is_empty(), "never blank");
        assert!(
            !rendered.contains(&stranger_id.to_string()),
            "never the UUID's string form"
        );
        assert_ne!(
            rendered,
            connection_label(&wires[0]),
            "never the ADIF-export connection's label — that is the lie this story exists to kill"
        );
    }

    #[test]
    fn a_via_resolves_by_id_and_never_by_position() {
        let wires = two_of_one_kind();
        let second = Via::Connection(wires[1].id);
        assert_eq!(
            via_label(&resolve_via(Some(&second), &wires)),
            Some(connection_label(&wires[1]))
        );
    }

    #[test]
    fn free_text_via_renders_the_operators_own_words_verbatim() {
        let wires = two_of_one_kind();
        let typed = Via::Unlisted("Bill's phone patch".to_owned());
        assert_eq!(
            via_label(&resolve_via(Some(&typed), &wires)),
            Some("Bill's phone patch".to_owned())
        );
    }

    #[test]
    fn a_via_crosses_the_wire_as_one_kind_discriminated_object_not_two_nullable_keys() {
        let id = Uuid::from_u128(7);
        let connection = serde_json::to_value(via_of(&Via::Connection(id))).expect("serializes");
        let object = connection.as_object().expect("an object");
        assert_eq!(
            object.get("kind").and_then(|k| k.as_str()),
            Some("connection")
        );
        assert_eq!(
            object.get("connectionId").and_then(|k| k.as_str()),
            Some(id.to_string().as_str())
        );
        assert!(
            !object.contains_key("text"),
            "`both set` and `neither set` must be unrepresentable, so the variants share no keys"
        );

        let unlisted =
            serde_json::to_value(via_of(&Via::Unlisted("patch".to_owned()))).expect("serializes");
        let object = unlisted.as_object().expect("an object");
        assert_eq!(
            object.get("kind").and_then(|k| k.as_str()),
            Some("unlisted")
        );
        assert_eq!(object.get("text").and_then(|k| k.as_str()), Some("patch"));
        assert!(!object.contains_key("connectionId"));
    }

    #[test]
    fn both_via_variants_survive_a_json_round_trip() {
        for via in [
            Via::Connection(Uuid::from_u128(11)),
            Via::Unlisted("moonbounce, honestly".to_owned()),
        ] {
            let json = serde_json::to_string(&via_of(&via)).expect("serializes");
            let read: ViaWire = serde_json::from_str(&json).expect("deserializes");
            assert_eq!(via_from_wire(&read), via);
        }
    }

    #[test]
    fn a_cross_mode_nets_two_ways_in_export_different_adif_fields_from_the_same_set() {
        let wires = wire_connections(&set_of(vec![
            NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
            NetConnectionKind::Hf {
                planned_frequency_hz: 14_230_000,
                band: Band::TwentyMeters,
                mode: Mode::Ssb,
            },
        ]));
        let over_echolink = adif_fields_for_via_wire(Some(&Via::Connection(wires[0].id)), &wires);
        let over_hf = adif_fields_for_via_wire(Some(&Via::Connection(wires[1].id)), &wires);
        assert_eq!(over_echolink.prop_mode, Some("ECH"));
        assert_eq!(over_echolink.band, None);
        assert_eq!(over_echolink.freq_hz, None);
        assert_eq!(over_hf.band, Some("20m"));
        assert_eq!(over_hf.freq_hz, Some(14_230_000));
        assert_eq!(over_hf.prop_mode, None);
    }

    #[test]
    fn an_unrecorded_via_falls_back_to_the_first_rf_way_in_not_to_position_zero() {
        // The defect this guards: a net leading with EchoLink and
        // listing HF second really is on 20m, and exporting no BAND/MODE/FREQ
        // for it discards an RF connection that genuinely exists.
        let wires = wire_connections(&set_of(vec![
            NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
            NetConnectionKind::Hf {
                planned_frequency_hz: 14_230_000,
                band: Band::TwentyMeters,
                mode: Mode::Ssb,
            },
        ]));
        let fields = adif_fields_for_via_wire(None, &wires);
        assert_eq!(fields.band, Some("20m"));
        assert_eq!(fields.freq_hz, Some(14_230_000));
    }

    #[test]
    fn an_internet_only_nets_fallback_is_still_the_one_it_leads_with() {
        // Ported from `NetConnectionSet::adif_export_connection`'s tests when
        // that resolver turned out to have no production caller and was
        // deleted. The RULE it pinned is production behaviour and
        // survives here, against the one resolver that ships.
        let wires = wire_connections(&set_of(vec![
            NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
            NetConnectionKind::AllStar {
                node: "55555".to_owned(),
            },
        ]));
        let fields = adif_fields_for_via_wire(None, &wires);
        assert_eq!(
            fields.prop_mode,
            Some("ECH"),
            "with no RF way in there is nothing to prefer, so the owner's lead stands"
        );
    }

    #[test]
    fn a_reorder_moves_which_way_in_the_fallback_describes() {
        // Also ported from the deleted domain-set resolver. Position, not vector
        // order, is what decides the lead when no way in carries a frequency.
        let mut wires = wire_connections(&set_of(vec![
            NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
            NetConnectionKind::AllStar {
                node: "55555".to_owned(),
            },
        ]));
        wires[0].position = 1;
        wires[1].position = 0;
        let fields = adif_fields_for_via_wire(None, &wires);
        assert_eq!(
            fields.prop_mode,
            Some("INTERNET"),
            "AllStar leads after the reorder, so the fallback describes it"
        );
    }

    #[test]
    fn an_rf_way_in_with_an_unreadable_band_still_exports_its_frequency() {
        // The `<FREQ>` anchor a logbook falls back on when it cannot map the
        // band. `connection_of` refuses this row outright — one unknown token
        // makes the whole connection unreadable — and returning a bare default
        // here would drop a frequency the snapshot actually holds.
        let mut wires = wire_connections(&set_of(vec![NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        }]));
        wires[0].band = Some("60cm".to_owned());
        assert!(
            connection_of(&wires[0]).is_err(),
            "the fixture is unreadable"
        );

        let fields = adif_fields_for_wire(&wires[0]);
        assert_eq!(fields.freq_hz, Some(14_230_000));
        assert_eq!(fields.band, None);
        assert_eq!(fields.mode, None);
        assert_eq!(fields.prop_mode, None);
    }

    #[test]
    fn an_unreadable_internet_way_in_invents_no_frequency() {
        let mut wires = wire_connections(&set_of(vec![NetConnectionKind::EchoLink {
            node: "12345".to_owned(),
        }]));
        wires[0].node = None;
        assert!(
            connection_of(&wires[0]).is_err(),
            "the fixture is unreadable"
        );

        assert_eq!(
            adif_fields_for_wire(&wires[0]),
            AdifConnectionFields::default()
        );
    }

    #[test]
    fn an_unresolvable_or_free_text_via_omits_every_adif_tag_rather_than_borrowing_one() {
        let wires = two_of_one_kind();
        for via in [
            Via::Connection(Uuid::from_u128(999)),
            Via::Unlisted("a phone patch".to_owned()),
        ] {
            let fields = adif_fields_for_via_wire(Some(&via), &wires);
            assert_eq!(fields, AdifConnectionFields::default());
            assert_ne!(
                fields,
                adif_fields_for_via_wire(None, &wires),
                "it must not collapse into the unrecorded fallback"
            );
        }
    }
}
