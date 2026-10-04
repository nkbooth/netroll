// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for versioned consent records. Append-only:
//! withdrawal and erasure live elsewhere — this repo exposes no UPDATE or DELETE.

use netroll_domain::consent::ConsentRecord;
use sqlx::PgPool;
use uuid::Uuid;

use super::utc_from_millis;

/// Postgres repository for `account_consents`.
#[derive(Clone)]
pub struct ConsentRepo {
    pool: PgPool,
}

impl ConsentRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persists a domain-decided consent record for `account_id`.
    ///
    /// Idempotent by design: `ON CONFLICT DO NOTHING` on the
    /// `(account_id, terms_version)` unique key makes a double-click (or a
    /// concurrent duplicate POST) converge on the one existing row — the
    /// first recorded timestamp is the acceptance of record. Returns
    /// whether this call was the one that actually inserted the row, so
    /// callers can tell a real acceptance from a no-op duplicate.
    pub async fn record(
        &self,
        account_id: Uuid,
        record: &ConsentRecord,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query!(
            "INSERT INTO account_consents (id, account_id, terms_version, consented_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (account_id, terms_version) DO NOTHING",
            Uuid::now_v7(),
            account_id,
            record.terms_version,
            utc_from_millis(record.consented_at_millis),
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Every terms version `account_id` has consented to; an account with no
    /// rows reads as unconsented.
    pub async fn consented_versions(&self, account_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT terms_version FROM account_consents WHERE account_id = $1",
            account_id,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.terms_version).collect())
    }
}
