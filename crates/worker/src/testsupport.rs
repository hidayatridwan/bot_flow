//! Shared setup for the worker's integration tests (phase 9b). Compiled only under `cfg(test)`.
//!
//! **Why these live in-crate rather than in `crates/worker/tests/`,** which is where CLAUDE.md
//! otherwise sends anything needing Postgres: the behaviour worth testing is the reaper's, and its
//! seams — `sweep_one`, `finish_deletions`, `PROCESSING_LEASE` — are all private. `reaper.rs`
//! exposes only `spawn`, a fire-and-forget loop with no deterministic seam. Reaching them from
//! `tests/` would mean making four items `pub` for no reason but the test, which is exactly what
//! "do not widen visibility just to test something" forbids.
//!
//! So the rule is one rule applied twice: **where the seam is real API, split the lib; where it
//! would exist only for the test, test in-crate.** `crates/api` took the first branch because
//! `app`/`build_state` are its composition root and `main` consumes them. The worker takes the
//! second. The asymmetry is deliberate.
//!
//! The guards below are duplicated from the api harness rather than shared. A `testkit` crate would
//! have to own the migrations (coupling it to `crates/api/migrations`) or be dev-depended on by a
//! binary crate; neither beats thirty lines.

use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use uuid::Uuid;

const TEST_DB_NAME: &str = "bot_flow_test";

/// See the api harness's `guard_not_superuser` for the full argument. In short: superusers bypass
/// RLS, so a worker test on the wrong role would exercise none of the tenant scoping it depends on,
/// and `claim`'s "no such document for this tenant" branch would become unreachable.
async fn guard_not_superuser(db: &PgPool) {
    let is_super: bool = sqlx::query("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
        .fetch_one(db)
        .await
        .expect("failed to read current_user's rolsuper")
        .get(0);
    assert!(
        !is_super,
        "SUPERUSER TRAP: the worker harness connected as a superuser, which bypasses RLS. \
         Point TEST_APP_DATABASE_URL at app_user."
    );
}

fn guard_test_database(url: &str) {
    let name = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    assert!(
        name.ends_with("_test"),
        "refusing to run against database {name:?}: worker tests write document rows directly, \
         so they only run against a database whose name ends in `_test` (expected {TEST_DB_NAME})."
    );
}

fn with_db_name(url: &str, name: &str) -> String {
    let (prefix, _) = url.rsplit_once('/').expect("malformed Postgres URL");
    format!("{prefix}/{name}")
}

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
        with_db_name(
            &std::env::var("DATABASE_URL").expect("DATABASE_URL is not set"),
            TEST_DB_NAME,
        )
    })
}

fn app_url() -> String {
    std::env::var("TEST_APP_DATABASE_URL").unwrap_or_else(|_| {
        with_db_name(
            &std::env::var("APP_DATABASE_URL").expect("APP_DATABASE_URL is not set"),
            TEST_DB_NAME,
        )
    })
}

/// An `app_user` pool against the test database, with both guards applied.
///
/// `max_connections(4)` is load-bearing for the concurrent-claim test: two `claim` calls race a
/// `SELECT … FOR UPDATE`, and on a single-connection pool the second would wait for a connection
/// that the first cannot release until it commits — a deadlock that looks like a hang, not a bug.
pub async fn test_pool() -> PgPool {
    dotenvy::dotenv().ok();

    let admin = admin_url();
    let app = app_url();
    guard_test_database(&admin);
    guard_test_database(&app);

    // The worker runs the api's migrations rather than assuming another test binary got there
    // first. The path coupling is real and deliberate: the alternative is an ordering dependency
    // between test binaries that cargo does not promise, whose failure mode is a confusing
    // "relation does not exist" rather than a clear one. sqlx takes an advisory lock, so two
    // binaries doing this concurrently serialise safely.
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&with_db_name(&admin, "postgres"))
            .await
            .expect("failed to connect to the postgres maintenance database");
        match sqlx::query(&format!("CREATE DATABASE {TEST_DB_NAME}"))
            .execute(&pool)
            .await
        {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P04") => {}
            Err(e) => panic!("failed to create {TEST_DB_NAME}: {e}"),
        }
        pool.close().await;

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin)
            .await
            .expect("failed to connect as the migration role");
        sqlx::migrate!("../api/migrations")
            .run(&admin_pool)
            .await
            .expect("failed to migrate the test database");
        admin_pool.close().await;
    })
    .await;

    let db = PgPoolOptions::new()
        .max_connections(4)
        .connect(&app)
        .await
        .expect("failed to connect as app_user — is docker compose up?");
    guard_not_superuser(&db).await;
    db
}

/// A tenant with a unique id. `tenants` has no RLS — it is the tenancy registry itself.
pub async fn seed_tenant(db: &PgPool) -> String {
    let id = format!("t{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'worker test')")
        .bind(&id)
        .execute(db)
        .await
        .expect("failed to insert tenant");
    id
}

/// A document row in a chosen state, written under tenant context because `documents` has RLS.
///
/// `processing_started_at` is a parameter rather than always `now()` because it is the reaper's
/// clock: the whole deletion-sweep question is whether a lease has elapsed.
pub async fn seed_document(
    db: &PgPool,
    tenant_id: &str,
    status: &str,
    processing_started_ago: Option<&str>,
) -> Uuid {
    let id = Uuid::new_v4();
    let object_key = format!("tenants/{tenant_id}/documents/{id}/original.txt");

    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .unwrap();

    // The interval is interpolated, not bound, exactly as reaper.rs does with its own constants —
    // test-controlled input, and `interval $1` is not bindable in this position.
    let started = match processing_started_ago {
        Some(ago) => format!("now() - interval '{ago}'"),
        None => "null".to_string(),
    };
    sqlx::query(&format!(
        "INSERT INTO documents (id, tenant_id, filename, object_key, status, processing_started_at)
         VALUES ($1, $2, 'test.txt', $3, $4, {started})"
    ))
    .bind(id)
    .bind(tenant_id)
    .bind(&object_key)
    .bind(status)
    .execute(&mut *tx)
    .await
    .expect("failed to insert document");
    tx.commit().await.unwrap();
    id
}

/// Read one document's status back, under tenant context. `None` if the row is gone.
/// The classified failure reason, read under tenant context for the same reason as [`status_of`]:
/// `documents` is RLS-FORCED, so a read on the plain pool matches zero rows and *reports success*.
pub async fn failure_reason_of(db: &PgPool, tenant_id: &str, id: Uuid) -> Option<String> {
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let row = sqlx::query("SELECT failure_reason FROM documents WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    row.and_then(|r| r.get("failure_reason"))
}

pub async fn status_of(db: &PgPool, tenant_id: &str, id: Uuid) -> Option<String> {
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let row = sqlx::query("SELECT status FROM documents WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    row.map(|r| r.get("status"))
}
