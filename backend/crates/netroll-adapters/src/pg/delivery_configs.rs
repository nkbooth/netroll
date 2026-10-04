// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Per-net delivery configuration. The `webhook_secret` is minted ONCE and
//! survives later URL edits via `COALESCE(existing, candidate)`; clearing the
//! URL nulls it. Stored RECOVERABLE because delivery must read it back to SIGN,
//! never carried into the HTTP-facing read model, and read with its URL from
//! the SAME row, or a signature would span two config generations.

use netroll_domain::net::delivery::DeliveryConfigFields;
use sqlx::PgPool;
use uuid::Uuid;

use super::utc_from_millis;

/// Postgres repository for `net_delivery_configs`.
#[derive(Clone)]
pub struct DeliveryConfigRepo {
    pool: PgPool,
}

/// The HTTP-facing read model of a net's delivery config. The plaintext
/// webhook secret is DELIBERATELY absent — only a boolean that one is set
/// reaches a client (the write-only-secret posture). Delivery fetches the
/// plaintext through [`DeliveryConfigRepo::webhook_secret`], never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryConfig {
    /// The normalized delivery addresses (possibly empty — "delivery off").
    pub emails: Vec<String>,
    /// The configured webhook URL, or `None`.
    pub webhook_url: Option<String>,
    /// Whether a webhook HMAC secret is stored — never the secret itself.
    pub webhook_secret_set: bool,
    /// The configured Discord channel-webhook URL, or `None`.
    ///
    /// Present in this read model exactly like `webhook_url` and unlike
    /// `webhook_secret`: it mirrors `webhook_url` exactly, because two adjacent
    /// URL fields with different read semantics is how the next author gets one
    /// of them wrong. The owner supplied it and only
    /// the owner can read it back.
    pub discord_webhook_url: Option<String>,
}

/// The outcome of a [`DeliveryConfigRepo::set`]: the plaintext secret IFF a
/// fresh one was minted on THIS call, so the handler can reveal it exactly once.
/// `None` when no webhook is configured or a secret already existed
/// (re-saving a URL never re-reveals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetOutcome {
    /// The freshly minted secret to reveal once, or `None`.
    pub minted_secret: Option<String>,
}

impl DeliveryConfigRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reads the delivery config for `definition_id`, or `None` when no row
    /// exists (a net that has never configured delivery). The secret plaintext
    /// never leaves storage — only `webhook_secret_set`.
    pub async fn get(&self, definition_id: Uuid) -> Result<Option<DeliveryConfig>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT delivery_emails,
                      webhook_url,
                      discord_webhook_url,
                      (webhook_secret IS NOT NULL) AS "webhook_secret_set!"
               FROM net_delivery_configs
               WHERE definition_id = $1"#,
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| DeliveryConfig {
            emails: r.delivery_emails,
            webhook_url: r.webhook_url,
            webhook_secret_set: r.webhook_secret_set,
            discord_webhook_url: r.discord_webhook_url,
        }))
    }

    /// Upserts the 1:1 delivery config for `definition_id`.
    ///
    /// `minted_secret` is a freshly generated candidate the caller supplies when
    /// the config carries a webhook URL; it is applied ONLY on the first webhook
    /// set and preserved verbatim across later URL edits (the `COALESCE`).
    /// Clearing the webhook (`fields.webhook_url == None`) nulls both the URL and
    /// the secret. Returns a [`SetOutcome`] whose `minted_secret` is `Some` only
    /// when this call actually minted a new secret — the reveal-once signal.
    pub async fn set(
        &self,
        definition_id: Uuid,
        fields: &DeliveryConfigFields,
        minted_secret: Option<&str>,
        now_millis: u64,
    ) -> Result<SetOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        // The candidate is only meaningful alongside a webhook URL; with no URL
        // there is nothing to sign, so no secret is stored.
        let insert_secret: Option<&str> = fields.webhook_url.as_deref().and(minted_secret);

        let row = sqlx::query!(
            r#"INSERT INTO net_delivery_configs
                   (definition_id, delivery_emails, webhook_url, webhook_secret,
                    discord_webhook_url, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $6, $5, $5)
               ON CONFLICT (definition_id) DO UPDATE SET
                   delivery_emails = excluded.delivery_emails,
                   webhook_url = excluded.webhook_url,
                   -- Preserve an existing secret across a URL edit (mint once);
                   -- drop it when the webhook itself is cleared.
                   webhook_secret = CASE
                       WHEN excluded.webhook_url IS NULL THEN NULL
                       ELSE COALESCE(net_delivery_configs.webhook_secret, excluded.webhook_secret)
                   END,
                   -- A plain replace: Discord mints nothing, so there is no
                   -- COALESCE to preserve and a PUT that omits the field
                   -- clears it (the same replace semantics `webhook_url` and
                   -- `delivery_emails` already have).
                   discord_webhook_url = excluded.discord_webhook_url,
                   updated_at = excluded.updated_at
               RETURNING webhook_secret"#,
            definition_id,
            &fields.emails,
            fields.webhook_url.as_deref(),
            insert_secret,
            now,
            fields.discord_webhook_url.as_deref(),
        )
        .fetch_one(&self.pool)
        .await?;

        // A fresh mint happened iff the stored secret is exactly the candidate
        // we passed (a preserved older secret is a different random value; a
        // cleared webhook stored NULL).
        let minted = match (minted_secret, row.webhook_secret.as_deref()) {
            (Some(candidate), Some(stored)) if candidate == stored => Some(candidate.to_owned()),
            _ => None,
        };
        Ok(SetOutcome {
            minted_secret: minted,
        })
    }

    /// Clears the delivery config for `definition_id` — deletes the row, the
    /// "delivery off" state. Idempotent: clearing a config-less net is a
    /// no-op (the `clear_schedule` posture).
    pub async fn clear(&self, definition_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM net_delivery_configs WHERE definition_id = $1",
            definition_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delivery-only accessor for the stored plaintext webhook secret.
    ///
    /// Returns `None` when the net has no config, or a config with no webhook.
    /// Deliberately separate from [`get`](Self::get) so the plaintext secret
    /// can never leak into the HTTP-facing [`DeliveryConfig`] read model.
    /// delivery path uses [`Self::webhook_target`] instead (URL +
    /// secret paired from one read); this accessor is kept for its own
    /// focused lifecycle tests.
    pub async fn webhook_secret(&self, definition_id: Uuid) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT webhook_secret FROM net_delivery_configs WHERE definition_id = $1",
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.webhook_secret))
    }

    /// Reads the current webhook URL and secret TOGETHER, in ONE query.
    ///
    /// Delivery must sign and POST a URL and secret drawn from the SAME config
    /// generation. Reading the URL from an earlier snapshot (e.g. `get`, taken
    /// before a slow email fan-out) and the secret from a later, separate query
    /// left a window where an owner's mid-flight config edit — clearing the
    /// webhook and reconfiguring a new URL, which mints a new secret — could
    /// pair the FRESH secret with the STALE (changed or removed) URL. This
    /// accessor closes that window: both columns come from one row read,
    /// immediately before use. Returns `None` when there is no config row, or
    /// the row has no webhook URL configured (delivery-off or email-only); the
    /// inner `Option<String>` is `None` only in the data-anomaly case (a
    /// webhook URL with no stored secret — the caller must record and skip,
    /// never fabricate one).
    pub async fn webhook_target(
        &self,
        definition_id: Uuid,
    ) -> Result<Option<(String, Option<String>)>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT webhook_url, webhook_secret FROM net_delivery_configs WHERE definition_id = $1",
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| {
            let url = r.webhook_url?;
            Some((url, r.webhook_secret))
        }))
    }

    /// Re-reads the Discord webhook URL immediately before use.
    ///
    /// Separate from [`Self::webhook_target`] rather than a third member of its
    /// tuple: a positional `(String, Option<String>)` is already at the limit of
    /// what a call site can destructure without mis-ordering, and Discord has no
    /// secret to pair.
    ///
    /// It exists for the same REASON as `webhook_target` though, narrowed: the
    /// deliverer's top-level config snapshot is taken before the fold and before
    /// the concurrent fan-out, so a URL read from it may be one the owner has
    /// since cleared. There is no secret-vs-URL generation race here (Discord
    /// has no secret), but there IS the plain stale-URL one — announcing a closed
    /// net into a channel the owner just disconnected. Returns `None` when there
    /// The CURRENT address list for a definition, read fresh at send time.
    ///
    /// The email leg decided "was this address cleared?" from the snapshot the
    /// executor took at the top of a delivery, while the webhook and Discord
    /// legs each re-read their own target — so an address removed mid-delivery
    /// was still mailed while an equivalently-timed webhook removal was
    /// skipped. The story's own ruling is that targets are re-resolved at send,
    /// every attempt; this is the email leg's half of it.
    pub async fn delivery_emails(&self, definition_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT delivery_emails FROM net_delivery_configs WHERE definition_id = $1"#,
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.delivery_emails).unwrap_or_default())
    }

    /// is no config row or no Discord destination on it.
    pub async fn discord_target(&self, definition_id: Uuid) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT discord_webhook_url FROM net_delivery_configs WHERE definition_id = $1",
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| r.discord_webhook_url))
    }
}
