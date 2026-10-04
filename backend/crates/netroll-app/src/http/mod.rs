// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! HTTP surface: resource-shaped, kebab-case auth endpoints.
//!
//! Handlers stay thin — verdicts and linking decisions come from
//! `netroll_domain::auth`; storage and mail go through adapters.

pub mod abuse_reports;
pub mod account_export;
pub mod admin;
pub mod audit;
pub mod avatar;
pub mod check_in_history;
pub mod discovery;
pub mod favorites;
pub mod health;
pub mod net_definitions;
pub mod net_sessions;
pub mod problem;
pub mod rate_limit;
pub(crate) mod tokens;
pub mod undefinable;

use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, QueryRejection};
// `Query` is denied project-wide by `clippy.toml`; this is the only place the
// denied type enters the crate, behind the `AppQuery`/`AppStrictQuery` wrappers.
// `expect`, not `allow`: if `clippy.toml` goes missing or stops being
// discovered, the lint stops firing and this becomes an
// `unfulfilled_lint_expectations` build failure instead of silence.
#[expect(clippy::disallowed_types)]
use axum::extract::Query;
use axum::extract::{Extension, FromRequest, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use netroll_adapters::crypto::EnvelopeCipher;
use netroll_adapters::egress::SsrfSafeEgress;
use netroll_adapters::lookup::hamcall::HamcallLookupProvider;
use netroll_adapters::lookup::qrz::QrzLookupProvider;
use netroll_adapters::mail::is_deliverable_address;
use netroll_adapters::pg::abuse_reports::AbuseReportRepo;
use netroll_adapters::pg::accounts::{AccountRepo, SetCallsignOutcome, SoftDeleteOutcome};
use netroll_adapters::pg::admin_search::AdminSearchRepo;
use netroll_adapters::pg::audit_log::AuditLogRepo;
use netroll_adapters::pg::consents::ConsentRepo;
use netroll_adapters::pg::delivery_configs::DeliveryConfigRepo;
use netroll_adapters::pg::delivery_jobs::DeliveryJobRepo;
use netroll_adapters::pg::discovery::DiscoveryRepo;
use netroll_adapters::pg::email_changes::{ConfirmEmailChangeOutcome, EmailChangeRepo};
use netroll_adapters::pg::favorites::FavoritesRepo;
use netroll_adapters::pg::health::HealthRepo;
use netroll_adapters::pg::magic_links::MagicLinkRepo;
use netroll_adapters::pg::net_definitions::NetDefinitionRepo;
use netroll_adapters::pg::net_session_roles::NetSessionRoleRepo;
use netroll_adapters::pg::net_sessions::NetSessionRepo;
use netroll_adapters::pg::qrz_credentials::QrzCredentialRepo;
use netroll_adapters::pg::roster_memory::RosterMemoryRepo;
use netroll_adapters::pg::schedules::ScheduleRepo;
use netroll_adapters::pg::session_events::SessionEventLog;
use netroll_adapters::pg::sessions::SessionRepo;
use netroll_domain::audit::AuditAction;
use netroll_domain::auth::{
    ConsumptionOutcome, EmailControlProof, MAGIC_LINK_TTL_MILLIS, MagicLinkVerdict,
    SESSION_ABSOLUTE_MILLIS, email_change_verdict, magic_link_verdict, normalize_email,
    on_magic_link_consumed,
};
use netroll_domain::bot_mitigation::BotVerdict;
use netroll_domain::callsign::parse_callsign;
use netroll_domain::consent::{
    CURRENT_TERMS_VERSION, ConsentVerdict, consent_verdict, record_consent,
};
use netroll_domain::deletion::{DELETION_GRACE_MILLIS, DeletionVerdict, deletion_verdict};
use netroll_domain::egress::Egress;
use netroll_domain::model::account::{Account, ProfileFields};
use netroll_domain::ports::{AvatarStore, Clock, CredentialCipher, LookupProvider, Mailer};
use netroll_domain::profile::{
    ProfileError, describe_illegal_character, gravatar_url, parse_avatar_url, parse_display_name,
    parse_grid, parse_location,
};
use netroll_domain::qrz::{CipherError, QrzCredentials, parse_qrz_password, parse_qrz_username};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::clock::SystemClock;
use crate::lookup::{LookupCache, LookupService};
use crate::middleware::consent::ConsentedAccount;
use crate::middleware::session::{CurrentAccount, require_session};
use problem::ApiError;
use rate_limit::{
    AbuseReportRateLimiter, EmailChangeRateLimiter, EmailRateLimiter, FavoriteRateLimiter,
    FavoritesReadRateLimiter, LookupRateLimiter, MagicLinkAggregateLimiter, NetCreationRateLimiter,
    SelfCheckInRateLimiter,
};

/// Name of the session cookie; the `__Host-` prefix pins Secure + Path=/ +
/// no Domain in the browser itself.
pub(crate) const SESSION_COOKIE: &str = "__Host-netroll-session";

/// Shared handler dependencies, cheap to clone per request.
#[derive(Clone)]
pub struct AppState {
    /// Accounts + auth methods storage.
    pub accounts: AccountRepo,
    /// Magic-link token storage.
    pub magic_links: MagicLinkRepo,
    /// Email-change confirmation token storage.
    pub email_changes: EmailChangeRepo,
    /// Session storage.
    pub sessions: SessionRepo,
    /// Versioned consent storage.
    pub consents: ConsentRepo,
    /// Net-definition storage.
    pub net_definitions: NetDefinitionRepo,
    /// Net schedule + planned-occurrence storage.
    pub schedules: ScheduleRepo,
    /// Per-net delivery-config storage: email/webhook targets and the HMAC secret.
    pub delivery_configs: DeliveryConfigRepo,
    /// Durable on-close delivery legs: the debt `close()` records in its own
    /// transaction and the sweeper pays off.
    pub delivery_jobs: DeliveryJobRepo,
    /// Public discovery read model.
    pub discovery: DiscoveryRepo,
    /// Per-account favorites storage — the "My Nets" bookmark list.
    pub favorites: FavoritesRepo,
    /// The database half of `GET /healthz`.
    pub health: HealthRepo,
    /// Net-session snapshot/projection storage.
    pub net_sessions: NetSessionRepo,
    /// Per-net session role-grant storage: the object-level authorization seam.
    pub net_session_roles: NetSessionRoleRepo,
    /// Append-only session event log.
    pub session_events: SessionEventLog,
    /// Per-account QRZ credential storage: opaque sealed bytes only, so the
    /// plaintext never reaches this repo.
    pub qrz_credentials: QrzCredentialRepo,
    /// Public abuse-report storage: the queue an admin actions.
    pub abuse_reports: AbuseReportRepo,
    /// Bounded object search behind the admin dashboard.
    pub admin_search: AdminSearchRepo,
    /// Append-only audit log: actor, action, target and timestamp, PII- and
    /// secret-free.
    pub audit_log: AuditLogRepo,
    /// Per-net-definition roster-memory projection: the prefill read model the
    /// check-in hot path looks up on callsign blur.
    pub roster_memory: RosterMemoryRepo,
    /// Outbound mail port.
    pub mailer: Arc<dyn Mailer + Send + Sync>,
    /// SSRF-safe outbound HTTP client. It re-resolves and re-pins the target IP
    /// AT DELIVERY TIME, which is the real SSRF control; the config-time
    /// `validate_egress_url` is necessary but not sufficient. A trait object so
    /// tests can supply a capturing fake.
    pub egress: Arc<dyn Egress + Send + Sync>,
    /// Envelope-encryption cipher for QRZ credentials, built once from the
    /// config-resolved KEK. The default is fail-closed: with no KEK every
    /// `seal`/`open` returns `KekUnavailable`, so an instance still boots and
    /// serves every non-QRZ route while the QRZ endpoints answer 503.
    pub credential_cipher: Arc<dyn CredentialCipher + Send + Sync>,
    /// Bounds concurrent in-flight on-close deliveries. Built ONCE here and
    /// threaded into every [`crate::delivery::DeliveryService`] this state hands
    /// out, so the bound is process-wide rather than reset per instance.
    pub delivery_concurrency: Arc<tokio::sync::Semaphore>,
    /// Injected time source.
    pub clock: Arc<dyn Clock + Send + Sync>,
    /// Base URL emailed links point at (no trailing slash).
    pub public_base_url: String,
    /// Per-email magic-link limiter.
    pub email_limiter: Arc<EmailRateLimiter>,
    /// Per-account email-change limiter: bounds how much of the shared
    /// instance-wide send budget ONE account can spend by rotating `new_email`
    /// targets. Checked before the per-address and instance-wide checks.
    pub email_change_limiter: Arc<EmailChangeRateLimiter>,
    /// Instance-wide magic-link send cap. It bounds TOTAL outbound mail, which
    /// the per-address `email_limiter` structurally cannot: that budget is
    /// independent per address, so a harvested address list drives unbounded
    /// mail through the relay. Both are in force, per-address first. A denial
    /// here is a silent uniform 202, never a 429.
    ///
    /// Despite the name, not magic-link-exclusive: `request_email_change` draws
    /// the same bucket, so the bound covers both endpoints.
    pub magic_link_aggregate_limiter: Arc<MagicLinkAggregateLimiter>,
    /// SECOND-TIER budget, drawn on only once `magic_link_aggregate_limiter` is
    /// exhausted. It keeps returning users signing in under a saturating
    /// campaign while leaving TOTAL mail bounded by general + reserve, which an
    /// unbounded exemption would not.
    ///
    /// Who may fall back differs per caller: `create_magic_link` requires the
    /// address to be in `known_addresses`; `request_email_change` requires only
    /// an authenticated caller, because the address it mails is not one the
    /// caller has proven control of and gating on the target's membership would
    /// leak the target's account status.
    pub magic_link_reserve_limiter: Arc<MagicLinkAggregateLimiter>,
    /// Addresses that recently completed a sign-in here: on the anonymous
    /// magic-link path, the only ones allowed to fall back to the reserve once
    /// the general pool is spent. Written ONLY by `create_session`, after a
    /// consumed link has proven control of the address and every refusal has
    /// cleared, so an attacker can neither seed nor read it. Never an
    /// account-existence check.
    pub known_addresses: Arc<crate::known_addresses::KnownAddresses>,
    /// Per-account favorite-write limiter.
    pub favorite_limiter: Arc<FavoriteRateLimiter>,
    /// Per-account favorites-read limiter: a SEPARATE bucket from
    /// `favorite_limiter`, so a read never spends the write budget.
    pub favorites_read_limiter: Arc<FavoritesReadRateLimiter>,
    /// Participant self-check-in write limiter. Account-keyed, NOT IP-keyed, so
    /// NAT'd participants keep independent buckets; the staff `LogCheckIn` path
    /// is UNGOVERNED.
    pub self_check_in_limiter: Arc<SelfCheckInRateLimiter>,
    /// Per-account callbook-lookup limiter, so a scripted flood cannot hammer
    /// the upstream providers or the shared egress client. Over-threshold
    /// degrades to no lookup, never a 429.
    pub lookup_limiter: Arc<LookupRateLimiter>,
    /// Per-account net-creation limiter; a denial is a 429 + `Retry-After`.
    pub net_creation_limiter: Arc<NetCreationRateLimiter>,
    /// Abuse-report submission limiter. IP-keyed, because the reporter may have
    /// no account; a denial is a 429 + `Retry-After`.
    pub abuse_report_limiter: Arc<AbuseReportRateLimiter>,
    /// Boot-resolved platform-admin email allowlist. Empty means no admins and
    /// an unreachable admin surface, the safe default. A SEPARATE authorization
    /// axis from the per-net `authz::Role` hierarchy.
    pub admin_allowlist: Arc<Vec<String>>,
    /// Boot-resolved cap on active (non-archived) nets one account may own
    /// (`MAX_NETS_PER_USER`, default 7).
    pub max_nets_per_user: usize,
    /// Boot-resolved cap on owners per net (`MAX_OWNERS_PER_NET`, default 5).
    pub max_owners_per_net: usize,
    /// The honeypot and timing gate on signup and net creation. Disabled, a
    /// no-op that admits everything, unless `BOT_MITIGATION_SECRET` is set.
    pub bot_mitigation: Arc<crate::bot_mitigation::BotMitigation>,
    /// In-process TTL cache for callbook lookups, shared process-wide so a
    /// repeat lookup of the same callsign costs no second upstream call.
    pub lookup_cache: Arc<LookupCache>,
    /// Persisted QRZ callbook-lookup adapter, built ONCE so its session-key
    /// cache survives across [`Self::lookup_service`] calls. Rebuilding it per
    /// call forces a fresh QRZ login on every lookup.
    pub qrz_lookup_provider: Arc<dyn LookupProvider + Send + Sync>,
    /// Persisted hamcall.dev adapter. Stateless, but stored beside
    /// `qrz_lookup_provider` so both are rebuilt together when the egress is swapped.
    pub hamcall_lookup_provider: Arc<dyn LookupProvider + Send + Sync>,
    /// Per-session broadcast fan-out for the live WS transport. Writers
    /// `publish` strictly post-commit; WS tasks `subscribe`. The seam a
    /// multi-process deployment swaps for Valkey.
    pub hub: Arc<crate::ws::hub::SessionHub>,
    /// Advisory editing-lease store the detail modal acquires, renews and
    /// releases over HTTP. In-memory only: never persisted, never on the wire.
    pub session_locks: Arc<crate::ws::locks::SessionLocks>,
    /// Heartbeat store fed by the active NCS's WS keepalive Pong and read by the
    /// presence monitor to detect stalls. Never persisted, never on the wire.
    pub session_presence: Arc<crate::ws::presence::SessionPresence>,
    /// WS keepalive-Ping and owner-authorization-recheck cadence. A field rather
    /// than a `const` so tests can shrink it and prove the periodic
    /// re-authorization eviction without a real 30-second wait.
    pub ws_keepalive_interval: std::time::Duration,
    /// Test-only sink for commit→WS-send propagation samples. `None` in
    /// production, where the measurement point emits via `tracing`; a
    /// per-live-send `Option` check is the only production cost.
    pub ws_latency_sink: Option<Arc<std::sync::Mutex<Vec<std::time::Duration>>>>,
    /// Blob storage for uploaded avatars. The default is FAIL-CLOSED: an
    /// instance that never installs a store still boots and serves everything
    /// else, with avatar upload answering 503 rather than pretending to save.
    pub avatar_store: Arc<dyn AvatarStore + Send + Sync>,
    /// Public instance settings served by `GET /api/app-config`. All-unset by
    /// default, so a self-hoster gets no analytics and no donation link unless
    /// they opt in. Nothing secret may be added: it ships to every visitor.
    pub app_config: Arc<crate::config::AppConfig>,
}

impl AppState {
    /// Wires the repos over one pool with the system clock.
    pub fn new(
        pool: PgPool,
        mailer: Arc<dyn Mailer + Send + Sync>,
        public_base_url: String,
    ) -> Self {
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
        // Pure construction, no DNS or socket: a failure here is a TLS/runtime
        // fault that must abort boot.
        let egress: Arc<dyn Egress + Send + Sync> = Arc::new(
            SsrfSafeEgress::new().expect("SSRF-safe egress client builds at construction"),
        );
        let qrz_lookup_provider: Arc<dyn LookupProvider + Send + Sync> =
            Arc::new(QrzLookupProvider::new(egress.clone()));
        let hamcall_lookup_provider: Arc<dyn LookupProvider + Send + Sync> =
            Arc::new(HamcallLookupProvider::new(egress.clone()));
        Self {
            accounts: AccountRepo::new(pool.clone()),
            magic_links: MagicLinkRepo::new(pool.clone()),
            email_changes: EmailChangeRepo::new(pool.clone()),
            sessions: SessionRepo::new(pool.clone()),
            consents: ConsentRepo::new(pool.clone()),
            net_definitions: NetDefinitionRepo::new(pool.clone()),
            schedules: ScheduleRepo::new(pool.clone()),
            delivery_configs: DeliveryConfigRepo::new(pool.clone()),
            delivery_jobs: DeliveryJobRepo::new(pool.clone()),
            discovery: DiscoveryRepo::new(pool.clone()),
            favorites: FavoritesRepo::new(pool.clone()),
            health: HealthRepo::new(pool.clone()),
            net_sessions: NetSessionRepo::new(pool.clone()),
            net_session_roles: NetSessionRoleRepo::new(pool.clone()),
            roster_memory: RosterMemoryRepo::new(pool.clone()),
            session_events: SessionEventLog::new(pool.clone()),
            qrz_credentials: QrzCredentialRepo::new(pool.clone()),
            abuse_reports: AbuseReportRepo::new(pool.clone()),
            admin_search: AdminSearchRepo::new(pool.clone()),
            audit_log: AuditLogRepo::new(pool),
            mailer,
            egress,
            qrz_lookup_provider,
            hamcall_lookup_provider,
            // Fail-closed: with no KEK, seal/open return `KekUnavailable`.
            credential_cipher: Arc::new(EnvelopeCipher::new(None)),
            delivery_concurrency: Arc::new(tokio::sync::Semaphore::new(
                crate::delivery::MAX_CONCURRENT_DELIVERIES,
            )),
            lookup_cache: Arc::new(LookupCache::new(clock.clone())),
            clock,
            public_base_url,
            email_limiter: Arc::new(EmailRateLimiter::default()),
            email_change_limiter: Arc::new(EmailChangeRateLimiter::default()),
            // Mirrors the config resolver's fallback, so the default has one source.
            magic_link_aggregate_limiter: Arc::new(MagicLinkAggregateLimiter::default()),
            // A SEPARATE bucket from the general pool above.
            magic_link_reserve_limiter: Arc::new(MagicLinkAggregateLimiter::reserve_default()),
            known_addresses: Arc::new(crate::known_addresses::KnownAddresses::new()),
            favorite_limiter: Arc::new(FavoriteRateLimiter::default()),
            favorites_read_limiter: Arc::new(FavoritesReadRateLimiter::default()),
            self_check_in_limiter: Arc::new(SelfCheckInRateLimiter::default()),
            lookup_limiter: Arc::new(LookupRateLimiter::default()),
            net_creation_limiter: Arc::new(NetCreationRateLimiter::default()),
            abuse_report_limiter: Arc::new(AbuseReportRateLimiter::default()),
            // No admins configured: the admin surface is unreachable until
            // `main` installs the allowlist.
            admin_allowlist: Arc::new(Vec::new()),
            // Fail-closed: boot, serve, and refuse the one feature whose
            // dependency is missing.
            avatar_store: Arc::new(avatar::UnavailableAvatarStore),
            // All integrations off until `main` installs the resolved settings.
            app_config: Arc::new(crate::config::AppConfig::default()),
            // Mirror the config resolver's fallbacks, so each default has one source.
            max_nets_per_user: crate::config::resolve_max_nets_per_user(None)
                .expect("None resolves to the default cap"),
            max_owners_per_net: crate::config::resolve_max_owners_per_net(None)
                .expect("None resolves to the default cap"),
            // Disabled: signup and net creation admit everything until a
            // secret is configured.
            bot_mitigation: Arc::new(crate::bot_mitigation::BotMitigation::disabled()),
            hub: Arc::new(crate::ws::hub::SessionHub::new()),
            session_locks: Arc::new(crate::ws::locks::SessionLocks::new()),
            session_presence: Arc::new(crate::ws::presence::SessionPresence::new()),
            ws_keepalive_interval: std::time::Duration::from_secs(30),
            ws_latency_sink: None,
        }
    }

    /// Builds the shared on-close deliverer. Arc clones only. BOTH close paths
    /// build it from here, so there is one definition and an injected fake
    /// flows straight through.
    pub fn delivery_service(&self) -> crate::delivery::DeliveryService {
        crate::delivery::DeliveryService::new(
            self.net_sessions.clone(),
            self.session_events.clone(),
            self.delivery_configs.clone(),
            self.delivery_jobs.clone(),
            self.accounts.clone(),
            self.mailer.clone(),
            self.egress.clone(),
            self.clock.clone(),
            self.public_base_url.clone(),
            self.delivery_concurrency.clone(),
        )
    }

    /// Builds the callbook [`LookupService`] over the persisted QRZ and hamcall
    /// adapters, both sharing ONE outbound client, plus the credential store,
    /// cipher, result cache and rate limiter. Arc clones only: the providers are
    /// built once and stored, because the QRZ session-key cache must survive
    /// across calls for session reuse to mean anything.
    pub fn lookup_service(&self) -> LookupService {
        LookupService::new(
            self.qrz_lookup_provider.clone(),
            self.hamcall_lookup_provider.clone(),
            self.qrz_credentials.clone(),
            self.credential_cipher.clone(),
            self.lookup_cache.clone(),
            self.lookup_limiter.clone(),
        )
    }

    /// Test-only: swaps in a fake `Egress` so the delivery tests observe the
    /// webhook POST without a real DNS-backed client. Also rebuilds the lookup
    /// providers over it — otherwise the fake is installed but unused, because
    /// the persisted providers still hold the original egress.
    pub fn with_egress_for_tests(mut self, egress: Arc<dyn Egress + Send + Sync>) -> Self {
        self.qrz_lookup_provider = Arc::new(QrzLookupProvider::new(egress.clone()));
        self.hamcall_lookup_provider = Arc::new(HamcallLookupProvider::new(egress.clone()));
        self.egress = egress;
        self
    }

    /// Installs the QRZ credential cipher built from the config-resolved KEK. A
    /// production wiring seam, not only a test one: the KEK comes from config,
    /// so the cipher is installed after `new` rather than constructed inline.
    pub fn with_credential_cipher(
        mut self,
        cipher: Arc<dyn CredentialCipher + Send + Sync>,
    ) -> Self {
        self.credential_cipher = cipher;
        self
    }

    /// Installs the boot-resolved resource caps.
    pub fn with_resource_caps(
        mut self,
        max_nets_per_user: usize,
        max_owners_per_net: usize,
    ) -> Self {
        self.max_nets_per_user = max_nets_per_user;
        self.max_owners_per_net = max_owners_per_net;
        self
    }

    /// Installs the boot-resolved instance-wide magic-link send cap. Replaces
    /// the limiter wholesale, resetting any budget already spent; harmless,
    /// because this is a boot-time seam.
    pub fn with_magic_link_aggregate_cap(mut self, sends_per_hour: u32) -> Self {
        self.magic_link_aggregate_limiter =
            Arc::new(MagicLinkAggregateLimiter::per_hour(sends_per_hour));
        self
    }

    /// Installs the boot-resolved proven-control RESERVE budget. The same
    /// wholesale-replacement seam as [`AppState::with_magic_link_aggregate_cap`].
    pub fn with_magic_link_reserve_cap(mut self, sends_per_hour: u32) -> Self {
        self.magic_link_reserve_limiter =
            Arc::new(MagicLinkAggregateLimiter::per_hour(sends_per_hour));
        self
    }

    /// Installs the bot-mitigation control: enabled when a secret is configured,
    /// disabled otherwise.
    pub fn with_bot_mitigation(
        mut self,
        bot_mitigation: crate::bot_mitigation::BotMitigation,
    ) -> Self {
        self.bot_mitigation = Arc::new(bot_mitigation);
        self
    }

    /// Installs the avatar blob store. `main` passes the filesystem store over
    /// the resolved `AVATAR_DIR`; integration tests pass one over a scratch
    /// directory. Without this the fail-closed default answers 503.
    pub fn with_avatar_store(mut self, store: Arc<dyn AvatarStore + Send + Sync>) -> Self {
        self.avatar_store = store;
        self
    }

    /// Installs the boot-resolved public instance settings (Plausible/Ko-fi).
    /// Everything here ships to every visitor — never put a secret in it.
    pub fn with_app_config(mut self, app_config: crate::config::AppConfig) -> Self {
        self.app_config = Arc::new(app_config);
        self
    }

    /// Installs the boot-resolved platform-admin email allowlist; empty means
    /// no admins.
    pub fn with_admin_allowlist(mut self, admin_allowlist: Vec<String>) -> Self {
        self.admin_allowlist = Arc::new(admin_allowlist);
        self
    }

    /// Test-only override of the WS keepalive cadence, so a test can observe the
    /// owner-authorization re-check without a real 30-second wait. Not
    /// `#[cfg(test)]`-gated because integration tests are a separate crate.
    pub fn with_ws_keepalive_interval_for_tests(mut self, interval: std::time::Duration) -> Self {
        self.ws_keepalive_interval = interval;
        self
    }

    /// Test-only: captures each commit→WS-send propagation sample so a test can
    /// assert the real-time SLO against the exact server-side latencies.
    pub fn with_ws_latency_sink_for_tests(
        mut self,
        sink: Arc<std::sync::Mutex<Vec<std::time::Duration>>>,
    ) -> Self {
        self.ws_latency_sink = Some(sink);
        self
    }
}

/// Builds the `/api` route tree. Magic-link endpoints are public; account and
/// session-management endpoints sit behind the session middleware.
///
/// NO governors, so `SmartIpKeyExtractor` — which needs a `ConnectInfo` — is
/// never invoked. This is the router a caller drives without wiring one up.
pub fn api_router(state: AppState) -> Router {
    compose_router(auth_routes(), public_read_routes(), state)
}

/// [`api_router`] with the IP governors layered: a blunt 10/min/IP limit on the
/// public auth routes and a more generous one on the public reads. Requires
/// serving with `into_make_service_with_connect_info::<SocketAddr>()`, or the
/// peer IP is not extractable.
pub fn api_router_ip_limited(state: AppState) -> Router {
    compose_router(
        auth_routes().layer(rate_limit::ip_governor_layer()),
        public_read_routes().layer(rate_limit::read_ip_governor_layer()),
        state,
    )
}

fn auth_routes() -> Router<AppState> {
    Router::new()
        .route("/api/magic-links", post(create_magic_link))
        .route("/api/sessions", post(create_session))
        // Token-scoped and public: the confirmation link opens from the NEW
        // mailbox, possibly on a device with no session cookie, so the token is
        // the credential exactly as sign-in treats its link.
        .route("/api/email-changes", post(confirm_email_change))
}

/// The public, unauthenticated reads: the by-token permalink with its bare
/// "no token" 404 paths, and the discovery landing. The caller decides whether
/// to govern them.
fn public_read_routes() -> Router<AppState> {
    net_definitions::public_net_routes()
        .merge(discovery::discovery_routes())
        // The account-less live read and WS, joined here so they ride the read
        // governor and stay OUTSIDE `require_session`.
        .merge(net_sessions::public_net_session_routes())
        // Form-token issue: public, ungated, stateless.
        .merge(Router::new().route("/api/form-tokens", get(issue_form_token_handler)))
        // Read by every visitor on first paint, so it lives with the public
        // reads rather than behind a session.
        .merge(Router::new().route("/api/app-config", get(app_config_handler)))
        // An unauthenticated write on a public surface, hardened in-handler by
        // the bot-mitigation gate and an IP-keyed limiter.
        .merge(abuse_reports::public_abuse_report_routes())
}

/// Response of `GET /api/app-config`: the public instance settings.
///
/// Every field is `null` on a default instance, which is what the client reads
/// as "this integration is off". PUBLIC by construction — no secret may be
/// added here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppConfigBody {
    plausible_domain: Option<String>,
    plausible_script_host: Option<String>,
    kofi_username: Option<String>,
}

/// `GET /api/app-config` — reports which optional integrations this instance
/// opted into.
///
/// A RUNTIME read rather than build-time constants: the published image is
/// built once and run by self-hosters who cannot rebuild the SPA, so baking
/// `VITE_*` values in would make these operator-only settings.
async fn app_config_handler(State(state): State<AppState>) -> Json<AppConfigBody> {
    Json(AppConfigBody {
        plausible_domain: state.app_config.plausible_domain.clone(),
        plausible_script_host: state.app_config.plausible_script_host.clone(),
        kofi_username: state.app_config.kofi_username.clone(),
    })
}

/// Response of `GET /api/form-tokens`: the signed form token to attach on the
/// next submit, or `null` when bot mitigation is disabled on this instance.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FormTokenBody {
    form_token: Option<String>,
}

/// `GET /api/form-tokens` — issues a bot-mitigation form token.
///
/// Public, ungated, and stateless (no DB): the client fetches one when the
/// signup or net-creation form renders and attaches it on submit. When
/// mitigation is disabled the token is `null`, so the client transparently
/// sends nothing and the endpoints behave exactly as before. Carries no
/// user data.
async fn issue_form_token_handler(State(state): State<AppState>) -> Json<FormTokenBody> {
    let now = state.clock.now_epoch_millis();
    Json(FormTokenBody {
        form_token: state.bot_mitigation.issue_token(now),
    })
}

fn compose_router(
    public_auth: Router<AppState>,
    public_read: Router<AppState>,
    state: AppState,
) -> Router {
    // NOT consent-gated: gating status, consent or sign-out deadlocks the flow.
    let protected = Router::new()
        .route("/api/accounts/me", get(get_me).delete(delete_account))
        // NOT consent-gated: exporting your own data is an always-available
        // right, the same carve-out as delete.
        .route(
            "/api/accounts/me/export",
            get(account_export::export_account_data),
        )
        // The same carve-out as the export beside it. It belongs in this tree
        // and nowhere else: the same route under `public_read_routes()` would
        // compile, receive no `CurrentAccount`, and bypass authentication entirely.
        .route(
            "/api/accounts/me/check-ins",
            get(check_in_history::list_check_in_history),
        )
        .route("/api/accounts/me/callsign", put(set_callsign))
        .route("/api/accounts/me/profile", put(update_profile))
        // NOT consent-gated: a profile edit, not a net action.
        .merge(avatar::avatar_routes())
        .route(
            "/api/accounts/me/qrz-credentials",
            put(set_qrz_credentials).delete(clear_qrz_credentials),
        )
        .route("/api/accounts/me/email-change", post(request_email_change))
        .route("/api/sessions/current", delete(delete_current_session))
        .route("/api/consents", post(create_consent))
        // Consent and callsign are checked in-handler.
        .merge(net_definitions::net_definition_routes())
        // NOT public: it joins the protected tree, never the ungated set where
        // discovery lives.
        .merge(favorites::favorite_routes())
        // Owner-only in-handler, over the session's definition owner set.
        .merge(net_sessions::net_session_routes())
        // The upgrade GET rides the same session cookie, so it sits behind the
        // same gates as its sibling REST routes.
        .merge(crate::ws::ws_routes())
        // Session-gated so `CurrentAccount` is injected, then admin-gated
        // in-handler. NOT consent-gated: an admin must always be able to act.
        .merge(admin::admin_routes())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    // The merge order is load-bearing. `/by-token/{token}` is a distinct,
    // longer path than the protected `/{id}`, so a well-formed token request
    // never contends with it; the two bare "no token" paths DO collide with
    // `/{id}` where id = "by-token", and matchit's static-over-dynamic
    // preference is what keeps those in the public set instead of falling into
    // `require_session`. The read governor wraps these handlers without
    // re-registering any path, so the routing is undisturbed.
    public_auth
        .merge(public_read)
        .merge(protected)
        .merge(health::health_routes())
        .with_state(state)
}

/// `axum::Json` whose rejections (malformed body, missing fields, wrong
/// content type) keep the problem+json contract instead of axum's built-in
/// plain responses.
struct AppJson<T>(T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::from(rejection)),
        }
    }
}

// The query-key classification, as a compile-time fact.

/// The recorded reason a query type sits on the strict or the lenient side of
/// the query-key rule.
///
/// Implemented for every type an [`AppQuery`] or [`AppStrictQuery`] handler
/// argument reads, together with exactly one of [`StrictQuery`] or
/// [`LenientQuery`]. Both extractors bound their [`FromRequestParts`] impls on
/// the matching marker, so a query type read through either extractor that
/// carries no classification — or whose extractor disagrees with the one it
/// carries — does not compile, in any file and in whatever shape the handler
/// argument is written.
///
/// The qualifier is load-bearing and measured, not assumed: a handler that
/// reads the query string off the `Uri` itself is an unclassified query-reading
/// route and compiles clean. Nothing here reaches it. What covers it in
/// practice is `clippy.toml`, which denies the raw axum extractors so the
/// wrappers are the only path an author takes.
///
/// This replaces a text-parsing guard rather than adding to one. Reading Rust
/// as text has now been defeated four times by a shape its author had not
/// anticipated — two more here (a call site below an item-level `#[cfg(test)]`,
/// and a non-destructured argument) on top of two earlier ones — and no shape
/// can beat the compiler. The text register survives as a tripwire over the
/// call-site count and over whether `#[serde(deny_unknown_fields)]` agrees
/// with the extractor, never as the detector of the classification.
pub(crate) trait QueryKeyPolicy {
    /// Why this query type sits on the side it does.
    ///
    /// Never read at run time. A trait item rather than a doc comment so that
    /// writing the classification REQUIRES writing the reason: the next author
    /// should read a rule instead of guessing one.
    const REASON: &'static str;

    /// Compile-time proof that [`Self::REASON`] is not the empty string.
    ///
    /// A defaulted associated const, so no impl has to remember to write it —
    /// the hole a per-impl `const _` assertion would leave open. It is evaluated
    /// wherever it is NAMED, and [`assert_reason_recorded`] names it inside both
    /// extractors, so it covers exactly the classified types a route reads.
    ///
    /// Measured: an empty `REASON` on a real query type once compiled with zero
    /// errors and zero warnings, because the only non-emptiness assertion here
    /// reached the two probe types alone. Nothing reads `REASON` at run time, so
    /// without this the trait forced a syntactic act and not a reason.
    const NON_EMPTY_REASON: () = assert!(
        !Self::REASON.is_empty(),
        "a query type's `QueryKeyPolicy::REASON` is empty: the classification was written \
         without the reason the trait exists to record"
    );
}

/// Forces [`QueryKeyPolicy::NON_EMPTY_REASON`] to be evaluated for `T`.
///
/// Called from both extractors' `from_request_parts`, which is what makes the
/// assertion cover every classified type reached through either extractor rather
/// than only the types that happen to be named in a `const` block. The failure
/// is a const-evaluation error at the point `T` is monomorphised, i.e. at the
/// route registration that reads it.
fn assert_reason_recorded<T: QueryKeyPolicy>() {
    T::NON_EMPTY_REASON
}

/// A query type whose routes **refuse** an unrecognised query key with a 400
/// `application/problem+json` carrying `/errors/validation`.
///
/// The refusal itself comes from `#[serde(deny_unknown_fields)]` on the type;
/// this marker is what makes [`AppStrictQuery`] legal for it, and what makes
/// [`AppQuery`] illegal.
///
/// The failure is hard but the message is poor, and that is measured rather
/// than assumed. At a route registration the bound is reached through axum's
/// `Handler`, so rustc reports only an unsatisfied `Handler` bound at the
/// `.route(…)` line and the `on_unimplemented` note below does NOT surface. It
/// does surface wherever the bound is named directly. The note is carried
/// anyway, because the alternative is a hard compile failure with nothing to
/// read. `#[axum::debug_handler]` would name the failing argument, but it needs
/// axum's `macros` feature, which this crate does not enable.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not classified as a STRICT query type, so `AppStrictQuery` cannot read it",
    label = "unclassified, or classified lenient",
    note = "a read is FILTERED when a silently-dropped key changes the MEANING of the answer. Implement `QueryKeyPolicy` (with the reason in `REASON`) plus `StrictQuery` for a filtered read or `LenientQuery` for a public/unfiltered one, and give the type `#[serde(deny_unknown_fields)]` iff it is strict"
)]
pub(crate) trait StrictQuery: QueryKeyPolicy {}

/// A query type whose routes **ignore** an unrecognised query key, answering as
/// they would with the key absent.
///
/// This marker makes [`AppQuery`] legal for the type and [`AppStrictQuery`]
/// illegal. It also closes the [`admin::PageQuery`]-reuse hazard: `PageQuery` is
/// `pub(super)` with a doc comment inviting reuse and is [`StrictQuery`], so a
/// genuinely unfiltered paged read reusing it cannot quietly inherit a
/// strictness nobody chose — `AppQuery<PageQuery>` fails to compile.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not classified as a LENIENT query type, so `AppQuery` cannot read it",
    label = "unclassified, or classified strict",
    note = "a read is FILTERED when a silently-dropped key changes the MEANING of the answer. Implement `QueryKeyPolicy` (with the reason in `REASON`) plus `LenientQuery` for a public/unfiltered read or `StrictQuery` for a filtered one, and give the type `#[serde(deny_unknown_fields)]` iff it is strict"
)]
pub(crate) trait LenientQuery: QueryKeyPolicy {}

/// `axum::Query` whose rejections (a non-numeric `?limit=abc`, a repeated
/// single-valued key, a wrong-typed value) keep the problem+json contract
/// instead of axum's built-in plain-text responses.
///
/// Deliberately [`FromRequestParts`] and NOT [`FromRequest`] like [`AppJson`]:
/// a query string is read off the URI, so this extractor must not consume the
/// request body. A `FromRequest` version would compile against today's handlers
/// — every query-reading route is a body-less GET — and then break the first
/// time one of them grows a body, because a body-consuming extractor has to be
/// the last handler argument and excludes every sibling.
///
/// `pub(crate)` rather than private, unlike [`AppJson`], because
/// [`crate::ws`]'s two upgrade handlers also read `?since=` and are not
/// descendants of this module.
pub(crate) struct AppQuery<T>(pub(crate) T);

#[expect(clippy::disallowed_types)]
impl<S, T> FromRequestParts<S> for AppQuery<T>
where
    T: LenientQuery,
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        assert_reason_recorded::<T>();
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::from(rejection)),
        }
    }
}

/// [`AppQuery`] for a FILTERED read: an unrecognised query parameter is refused
/// rather than silently dropped.
///
/// A read is filtered when a silently-dropped key changes the MEANING of the
/// answer — the audit log, the abuse-report queue, admin search, check-in
/// history. Public unfiltered reads stay on [`AppQuery`].
///
/// The strictness lives in two places on purpose, and this type is the half
/// visible where the choice is made. `#[serde(deny_unknown_fields)]` on the
/// struct is what refuses the key; this extractor declares in the handler
/// signature that the route intends it. Otherwise strictness would be readable
/// only from a struct definition hundreds of lines away.
/// `tests/query_key_strictness.rs` reds if the attribute and the extractor
/// disagree in either direction.
pub(crate) struct AppStrictQuery<T>(pub(crate) T);

#[expect(clippy::disallowed_types)]
impl<S, T> FromRequestParts<S> for AppStrictQuery<T>
where
    T: StrictQuery,
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        assert_reason_recorded::<T>();
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(problem::strict_query_rejection(rejection)),
        }
    }
}

/// Both extractors MUST be [`FromRequestParts`], and whether the compiler
/// notices a `FromRequest` version depends on how it is written — for one of
/// them, on nothing more than how handler signatures happen to be ordered. Do
/// not plan against "the build will catch it".
///
/// A naive copy of [`AppJson`]'s shape is unsatisfiable, because `Query`
/// implements `FromRequest` only through axum's `ViaParts` blanket impl, so it
/// reds every route registration. A WELL-FORMED `FromRequest` impl compiles,
/// because a body-consuming extractor is legal, and reds only the registrations
/// where the extractor is not the last handler argument — so on the day every
/// query-reading handler happens to take it last, that branch goes silent and
/// the extractor quietly forbids any sibling body extractor from then on.
///
/// This assertion is the detector that does NOT depend on argument order: it
/// reds at the definition site, naming the trait, whatever the handlers look
/// like. For [`AppStrictQuery`] it has been measured to be the ONLY one.
const _: fn() = || {
    fn assert_from_request_parts<T: FromRequestParts<AppState>>() {}
    assert_from_request_parts::<AppQuery<LenientProbeQuery>>();
    assert_from_request_parts::<AppStrictQuery<StrictProbeQuery>>();
};

/// Stand-in query types for the assertion above, one per side.
///
/// A `HashMap<String, String>` cannot stand here: each extractor demands the
/// matching classification, and a classification for `HashMap` would be a lie.
/// Every real query type is private to its own module, so the assertion needs
/// its own pair. Neither probe reaches a route.
#[derive(Deserialize)]
struct LenientProbeQuery {}

impl QueryKeyPolicy for LenientProbeQuery {
    const REASON: &'static str =
        "not a route: the lenient half of the FromRequestParts assertion above";
}

impl LenientQuery for LenientProbeQuery {}

/// The strict half of the pair. It carries the attribute for the same reason
/// every real [`StrictQuery`] does: the marker declares the intent, the
/// attribute performs it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictProbeQuery {}

impl QueryKeyPolicy for StrictProbeQuery {
    const REASON: &'static str =
        "not a route: the strict half of the FromRequestParts assertion above";
}

impl StrictQuery for StrictProbeQuery {}

/// The two PROBE types' reasons, specifically: neither probe reaches a route,
/// so [`QueryKeyPolicy::NON_EMPTY_REASON`] is never monomorphised for them and
/// this is the only thing that checks them. The general statement lives on
/// `NON_EMPTY_REASON`, which the extractors name.
const _: () = {
    assert!(!LenientProbeQuery::REASON.is_empty());
    assert!(!StrictProbeQuery::REASON.is_empty());
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MagicLinkRequest {
    email: String,
    /// Bot-mitigation form token — the signed, timestamped token the
    /// client fetched from `GET /api/form-tokens`. Absent when mitigation is off.
    #[serde(default)]
    form_token: Option<String>,
    /// Honeypot field — a hidden input a real user leaves empty. Any
    /// non-empty value silently drops the request. Named `hp_field` rather than
    /// something like `website`/`url`/`email`, which some password managers and
    /// privacy/anti-fingerprinting extensions autofill into hidden inputs
    /// regardless of visibility because the name matches a known profile field.
    /// A neutral name has no such semantic target.
    #[serde(default)]
    hp_field: Option<String>,
}

#[derive(Deserialize)]
struct SessionRequest {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsentRequest {
    terms_version: String,
}

#[derive(Deserialize)]
struct CallsignRequest {
    callsign: String,
}

#[derive(Deserialize)]
struct EmailChangeRequest {
    email: String,
}

#[derive(Deserialize)]
struct ConfirmEmailChangeRequest {
    token: String,
}

/// PUT full-replace body: each field's submitted value is
/// parsed and stored; `null`, omitted, or empty-after-trim clears it.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ProfileRequest {
    display_name: Option<String>,
    location: Option<String>,
    grid: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountBody {
    id: Uuid,
    email: String,
    email_verified_at: Option<String>,
    consent_required: bool,
    required_terms_version: &'static str,
    callsign: Option<String>,
    display_name: Option<String>,
    location: Option<String>,
    grid: Option<String>,
    avatar_url: Option<String>,
    /// Always derived from the stored email; the client's effective avatar
    /// is `avatarUrl ?? gravatarUrl`.
    gravatar_url: String,
    /// Whether QRZ callbook credentials are stored for this account.
    /// Write-only surface: only this boolean is ever exposed — never the stored
    /// callsign/password. Additive to the account shape; every prior field is
    /// unchanged.
    qrz_credentials_set: bool,
    /// Whether this account is a platform admin — the boot `ADMIN_ACCOUNT_EMAILS`
    /// allowlist decision, surfaced so the SPA knows whether to offer the admin
    /// surface at all. A render hint only: the `AdminAccount` gate is what
    /// actually enforces it, so a forged `true` buys nothing.
    is_admin: bool,
}

/// Builds the wire account view, including consent status.
async fn account_body(state: &AppState, account: &Account) -> Result<AccountBody, ApiError> {
    let versions = state.consents.consented_versions(account.id).await?;
    let consent_required =
        consent_verdict(&versions, CURRENT_TERMS_VERSION) == ConsentVerdict::ConsentRequired;
    // Reading existence is not decryption — no KEK needed for this boolean.
    let qrz_credentials_set = state.qrz_credentials.is_set(account.id).await?;
    Ok(AccountBody {
        id: account.id,
        email: account.email.clone(),
        email_verified_at: account.email_verified_at_millis.map(rfc3339),
        consent_required,
        required_terms_version: CURRENT_TERMS_VERSION,
        callsign: account.callsign.clone(),
        display_name: account.display_name.clone(),
        location: account.location.clone(),
        grid: account.grid.clone(),
        avatar_url: account.avatar_url.clone(),
        gravatar_url: gravatar_url(&account.email),
        qrz_credentials_set,
        is_admin: netroll_domain::admin::is_admin(&account.email, &state.admin_allowlist),
    })
}

pub(crate) fn rfc3339(epoch_millis: u64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_millis as i64)
        .expect("stored timestamps are in chrono range")
        .to_rfc3339()
}

/// RFC 5321 forward-path limit; anything longer cannot be a real mailbox
/// and would otherwise flow unbounded into storage and the limiter map.
const MAX_EMAIL_LEN: usize = 254;

fn plausible_email(email: &str) -> bool {
    // Validated with the SAME parser the mail adapter delivers with, so an
    // accepted address can never resurface later as a mail-path failure.
    email.len() <= MAX_EMAIL_LEN && is_deliverable_address(email)
}

async fn create_magic_link(
    State(state): State<AppState>,
    AppJson(body): AppJson<MagicLinkRequest>,
) -> Result<StatusCode, ApiError> {
    let email = normalize_email(&body.email);
    if !plausible_email(&email) {
        return Err(ApiError::validation("email is not a deliverable address"));
    }

    let now = state.clock.now_epoch_millis();
    // Bot mitigation: a honeypot/timing failure yields the SAME
    // uniform 202 a legitimate request gets — issuing no token, sending no mail —
    // so a bot cannot distinguish rejection from success (and no new enumeration
    // oracle is introduced). A no-op when mitigation is disabled.
    if state
        .bot_mitigation
        .verify(body.form_token.as_deref(), body.hp_field.as_deref(), now)
        == BotVerdict::Bot
    {
        // Account-id-less: there is no PII to log, and the reason stays vague.
        tracing::info!("magic link request dropped by bot mitigation");
        return Ok(StatusCode::ACCEPTED);
    }

    // Checked BEFORE any token is issued or mail is sent.
    if let Err(retry_after_secs) = state.email_limiter.check(&email) {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    // Checked AFTER the per-address quota, so a single-address abuser still
    // gets an honest 429 rather than silently spending instance budget behind
    // a 202.
    //
    // TWO-TIER: every request draws the general pool first, and only once that
    // is spent may an address that recently signed in here fall back to the
    // smaller reserve. A campaign against unknown addresses therefore cannot
    // lock returning users out of the only authentication path this app has,
    // and total mail stays bounded by general + reserve. Letting a proven
    // address SKIP the bucket left the total unbounded once an attacker spent
    // one capped cell per catch-all address to establish exemptions.
    //
    // Membership is proof of control, NOT an account lookup: this handler never
    // asks whether the address has an account and its response is uniform
    // either way. Drawing the general pool first is what makes the two branches
    // indistinguishable in the unsaturated case, where `contains` is not
    // consulted at all. A denied `check()` spends nothing, so the fallback
    // cannot be starved by the general pool's refusal.
    let admitted = state.magic_link_aggregate_limiter.check().is_ok()
        || (state.known_addresses.contains(&email, now)
            && state.magic_link_reserve_limiter.check().is_ok());
    if !admitted {
        // Drops onto the same silent 202 the bot verdict uses: no token, no
        // mail, and nothing in the response to tell an attacker the cap fired
        // (a `Retry-After` here would itself be the announcement). Static
        // reason only — no address, no count.
        tracing::warn!("magic link aggregate send cap reached");
        return Ok(StatusCode::ACCEPTED);
    }

    let token = tokens::generate();
    state
        .magic_links
        .issue(&email, token.hash, now + MAGIC_LINK_TTL_MILLIS)
        .await?;
    let link = format!("{}/auth/verify?token={}", state.public_base_url, token.wire);
    if state.mailer.send_magic_link(&email, &link).await.is_err() {
        // Delivery failure keeps the uniform 202: a 5xx here would let a
        // relay's RCPT-time rejections probe which addresses exist. Static
        // message only — SMTP error text can echo the recipient.
        tracing::error!("magic link mail delivery failed");
    }
    // No email, no token in the event.
    tracing::info!("magic link issued");

    // Identical response whether or not the email has an account — no
    // enumeration oracle.
    Ok(StatusCode::ACCEPTED)
}

async fn create_session(
    State(state): State<AppState>,
    AppJson(body): AppJson<SessionRequest>,
) -> Result<Response, ApiError> {
    let Some(hash) = tokens::decode_to_hash(&body.token) else {
        return Err(ApiError::MagicLinkInvalid);
    };
    let now = state.clock.now_epoch_millis();

    let found = state.magic_links.find(hash).await?;
    match magic_link_verdict(found.as_ref(), now) {
        MagicLinkVerdict::Valid => {}
        MagicLinkVerdict::Expired => return Err(ApiError::MagicLinkExpired),
        MagicLinkVerdict::Consumed => return Err(ApiError::MagicLinkConsumed),
        MagicLinkVerdict::Invalid => return Err(ApiError::MagicLinkInvalid),
    }

    // One atomic UPDATE enforces single-use, judged against the same `now`
    // as the verdict above — the database clock never decides expiry.
    let Some(email) = state.magic_links.consume(hash, now).await? else {
        // A miss after a Valid verdict means another request won the race;
        // re-judge from storage so the refusal states the truthful reason.
        let lost = state.magic_links.find(hash).await?;
        return Err(match magic_link_verdict(lost.as_ref(), now) {
            MagicLinkVerdict::Expired => ApiError::MagicLinkExpired,
            MagicLinkVerdict::Invalid => ApiError::MagicLinkInvalid,
            MagicLinkVerdict::Consumed | MagicLinkVerdict::Valid => ApiError::MagicLinkConsumed,
        });
    };

    // The consumed link is the proof of email control: the domain
    // decision is gated on that proof, never an email-string match.
    let proof = EmailControlProof { email };
    let existing = state.accounts.find_by_email(&proof.email).await?;
    // Finalize-on-access: a soft-deleted account whose
    // grace window has elapsed but that the background sweep hasn't reached
    // is hard-deleted here and treated as absent, so `on_magic_link_consumed`
    // mints a FRESH account rather than resurrecting the past-window one —
    // making correctness independent of the finalizer's timing and freeing
    // the email. `Active` and `InGraceWindow` flow through VerifyAndAttach
    // unchanged, whose UPDATE now clears `deleted_at` (the undelete).
    let existing = match existing {
        Some(account)
            if deletion_verdict(account.deleted_at_millis, now, DELETION_GRACE_MILLIS)
                == DeletionVerdict::Finalizable =>
        {
            // `finalize_account` refuses to remove a `disabled_at`-set account,
            // because disabling must not hard-delete data even once an
            // independently pending self-deletion's grace window elapses. When
            // it reports no row removed, treat the account as the ordinary
            // EXISTING account below, never as freshly absent, so the
            // `disabled_at` check further down refuses it distinctly instead of
            // this login resurrecting it under the "create a fresh account" branch.
            // `_erasing` reports the row's avatar_url so the uploaded file is
            // deleted with it — blob storage has no cascade (see
            // `avatar_cleanup`).
            let erased = state.accounts.finalize_account_erasing(account.id).await?;
            if let Some(avatar_url) = erased.as_ref() {
                crate::avatar_cleanup::delete_stored_avatar(
                    state.avatar_store.as_ref(),
                    avatar_url.as_deref(),
                )
                .await;
            }
            let finalized = erased.is_some();
            if !finalized {
                Some(account)
            } else {
                // Finalizing here may have emptied a solely-owned net's owner set
                // (the account cascade) — archive it immediately rather than
                // waiting up to a finalizer tick. Cheap idempotent
                // UPDATE; the ≤60 s sweep is the backstop either way — so a
                // transient failure here is swallowed-and-logged, matching the
                // finalizer's own posture (finalizer.rs), rather than propagated:
                // the account is ALREADY finalized (irreversible) and the
                // single-use magic-link token is already consumed by this point,
                // so failing the whole login over best-effort archival housekeeping
                // would strand the user with a burned token and no session for no
                // corrective benefit — the next tick reconciles regardless.
                if let Err(_err) = state.net_definitions.archive_ownerless(now).await {
                    tracing::error!(
                        "ownerless-net archive-on-access sweep failed; the periodic finalizer tick will reconcile"
                    );
                }
                None
            }
        }
        other => other,
    };

    // A disabled account is refused BEFORE `VerifyAndAttach`'s
    // mutating UPDATE runs, not just after: that UPDATE
    // unconditionally clears `deleted_at` (the undelete) as a side effect,
    // so checking only afterward would let an ultimately-refused sign-in attempt
    // silently cancel a disabled account's own independent self-deletion grace
    // window. The magic-link token is already single-use-consumed either way
    // (no oracle gained by checking early), so refusing here changes nothing
    // observable except removing this unwanted mutation.
    if existing
        .as_ref()
        .is_some_and(|account| account.disabled_at_millis.is_some())
    {
        return Err(ApiError::AccountDisabled);
    }
    let account = match on_magic_link_consumed(&proof, existing.as_ref()) {
        ConsumptionOutcome::CreateAccountVerifyAndAttach { email } => {
            state
                .accounts
                .create_verified_and_attach(&email, now)
                .await?
        }
        ConsumptionOutcome::VerifyAndAttach { account_id } => {
            state.accounts.verify_and_attach(account_id, now).await?;
            state
                .accounts
                .find_by_id(account_id)
                .await?
                .ok_or(sqlx::Error::RowNotFound)?
        }
    };

    // A disabled account is distinctly refused at the
    // VERIFY/redemption step — NOT at the magic-link REQUEST step (which stays a
    // uniform 202, no enumeration oracle). Refused BEFORE any session row is
    // minted, so a disabled abuser cannot sign into a broken state; only an
    // admin `reenable` clears `disabled_at` (a self-sign-in never does).
    if account.disabled_at_millis.is_some() {
        return Err(ApiError::AccountDisabled);
    }

    // The ONLY writer of the proven-control set. Consuming the
    // single-use link above is what proves the requester controls the address —
    // seeding this from `create_magic_link` would hand an attacker the cap
    // exemption — but it sits BELOW both `AccountDisabled` refusals, not
    // immediately after `consume`: an operator who disables an
    // abuser must not keep having that abuser's every refused redemption refresh
    // its 30-day reserve access. Membership therefore means control was proven
    // and no refusal above fired; the steps BELOW can still fail the request
    // (`account_body` or the session insert returning 500) with the address
    // already remembered, so it is not quite "a session was minted". No security
    // consequence — control was proven and neither disabled check tripped. A pure
    // in-memory side effect: it cannot fail, and it changes no status, no error
    // path, and no verdict handling above or below.
    state.known_addresses.remember(&proof.email, now);

    // Built before the session is inserted: if this lookup fails, the
    // request must fail with no session row ever created — never an
    // orphaned session the client has no cookie to prove.
    let body = account_body(&state, &account).await?;

    let session = tokens::generate();
    state
        .sessions
        .insert(account.id, session.hash, now, now + SESSION_ABSOLUTE_MILLIS)
        .await?;
    tracing::info!(account_id = %account.id, "session established");
    // No target id (the session is the actor's own) and no email or token in
    // the row. Post-commit swallow-and-log: a failed append must never fail an
    // already-minted session.
    audit::append_audit(
        &state,
        account.id,
        AuditAction::SignedIn.as_str(),
        audit::AuditSubject::target("session", None),
        None,
        now,
    )
    .await;

    let mut response = (StatusCode::CREATED, Json(body)).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&session.wire)
            .parse()
            .expect("cookie value is base64url"),
    );
    Ok(response)
}

async fn get_me(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
) -> Result<Json<AccountBody>, ApiError> {
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    Ok(Json(account_body(&state, &account).await?))
}

/// `DELETE /api/accounts/me` — soft-deletes the signed-in account into the
/// grace/undelete window and revokes every session in one transaction.
///
/// Deliberately NOT consent-gated: deletion is a data right a user must be able
/// to exercise even while refusing the terms, and a consent-gated delete would
/// trap an unconsented user. Returns 204 and a cleared cookie.
///
/// Idempotent: both `Deleted` and `AlreadyPending` are a successful 204. A
/// sequential repeat is unreachable, since the first delete revokes this
/// session, but a truly concurrent race can still land `AlreadyPending` when
/// both auth checks pass before either revoke commits. Only `Deleted` appends
/// the audit row, so a racing repeat never mints a second one.
async fn delete_account(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
) -> Result<Response, ApiError> {
    let now = state.clock.now_epoch_millis();
    let outcome = match state.accounts.soft_delete(current.account_id, now).await {
        Ok(outcome) => outcome,
        // The account vanished mid-request — the `get_me`/`set_callsign`
        // "account no longer exists ⇒ 401" convention.
        Err(sqlx::Error::RowNotFound) => return Err(ApiError::Unauthenticated),
        Err(err) => return Err(err.into()),
    };
    // Account id ONLY — never the email.
    tracing::info!(account_id = %current.account_id, "account deletion requested");
    // Audited ONLY on `Deleted`: a concurrent racing repeat landing
    // `AlreadyPending` recorded the SAME decision a moment earlier and must not
    // mint a second row for it. The later timer-driven hard delete is a
    // mechanical consequence of this decision and has no acting account, so it
    // is not separately audited.
    if outcome == SoftDeleteOutcome::Deleted {
        audit::append_audit(
            &state,
            current.account_id,
            AuditAction::AccountSelfDeleted.as_str(),
            audit::AuditSubject::target("account", Some(current.account_id)),
            None,
            now,
        )
        .await;
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        clear_session_cookie()
            .parse()
            .expect("static cookie string"),
    );
    Ok(response)
}

async fn set_callsign(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppJson(body): AppJson<CallsignRequest>,
) -> Result<Json<AccountBody>, ApiError> {
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    // Defensive: every account is verified by construction today, but a future
    // auth method may not be, so refuse the claim rather than assume it.
    if account.email_verified_at_millis.is_none() {
        return Err(ApiError::EmailUnverified);
    }

    let parsed =
        parse_callsign(&body.callsign).map_err(|e| ApiError::CallsignInvalid(e.to_string()))?;

    let now = state.clock.now_epoch_millis();
    match state
        .accounts
        .set_callsign(account.id, parsed.as_str(), now)
        .await?
    {
        SetCallsignOutcome::Taken => return Err(ApiError::CallsignTaken),
        SetCallsignOutcome::Reserved => {}
    }
    tracing::info!(account_id = %account.id, callsign = %parsed, "callsign set");

    // A callsign is public radio data, so it is safe in a log line; the email
    // is the sensitive field, never this one.
    let updated = state
        .accounts
        .find_by_id(account.id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    Ok(Json(account_body(&state, &updated).await?))
}

/// Whole-request validation copy for a free-text field guarded by
/// [`netroll_domain::profile`]'s bounded-text parsers.
///
/// Shared by the profile PUT and by every roster-entry field guarded by those
/// parsers — name, location, note and signal report — because both surfaces
/// render the `detail` in a SECTION-level alert covering several fields at once
/// (`ProfilePage.tsx:932-937`, `LiveSessionPage.tsx:491`) — so the sentence has
/// to name the field itself; the reader has no highlighted input to look at.
///
/// Every caller MUST go through this (or another composer). `ProfileError`'s own
/// `Display` is a subjectless fragment: rendering it straight into `detail`
/// names no field and is neither register `problem.rs` codifies.
///
/// It exists because the typed [`ProfileError`] used to be discarded here
/// (`.map_err(|_| …)`, `fn name_error(_e: …)`) and both faults answered by
/// reciting BOTH rules. The domain knows which rule was broken; this says it.
pub(crate) fn profile_field_message(field: &str, error: ProfileError) -> String {
    match error {
        ProfileError::TooLong { max_chars } => {
            format!("The {field} is longer than {max_chars} characters — shorten it and try again.")
        }
        ProfileError::IllegalCharacter(offender) => format!(
            "The {field} contains {} — remove it and try again.",
            describe_illegal_character(offender)
        ),
        ProfileError::NotHttps => {
            format!("The {field} needs to start with https:// — correct it and try again.")
        }
        ProfileError::WhitespaceInUrl => {
            format!("The {field} contains a space — remove it and try again.")
        }
    }
}

/// Normalizes one submitted profile field: `None` or empty-after-trim
/// means "clear"; anything else must pass `parse` (fail fast on the first
/// error — PUT-replace semantics).
fn parse_profile_field<E>(
    submitted: Option<&str>,
    parse: impl Fn(&str) -> Result<String, E>,
) -> Result<Option<String>, E> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => parse(s).map(Some),
    }
}

/// `PUT /api/accounts/me/profile` — full-replace of the four profile
/// fields.
///
/// Consent-gated but, unlike `set_callsign`, deliberately NOT
/// email-verified-gated: the verified gate covers callsign, net creation and
/// self-check-in only, and profile fields are not squattable identity.
///
/// Gravatar privacy note, accepted and documented: `gravatarUrl` embeds a
/// SHA-256 of the account email, so rendering it sends that hash to
/// gravatar.com. Only the user's own profile page renders it today; putting
/// other people's avatars on a public page must re-confront the leak.
async fn update_profile(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppJson(body): AppJson<ProfileRequest>,
) -> Result<Json<AccountBody>, ApiError> {
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    let display_name = parse_profile_field(body.display_name.as_deref(), parse_display_name)
        .map_err(|e| ApiError::validation(profile_field_message("display name", e)))?;
    let location = parse_profile_field(body.location.as_deref(), parse_location)
        .map_err(|e| ApiError::validation(profile_field_message("location", e)))?;
    let grid = parse_profile_field(body.grid.as_deref(), |s| {
        parse_grid(s).map(|g| g.into_inner())
    })
    .map_err(|e| ApiError::GridInvalid(e.to_string()))?;
    let avatar_url = parse_profile_field(body.avatar_url.as_deref(), parse_avatar_url)
        .map_err(|e| ApiError::validation(profile_field_message("avatar URL", e)))?;

    let fields = ProfileFields {
        display_name,
        location,
        grid,
        avatar_url,
    };
    let now = state.clock.now_epoch_millis();
    state
        .accounts
        .update_profile(account.id, &fields, now)
        .await?;
    // Account id ONLY: location and grid are home-location PII, the opposite of
    // callsign's public radio data, and a display name adds nothing here.
    tracing::info!(account_id = %account.id, "profile updated");

    let updated = state
        .accounts
        .find_by_id(account.id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    Ok(Json(account_body(&state, &updated).await?))
}

#[derive(Deserialize)]
struct QrzCredentialsRequest {
    callsign: String,
    password: String,
}

/// `PUT /api/accounts/me/qrz-credentials` — seals and stores the signed-in
/// account's QRZ callbook credentials.
///
/// Write-only: the response carries no credential value, and no endpoint ever
/// returns the stored password (`GET /me` exposes only `qrzCredentialsSet`).
/// The domain validates BEFORE any crypto runs (422 on invalid); the
/// cipher then seals under a fresh per-record DEK wrapped by the instance KEK,
/// and the repo persists opaque bytes. With no KEK the seal fails closed and the
/// request is a 503 `/errors/crypto-unavailable`. The request body is
/// NEVER logged.
///
/// Consent-gated via `ConsentedAccount` — a credential write is a gated action,
/// the `set_callsign`/`update_profile` posture.
async fn set_qrz_credentials(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppJson(body): AppJson<QrzCredentialsRequest>,
) -> Result<StatusCode, ApiError> {
    // Validate first — nothing is sealed or stored on a rejected input.
    // The enum serves both fields and cannot name itself; the "<field>: " prefix
    // is composed here exactly as the composed domain enums compose theirs.
    let username = parse_qrz_username(&body.callsign)
        .map_err(|e| ApiError::QrzCredentialsInvalid(format!("QRZ callsign: {e}")))?;
    let password = parse_qrz_password(&body.password)
        .map_err(|e| ApiError::QrzCredentialsInvalid(format!("QRZ password: {e}")))?;
    let creds = QrzCredentials { username, password };

    let sealed = state
        .credential_cipher
        .seal(current.account_id, &creds)
        .map_err(|e| match e {
            // No KEK on this instance ⇒ fail closed, surfaced as 503.
            CipherError::KekUnavailable => ApiError::CryptoUnavailable,
            // A seal that fails for any other reason is an internal fault; its
            // detail is deliberately dropped (never a crypto oracle).
            CipherError::SealFailed | CipherError::OpenFailed => {
                ApiError::Internal("sealing QRZ credentials failed")
            }
        })?;
    state
        .qrz_credentials
        .set(current.account_id, &sealed)
        .await?;
    // Account id ONLY — never the credential values; no request-body log.
    tracing::info!(account_id = %current.account_id, "qrz credentials set");
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/accounts/me/qrz-credentials` — clears the stored QRZ
/// credentials. Idempotent: clearing a credential-less
/// account is a 204 no-op. Needs no KEK (deleting ciphertext is not decryption).
async fn clear_qrz_credentials(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
) -> Result<StatusCode, ApiError> {
    state.qrz_credentials.delete(current.account_id).await?;
    tracing::info!(account_id = %current.account_id, "qrz credentials cleared");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/accounts/me/email-change` — requests a change of the
/// identifying email to a new address, mailing a single-use confirmation
/// link there. The old email stays in effect until the
/// link is confirmed; nothing on `accounts` changes here.
///
/// Consent-gated via `ConsentedAccount` — an identity change is a gated
/// action (the profile-write posture).
async fn request_email_change(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppJson(body): AppJson<EmailChangeRequest>,
) -> Result<StatusCode, ApiError> {
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    let new_email = normalize_email(&body.email);
    if !plausible_email(&new_email) {
        return Err(ApiError::validation("email is not a deliverable address"));
    }
    // Same-address is a no-op the user should not be asked to confirm; both
    // sides are already normalized (accounts store normalized email).
    if new_email == account.email {
        return Err(ApiError::validation(
            "that is already the email on this account",
        ));
    }

    // The caller's OWN quota comes first: keyed on the
    // account, so it reveals nothing about `new_email` and can be an honest
    // 429, and placed before the two SHARED checks below so a throttled account
    // spends no per-address or instance-wide cell. Without it one consented
    // account could drain the instance budget by rotating targets.
    if let Err(retry_after_secs) = state.email_change_limiter.check(&account.id.to_string()) {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    // Checked BEFORE the taken-probe or any mail: the shared per-address
    // limiter caps the authenticated enumeration oracle too (rationale).
    if let Err(retry_after_secs) = state.email_limiter.check(&new_email) {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    // The instance-wide send cap, whose buckets this path shares. Checked
    // BEFORE the taken-probe so the decision cannot correlate with
    // `new_email`'s account status, and it must land here rather than after,
    // for the same reason the per-address check above does.
    //
    // Unlike `create_magic_link`, reserve access here is NOT gated on
    // `known_addresses.contains`. Gating on the TARGET's membership would make
    // admit-vs-capped correlate with whether `new_email` has signed in here, an
    // account-status leak, and `new_email` is not an address THIS caller has
    // proven control of, so membership would prove nothing about them. The
    // caller's live, unrevoked session is that proof, checked live rather than
    // through a 30-day set.
    let admitted = state.magic_link_aggregate_limiter.check().is_ok()
        || state.magic_link_reserve_limiter.check().is_ok();
    if !admitted {
        // Same silent-success shape `create_magic_link` uses: no token, no mail,
        // and the taken-probe below never runs, so a capped response cannot
        // leak `new_email`'s account status. Static reason only — no
        // address, no count.
        tracing::warn!("email change aggregate send cap reached");
        return Ok(StatusCode::ACCEPTED);
    }

    // Courteous request-time fast-fail; the `accounts.email` unique
    // constraint is the invariant, re-checked atomically at confirm.
    if state.accounts.find_by_email(&new_email).await?.is_some() {
        return Err(ApiError::EmailTaken);
    }

    let now = state.clock.now_epoch_millis();
    let token = tokens::generate();
    state
        .email_changes
        .issue(
            account.id,
            &new_email,
            token.hash,
            now + MAGIC_LINK_TTL_MILLIS,
        )
        .await?;
    let link = format!(
        "{}/auth/confirm-email-change?token={}",
        state.public_base_url, token.wire
    );
    if state
        .mailer
        .send_email_change(&new_email, &link)
        .await
        .is_err()
    {
        // Mail failure keeps the 202 (the sign-in posture): SMTP error text can
        // echo the recipient; the user's recovery is re-requesting.
        tracing::error!("email change mail delivery failed");
    }
    // Account id only — either address is PII in logs.
    tracing::info!(account_id = %account.id, "email change requested");

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/email-changes` — consumes a confirmation link, swapping the
/// identifying email, re-verifying it, and revoking every session for the
/// account in one transaction.
///
/// Public + token-scoped: the link opens from the NEW mailbox, possibly on a
/// device with no session — the token is the credential (as sign-in treats
/// its link). No cookie is set or cleared: the presented cookie (if any)
/// points at a now-revoked session, and the middleware's verdict handles it.
async fn confirm_email_change(
    State(state): State<AppState>,
    AppJson(body): AppJson<ConfirmEmailChangeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(hash) = tokens::decode_to_hash(&body.token) else {
        return Err(ApiError::EmailChangeInvalid);
    };
    let now = state.clock.now_epoch_millis();

    let found = state.email_changes.find(hash).await?;
    match email_change_verdict(found.as_ref(), now) {
        MagicLinkVerdict::Valid => {}
        MagicLinkVerdict::Expired => return Err(ApiError::EmailChangeExpired),
        MagicLinkVerdict::Consumed => return Err(ApiError::EmailChangeConsumed),
        MagicLinkVerdict::Invalid => return Err(ApiError::EmailChangeInvalid),
    }

    match state.email_changes.confirm(hash, now).await? {
        ConfirmEmailChangeOutcome::Changed {
            account_id,
            old_email,
            new_email,
        } => {
            // Fire-and-forget courtesy notice to the old address; delivery
            // failure logs a static line and never fails the request. The
            // old address comes from `confirm`'s own transaction (read
            // alongside the swap), never a separate pre-confirm query — a
            // sibling pending token for this account could otherwise be
            // confirmed in the gap and make a pre-fetched address stale.
            if state
                .mailer
                .send_email_change_notice(&old_email, &new_email)
                .await
                .is_err()
            {
                tracing::error!("email change notice delivery failed");
            }
            // Account id ONLY — neither address in the audit line.
            tracing::info!(account_id = %account_id, "identifying email changed");
            Ok(Json(serde_json::json!({ "email": new_email })))
        }
        ConfirmEmailChangeOutcome::EmailTaken => Err(ApiError::EmailTaken),
        ConfirmEmailChangeOutcome::NotConsumable => {
            // A miss after a Valid verdict means another request won the
            // race; re-judge from storage for the truthful refusal.
            let lost = state.email_changes.find(hash).await?;
            Err(match email_change_verdict(lost.as_ref(), now) {
                MagicLinkVerdict::Expired => ApiError::EmailChangeExpired,
                MagicLinkVerdict::Invalid => ApiError::EmailChangeInvalid,
                MagicLinkVerdict::Consumed | MagicLinkVerdict::Valid => {
                    ApiError::EmailChangeConsumed
                }
            })
        }
    }
}

async fn create_consent(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
    AppJson(body): AppJson<ConsentRequest>,
) -> Result<StatusCode, ApiError> {
    let now = state.clock.now_epoch_millis();
    // Only the version the server currently asks for may be recorded; a
    // stale gate page must not consent the user to superseded terms.
    let record = record_consent(&body.terms_version, CURRENT_TERMS_VERSION, now)
        .map_err(|_| ApiError::ConsentVersionMismatch)?;
    let inserted = state.consents.record(current.account_id, &record).await?;
    if inserted {
        // Account id + version only — no email/PII in fields. Only
        // the real acceptance is logged; a duplicate POST that hit the
        // idempotent no-op path must not multiply the audit trail.
        tracing::info!(
            account_id = %current.account_id,
            terms_version = %record.terms_version,
            "consent recorded"
        );
    }
    Ok(StatusCode::CREATED)
}

async fn delete_current_session(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
) -> Result<Response, ApiError> {
    // The audit row needs the injected clock: `sessions.revoke` uses the DB
    // clock, which must never decide an audit `occurred_at` (NFR — injected
    // clocks only). This handler did not previously fetch `now`.
    let now = state.clock.now_epoch_millis();
    let newly_revoked = state.sessions.revoke(current.token_hash).await?;
    tracing::info!(account_id = %current.account_id, "session revoked");
    // Post-commit swallow-and-log; the session is already revoked. No target
    // id, no PII. Audited ONLY when THIS request revoked the session: a
    // concurrent repeat, where both pass the not-yet-revoked check before
    // either commits, must not mint a second row for the same sign-out.
    if newly_revoked {
        audit::append_audit(
            &state,
            current.account_id,
            AuditAction::SignedOut.as_str(),
            audit::AuditSubject::target("session", None),
            None,
            now,
        )
        .await;
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        clear_session_cookie()
            .parse()
            .expect("static cookie string"),
    );
    Ok(response)
}

fn session_cookie(wire_token: &str) -> String {
    // Max-Age mirrors the 30-day absolute cap; the Postgres row remains the
    // authority (idle expiry and revocation are server-side).
    format!(
        "{SESSION_COOKIE}={wire_token}; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        SESSION_ABSOLUTE_MILLIS / 1000
    )
}

fn clear_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}
