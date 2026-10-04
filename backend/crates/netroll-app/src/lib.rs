// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! NetRoll application crate: HTTP surface, configuration, and DI wiring
//! composing the pure domain with the Postgres/SMTP adapters.

pub mod avatar_cleanup;
pub mod bot_mitigation;
pub mod clock;
/// Resolves and validates the process configuration from raw environment values.
pub mod config;
pub mod csp;
pub mod delivery;
pub mod delivery_sweeper;
pub mod finalizer;
pub mod http;
pub mod known_addresses;
pub mod lookup;
pub mod middleware;
pub mod occurrence_spawner;
pub mod presence_monitor;
pub mod retention;
pub mod ws;
// `static` is a keyword; the raw identifier keeps the file name mandated by
// the architecture directory tree.
/// The routers that serve the built SPA shell and the uploaded avatars.
pub mod r#static;
