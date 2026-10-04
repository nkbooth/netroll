// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the per-net delivery-config sub-resource: real
//! router, real Postgres (testcontainers), capturing mailer.
//! Asserts status codes, problem+json type slugs, the reveal-once secret
//! lifecycle, ownership refusals, and DB side-effects — never message prose
//! (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct CapturingMailer {
    sent: Mutex<Vec<(String, String)>>,
}

impl CapturingMailer {
    fn last_link(&self) -> String {
        self.sent
            .lock()
            .expect("mailer lock")
            .last()
            .expect("at least one mail sent")
            .1
            .clone()
    }
}

impl Mailer for CapturingMailer {
    fn send_magic_link<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("lock")
                .push((to.to_owned(), link.to_owned()));
            Ok(())
        })
    }
    fn send_email_change<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("lock")
                .push((to.to_owned(), link.to_owned()));
            Ok(())
        })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        to: &'a str,
        new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("lock")
                .push((to.to_owned(), new_email.to_owned()));
            Ok(())
        })
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

async fn send_json(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, json)
}

async fn sign_in_and_consent(app: &TestApp, email: &str) -> String {
    let (status, _) = send_json(
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

    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "token": token }).to_string()))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("ascii")
        .split(';')
        .next()
        .expect("pair")
        .to_owned();

    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    cookie
}

async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> String {
    let cookie = sign_in_and_consent(app, email).await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": callsign })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie
}

fn minimal_definition_json() -> Value {
    json!({
        "title": "Sunday Traffic Net",
        "connections": [
            { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
        ],
        "netCategory": "traffic",
        "netType": "open"
    })
}

async fn create_net(app: &TestApp, cookie: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(minimal_definition_json()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().expect("id").to_owned()
}

async fn config_row_count(app: &TestApp, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_delivery_configs WHERE definition_id = $1::uuid")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .expect("count configs")
}

async fn webhook_secret_in_db(app: &TestApp, id: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT webhook_secret FROM net_delivery_configs WHERE definition_id = $1::uuid",
    )
    .bind(id)
    .fetch_optional(&app.pool)
    .await
    .expect("read secret")
    .flatten()
}

#[tokio::test]
async fn owner_sets_targets_and_the_secret_is_revealed_exactly_once() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    // Set emails + a valid webhook — 200, secret revealed in THIS response.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({
            "emails": ["Alerts@Example.com", "log@example.org"],
            "webhookUrl": "https://hooks.example.com/net"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["emails"],
        json!(["alerts@example.com", "log@example.org"]),
        "emails are normalized and round-trip"
    );
    assert_eq!(body["webhookUrl"], "https://hooks.example.com/net");
    assert_eq!(body["webhookSecretSet"], json!(true));
    let revealed = body["webhookSecret"]
        .as_str()
        .expect("the mint-time response reveals the secret")
        .to_owned();
    assert!(!revealed.is_empty());

    // A subsequent GET reports the secret is set but NEVER returns the plaintext.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/delivery-config"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["webhookSecretSet"], json!(true));
    assert!(
        body.get("webhookSecret").is_none() || body["webhookSecret"].is_null(),
        "GET must never carry the plaintext secret"
    );
    assert_eq!(
        body["emails"],
        json!(["alerts@example.com", "log@example.org"])
    );

    // The stored secret matches what was revealed once.
    assert_eq!(
        webhook_secret_in_db(&app, &id).await.as_deref(),
        Some(revealed.as_str())
    );
}

#[tokio::test]
async fn a_disallowed_webhook_is_refused_distinctly_and_persists_no_row() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    // A cloud-metadata IP literal.
    let (status, meta) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({ "emails": [], "webhookUrl": "https://169.254.169.254/latest/meta-data/" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(meta["type"], "/errors/delivery-config-invalid");

    // A plain http:// URL — a DIFFERENT reason, a distinct detail.
    let (status, insecure) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({ "emails": [], "webhookUrl": "http://hooks.example.com/net" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(insecure["type"], "/errors/delivery-config-invalid");
    assert_ne!(
        meta["detail"], insecure["detail"],
        "blocked-address and not-https carry distinct details"
    );
    // No metadata address leaks back in the detail.
    assert!(!meta["detail"].as_str().expect("detail").contains("169.254"));

    // No row was written on either bad save ("no row is written").
    assert_eq!(config_row_count(&app, &id).await, 0);
}

#[tokio::test]
async fn a_non_owner_is_denied_and_a_nonexistent_net_is_404() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "k2xyz").await;
    let id = create_net(&app, &owner).await;

    // Owner can read (2xx) — the empty/off shape before any config.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/delivery-config"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["emails"], json!([]));
    assert_eq!(body["webhookConfigured"], json!(false));
    assert_eq!(body["webhookSecretSet"], json!(false));

    // A non-owner is forbidden on GET/PUT/DELETE (net exists, not theirs → 403).
    for (method, payload) in [
        ("GET", None),
        (
            "PUT",
            Some(json!({ "emails": ["x@example.com"], "webhookUrl": null })),
        ),
        ("DELETE", None),
    ] {
        let (status, body) = send_json(
            app.router(),
            method,
            &format!("/api/net-definitions/{id}/delivery-config"),
            payload,
            Some(&stranger),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} by a non-owner is 403"
        );
        assert_eq!(body["type"], "/errors/forbidden");
    }
    // The stranger never wrote a config.
    assert_eq!(config_row_count(&app, &id).await, 0);

    // A net that does not exist is 404 (not 403) for its would-be owner.
    let ghost = Uuid::now_v7();
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{ghost}/delivery-config"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-definition-not-found");
}

#[tokio::test]
async fn editing_the_url_keeps_the_secret_and_clearing_nulls_it() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    let set = |url: &'static str| {
        let router = app.router();
        let cookie = cookie.clone();
        let id = id.clone();
        async move {
            send_json(
                router,
                "PUT",
                &format!("/api/net-definitions/{id}/delivery-config"),
                Some(json!({ "emails": [], "webhookUrl": url })),
                Some(&cookie),
            )
            .await
        }
    };

    let (status, first) = set("https://a.example.com/hook").await;
    assert_eq!(status, StatusCode::OK);
    let original_secret = first["webhookSecret"]
        .as_str()
        .expect("first mint")
        .to_owned();

    // Editing the URL does NOT reveal a new secret and does NOT rotate it.
    let (status, second) = set("https://b.example.com/hook").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        second.get("webhookSecret").is_none() || second["webhookSecret"].is_null(),
        "a URL edit reveals no new plaintext"
    );
    assert_eq!(second["webhookSecretSet"], json!(true));
    assert_eq!(
        webhook_secret_in_db(&app, &id).await.as_deref(),
        Some(original_secret.as_str()),
        "the stored secret is unchanged across the URL edit"
    );

    // Clearing the webhook (null URL) nulls the secret.
    let (status, cleared) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({ "emails": [], "webhookUrl": null })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["webhookSecretSet"], json!(false));
    assert_eq!(webhook_secret_in_db(&app, &id).await, None);

    // Re-adding the webhook mints a NEW secret, distinct from the original.
    let (status, readded) = set("https://c.example.com/hook").await;
    assert_eq!(status, StatusCode::OK);
    let new_secret = readded["webhookSecret"]
        .as_str()
        .expect("re-add mints")
        .to_owned();
    assert_ne!(new_secret, original_secret, "a re-add mints a fresh secret");
}

#[tokio::test]
async fn delete_clears_the_config_and_is_idempotent() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({ "emails": ["a@example.com"], "webhookUrl": "https://a.example.com/hook" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config_row_count(&app, &id).await, 1);

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}/delivery-config"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(config_row_count(&app, &id).await, 0);

    // Idempotent — a second delete is still 204.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}/delivery-config"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// --- The Discord destination on the config endpoints -------------

#[tokio::test]
async fn a_discord_url_round_trips_on_the_config_endpoints_and_mints_no_secret() {
    // The read mirrors `webhook_url` exactly.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({
            "emails": [],
            "webhookUrl": null,
            "discordWebhookUrl": "https://discord.com/api/webhooks/12/tok-abc"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["discordWebhookUrl"],
        "https://discord.com/api/webhooks/12/tok-abc"
    );
    assert_eq!(
        body["webhookSecretSet"],
        json!(false),
        "a Discord destination mints NO HMAC secret — the signature contract does not cover it"
    );
    assert!(
        body.get("webhookSecret").is_none(),
        "and nothing is revealed"
    );
    assert_eq!(
        webhook_secret_in_db(&app, &id).await,
        None,
        "no secret reached storage either"
    );

    // The GET mirrors `webhook_url`'s visibility exactly: returned in the clear
    // to the owner, who supplied it.
    let (status, got) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/delivery-config"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        got["discordWebhookUrl"],
        "https://discord.com/api/webhooks/12/tok-abc"
    );
}

#[tokio::test]
async fn a_refused_discord_url_names_the_discord_field_and_persists_no_row() {
    // The SAME egress policy as the generic webhook, no host exemption —
    // and a distinct field name in the problem detail, which never echoes the
    // submitted URL.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    for url in [
        "http://discord.com/api/webhooks/12/tok",
        "https://169.254.169.254/api/webhooks/12/tok",
    ] {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{id}/delivery-config"),
            Some(json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": url })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "refused: {url}");
        let detail = problem["detail"].as_str().expect("a detail");
        assert!(
            detail.starts_with("discord webhook url:"),
            "the detail names the Discord field: {detail}"
        );
        assert!(
            !detail.contains("discord.com")
                && !detail.contains("169.254")
                && !detail.contains("tok"),
            "and never echoes the submitted URL: {detail}"
        );
    }
    assert_eq!(
        config_row_count(&app, &id).await,
        0,
        "validation fails at the boundary — no row is written"
    );
}

#[tokio::test]
async fn a_put_that_omits_the_discord_key_clears_a_configured_destination() {
    // A PUT is a REPLACE, and `DeliveryConfigRequest` is `#[serde(default)]`, so
    // an older client that does not know about `discordWebhookUrl` clears it.
    // That is the same semantics `emails` and `webhookUrl` already have and it
    // is consistent — but it is pinned here rather than left for an owner to
    // discover.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let id = create_net(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({
            "emails": [],
            "webhookUrl": null,
            "discordWebhookUrl": "https://discord.com/api/webhooks/12/tok"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // An old-client body: no `discordWebhookUrl` key at all.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/delivery-config"),
        Some(json!({ "emails": ["a@example.com"] })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["discordWebhookUrl"],
        json!(null),
        "an omitted key clears the destination — a PUT replaces, it does not merge"
    );
}
