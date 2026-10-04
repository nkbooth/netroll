// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for `net_connections`, the authoritative record of the ways a net
//! can be reached. [`replace_connections`] is the ONLY writer; the scalar
//! `update` never touches a connection. A row the domain refuses DEGRADES
//! rather than poisoning the read — the migration backfill and a hand-written
//! INSERT both bypass the constructor, and the owner's net must still open.

use netroll_domain::net::connection::{
    NetConnection, NetConnectionKind, NetConnectionSet, is_reserved_label,
};
use netroll_domain::net::enums::{Band, Mode, ToneMode};
use sqlx::{PgExecutor, Postgres, Transaction};
use uuid::Uuid;

/// One `net_connections` row, before the domain decides what it means.
struct ConnectionRow {
    id: Uuid,
    definition_id: Uuid,
    position: i32,
    kind: String,
    planned_frequency_hz: Option<i64>,
    band: Option<String>,
    mode: Option<String>,
    repeater_offset_hz: Option<i64>,
    tone_mode: Option<String>,
    tone_value: Option<String>,
    node: Option<String>,
    reflector: Option<String>,
    network: Option<String>,
    talkgroup: Option<String>,
    label: Option<String>,
    detail: Option<String>,
}

/// Loads the connection sets for every id in `definition_ids`, in ONE query.
///
/// One query rather than one per definition: the owned-nets page and the
/// discovery source both list definitions, and a per-row connection query would
/// turn each into an N+1. (`resolve_definitions`, which `list_owned_page` and
/// the discovery source share, already N+1s on owners; adding a second per-row
/// query would double their round trips.)
///
/// A definition with no rows is absent from the result — the caller decides
/// what an unreachable net means.
pub async fn load_for_definitions<'e, E: PgExecutor<'e>>(
    executor: E,
    definition_ids: &[Uuid],
) -> Result<Vec<(Uuid, NetConnectionSet)>, sqlx::Error> {
    if definition_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as!(
        ConnectionRow,
        "SELECT id, definition_id, position, kind, planned_frequency_hz, band, mode,
                repeater_offset_hz, tone_mode, tone_value, node, reflector, network,
                talkgroup, label, detail
           FROM net_connections
          WHERE definition_id = ANY($1)
          ORDER BY definition_id, position",
        definition_ids,
    )
    .fetch_all(executor)
    .await?;

    let mut grouped: Vec<(Uuid, Vec<NetConnection>)> = Vec::new();
    for row in rows {
        let definition_id = row.definition_id;
        let connection = NetConnection {
            id: row.id,
            position: row.position,
            kind: decode_kind(row),
        };
        match grouped.last_mut() {
            Some((id, connections)) if *id == definition_id => connections.push(connection),
            _ => grouped.push((definition_id, vec![connection])),
        }
    }

    let mut sets = Vec::with_capacity(grouped.len());
    for (definition_id, connections) in grouped {
        // The set is non-empty by construction here — it exists because rows
        // were read — so `new` cannot refuse it, but the error still
        // propagates rather than being unwrapped.
        let set = NetConnectionSet::new(connections)
            .map_err(|e| sqlx::Error::Decode(e.to_string().into()))?;
        sets.push((definition_id, set));
    }
    Ok(sets)
}

/// Loads one definition's connection set.
///
/// Generic over the executor so a caller inside a transaction reads what that
/// transaction will write. A definition's connection set is the input to the
/// scalar edit's merge, and reading it on the pool while the row is being
/// locked and updated elsewhere is a lost update that deletes connections.
pub async fn load_for_definition<'e, E: PgExecutor<'e>>(
    executor: E,
    definition_id: Uuid,
) -> Result<Option<NetConnectionSet>, sqlx::Error> {
    Ok(load_for_definitions(executor, &[definition_id])
        .await?
        .pop()
        .map(|(_, set)| set))
}

/// Replaces a definition's connections — the ONE writer, inside the caller's
/// transaction so the connection rows and the definition row they belong to
/// move together.
pub async fn replace_connections(
    tx: &mut Transaction<'_, Postgres>,
    definition_id: Uuid,
    connections: &NetConnectionSet,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    // A replace is a delete and a re-insert, so `created_at` would otherwise be
    // reset on every scalar edit — including for connections the merge carried
    // through untouched, which is a lie about a row nobody changed.
    let prior = sqlx::query!(
        "SELECT id, created_at FROM net_connections WHERE definition_id = $1",
        definition_id
    )
    .fetch_all(&mut **tx)
    .await?;

    sqlx::query!(
        "DELETE FROM net_connections WHERE definition_id = $1",
        definition_id
    )
    .execute(&mut **tx)
    .await?;

    for connection in connections.connections() {
        let payload = StoragePayload::of(&connection.kind);
        let created_at = prior
            .iter()
            .find(|row| row.id == connection.id)
            .map_or(now, |row| row.created_at);
        sqlx::query!(
            "INSERT INTO net_connections
                (id, definition_id, position, kind, planned_frequency_hz, band, mode,
                 repeater_offset_hz, tone_mode, tone_value, node, reflector, network,
                 talkgroup, label, detail, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18)",
            connection.id,
            definition_id,
            connection.position,
            connection.kind.as_str(),
            payload.planned_frequency_hz,
            payload.band,
            payload.mode,
            payload.repeater_offset_hz,
            payload.tone_mode,
            payload.tone_value,
            payload.node,
            payload.reflector,
            payload.network,
            payload.talkgroup,
            payload.label,
            payload.detail,
            created_at,
            now,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

/// The column values one kind writes. A kind uses only the columns it has.
#[derive(Default)]
struct StoragePayload<'a> {
    planned_frequency_hz: Option<i64>,
    band: Option<&'a str>,
    mode: Option<&'a str>,
    repeater_offset_hz: Option<i64>,
    tone_mode: Option<&'a str>,
    tone_value: Option<&'a str>,
    node: Option<&'a str>,
    reflector: Option<&'a str>,
    network: Option<&'a str>,
    talkgroup: Option<&'a str>,
    label: Option<&'a str>,
    detail: Option<&'a str>,
}

impl<'a> StoragePayload<'a> {
    fn of(kind: &'a NetConnectionKind) -> Self {
        match kind {
            NetConnectionKind::Hf {
                planned_frequency_hz,
                band,
                mode,
            } => Self {
                planned_frequency_hz: Some(*planned_frequency_hz),
                band: Some(band.as_str()),
                mode: Some(mode.as_str()),
                ..Self::default()
            },
            NetConnectionKind::Repeater {
                planned_frequency_hz,
                band,
                mode,
                offset_hz,
                tone_mode,
                tone_value,
            } => Self {
                planned_frequency_hz: Some(*planned_frequency_hz),
                band: Some(band.as_str()),
                mode: Some(mode.as_str()),
                repeater_offset_hz: *offset_hz,
                tone_mode: tone_mode.map(ToneMode::as_str),
                tone_value: tone_value.as_deref(),
                ..Self::default()
            },
            NetConnectionKind::EchoLink { node } | NetConnectionKind::AllStar { node } => Self {
                node: Some(node.as_str()),
                ..Self::default()
            },
            NetConnectionKind::Dmr { talkgroup, network } => Self {
                talkgroup: Some(talkgroup.as_str()),
                network: network.as_deref(),
                ..Self::default()
            },
            NetConnectionKind::DStar { reflector }
            | NetConnectionKind::Ysf { reflector }
            | NetConnectionKind::Urf { reflector } => Self {
                reflector: Some(reflector.as_str()),
                ..Self::default()
            },
            NetConnectionKind::Other { label, detail } => Self {
                label: Some(label.as_str()),
                detail: detail.as_deref(),
                ..Self::default()
            },
        }
    }
}

/// Reads a stored row into its domain kind, degrading anything the closed kind
/// set refuses into `other` rather than failing the whole definition's load.
///
/// The degraded label lives in the same reserved namespace the backfill uses,
/// so residue from either source counts apart from an owner's own `other`, and
/// the row's own values are preserved verbatim in `detail` — nothing is thrown
/// away by a read.
fn decode_kind(row: ConnectionRow) -> NetConnectionKind {
    match decode_named_kind(&row) {
        Some(kind) => kind,
        None => NetConnectionKind::unclassified(&row.kind, describe(&row)),
    }
}

fn decode_named_kind(row: &ConnectionRow) -> Option<NetConnectionKind> {
    let rf = || -> Option<(i64, Band, Mode)> {
        Some((
            row.planned_frequency_hz?,
            Band::try_from(row.band.as_deref()?).ok()?,
            Mode::try_from(row.mode.as_deref()?).ok()?,
        ))
    };
    match row.kind.as_str() {
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
            let tone_mode = match row.tone_mode.as_deref() {
                None => None,
                Some(token) => Some(ToneMode::try_from(token).ok()?),
            };
            Some(NetConnectionKind::Repeater {
                planned_frequency_hz,
                band,
                mode,
                offset_hz: row.repeater_offset_hz,
                tone_mode,
                tone_value: row.tone_value.clone(),
            })
        }
        "echolink" => Some(NetConnectionKind::EchoLink {
            node: row.node.clone()?,
        }),
        "allstar" => Some(NetConnectionKind::AllStar {
            node: row.node.clone()?,
        }),
        "dmr" => Some(NetConnectionKind::Dmr {
            talkgroup: row.talkgroup.clone()?,
            // NO `?`. A NULL network is a DMR connection whose network was
            // never recorded; `?` here would degrade every such row — which is
            // every DMR connection minted before the network column existed — to
            // `unclassified-dmr` residue on its next read.
            network: row.network.clone(),
        }),
        "dstar" => Some(NetConnectionKind::DStar {
            reflector: row.reflector.clone()?,
        }),
        "ysf" => Some(NetConnectionKind::Ysf {
            reflector: row.reflector.clone()?,
        }),
        "urf" => Some(NetConnectionKind::Urf {
            reflector: row.reflector.clone()?,
        }),
        "other" => {
            let label = row.label.clone()?;
            // A reserved label is machine-minted and must survive a round trip
            // verbatim; the owner-facing constructor would refuse it.
            if is_reserved_label(&label) {
                Some(NetConnectionKind::Other {
                    label,
                    detail: row.detail.clone(),
                })
            } else {
                NetConnectionKind::other(&label, row.detail.clone()).ok()
            }
        }
        _ => None,
    }
}

/// Renders a row's populated payload columns so a degraded connection keeps
/// every value it held, in a shape an operator can read back.
fn describe(row: &ConnectionRow) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |name: &str, value: Option<&str>| {
        if let Some(value) = value {
            parts.push(format!("{name}={value}"));
        }
    };
    let frequency = row.planned_frequency_hz.map(|hz| hz.to_string());
    let offset = row.repeater_offset_hz.map(|hz| hz.to_string());
    push("plannedFrequencyHz", frequency.as_deref());
    push("band", row.band.as_deref());
    push("mode", row.mode.as_deref());
    push("repeaterOffsetHz", offset.as_deref());
    push("toneMode", row.tone_mode.as_deref());
    push("toneValue", row.tone_value.as_deref());
    push("node", row.node.as_deref());
    push("reflector", row.reflector.as_deref());
    push("network", row.network.as_deref());
    push("talkgroup", row.talkgroup.as_deref());
    push("label", row.label.as_deref());
    push("detail", row.detail.as_deref());
    parts.join("; ")
}
