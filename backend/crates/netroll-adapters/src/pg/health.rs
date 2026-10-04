// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The database half of the liveness probe.

use sqlx::PgPool;

/// Answers whether the database is reachable through the shared pool.
#[derive(Clone)]
pub struct HealthRepo {
    pool: PgPool,
}

impl HealthRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Round-trips a constant query, so a pool that cannot reach Postgres
    /// fails here rather than on the next real request.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
