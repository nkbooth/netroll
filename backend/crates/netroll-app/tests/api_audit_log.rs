// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the consolidated security audit log: real router,
//! real Postgres, capturing fake mailer. Covers the write-sites (sign-in and
//! sign-out, role grant and revoke, account self-deletion), the review surface
//! `GET /api/admin/audit-log`, and the cross-source check that no row carries
//! PII or a secret. Asserts rows, gating decisions and absences.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::admin::AdminCapability;
use netroll_domain::audit::AuditAction;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

const ADMIN_EMAIL: &str = "admin@example.com";

/// A whole `audit_log` row read raw for the cross-source PII scan
/// (`actor`, `action`, `target_type`, `target_id`, `metadata`).
type RawAuditRow = (
    uuid::Uuid,
    String,
    Option<String>,
    Option<uuid::Uuid>,
    Option<Value>,
);

#[derive(Default)]
struct CapturingMailer {
    /// Every (recipient, link) pair sent — the links carry raw magic-link
    /// tokens, which the PII test proves never leak into an audit row.
    sent: Mutex<Vec<(String, String)>>,
}
impl CapturingMailer {
    fn last_link(&self) -> String {
        self.sent
            .lock()
            .expect("lock")
            .last()
            .expect("a mail sent")
            .1
            .clone()
    }
    fn all_links(&self) -> Vec<String> {
        self.sent
            .lock()
            .expect("lock")
            .iter()
            .map(|p| p.1.clone())
            .collect()
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
        n: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("lock")
                .push((to.to_owned(), n.to_owned()));
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

async fn audit_app() -> TestApp {
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
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into())
        .with_admin_allowlist(vec![ADMIN_EMAIL.to_owned()]);
    TestApp {
        _container: container,
        state,
        mailer,
        pool,
    }
}

async fn send(
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
        .expect("build");
    let response = router.oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    (status, json)
}

/// Signs in via the full magic-link flow; returns `(session cookie, account id)`.
async fn sign_in(app: &TestApp, email: &str) -> (String, String) {
    let (status, _) = send(
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
        .expect("build");
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let id = body["id"].as_str().expect("id").to_owned();
    (cookie, id)
}

/// Signs in + records consent + reserves a callsign — the full gate to be an
/// owner or a role-grant target.
async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> (String, String) {
    let (cookie, id) = sign_in(app, email).await;
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = send(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": callsign })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (cookie, id)
}

async fn create_net(app: &TestApp, cookie: &str) -> String {
    let (status, body) = send(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(json!({
            "title": "Sunday Traffic Net",
            "connections": [
                { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
            ],
            "netCategory": "traffic",
            "netType": "open"
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().expect("definition id").to_owned()
}

async fn start_session(app: &TestApp, cookie: &str, definition_id: &str) -> String {
    let (status, body) = send(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().expect("session id").to_owned()
}

/// All audit rows for one `action`, as `(actor, target, metadata)`.
async fn rows(pool: &PgPool, action: &str) -> Vec<(uuid::Uuid, Option<uuid::Uuid>, Option<Value>)> {
    sqlx::query_as::<_, (uuid::Uuid, Option<uuid::Uuid>, Option<Value>)>(
        "SELECT actor_account_id, target_id, metadata FROM audit_log WHERE action = $1",
    )
    .bind(action)
    .fetch_all(pool)
    .await
    .expect("audit rows")
}

fn uuid_of(id: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(id).expect("uuid")
}

// ---- Task 3: authentication events -------------------------------------------

#[tokio::test]
async fn a_successful_sign_in_lands_exactly_one_signed_in_row() {
    let app = audit_app().await;
    let (_cookie, id) = sign_in(&app, "op@example.com").await;
    let signed_in = rows(&app.pool, AuditAction::SignedIn.as_str()).await;
    assert_eq!(signed_in.len(), 1, "one signed-in row per sign-in");
    assert_eq!(
        signed_in[0].0,
        uuid_of(&id),
        "actor is the signing-in account"
    );
}

#[tokio::test]
async fn signing_out_lands_one_signed_out_row_with_the_self_actor() {
    let app = audit_app().await;
    let (cookie, id) = sign_in(&app, "op@example.com").await;
    let (status, _) = send(
        app.router(),
        "DELETE",
        "/api/sessions/current",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let signed_out = rows(&app.pool, AuditAction::SignedOut.as_str()).await;
    assert_eq!(signed_out.len(), 1, "one signed-out row per sign-out");
    assert_eq!(
        signed_out[0].0,
        uuid_of(&id),
        "actor is the signing-out account"
    );
}

// ---- Task 4: role/permission-change events -----------------------------------

#[tokio::test]
async fn granting_a_role_lands_one_role_granted_row_targeting_the_grantee() {
    let app = audit_app().await;
    let (owner, owner_id) = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_staff, staff_id) = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": "w2bcd", "role": "logger" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let granted = rows(&app.pool, AuditAction::RoleGranted.as_str()).await;
    assert_eq!(granted.len(), 1, "one role-granted row");
    assert_eq!(granted[0].0, uuid_of(&owner_id), "actor is the granter");
    assert_eq!(
        granted[0].1,
        Some(uuid_of(&staff_id)),
        "target is the grantee"
    );
    let meta = granted[0].2.clone().expect("metadata present");
    assert_eq!(
        meta["role"], "logger",
        "metadata carries the role kebab verb"
    );
    assert_eq!(
        meta["netSessionId"], session_id,
        "metadata carries the session uuid"
    );
}

#[tokio::test]
async fn revoking_a_role_lands_one_role_revoked_row_written_at_the_handler() {
    let app = audit_app().await;
    let (owner, owner_id) = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_staff, staff_id) = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": "w2bcd", "role": "logger" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{staff_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let revoked = rows(&app.pool, AuditAction::RoleRevoked.as_str()).await;
    assert_eq!(revoked.len(), 1, "one role-revoked row");
    assert_eq!(revoked[0].0, uuid_of(&owner_id), "actor is the revoker");
    assert_eq!(
        revoked[0].1,
        Some(uuid_of(&staff_id)),
        "target is the account revoked"
    );
    let meta = revoked[0].2.clone().expect("metadata present");
    assert_eq!(meta["role"], "logger");
    assert_eq!(meta["netSessionId"], session_id);
}

// ---- Task 5: deletion event --------------------------------------------------

#[tokio::test]
async fn self_deleting_lands_one_row_where_actor_equals_target_equals_self() {
    let app = audit_app().await;
    let (cookie, id) = sign_in(&app, "leaving@example.com").await;
    let (status, _) = send(
        app.router(),
        "DELETE",
        "/api/accounts/me",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let deleted = rows(&app.pool, AuditAction::AccountSelfDeleted.as_str()).await;
    assert_eq!(deleted.len(), 1, "one self-delete row");
    assert_eq!(deleted[0].0, uuid_of(&id), "actor is self");
    assert_eq!(deleted[0].1, Some(uuid_of(&id)), "target is self");
}

// ---- Task 6: review surface --------------------------------------------------

#[tokio::test]
async fn an_unauthenticated_caller_gets_401_on_the_audit_log_endpoint() {
    let app = audit_app().await;
    let (status, _) = send(app.router(), "GET", "/api/admin/audit-log", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_signed_in_non_admin_gets_403_on_the_audit_log_endpoint() {
    let app = audit_app().await;
    let (cookie, _) = sign_in(&app, "regular@example.com").await;
    let (status, problem) = send(
        app.router(),
        "GET",
        "/api/admin/audit-log",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/forbidden");
}

#[tokio::test]
async fn an_admin_reads_the_audit_log_newest_first_and_the_read_is_itself_audited() {
    let app = audit_app().await;
    // Drive a few auditable events so the log has ordered content.
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    let (leaver_cookie, leaver_id) = sign_in(&app, "leaver@example.com").await;
    let (status, _) = send(
        app.router(),
        "DELETE",
        "/api/accounts/me",
        None,
        Some(&leaver_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/audit-log",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = body["items"].as_array().expect("items array");
    assert!(
        arr.len() >= 3,
        "at least the two sign-ins + the self-delete"
    );

    // Newest-first: occurredAt is monotonically non-increasing.
    let times: Vec<&str> = arr
        .iter()
        .map(|r| r["occurredAt"].as_str().expect("occurredAt"))
        .collect();
    for pair in times.windows(2) {
        assert!(pair[0] >= pair[1], "rows are newest-first by occurredAt");
    }

    // The exercised actions are present with the expected camelCase shape.
    let actions: Vec<&str> = arr
        .iter()
        .map(|r| r["action"].as_str().expect("action"))
        .collect();
    assert!(actions.contains(&AuditAction::SignedIn.as_str()));
    assert!(actions.contains(&AuditAction::AccountSelfDeleted.as_str()));

    let self_delete_row = arr
        .iter()
        .find(|r| r["action"] == AuditAction::AccountSelfDeleted.as_str())
        .expect("self-delete row surfaced");
    assert_eq!(self_delete_row["actorAccountId"], leaver_id);
    assert_eq!(self_delete_row["targetType"], "account");
    assert_eq!(self_delete_row["targetId"], leaver_id);

    // The view itself is audited (consistent with ViewReports) — one row for the
    // admin, keyed by the admin actor.
    let views = rows(&app.pool, AdminCapability::ViewAuditLog.as_str()).await;
    assert_eq!(views.len(), 1, "the audit-log read logs itself once");
    assert_eq!(
        views[0].0,
        uuid_of(&admin_id),
        "the view actor is the admin"
    );
}

// ---- Task 7: cross-source PII/secret-free verification -----------------------

#[tokio::test]
async fn no_audit_row_from_any_source_contains_pii_or_a_secret() {
    let app = audit_app().await;

    // --- Drive EVERY current write-site ---
    // Admin (four bounded actions) + auth + role + deletion.
    let (admin_cookie, _admin_id) = sign_in(&app, ADMIN_EMAIL).await;

    // Auth: a plain sign-in and a sign-out.
    let (bystander_cookie, _bystander_id) = sign_in(&app, "bystander@example.com").await;
    let (status, _) = send(
        app.router(),
        "DELETE",
        "/api/sessions/current",
        None,
        Some(&bystander_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Role grant + revoke on a real session.
    let (owner, _owner_id) = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_staff, staff_id) = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": "w2bcd", "role": "logger" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{staff_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Self-delete.
    let (leaver_cookie, _leaver_id) = sign_in(&app, "leaver@example.com").await;
    let (status, _) = send(
        app.router(),
        "DELETE",
        "/api/accounts/me",
        None,
        Some(&leaver_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Admin actions: an abuse report to view + resolve, and a victim to
    // disable + reenable.
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/abuse-reports",
        Some(json!({ "body": "spam" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let report_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM abuse_reports LIMIT 1")
        .fetch_one(&app.pool)
        .await
        .expect("report id");
    let (_victim_cookie, victim_id) = sign_in(&app, "victim@example.com").await;
    let (status, _) = send(
        app.router(),
        "GET",
        "/api/admin/abuse-reports",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/abuse-reports/{report_id}/resolve"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{victim_id}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{victim_id}/reenable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The admin also reads the audit log (view-audit-log write-site).
    let (status, _) = send(
        app.router(),
        "GET",
        "/api/admin/audit-log",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // --- Read the WHOLE audit_log and verify no source leaked PII/secrets. ---
    let all: Vec<RawAuditRow> = sqlx::query_as(
        "SELECT actor_account_id, action, target_type, target_id, metadata FROM audit_log",
    )
    .fetch_all(&app.pool)
    .await
    .expect("all audit rows");

    assert!(all.len() >= 8, "every driven write-site produced a row");

    // The closed vocabulary: every action is a known verb (AuditAction ∪
    // AdminCapability) — a scattered/ad-hoc action string fails here.
    let closed: std::collections::HashSet<&str> = AuditAction::EVERY
        .iter()
        .map(|a| a.as_str())
        .chain(AdminCapability::EVERY.iter().map(|c| c.as_str()))
        .collect();

    // Known secrets/PII that flowed through the driven requests: every email
    // used, and every raw magic-link token minted. None may appear in any row.
    let emails = [
        ADMIN_EMAIL,
        "bystander@example.com",
        "owner@example.com",
        "staff@example.com",
        "leaver@example.com",
        "victim@example.com",
    ];
    let tokens: Vec<String> = app
        .mailer
        .all_links()
        .iter()
        .filter_map(|l| l.split_once("token=").map(|(_, t)| t.to_owned()))
        .collect();
    assert!(
        !tokens.is_empty(),
        "the flow minted magic-link tokens to check against"
    );

    for (actor, action, target_type, target_id, metadata) in &all {
        assert!(
            closed.contains(action.as_str()),
            "every action is in the closed vocabulary: {action}"
        );
        // actor/target are id-only (uuids, enforced by the column type at the
        // `query_as::<_, RawAuditRow>` boundary) AND real, non-placeholder
        // identities — a nil uuid would mean a call-site forgot to pass the
        // actual actor/target. An earlier version of this check compared a
        // value to itself and could never fail.
        assert_ne!(
            *actor,
            uuid::Uuid::nil(),
            "actor is a real (non-nil) account id"
        );
        if let Some(target) = target_id {
            assert_ne!(
                *target,
                uuid::Uuid::nil(),
                "target id, when present, is a real (non-nil) id"
            );
        }

        // Serialize the whole row's free-text/structured content and assert no
        // secret/PII pattern appears (the codebase's substring-heuristic, as in
        // the QRZ log-capture tests).
        let serialized = format!(
            "{} {} {}",
            action,
            target_type.clone().unwrap_or_default(),
            metadata.clone().map(|m| m.to_string()).unwrap_or_default(),
        );
        assert!(
            !serialized.contains('@'),
            "no audit field holds an email-shaped string: {serialized}"
        );
        for email in emails {
            assert!(
                !serialized.contains(email),
                "no row contains an email: {serialized}"
            );
        }
        for token in &tokens {
            assert!(
                !serialized.contains(token.as_str()),
                "no row contains a magic-link token"
            );
        }
    }
}

// ---- Net & session lifecycle events (the admin dashboard's object history) ----

/// The audit rows for one action verb, as `(target_id, context_session_id,
/// context_definition_id, metadata)`.
async fn lifecycle_rows(
    pool: &PgPool,
    action: &str,
) -> Vec<(
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    Option<uuid::Uuid>,
    Option<Value>,
)> {
    sqlx::query_as(
        "SELECT target_id, context_session_id, context_definition_id, metadata
           FROM audit_log WHERE action = $1 ORDER BY occurred_at, id",
    )
    .bind(action)
    .fetch_all(pool)
    .await
    .expect("lifecycle audit rows")
}

#[tokio::test]
async fn creating_a_net_records_it_against_the_net() {
    let app = audit_app().await;
    let (cookie, actor) = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let def = create_net(&app, &cookie).await;
    let def_uuid = uuid_of(&def);

    let rows = lifecycle_rows(&app.pool, "net-created").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, Some(def_uuid), "targets the net");
    assert_eq!(rows[0].2, Some(def_uuid), "carries the net as context");

    let actors = rows_actor(&app.pool, "net-created").await;
    assert_eq!(actors, vec![uuid_of(&actor)]);
}

#[tokio::test]
async fn editing_a_net_records_only_the_fields_that_actually_changed() {
    let app = audit_app().await;
    let (cookie, _) = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let def = create_net(&app, &cookie).await;

    let edited = json!({
        "title": "Monday Traffic Net",
        "netCategory": "traffic",
        "netType": "open"
    });
    let (status, _) = send(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{def}"),
        Some(edited),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let rows = lifecycle_rows(&app.pool, "net-updated").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3, Some(json!({ "changedFields": ["title"] })));

    // The new title must NOT appear anywhere in the row — names only.
    let blob = serde_json::to_string(&rows[0].3).expect("serialize");
    assert!(
        !blob.contains("Monday"),
        "no field VALUE reaches the audit row: {blob}"
    );
}

#[tokio::test]
async fn resaving_a_net_unchanged_records_nothing() {
    // A full-replace PUT arrives on every save. Auditing no-op saves would bury
    // real edits under noise and make "who acted on this net?" useless.
    let app = audit_app().await;
    let (cookie, _) = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let def = create_net(&app, &cookie).await;

    let identical = json!({
        "title": "Sunday Traffic Net",
        "netCategory": "traffic",
        "netType": "open"
    });
    for _ in 0..3 {
        let (status, _) = send(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{def}"),
            Some(identical.clone()),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    assert!(
        lifecycle_rows(&app.pool, "net-updated").await.is_empty(),
        "an unchanged re-save is not an event"
    );
}

#[tokio::test]
async fn archiving_a_net_records_the_transition_once() {
    // The endpoint is idempotent; only the transition is an event.
    let app = audit_app().await;
    let (cookie, _) = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let def = create_net(&app, &cookie).await;

    for _ in 0..2 {
        send(
            app.router(),
            "DELETE",
            &format!("/api/net-definitions/{def}"),
            None,
            Some(&cookie),
        )
        .await;
    }

    let rows = lifecycle_rows(&app.pool, "net-archived").await;
    assert_eq!(rows.len(), 1, "re-archiving is not a second action");
    assert_eq!(rows[0].0, Some(uuid_of(&def)));
}

#[tokio::test]
async fn a_session_lifecycle_carries_both_the_session_and_its_net() {
    // The net context is what makes "everything that touched this net" reach a
    // session without the admin first knowing the session's id.
    let app = audit_app().await;
    let (cookie, _) = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let def = create_net(&app, &cookie).await;
    let session = start_session(&app, &cookie, &def).await;

    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for action in ["session-started", "session-closed"] {
        let rows = lifecycle_rows(&app.pool, action).await;
        assert_eq!(rows.len(), 1, "{action} recorded once");
        assert_eq!(rows[0].0, Some(uuid_of(&session)), "{action} targets it");
        assert_eq!(rows[0].1, Some(uuid_of(&session)), "{action} session ctx");
        assert_eq!(rows[0].2, Some(uuid_of(&def)), "{action} net ctx");
    }
}

/// The actor ids for one action verb.
async fn rows_actor(pool: &PgPool, action: &str) -> Vec<uuid::Uuid> {
    sqlx::query_scalar("SELECT actor_account_id FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("actors")
}
