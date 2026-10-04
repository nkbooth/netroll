// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Integration tests for `DeliveryConfigRepo` against a real
//! containerized Postgres (no mocked SQL). Proves the set/get round-trip,
//! the webhook-secret lifecycle (mint once, stable across URL edits, nulled on
//! clear), the empty-email-set round-trip, and the delivery-only secret
//! accessor.

use netroll_adapters::pg::delivery_configs::DeliveryConfigRepo;
use netroll_domain::net::delivery::DeliveryConfigFields;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

async fn migrated_pool() -> (ContainerAsync<Postgres>, PgPool) {
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
    (container, pool)
}

async fn insert_definition(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO net_definitions
            (id, title, net_category, net_type, link_token)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind("Sunday Traffic Net")
    .bind("traffic")
    .bind("open")
    .bind(Uuid::now_v7().simple().to_string())
    .execute(pool)
    .await
    .expect("insert minimal net definition");
    // Every definition is born with at least one connection, and
    // the definition row itself carries no connection fact —
    // so a fixture that inserts the row alone builds a net the application
    // refuses to read. One HF way, at the export position.
    sqlx::query(
        "INSERT INTO net_connections
            (id, definition_id, position, kind, planned_frequency_hz, band, mode)
         VALUES ($1, $2, 0, 'hf', 14230000, '20m', 'ssb')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(pool)
    .await
    .expect("insert the definition's one connection");
    id
}

fn fields(emails: &[&str], webhook: Option<&str>, discord: Option<&str>) -> DeliveryConfigFields {
    DeliveryConfigFields {
        emails: emails.iter().map(|e| (*e).to_owned()).collect(),
        webhook_url: webhook.map(|w| w.to_owned()),
        discord_webhook_url: discord.map(|d| d.to_owned()),
    }
}

const NOW: u64 = 1_770_000_000_000;

#[tokio::test]
async fn get_returns_none_when_no_config_exists() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;
    assert!(
        repo.get(def).await.expect("get").is_none(),
        "a net that never configured delivery has no config row"
    );
}

#[tokio::test]
async fn emails_round_trip_including_the_empty_set() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&["a@example.com", "b@example.com"], None, None),
        None,
        NOW,
    )
    .await
    .expect("set emails");
    let got = repo.get(def).await.expect("get").expect("config exists");
    assert_eq!(got.emails, vec!["a@example.com", "b@example.com"]);
    assert!(got.webhook_url.is_none());
    assert!(!got.webhook_secret_set, "no webhook ⇒ no secret");

    // Replace with the empty set — the "no email targets" state round-trips.
    repo.set(def, &fields(&[], None, None), None, NOW)
        .await
        .expect("set empty");
    let got = repo.get(def).await.expect("get").expect("still a row");
    assert!(got.emails.is_empty(), "the empty email set round-trips");
}

#[tokio::test]
async fn a_webhook_secret_is_minted_once_and_stable_across_a_url_edit() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    // First webhook set with a candidate ⇒ that candidate is minted.
    let out = repo
        .set(
            def,
            &fields(&[], Some("https://a.example.com/hook"), None),
            Some("secret-one"),
            NOW,
        )
        .await
        .expect("first webhook set");
    assert_eq!(
        out.minted_secret.as_deref(),
        Some("secret-one"),
        "the fresh candidate is the reveal-once secret"
    );
    assert_eq!(
        repo.webhook_secret(def).await.expect("secret"),
        Some("secret-one".to_owned())
    );

    // Edit the URL, passing a DIFFERENT candidate — the stored secret must NOT
    // rotate, and NO new secret is revealed.
    let out = repo
        .set(
            def,
            &fields(&[], Some("https://b.example.com/hook"), None),
            Some("secret-two"),
            NOW,
        )
        .await
        .expect("url edit");
    assert_eq!(out.minted_secret, None, "a URL edit reveals no new secret");
    assert_eq!(
        repo.webhook_secret(def).await.expect("secret"),
        Some("secret-one".to_owned()),
        "the original secret is preserved across the URL edit"
    );
    let got = repo.get(def).await.expect("get").expect("row");
    assert_eq!(
        got.webhook_url.as_deref(),
        Some("https://b.example.com/hook")
    );
    assert!(got.webhook_secret_set);
}

#[tokio::test]
async fn clearing_the_webhook_nulls_the_secret_and_a_re_add_mints_a_new_one() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&[], Some("https://a.example.com/hook"), None),
        Some("secret-one"),
        NOW,
    )
    .await
    .expect("set webhook");

    // Remove the webhook (URL None) — the secret is nulled with it.
    let out = repo
        .set(def, &fields(&["keep@example.com"], None, None), None, NOW)
        .await
        .expect("clear webhook, keep emails");
    assert_eq!(out.minted_secret, None);
    assert_eq!(
        repo.webhook_secret(def).await.expect("secret"),
        None,
        "secret nulled with the webhook"
    );
    let got = repo.get(def).await.expect("get").expect("row");
    assert!(got.webhook_url.is_none());
    assert!(!got.webhook_secret_set);
    assert_eq!(
        got.emails,
        vec!["keep@example.com"],
        "emails survive the webhook clear"
    );

    // Re-adding the webhook mints a NEW secret.
    let out = repo
        .set(
            def,
            &fields(
                &["keep@example.com"],
                Some("https://c.example.com/hook"),
                None,
            ),
            Some("secret-three"),
            NOW,
        )
        .await
        .expect("re-add webhook");
    assert_eq!(
        out.minted_secret.as_deref(),
        Some("secret-three"),
        "a re-add mints a fresh secret"
    );
    assert_eq!(
        repo.webhook_secret(def).await.expect("secret"),
        Some("secret-three".to_owned())
    );
}

#[tokio::test]
async fn a_save_with_an_unchanged_url_self_heals_when_the_stored_secret_is_missing() {
    // Regression test for the handler-level TOCTOU this repo primitive must
    // stay safe against: a concurrent PUT that keeps an existing webhook URL
    // can race a concurrent clear that nulls the secret between the first
    // request's own pre-flight state and its write. The handler's fix is to
    // ALWAYS pass a fresh candidate on every webhook-bearing save and let this
    // `set`'s atomic COALESCE be the sole arbiter — proving here that when the
    // stored secret is (for whatever reason, including that race) absent
    // while `webhook_url` is unchanged, a fresh candidate is adopted and
    // revealed rather than the row being left with a webhook and no secret.
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&[], Some("https://a.example.com/hook"), None),
        Some("secret-one"),
        NOW,
    )
    .await
    .expect("first webhook set");

    // Simulate the race outcome directly: the secret is gone but the webhook
    // URL is still set (an invariant violation this test proves `set` heals).
    sqlx::query("UPDATE net_delivery_configs SET webhook_secret = NULL WHERE definition_id = $1")
        .bind(def)
        .execute(&pool)
        .await
        .expect("simulate a secret nulled out from under an unchanged webhook_url");

    // The next save keeps the SAME URL but (per the fixed handler contract)
    // still supplies a fresh candidate.
    let out = repo
        .set(
            def,
            &fields(&[], Some("https://a.example.com/hook"), None),
            Some("secret-healed"),
            NOW,
        )
        .await
        .expect("save with unchanged url, secret missing");
    assert_eq!(
        out.minted_secret.as_deref(),
        Some("secret-healed"),
        "an absent stored secret is always adopted and revealed, even on an unchanged URL"
    );
    assert_eq!(
        repo.webhook_secret(def).await.expect("secret"),
        Some("secret-healed".to_owned())
    );
}

#[tokio::test]
async fn clear_deletes_the_row_and_is_idempotent() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&["a@example.com"], Some("https://a.example.com/hook"), None),
        Some("s"),
        NOW,
    )
    .await
    .expect("set");
    repo.clear(def).await.expect("clear");
    assert!(
        repo.get(def).await.expect("get").is_none(),
        "clear removes the config"
    );
    // Idempotent — a second clear on a config-less net is a no-op.
    repo.clear(def).await.expect("second clear is a no-op");
}

#[tokio::test]
async fn webhook_target_reads_url_and_secret_together() {
    // Delivery must sign and post a URL and
    // secret from the SAME read, not a URL snapshotted earlier and a secret
    // fetched separately later. Proves the paired accessor returns both.
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&[], Some("https://hooks.example.com/net"), None),
        Some("paired-secret"),
        NOW,
    )
    .await
    .expect("set webhook");

    assert_eq!(
        repo.webhook_target(def).await.expect("webhook_target"),
        Some((
            "https://hooks.example.com/net".to_owned(),
            Some("paired-secret".to_owned())
        )),
        "the URL and secret come from the same row read"
    );
}

#[tokio::test]
async fn webhook_target_is_none_with_no_row_or_no_webhook_url() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    assert_eq!(
        repo.webhook_target(def).await.expect("webhook_target"),
        None,
        "no config row at all"
    );

    repo.set(def, &fields(&["a@example.com"], None, None), None, NOW)
        .await
        .expect("email-only config");
    assert_eq!(
        repo.webhook_target(def).await.expect("webhook_target"),
        None,
        "a config row with no webhook URL"
    );
}

#[tokio::test]
async fn webhook_target_surfaces_a_url_with_no_stored_secret_as_the_anomaly_it_is() {
    // A webhook URL with no secret is a data anomaly the caller must record
    // and skip, never fabricate a key for — proven here by directly
    // engineering the anomalous row state, mirroring the TOCTOU-heal test
    // above.
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&[], Some("https://hooks.example.com/net"), None),
        Some("will-be-nulled"),
        NOW,
    )
    .await
    .expect("set webhook");
    sqlx::query("UPDATE net_delivery_configs SET webhook_secret = NULL WHERE definition_id = $1")
        .bind(def)
        .execute(&pool)
        .await
        .expect("simulate the anomaly");

    assert_eq!(
        repo.webhook_target(def).await.expect("webhook_target"),
        Some(("https://hooks.example.com/net".to_owned(), None)),
        "the URL is present but the secret is None, distinct from no-webhook-at-all"
    );
}

// --- The Discord webhook URL column ------------------------------

#[tokio::test]
async fn a_discord_webhook_url_round_trips_and_is_replaced_and_cleared_independently() {
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    // Discord ALONE — no emails, no generic webhook.
    repo.set(
        def,
        &fields(&[], None, Some("https://discord.com/api/webhooks/1/tok-a")),
        None,
        NOW,
    )
    .await
    .expect("set discord only");
    let got = repo.get(def).await.expect("get").expect("config exists");
    assert_eq!(
        got.discord_webhook_url.as_deref(),
        Some("https://discord.com/api/webhooks/1/tok-a"),
        "the Discord URL round-trips through the read model"
    );
    assert!(
        got.webhook_url.is_none(),
        "no generic webhook was configured"
    );
    assert!(
        !got.webhook_secret_set,
        "a Discord destination mints NO HMAC secret"
    );
    assert_eq!(
        repo.discord_target(def).await.expect("discord_target"),
        Some("https://discord.com/api/webhooks/1/tok-a".to_owned()),
        "the delivery-time accessor reads the same value"
    );

    // A replace swaps it (no COALESCE — there is nothing to preserve).
    repo.set(
        def,
        &fields(&[], None, Some("https://discord.com/api/webhooks/2/tok-b")),
        None,
        NOW,
    )
    .await
    .expect("replace discord");
    assert_eq!(
        repo.get(def)
            .await
            .expect("get")
            .expect("row")
            .discord_webhook_url
            .as_deref(),
        Some("https://discord.com/api/webhooks/2/tok-b")
    );

    // Clearing the Discord URL leaves the row and the other targets alone.
    repo.set(def, &fields(&["keep@example.com"], None, None), None, NOW)
        .await
        .expect("clear discord");
    let got = repo.get(def).await.expect("get").expect("row");
    assert!(got.discord_webhook_url.is_none(), "the Discord URL cleared");
    assert_eq!(got.emails, vec!["keep@example.com"]);
    assert_eq!(
        repo.discord_target(def).await.expect("discord_target"),
        None
    );
}

#[tokio::test]
async fn setting_a_discord_url_leaves_the_webhook_secret_lifecycle_untouched() {
    // The regression contract: Discord must not perturb the mint-once /
    // preserve-across-edit / null-on-clear behaviour of the webhook secret.
    let (_c, pool) = migrated_pool().await;
    let repo = DeliveryConfigRepo::new(pool.clone());
    let def = insert_definition(&pool).await;

    repo.set(
        def,
        &fields(&[], Some("https://hooks.example.com/net"), None),
        Some("secret-one"),
        NOW,
    )
    .await
    .expect("mint");
    // Adding a Discord destination alongside, with a fresh candidate offered.
    let out = repo
        .set(
            def,
            &fields(
                &[],
                Some("https://hooks.example.com/net"),
                Some("https://discord.com/api/webhooks/1/tok"),
            ),
            Some("secret-two"),
            NOW,
        )
        .await
        .expect("add discord");
    assert_eq!(
        out.minted_secret, None,
        "adding a Discord destination does not re-mint or re-reveal the webhook secret"
    );
    assert_eq!(
        repo.webhook_secret(def).await.expect("secret").as_deref(),
        Some("secret-one"),
        "the original secret is preserved verbatim"
    );

    // Clearing the GENERIC webhook nulls its secret and leaves Discord armed.
    repo.set(
        def,
        &fields(&[], None, Some("https://discord.com/api/webhooks/1/tok")),
        None,
        NOW,
    )
    .await
    .expect("clear webhook");
    let got = repo.get(def).await.expect("get").expect("row");
    assert!(
        !got.webhook_secret_set,
        "clearing the webhook nulls its secret"
    );
    assert!(
        got.discord_webhook_url.is_some(),
        "and leaves the Discord destination configured"
    );
}
