// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net definition domain: taxonomies, field validation and the
//! [`NetDefinition`] read model. Pure, no I/O. The adapter trusts the validated
//! write shape, and the wire vocabulary lives solely in [`enums`], extensible
//! without touching adapters or HTTP.

pub mod connection;
pub mod delivery;
pub mod discovery;
pub mod enums;
pub mod schedule;
pub mod validation;
pub mod wire;

use uuid::Uuid;

use connection::NetConnectionSet;
use enums::{NetCategory, NetType, Visibility};

/// A persisted net definition as read back from storage: the
/// server-owned `id`/`definition_version`, every captured field in its typed
/// form, and the resolved `owner_account_ids` set the object-level authz
/// check ([`crate::authz::can_manage_definition`]) decides against.
///
/// `definition_version` is a monotonic counter the server increments on each
/// edit; it is the provenance stamp a later session snapshots
/// `(id, definition_version)` under. Times are epoch
/// millis, the domain's currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDefinition {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Monotonic version, starting at `1`, incremented on every edit.
    pub definition_version: i32,
    /// Net title.
    pub title: String,
    /// Free-text description, or `None`.
    pub description: Option<String>,
    /// Country, or `None`.
    pub country: Option<String>,
    /// State/province, or `None`.
    pub state: Option<String>,
    /// Canonical Maidenhead grid, or `None`.
    pub grid: Option<String>,
    /// Every way to reach this net — the ONLY home a
    /// connection fact has on a definition. Frequency, band, mode, offset,
    /// tone, node and reflector are each a property of one connection, and a
    /// net with no RF way has none of them anywhere.
    pub connections: NetConnectionSet,
    /// Net category.
    pub net_category: NetCategory,
    /// Net type.
    pub net_type: NetType,
    /// Expected duration in minutes, or `None`.
    pub expected_duration_minutes: Option<i32>,
    /// Discovery visibility: `Listed` appears in discovery,
    /// `Unlisted` is reachable only by `link_token`.
    pub visibility: Visibility,
    /// Unguessable, app-minted plaintext permalink token — the capability the
    /// public read-by-token endpoint resolves. Stable across
    /// edits and present for every net regardless of visibility. NOT hashed
    /// at rest (contrast the auth/session tokens): a low-sensitivity,
    /// re-readable, read-only bearer permalink, not a login secret.
    pub link_token: String,
    /// The account ids that own this definition: the creator at first, and
    /// whoever the owner endpoints add after.
    pub owner_account_ids: Vec<Uuid>,
    /// Creation instant, epoch millis.
    pub created_at_millis: u64,
    /// Last-update instant, epoch millis.
    pub updated_at_millis: u64,
    /// Set when the net was archived by the ownership-lifecycle path — `None`
    /// for an active net; a later story extends archival to
    /// owner-initiated delete.
    pub archived_at_millis: Option<u64>,
}
