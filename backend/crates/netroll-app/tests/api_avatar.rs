// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for avatar upload/removal and the public app-config
//! read: real router, real Postgres (testcontainers), real filesystem store
//! over a scratch directory. Asserts status codes, problem+json `type` slugs,
//! stored bytes, and profile effects — never message prose (house TDD
//! rule).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_adapters::avatar::FsAvatarStore;
use netroll_app::config::resolve_app_config;
use netroll_app::http::avatar::UnavailableAvatarStore;
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

/// Mailer that records sends instead of speaking SMTP.
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
                .expect("mailer lock")
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
                .expect("mailer lock")
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
                .expect("mailer lock")
                .push((to.to_owned(), new_email.to_owned()));
            Ok(())
        })
    }
}

/// A scratch avatar directory removed when the test app drops.
struct ScratchDir(PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    avatar_dir: ScratchDir,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }

    /// The same app with the FAIL-CLOSED default store swapped back in, for the
    /// "no store configured" posture.
    fn router_without_store(&self) -> Router {
        api_router(
            self.state
                .clone()
                .with_avatar_store(Arc::new(UnavailableAvatarStore)),
        )
    }

    /// Files currently in the avatar directory, sorted for stable assertions.
    fn stored_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.avatar_dir.0)
            .expect("read avatar dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn read_stored(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.avatar_dir.0.join(name)).expect("stored avatar readable")
    }
}

/// Builds the app with an avatar store over a fresh scratch directory, plus the
/// public app-config the analytics/donation test asserts on.
async fn test_app_with(app_config_env: (Option<&str>, Option<&str>, Option<&str>)) -> TestApp {
    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("resolve container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("resolve mapped postgres port");
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect to containerized postgres");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    let dir = std::env::temp_dir().join(format!(
        "netroll-api-avatar-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch avatar dir");

    let mailer = Arc::new(CapturingMailer::default());
    let (plausible, script_host, kofi) = app_config_env;
    let state = AppState::new(pool, mailer.clone(), "http://localhost:5173".into())
        .with_avatar_store(Arc::new(FsAvatarStore::new(dir.clone())))
        .with_app_config(resolve_app_config(
            plausible.map(str::to_owned),
            script_host.map(str::to_owned),
            kofi.map(str::to_owned),
        ));
    TestApp {
        _container: container,
        state,
        mailer,
        avatar_dir: ScratchDir(dir),
    }
}

async fn test_app() -> TestApp {
    test_app_with((None, None, None)).await
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, json)
}

/// Posts `bytes` as a multipart file part named `file`.
async fn upload_avatar(
    router: Router,
    cookie: &str,
    file_name: &str,
    content_type: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "netrollboundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let request = Request::builder()
        .method("POST")
        .uri("/api/accounts/me/avatar")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header(header::COOKIE, cookie)
        .body(Body::from(body))
        .expect("build multipart request");

    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let raw = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    let json = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).expect("body is JSON")
    };
    (status, json)
}

/// Signs a fresh email all the way in and records consent. Returns the cookie.
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
        .expect("link carries a token")
        .1
        .to_owned();

    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "token": token }).to_string()))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route request");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("201 sets the session cookie")
        .to_str()
        .expect("cookie header is ascii")
        .split(';')
        .next()
        .expect("cookie pair")
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

/// A minimal but genuine PNG header.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRpixels";
/// A minimal but genuine JPEG header.
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];

#[tokio::test]
async fn uploading_an_avatar_stores_the_bytes_and_points_the_profile_at_them() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, body) = upload_avatar(app.router(), &cookie, "me.png", "image/png", PNG).await;

    assert_eq!(status, StatusCode::OK);
    let avatar_url = body["avatarUrl"].as_str().expect("avatarUrl in response");
    assert!(
        avatar_url.starts_with("/avatars/"),
        "same-origin path, got {avatar_url}"
    );
    assert!(avatar_url.ends_with(".png"));

    // The bytes really landed, under the name the response advertises.
    let file_name = avatar_url.trim_start_matches("/avatars/");
    assert_eq!(app.read_stored(file_name), PNG);

    // And /me agrees, so a reload shows the same avatar.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["avatarUrl"].as_str(), Some(avatar_url));
}

#[tokio::test]
async fn the_stored_extension_follows_the_sniffed_bytes_not_the_claimed_name() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    // JPEG bytes uploaded as "portrait.png" with a lying Content-Type.
    let (status, body) =
        upload_avatar(app.router(), &cookie, "portrait.png", "image/png", JPEG).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body["avatarUrl"]
            .as_str()
            .expect("avatarUrl")
            .ends_with(".jpg"),
        "sniffed type wins over the claimed name"
    );
}

#[tokio::test]
async fn re_uploading_a_different_type_replaces_the_file_and_leaves_no_orphan() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    upload_avatar(app.router(), &cookie, "me.png", "image/png", PNG).await;
    assert_eq!(app.stored_files().len(), 1);

    let (status, body) = upload_avatar(app.router(), &cookie, "me.jpg", "image/jpeg", JPEG).await;
    assert_eq!(status, StatusCode::OK);

    // The .png from the first upload must be gone: a different sniffed type
    // means a different filename, so without cleanup it would linger forever.
    let files = app.stored_files();
    assert_eq!(files.len(), 1, "exactly one avatar on disk, got {files:?}");
    assert!(files[0].ends_with(".jpg"), "got {files:?}");
    assert_eq!(
        body["avatarUrl"].as_str().map(|u| u.ends_with(".jpg")),
        Some(true)
    );
}

#[tokio::test]
async fn a_non_image_upload_is_refused_with_its_own_problem_slug() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    // The stored-XSS case: an HTML document with an image filename and type.
    let (status, body) = upload_avatar(
        app.router(),
        &cookie,
        "me.png",
        "image/png",
        b"<!doctype html><script>alert(1)</script>",
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"].as_str(), Some("/errors/avatar-invalid"));
    // Nothing was written for a rejected upload.
    assert!(app.stored_files().is_empty());
}

#[tokio::test]
async fn an_oversize_upload_is_refused_before_it_is_stored() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let mut oversize = PNG.to_vec();
    oversize.resize(netroll_domain::avatar::MAX_AVATAR_BYTES + 1, 0);
    let (status, body) =
        upload_avatar(app.router(), &cookie, "big.png", "image/png", &oversize).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"].as_str(), Some("/errors/avatar-invalid"));
    assert!(app.stored_files().is_empty());
}

#[tokio::test]
async fn an_upload_without_a_session_is_unauthenticated_and_stores_nothing() {
    let app = test_app().await;

    let (status, _) = upload_avatar(
        app.router(),
        "netroll_session=nope",
        "me.png",
        "image/png",
        PNG,
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(app.stored_files().is_empty());
}

#[tokio::test]
async fn deleting_an_uploaded_avatar_removes_the_file_and_clears_the_profile() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;
    upload_avatar(app.router(), &cookie, "me.png", "image/png", PNG).await;

    let (status, body) = send_json(
        app.router(),
        "DELETE",
        "/api/accounts/me/avatar",
        None,
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["avatarUrl"].is_null(), "falls back to Gravatar");
    assert!(app.stored_files().is_empty());
    // The Gravatar URL is always present, so the UI still has something to show.
    assert!(body["gravatarUrl"].as_str().is_some());
}

#[tokio::test]
async fn deleting_leaves_an_externally_hosted_avatar_url_alone() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "avatarUrl": "https://example.com/me.png" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "DELETE",
        "/api/accounts/me/avatar",
        None,
        Some(&cookie),
    )
    .await;

    // There is no file of ours to delete, and silently blanking an operator's
    // own URL would be a surprise.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["avatarUrl"].as_str(),
        Some("https://example.com/me.png")
    );
}

#[tokio::test]
async fn saving_the_profile_after_an_upload_keeps_the_stored_avatar_path() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;
    let (_, uploaded) = upload_avatar(app.router(), &cookie, "me.png", "image/png", PNG).await;
    let stored_path = uploaded["avatarUrl"]
        .as_str()
        .expect("avatarUrl")
        .to_owned();

    // The client round-trips whatever it holds on the next profile save. If the
    // profile validator rejected our own path, "save display name" would 422
    // for every account with an uploaded avatar.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "displayName": "Maria", "avatarUrl": stored_path })),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["avatarUrl"].as_str(), Some(stored_path.as_str()));
    assert_eq!(body["displayName"].as_str(), Some("Maria"));
}

#[tokio::test]
async fn a_crafted_traversal_path_is_still_refused_by_the_profile_validator() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "avatarUrl": "/avatars/../../etc/passwd" })),
        Some(&cookie),
    )
    .await;

    // The profile validator's own slug/status (`ApiError::Validation`), not the
    // avatar-specific one: this never reached the upload path at all.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"].as_str(), Some("/errors/validation"));
}

#[tokio::test]
async fn upload_reports_unavailable_when_no_store_is_installed() {
    // The fail-closed default (no `with_avatar_store`): the instance still
    // serves everything else, and upload says "not configured here" rather
    // than pretending to save a file.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, body) = upload_avatar(
        app.router_without_store(),
        &cookie,
        "me.png",
        "image/png",
        PNG,
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["type"].as_str(),
        Some("/errors/avatar-storage-unavailable")
    );
}

#[tokio::test]
async fn app_config_reports_nothing_configured_by_default() {
    let app = test_app().await;

    let (status, body) = send_json(app.router(), "GET", "/api/app-config", None, None).await;

    // The default posture for every self-hoster: no analytics, no donation link.
    assert_eq!(status, StatusCode::OK);
    assert!(body["plausibleDomain"].is_null());
    assert!(body["plausibleScriptHost"].is_null());
    assert!(body["kofiUsername"].is_null());
}

#[tokio::test]
async fn app_config_is_public_and_reports_the_configured_integrations() {
    let app = test_app_with((
        Some("netroll.radio"),
        Some("https://analytics.example"),
        Some("n1cck"),
    ))
    .await;

    // No cookie: every visitor reads this on first paint.
    let (status, body) = send_json(app.router(), "GET", "/api/app-config", None, None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plausibleDomain"].as_str(), Some("netroll.radio"));
    assert_eq!(
        body["plausibleScriptHost"].as_str(),
        Some("https://analytics.example")
    );
    assert_eq!(body["kofiUsername"].as_str(), Some("n1cck"));
}
