// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the personal-data export: real router, real Postgres.
//! Proves the export composes the caller's OWN profile, callsign, favorites,
//! owned-net delivery configs and self check-ins, isolates other accounts' data,
//! exposes the two write-only secrets ONLY as booleans (asserting the raw body
//! contains neither), and 401s when unauthenticated.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::admin::DEFAULT_PAGE_LIMIT;
use netroll_domain::callsign::parse_callsign;
use netroll_domain::check_in::{
    CheckInSource, StayingStatus, parse_location, parse_name, parse_signal_report,
};
use netroll_domain::model::account::ProfileFields;
use netroll_domain::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet};
use netroll_domain::net::delivery::DeliveryConfigFields;
use netroll_domain::net::enums::{Band, Mode};
use netroll_domain::net::validation::{
    NetDefinitionFields, RawNetDefinition, parse_net_definition_fields,
};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

/// The webhook secret seeded onto the owned net — the export must never leak it.
const WEBHOOK_SECRET: &str = "SUPER-SECRET-WEBHOOK-KEY-9f3a";
/// A marker embedded in the seeded QRZ ciphertext — the export must never leak it.
const QRZ_CIPHERTEXT_MARKER: &str = "QRZ-CIPHERTEXT-DO-NOT-LEAK";
/// The bearer token half of the seeded Discord webhook URL — the export must
/// never leak it (the URL IS the credential).
const DISCORD_WEBHOOK_TOKEN: &str = "DISCORD-TOKEN-DO-NOT-LEAK";
/// The seeded Discord channel-webhook URL.
const DISCORD_WEBHOOK_URL: &str =
    "https://discord.com/api/webhooks/1234567890/DISCORD-TOKEN-DO-NOT-LEAK";

#[derive(Default)]
struct CapturingMailer {
    magic_links: Mutex<Vec<String>>,
}

impl Mailer for CapturingMailer {
    fn send_magic_link<'a>(
        &'a self,
        _to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.magic_links.lock().expect("lock").push(link.to_owned());
            Ok(())
        })
    }
    fn send_email_change<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move { Ok(()) })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        _to: &'a str,
        _new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move { Ok(()) })
    }
}

impl CapturingMailer {
    fn last_link(&self) -> String {
        self.magic_links
            .lock()
            .expect("lock")
            .last()
            .expect("a magic link was sent")
            .clone()
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    pool: PgPool,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

fn now_millis() -> u64 {
    1_800_000_000_000
}

async fn test_app() -> TestApp {
    let container = Postgres::default().start().await.expect("start postgres");
    let host = container.get_host().await.expect("host");
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");

    let mailer = Arc::new(CapturingMailer::default());
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into());
    TestApp {
        _container: container,
        state,
        mailer,
        pool,
    }
}

/// Signs in `email` (magic-link → session), returning the session cookie and the
/// account body. Export is not consent-gated, so no consent step is needed.
async fn sign_in(app: &TestApp, email: &str) -> (String, Value) {
    let (status, _, _) = send_raw(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let token = app
        .mailer
        .last_link()
        .split_once("token=")
        .expect("token")
        .1
        .to_owned();
    let (status, headers, account) = send_raw(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = headers
        .get(header::SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("ascii")
        .split(';')
        .next()
        .expect("pair")
        .to_owned();
    (cookie, account)
}

async fn send_raw(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, bytes) = send_bytes(router, method, uri, body, cookie).await;
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, headers, json)
}

async fn send_bytes(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = builder
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .expect("build request");
    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body")
        .to_vec();
    (status, headers, bytes)
}

/// The one connection every fixture net is born with: a
/// definition carries no connection fact of its own, so `create` takes the set
/// as an argument.
fn sample_connections() -> NetConnectionSet {
    NetConnectionSet::new(vec![NetConnection {
        id: Uuid::now_v7(),
        position: 0,
        kind: NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        },
    }])
    .expect("one connection is a valid set")
}

fn sample_fields(title: &str, token_grid: &str) -> NetDefinitionFields {
    parse_net_definition_fields(RawNetDefinition {
        title: Some(title.to_owned()),
        description: Some("Weekly NTS".to_owned()),
        country: Some("USA".to_owned()),
        state: Some("CT".to_owned()),
        grid: Some(token_grid.to_owned()),
        net_category: Some("traffic".to_owned()),
        net_type: Some("open".to_owned()),
        expected_duration: Some("90".to_owned()),
        visibility: None,
    })
    .expect("valid fields")
}

/// Seeds a QRZ credential row directly (no KEK needed — the export only reads
/// the existence boolean). The ciphertext carries a recognizable marker so the
/// test can prove it never appears in the export body.
async fn seed_qrz(pool: &PgPool, account_id: Uuid) {
    sqlx::query(
        "INSERT INTO qrz_credentials
             (account_id, wrapped_dek, dek_nonce, credential_ciphertext, credential_nonce)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(account_id)
    .bind(&b"wrapped-dek-bytes"[..])
    .bind(&b"dek-nonce-12"[..])
    .bind(QRZ_CIPHERTEXT_MARKER.as_bytes())
    .bind(&b"cred-nonce-12"[..])
    .execute(pool)
    .await
    .expect("seed qrz credentials");
}

#[tokio::test]
async fn export_composes_the_callers_own_data_and_isolates_other_accounts() {
    let app = test_app().await;
    let now = now_millis();
    let (cookie, account) = sign_in(&app, "owner-export@example.com").await;
    let account_id: Uuid = account["id"].as_str().expect("id").parse().expect("uuid");

    // Callsign + profile.
    app.state
        .accounts
        .set_callsign(account_id, "W1AW", now)
        .await
        .expect("set callsign");
    app.state
        .accounts
        .update_profile(
            account_id,
            &ProfileFields {
                display_name: Some("Hiram Percy".to_owned()),
                location: Some("Hartford, CT".to_owned()),
                grid: Some("FN31pr".to_owned()),
                avatar_url: None,
            },
            now,
        )
        .await
        .expect("update profile");

    // QRZ credentials set (existence only).
    seed_qrz(&app.pool, account_id).await;

    // An owned net WITH a delivery config (emails + webhook secret).
    let owned_net = app
        .state
        .net_definitions
        .create(
            &sample_fields("My Owned Net", "fn31pr"),
            &sample_connections(),
            account_id,
            "tok-owned",
            now,
        )
        .await
        .expect("create owned net");
    app.state
        .delivery_configs
        .set(
            owned_net.id,
            &DeliveryConfigFields {
                emails: vec!["dispatch@example.com".to_owned()],
                webhook_url: Some("https://hooks.example.com/net".to_owned()),
                discord_webhook_url: None,
            },
            Some(WEBHOOK_SECRET),
            now,
        )
        .await
        .expect("set delivery config");

    // A favorite of a net owned by SOMEONE ELSE (so it is a favorite, not an
    // owned net, for this account).
    let stranger = app
        .state
        .accounts
        .create_verified_and_attach("stranger-export@example.com", now)
        .await
        .expect("seed stranger")
        .id;
    let favorited_net = app
        .state
        .net_definitions
        .create(
            &sample_fields("Someone Elses Net", "em29"),
            &sample_connections(),
            stranger,
            "tok-fav",
            now,
        )
        .await
        .expect("create favorited net");
    app.state
        .favorites
        .add(account_id, favorited_net.id, now)
        .await
        .expect("favorite");

    // A self check-in by the account into a net it does NOT own (the ordinary
    // participant case) — so the account still owns exactly one net.
    let ci_net = app
        .state
        .net_definitions
        .create(
            &sample_fields("Checkin Net", "dm79"),
            &sample_connections(),
            stranger,
            "tok-ci",
            now,
        )
        .await
        .expect("create ci net");
    let session = match app
        .state
        .net_sessions
        .start(&ci_net, Some(account_id), now)
        .await
        .expect("start session")
    {
        netroll_adapters::pg::net_sessions::StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let own_call = parse_callsign("W1AW").expect("valid");
    let report = parse_signal_report("599").expect("valid").expect("some");
    let name = parse_name("Hiram").expect("valid").expect("some");
    let location = parse_location("Hartford, CT")
        .expect("valid")
        .expect("some");
    app.state
        .net_sessions
        .add_check_in(
            session.id,
            &own_call,
            Uuid::now_v7(),
            None,
            Some(&report),
            StayingStatus::StayingForComments,
            Some(&name),
            Some(&location),
            None,
            CheckInSource::SelfService,
            // WHICH way in the account came in on. Free text rather
            // than a connection id so the assertion does not depend on this
            // fixture's snapshot ids — the export carries the LABEL either way,
            // because a snapshot-local UUID is not a fact the account holder can
            // use.
            Some(&netroll_domain::net::connection::Via::Unlisted(
                "Bill's phone patch".to_owned(),
            )),
            None,
            Some(account_id),
            now,
        )
        .await
        .expect("self check-in");

    // A SECOND account with its own favorite + self check-in — must never appear
    // in the first account's export.
    let other_net = app
        .state
        .net_definitions
        .create(
            &sample_fields("Other Persons Net", "jn58"),
            &sample_connections(),
            stranger,
            "tok-other",
            now,
        )
        .await
        .expect("create other net");
    app.state
        .favorites
        .add(stranger, other_net.id, now)
        .await
        .expect("stranger favorite");
    let other_session = match app
        .state
        .net_sessions
        .start(&other_net, Some(stranger), now)
        .await
        .expect("start other session")
    {
        netroll_adapters::pg::net_sessions::StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let stranger_call = parse_callsign("K9XYZ").expect("valid");
    app.state
        .net_sessions
        .add_check_in(
            other_session.id,
            &stranger_call,
            Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(stranger),
            now,
        )
        .await
        .expect("stranger self check-in");

    // The export.
    let (status, headers, bytes) = send_bytes(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_DISPOSITION)
            .expect("content-disposition")
            .to_str()
            .expect("ascii"),
        "attachment; filename=\"netroll-data-export.json\""
    );
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .expect("content-type")
            .to_str()
            .expect("ascii")
            .starts_with("application/json"),
        "the export must be served as JSON"
    );
    let raw = String::from_utf8(bytes.clone()).expect("utf8 body");
    let body: Value = serde_json::from_slice(&bytes).expect("json body");

    // Account block.
    assert_eq!(body["account"]["callsign"], "W1AW");
    assert_eq!(body["account"]["displayName"], "Hiram Percy");
    assert_eq!(body["account"]["location"], "Hartford, CT");
    assert_eq!(body["account"]["grid"], "FN31pr");
    assert_eq!(body["account"]["qrzCredentialsConfigured"], true);

    // QRZ: the boolean is the ONLY qrz signal; no ciphertext/password leaks.
    assert!(
        !raw.contains(QRZ_CIPHERTEXT_MARKER),
        "the QRZ ciphertext must never appear in the export"
    );
    assert!(
        !raw.to_lowercase().contains("ciphertext") && !raw.to_lowercase().contains("password"),
        "no qrz ciphertext/password key may appear (got {raw})"
    );

    // Favorites: exactly the favorited (stranger-owned) net.
    let favorites = body["favorites"].as_array().expect("favorites array");
    assert_eq!(favorites.len(), 1, "exactly one favorite");
    assert_eq!(favorites[0]["id"], favorited_net.id.to_string());
    assert_eq!(favorites[0]["title"], "Someone Elses Net");

    // Owned net delivery configs: exactly the owned net, with emails + boolean.
    let owned = body["ownedNetDeliveryConfigs"]
        .as_array()
        .expect("owned array");
    assert_eq!(owned.len(), 1, "exactly one owned net delivery config");
    assert_eq!(owned[0]["netDefinitionId"], owned_net.id.to_string());
    assert_eq!(owned[0]["netTitle"], "My Owned Net");
    assert_eq!(owned[0]["deliveryEmails"][0], "dispatch@example.com");
    assert_eq!(owned[0]["webhookConfigured"], true);
    // The two booleans are INDEPENDENT and derived from different fields. This
    // net has a generic webhook (and thus a minted secret) and NO Discord
    // destination, so a `discordConfigured` copied from the neighbouring
    // `webhook_secret_set` expression would report `true` here.
    assert_eq!(
        owned[0]["discordConfigured"], false,
        "no Discord destination is configured on this net"
    );

    // The webhook secret NEVER appears, nor any webhookSecret key.
    assert!(
        !raw.contains(WEBHOOK_SECRET),
        "the webhook secret must never appear in the export"
    );
    assert!(
        !raw.to_lowercase().contains("webhooksecret") && !raw.contains("webhook_secret"),
        "no webhookSecret key may appear in the export"
    );

    // Self check-ins: exactly the one, with net title + callsign.
    let checkins = body["selfCheckIns"].as_array().expect("checkins array");
    assert_eq!(checkins.len(), 1, "exactly one self check-in");
    assert_eq!(checkins[0]["netSessionId"], session.id.to_string());
    assert_eq!(checkins[0]["netTitle"], "Checkin Net");
    assert_eq!(checkins[0]["callsign"], "W1AW");
    assert_eq!(checkins[0]["signalReport"], "599");
    // The personal-data export carries `via`. It is the
    // surface that already tells the truth about check-ins the profile widget
    // cannot render, and a way in omitted here would be personal data the
    // account holder has no copy of. The LABEL, never the id.
    assert_eq!(checkins[0]["via"], "Bill's phone patch");
    assert!(checkins[0]["checkedInAt"].is_string());

    // Isolation: the second account's net titles / callsign never appear.
    assert!(
        !raw.contains("Other Persons Net"),
        "another account's favorited net must not appear"
    );
    assert!(
        !raw.contains("K9XYZ"),
        "another account's self check-in must not appear"
    );
}

#[tokio::test]
async fn the_export_labels_each_check_in_with_the_frequency_it_was_worked_on() {
    // On the personal-data export (`self_check_ins`). The
    // fixture QSYs BETWEEN two self check-ins on the same connection; a
    // never-moving one cannot tell the frozen, live and correct reads apart.
    use netroll_adapters::pg::net_sessions::{AddCheckInOutcome, ChangeFrequencyOutcome};
    use netroll_domain::net::connection::Via;

    let app = test_app().await;
    let now = now_millis();
    let (cookie, account) = sign_in(&app, "mover-export@example.com").await;
    let account_id: Uuid = account["id"].as_str().expect("id").parse().expect("uuid");
    app.state
        .accounts
        .set_callsign(account_id, "W1AW", now)
        .await
        .expect("set callsign");
    let net = app
        .state
        .net_definitions
        .create(
            &sample_fields("Moving Net", "fn31pr"),
            &sample_connections(),
            account_id,
            "tok-move",
            now,
        )
        .await
        .expect("create net");
    let session = match app
        .state
        .net_sessions
        .start(&net, Some(account_id), now)
        .await
        .expect("start session")
    {
        netroll_adapters::pg::net_sessions::StartOutcome::Started(row) => row,
        other => panic!("expected Started, got {other:?}"),
    };
    let hf = session.definition_snapshot.connections[0].id;
    let session_id = session.id;
    let own_call = parse_callsign("W1AW").expect("valid");

    let app_ref = &app;
    let own_call_ref = &own_call;
    let check_in_over_hf = |at: u64| async move {
        let outcome = app_ref
            .state
            .net_sessions
            .add_check_in(
                session_id,
                own_call_ref,
                Uuid::now_v7(),
                None,
                None,
                StayingStatus::InAndOut,
                None,
                None,
                None,
                CheckInSource::SelfService,
                Some(&Via::Connection(hf)),
                None,
                Some(account_id),
                at,
            )
            .await
            .expect("self check-in");
        assert!(matches!(outcome, AddCheckInOutcome::Added(_)));
    };
    check_in_over_hf(now + 1_000).await;
    let moved = app
        .state
        .net_sessions
        .change_frequency(session_id, hf, 14_250_000, Some(account_id), now + 1_500)
        .await
        .expect("change frequency");
    assert!(
        matches!(moved, ChangeFrequencyOutcome::Changed(_)),
        "{moved:?}"
    );
    check_in_over_hf(now + 2_000).await;

    let (status, _headers, bytes) = send_bytes(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&bytes).expect("json body");
    let checkins = body["selfCheckIns"].as_array().expect("checkins array");
    assert_eq!(
        checkins.len(),
        2,
        "both self check-ins are the account's own"
    );

    // Oldest first (the export's order): the pre-move check-in leads.
    let before_move = &checkins[0];
    let after_move = &checkins[1];
    assert_ne!(
        before_move["via"], after_move["via"],
        "two check-ins either side of a QSY must not read the same frequency"
    );
    assert_eq!(before_move["via"], "HF — 14.230 MHz");
    assert_eq!(after_move["via"], "HF — 14.250 MHz");
}

#[tokio::test]
async fn export_without_a_session_is_401_unauthenticated() {
    let app = test_app().await;
    let (status, _, problem) =
        send_raw(app.router(), "GET", "/api/accounts/me/export", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

/// The central carve-out — session-gated, NOT consent-gated — proven end to
/// end rather than assumed from the router wiring: the SAME freshly signed-in
/// account (no consent ever recorded) is genuinely unconsented, evidenced by a
/// real consent-gated sibling endpoint (`PUT .../callsign`, `ConsentedAccount`)
/// rejecting it with 403 `/errors/consent-required` — yet the export still
/// succeeds for that exact account.
#[tokio::test]
async fn export_succeeds_for_a_signed_in_but_unconsented_account() {
    let app = test_app().await;
    let (cookie, _account) = sign_in(&app, "unconsented-export@example.com").await;

    // Proves the account is genuinely unconsented (not merely untested):
    // a real consent-gated endpoint rejects it.
    let (gated_status, _, gated_problem) = send_raw(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(gated_status, StatusCode::FORBIDDEN);
    assert_eq!(gated_problem["type"], "/errors/consent-required");

    // The export is unaffected by that same missing consent.
    let (export_status, _, _) = send_bytes(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(export_status, StatusCode::OK);
}

/// Empty-state and unconfigured-delivery boundary conditions: a fresh account
/// with no favorites/owned nets/check-ins gets empty arrays (not an error),
/// and an owned net with NO delivery config configured surfaces the `None`
/// branch (`deliveryEmails: []`, `webhookConfigured: false`) rather than
/// panicking or omitting the net entirely.
#[tokio::test]
async fn export_reflects_empty_state_and_an_unconfigured_owned_net() {
    let app = test_app().await;
    let now = now_millis();
    let (cookie, account) = sign_in(&app, "empty-export@example.com").await;
    let account_id: Uuid = account["id"].as_str().expect("id").parse().expect("uuid");

    let (status, _, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["account"]["qrzCredentialsConfigured"], false);
    assert_eq!(
        body["favorites"].as_array().expect("favorites array").len(),
        0
    );
    assert_eq!(
        body["ownedNetDeliveryConfigs"]
            .as_array()
            .expect("owned array")
            .len(),
        0
    );
    assert_eq!(
        body["selfCheckIns"]
            .as_array()
            .expect("checkins array")
            .len(),
        0
    );

    // Now own a net but never configure delivery for it.
    let owned_net = app
        .state
        .net_definitions
        .create(
            &sample_fields("Unconfigured Net", "fn31pr"),
            &sample_connections(),
            account_id,
            "tok-unconfigured",
            now,
        )
        .await
        .expect("create owned net");

    let (status, _, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let owned = body["ownedNetDeliveryConfigs"]
        .as_array()
        .expect("owned array");
    assert_eq!(owned.len(), 1, "the unconfigured net still appears");
    assert_eq!(owned[0]["netDefinitionId"], owned_net.id.to_string());
    assert_eq!(
        owned[0]["deliveryEmails"]
            .as_array()
            .expect("emails array")
            .len(),
        0
    );
    assert_eq!(owned[0]["webhookConfigured"], false);
    assert_eq!(owned[0]["discordConfigured"], false);
}

/// The export is the one caller of the favorites read that
/// must NOT be bounded. `GET /api/favorites` now pages at `DEFAULT_PAGE_LIMIT`;
/// a data-rights export is complete by definition, so it keeps the
/// unbounded read. Green today — this is the fence against a refactor that
/// "tidies" the export onto the paged sibling. Its mutation is routing the export
/// through `list_for_account_page(…, DEFAULT_PAGE_LIMIT, None)`.
#[tokio::test]
async fn the_export_carries_every_favorite_past_the_page_limit() {
    let app = test_app().await;
    let now = now_millis();
    let (cookie, account) = sign_in(&app, "many-favorites@example.com").await;
    let account_id: Uuid = account["id"].as_str().expect("id").parse().expect("uuid");
    let stranger = app
        .state
        .accounts
        .create_verified_and_attach("stranger-many@example.com", now)
        .await
        .expect("seed stranger")
        .id;

    // Seeded through the repos: `create` has no cap (the cap lives in the
    // handler), so one account can favorite more than a page's worth.
    let total = DEFAULT_PAGE_LIMIT + 1;
    for i in 0..total {
        let net = app
            .state
            .net_definitions
            .create(
                &sample_fields(&format!("Net {i}"), "fn31pr"),
                &sample_connections(),
                stranger,
                &format!("tok-many-{i}"),
                now,
            )
            .await
            .expect("create net");
        app.state
            .favorites
            .add(account_id, net.id, now + i as u64)
            .await
            .expect("favorite");
    }

    let (status, _, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["favorites"].as_array().expect("favorites array").len(),
        total,
        "the export carries every favorite, not the first page of them"
    );
}

/// A Discord-only net must be VISIBLE in the data-rights surface: the export
/// carries a `discordConfigured` boolean beside `webhookConfigured`, because a
/// configured integration invisible to the export is a real gap.
///
/// This is also the test that pins the derivation. `webhookConfigured` is read
/// off `webhook_secret_set`, NOT off `webhook_url` — so the Discord sibling
/// cannot be the same expression: Discord mints no secret, and a net whose only
/// destination is Discord has `webhook_secret_set == false`. A copied
/// expression would export `false` here and `true` in the test above; both
/// directions are asserted.
#[tokio::test]
async fn export_reports_a_discord_only_net_as_configured_without_carrying_the_url() {
    let app = test_app().await;
    let now = now_millis();
    let (cookie, account) = sign_in(&app, "discord-export@example.com").await;
    let account_id: Uuid = account["id"].as_str().expect("id").parse().expect("uuid");

    let owned_net = app
        .state
        .net_definitions
        .create(
            &sample_fields("Discord Only Net", "fn31pr"),
            &sample_connections(),
            account_id,
            "tok-discord-only",
            now,
        )
        .await
        .expect("create owned net");
    app.state
        .delivery_configs
        .set(
            owned_net.id,
            &DeliveryConfigFields {
                emails: Vec::new(),
                webhook_url: None,
                discord_webhook_url: Some(DISCORD_WEBHOOK_URL.to_owned()),
            },
            None,
            now,
        )
        .await
        .expect("set delivery config");

    let (status, _, bytes) = send_bytes(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw = String::from_utf8(bytes.clone()).expect("utf8 body");
    let body: Value = serde_json::from_slice(&bytes).expect("json body");

    let owned = body["ownedNetDeliveryConfigs"]
        .as_array()
        .expect("owned array");
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0]["netDefinitionId"], owned_net.id.to_string());
    assert_eq!(
        owned[0]["discordConfigured"], true,
        "a Discord-only net must not export byte-identically to a net with delivery off"
    );
    assert_eq!(
        owned[0]["webhookConfigured"], false,
        "no generic webhook and no minted secret on this net"
    );
    assert_eq!(
        owned[0]["deliveryEmails"]
            .as_array()
            .expect("emails array")
            .len(),
        0
    );

    // Boolean-only, exactly like the webhook secret: the Discord webhook URL
    // carries its bearer token in the path, so it is a credential and never
    // leaves the owner's own config read-back.
    assert!(
        !raw.contains(DISCORD_WEBHOOK_TOKEN),
        "the Discord webhook token must never appear in the export"
    );
    assert!(
        !raw.to_lowercase().contains("discordwebhookurl"),
        "no Discord URL key may appear (got {raw})"
    );
}
