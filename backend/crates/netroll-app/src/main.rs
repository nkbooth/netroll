// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The NetRoll server binary: reads configuration, wires the adapters into
//! the application and serves HTTP.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use netroll_adapters::avatar::FsAvatarStore;
use netroll_adapters::crypto::EnvelopeCipher;
use netroll_adapters::mail::SmtpMailer;
use netroll_app::{config, csp, http, r#static};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    // Auth events flow through tracing; secrets and PII
    // (tokens, emails) must never appear in log fields.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Display, not Debug: boot failures are operator-facing diagnostics.
            tracing::error!("{err}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let port = config::resolve_port(std::env::var("PORT").ok())?;
    let dist_dir = config::resolve_static_dir(std::env::var("STATIC_DIR").ok())?;
    config::validate_static_dir(&dist_dir)?;
    let database_url = config::resolve_database_url(std::env::var("DATABASE_URL").ok())?;
    let smtp = config::resolve_smtp(
        std::env::var("SMTP_HOST").ok(),
        std::env::var("SMTP_PORT").ok(),
        std::env::var("SMTP_USERNAME").ok(),
        std::env::var("SMTP_PASSWORD").ok(),
        std::env::var("MAIL_FROM").ok(),
    )?;
    let public_base_url = config::resolve_public_base_url(std::env::var("PUBLIC_BASE_URL").ok())?;
    // KEK is OPTIONAL at boot: absent ⇒ the app still serves every
    // non-QRZ feature with a fail-closed cipher; a present-but-malformed KEK is
    // a hard boot error (`?`), never a silent fallback.
    let kek = config::resolve_kek(std::env::var("KEK").ok())?;
    // Resource caps: unset ⇒ documented defaults (7 / 5); a
    // set-but-invalid value is a hard boot error (`?`), never a silent fallback.
    let max_nets_per_user =
        config::resolve_max_nets_per_user(std::env::var("MAX_NETS_PER_USER").ok())?;
    let max_owners_per_net =
        config::resolve_max_owners_per_net(std::env::var("MAX_OWNERS_PER_NET").ok())?;
    // A cap raised at or above the net-creation rate limiter's burst would
    // silently reintroduce the under-throttling bug this code caught and
    // fixed once — warn loudly
    // rather than leave an operator to discover it via user complaints.
    if config::cap_may_be_throttled_by_rate_limiter(
        max_nets_per_user,
        netroll_app::http::rate_limit::NET_CREATION_BURST,
    ) {
        tracing::warn!(
            max_nets_per_user,
            net_creation_burst = netroll_app::http::rate_limit::NET_CREATION_BURST,
            "MAX_NETS_PER_USER is at or above the net-creation rate-limiter burst — \
             a user filling their full quota in one sitting may be throttled \
             before ever reaching the cap"
        );
    }
    // Instance-wide magic-link send cap: unset ⇒ the
    // documented 60/hour default; a set-but-invalid value is a hard boot error
    // (`?`). Bounds total outbound magic-link mail across ALL addresses, which
    // the per-address quota structurally cannot — and magic-link delivery is
    // this app's only authentication path, so an unbounded relay is an
    // availability risk to sign-in itself, not just an abuse-of-strangers one.
    let magic_link_aggregate_sends_per_hour = config::resolve_magic_link_aggregate_sends_per_hour(
        std::env::var("MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR").ok(),
    )?;
    // Second-tier reserve: drawn on only once the general pool above is spent,
    // and only by addresses that have proven control here. It is what keeps
    // returning users signing in during a campaign — and because it is a BUDGET
    // rather than an exemption bypass, total outbound magic-link mail stays
    // bounded at general + reserve. Read that bound as burst PLUS sustained: each
    // pool's burst equals its per-hour number and a fresh bucket starts full, so
    // the defaults admit up to 360 in the first hour after boot (120 + 240) and
    // 180/hour sustained. Every boot resets both buckets — size relay reputation
    // against 360. See `config::DEFAULT_MAGIC_LINK_RESERVE_SENDS_PER_HOUR`.
    let magic_link_reserve_sends_per_hour = config::resolve_magic_link_reserve_sends_per_hour(
        std::env::var("MAGIC_LINK_RESERVE_SENDS_PER_HOUR").ok(),
    )?;
    // Retention window for expired tokens and dead sessions:
    // unset ⇒ the documented 30-day default; a set-but-invalid value is a hard
    // boot error (`?`). One window covers all three credential-artifact tables.
    let token_retention_days =
        config::resolve_token_retention_days(std::env::var("TOKEN_RETENTION_DAYS").ok())?;
    // Bot mitigation: the secret's presence is the on/off switch —
    // absent ⇒ disabled (signup + net creation behave exactly as before). A
    // present-but-weak secret is a hard boot error (`?`), never a silent
    // accept-anything.
    let bot_mitigation =
        match config::resolve_bot_mitigation_secret(std::env::var("BOT_MITIGATION_SECRET").ok())? {
            Some(secret) => netroll_app::bot_mitigation::BotMitigation::with_secret(secret),
            None => netroll_app::bot_mitigation::BotMitigation::disabled(),
        };
    // Platform-admin allowlist: unset/empty ⇒ no admins configured
    // (the app still boots, the admin surface simply unreachable — the optional
    // config posture). A set-but-malformed value is a hard boot error (`?`).
    let admin_allowlist =
        config::resolve_admin_allowlist(std::env::var("ADMIN_ACCOUNT_EMAILS").ok())?;
    // Uploaded avatars live on a plain volume (no infrastructure operated
    // by N1CCK). The directory is created and proved writable HERE so a
    // misconfigured mount fails at boot rather than on a user's first upload.
    let avatar_dir = config::resolve_avatar_dir(std::env::var("AVATAR_DIR").ok())?;
    config::prepare_avatar_dir(&avatar_dir)?;
    // Public, operator-opt-in integrations. Absent ⇒ off: no analytics script is
    // served and no donation link renders, which is the default for every
    // self-hoster. The script host alone is validated first (`?`): it also
    // shapes the Content-Security-Policy below, so a malformed one is a hard
    // boot error rather than merely the operator's own broken analytics.
    let plausible_script_host =
        config::resolve_plausible_script_host(std::env::var("PLAUSIBLE_SCRIPT_HOST").ok())?;
    let app_config = config::resolve_app_config(
        std::env::var("PLAUSIBLE_DOMAIN").ok(),
        plausible_script_host,
        std::env::var("KOFI_USERNAME").ok(),
    );
    // The Content-Security-Policy is the app's own, not the proxy's: derived
    // from the two settings above and `PUBLIC_BASE_URL` unless the operator
    // replaces the whole policy with `CSP_POLICY`. Both origins were already
    // refused at their own resolvers unless they reduce to a CSP host-source,
    // so the derivation can never interpolate a fragment that is not one.
    // Set-but-invalid is a hard boot error (`?`) for both CSP variables, never
    // a silent fallback. It goes on HTML responses ONLY — API clients ignore a
    // CSP and a JSON body has nothing for one to govern, while the docs under
    // /docs are HTML from the same static dir and depend on `script-src 'self'`
    // being enforced there.
    let csp_policy = config::resolve_csp_policy(std::env::var("CSP_POLICY").ok())?
        .unwrap_or_else(|| csp::default_policy(&public_base_url, &app_config));
    let csp_report_only = config::resolve_csp_report_only(std::env::var("CSP_REPORT_ONLY").ok())?;
    let csp_header = csp::CspHeader::new(&csp_policy, csp_report_only)?;

    // The single self-host binary migrates itself at boot; a failed
    // migration aborts startup rather than serving against a stale schema.
    let pool = PgPoolOptions::new().connect(&database_url).await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    let mailer = Arc::new(SmtpMailer::new(&smtp)?);
    let credential_cipher = Arc::new(EnvelopeCipher::new(kek));
    let state = http::AppState::new(pool, mailer, public_base_url)
        .with_credential_cipher(credential_cipher)
        .with_resource_caps(max_nets_per_user, max_owners_per_net)
        .with_magic_link_aggregate_cap(magic_link_aggregate_sends_per_hour)
        .with_magic_link_reserve_cap(magic_link_reserve_sends_per_hour)
        .with_bot_mitigation(bot_mitigation)
        .with_admin_allowlist(admin_allowlist)
        .with_avatar_store(Arc::new(FsAvatarStore::new(avatar_dir.clone())))
        .with_app_config(app_config);

    // Finalizes accounts whose undelete window has elapsed.
    netroll_app::finalizer::spawn_deletion_finalizer(
        state.accounts.clone(),
        state.net_definitions.clone(),
        state.clock.clone(),
        state.avatar_store.clone(),
    );

    // Materializes recurring net occurrences into a rolling horizon.
    netroll_app::occurrence_spawner::spawn_occurrence_materializer(
        state.schedules.clone(),
        state.clock.clone(),
    );

    // Detects NCS stalls, presence-driven resume, and auto-closes abandoned nets
    // after 15 min. The same shared on-close deliverer both close paths use — the
    // manual `POST /close` handler builds it from `AppState`, and the presence
    // monitor's auto-close path is handed this one so an abandoned net still
    // delivers its summary. Fire-and-forget inside each; never awaited.
    netroll_app::presence_monitor::spawn_session_presence_monitor(
        state.net_sessions.clone(),
        state.hub.clone(),
        state.session_presence.clone(),
        state.clock.clone(),
        state.delivery_service(),
    );

    // Removes expired tokens and dead sessions from the three credential-artifact
    // tables. The in-process interval loop is the STANDING mechanism for
    // single-instance recurring maintenance, not a placeholder for a general
    // job runner.
    netroll_app::retention::spawn_retention_pruner(
        state.magic_links.clone(),
        state.sessions.clone(),
        state.email_changes.clone(),
        state.clock.clone(),
        // Cannot overflow: `resolve_token_retention_days` bounds the days so this
        // conversion is total, because release builds run with overflow-checks
        // off and a wrap here would be a silent fallback.
        token_retention_days * config::MILLIS_PER_DAY,
    );

    // Settle every delivery attempt the PREVIOUS process left in
    // flight BEFORE serving. At boot no attempt of this process can be running,
    // so every `pending` leg still carrying a lease is by definition an
    // interrupted one — email and webhook legs are released to retry, an
    // interrupted Discord POST is recorded and never re-posted. Awaited, with
    // `?`: a database that cannot answer this cannot serve either.
    netroll_app::delivery_sweeper::recover_with_scope(
        &state.delivery_jobs,
        state.clock.now_epoch_millis(),
        netroll_app::delivery_sweeper::RecoveryScope::EveryClaim,
    )
    .await?;

    // Pays off the durable on-close delivery legs `NetSessionRepo::close` records
    // in its own transaction — the one job whose loss is user-visible, which is
    // why it alone has a durable queue. The durability is in the table, not in
    // the loop.
    netroll_app::delivery_sweeper::spawn_delivery_sweeper(
        state.delivery_jobs.clone(),
        state.delivery_service(),
        state.clock.clone(),
    );

    let app = http::api_router_ip_limited(state)
        .merge(r#static::avatar_router(&avatar_dir))
        .merge(r#static::spa_router(&dist_dir, &csp_header));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        "netroll-app serving {} on http://{}",
        dist_dir.display(),
        listener.local_addr()?
    );
    // connect_info feeds the IP-keyed limiter's peer extractor.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
