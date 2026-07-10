use sqlx::{PgPool, Postgres, Transaction};

use crate::error::AppError;

/// Begin a transaction with the RLS tenant variable set for its duration.
/// `set_config(_, _, true)` is transaction-local (auto-resets on commit/rollback) — the
/// pool-safe way to scope RLS. We use set_config (not `SET LOCAL`) because only it accepts a
/// bound parameter, so the tenant id can never be SQL-injected.
pub async fn tenant_tx<'a>(
    db: &'a PgPool,
    tenant_id: &str,
) -> Result<Transaction<'a, Postgres>, AppError> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
