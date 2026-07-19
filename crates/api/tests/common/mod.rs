//! The integration harness.
//!
//! Every test here drives the **real** `Router` over a **real** database, because the guarantees
//! worth testing live in places a unit test cannot reach: RLS is enforced by Postgres, the tenant
//! filter by Qdrant, and `Actor::from_request_parts` needs a database to resolve a token at all.
//!
//! Two things make this harness trustworthy rather than merely green — see [`guard_test_database`]
//! and [`guard_not_superuser`]. Read those before adding a test.

#![allow(dead_code)] // each test binary uses a different subset of this module

use std::sync::Arc;

use api::config::Config;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use qdrant_client::qdrant::{Condition, DeletePointsBuilder, Filter};
use qdrant_client::Qdrant;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use tower::ServiceExt; // for `oneshot`

mod gateway;
pub use gateway::{fake_embedding, FakeGateway};

/// Name of the database these tests are allowed to touch. Never the dev database: this harness
/// creates and deletes tenants, and a stray truncation against real dev data is a bad afternoon.
const TEST_DB_NAME: &str = "bot_flow_test";

/// **A harness that connects as a superuser proves nothing, and passes.**
///
/// Postgres superusers bypass RLS entirely — that is why migration 0005 created `app_user` and why
/// the runtime connects as it (isolation layer 3). A test on the wrong credential would assert
/// "tenant B cannot see tenant A's document", watch it pass, and have tested *nothing*, because the
/// query it ran was never subject to the policy. Green, meaningless, and permanently reassuring in
/// the worst way.
///
/// So this is a runtime assertion rather than a review checklist. Verifying it once by hand leaves
/// it re-breakable forever by anyone editing `.env`; asserting it on every run does not.
async fn guard_not_superuser(db: &PgPool) {
    let is_super: bool = sqlx::query("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
        .fetch_one(db)
        .await
        .expect("failed to read current_user's rolsuper")
        .get(0);

    assert!(
        !is_super,
        "SUPERUSER TRAP: the harness connected as a superuser, which bypasses RLS entirely. \
         Every isolation assertion in this suite would pass without testing anything. \
         Point TEST_APP_DATABASE_URL at app_user, not at the migration role."
    );
}

/// Refuse to run against anything but the dedicated test database.
fn guard_test_database(url: &str) {
    let db_name = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    assert!(
        db_name.ends_with("_test"),
        "refusing to run against database {db_name:?}: these tests create and delete tenants, \
         so they only run against a database whose name ends in `_test` (expected {TEST_DB_NAME})."
    );
}

/// Swap the database name in a Postgres URL, preserving everything else.
fn with_db_name(url: &str, name: &str) -> String {
    match url.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{name}"),
        None => panic!("malformed Postgres URL: {url}"),
    }
}

fn test_admin_url() -> String {
    std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
        with_db_name(
            &std::env::var("DATABASE_URL").expect("DATABASE_URL is not set"),
            TEST_DB_NAME,
        )
    })
}

fn test_app_url() -> String {
    std::env::var("TEST_APP_DATABASE_URL").unwrap_or_else(|_| {
        with_db_name(
            &std::env::var("APP_DATABASE_URL").expect("APP_DATABASE_URL is not set"),
            TEST_DB_NAME,
        )
    })
}

/// Create `bot_flow_test` and migrate it — once per test binary.
async fn ensure_test_database() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let admin_url = test_admin_url();
        guard_test_database(&admin_url);

        // CREATE DATABASE cannot run inside a transaction, nor from a connection to the database
        // being created — so connect to the `postgres` maintenance DB.
        let maintenance = with_db_name(&admin_url, "postgres");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&maintenance)
            .await
            .expect("failed to connect to the postgres maintenance database");

        match sqlx::query(&format!("CREATE DATABASE {TEST_DB_NAME}"))
            .execute(&pool)
            .await
        {
            Ok(_) => {}
            // 42P04 = duplicate_database. Test binaries race here; losing the race is fine.
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P04") => {}
            Err(e) => panic!("failed to create {TEST_DB_NAME}: {e}"),
        }
        pool.close().await;

        // The same code path production runs. Migration 0005 finds `app_user` already present
        // (roles are cluster-wide, so its pg_roles guard short-circuits) but re-issues the GRANTs,
        // which are per-database — that is what gives app_user privileges on bot_flow_test.
        api::run_migrations(&admin_url)
            .await
            .expect("failed to migrate the test database");
    })
    .await;
}

/// How old a test tenant must be before the startup sweep will erase it.
///
/// Long enough that it cannot be a tenant belonging to a run in progress, short enough that debris
/// does not accumulate for days. Absolute time, not "this run" — see [`sweep_stale_test_tenants`].
const STALE_AFTER: &str = "1 hour";

/// Erase vectors belonging to test tenants from *previous* runs.
///
/// **`TestApp::cleanup()` only runs when a test passes.** A panicking test — which is to say, every
/// failing test, and every deliberate break-verification — skips it and strands its points in the
/// shared collection forever, because `/ingest` writes them with random ids and no `document_id`,
/// so nothing in the product can ever remove them. Per-test teardown alone therefore leaks by
/// construction, and it did: four tenants survived this suite's own break table.
///
/// Sweeping by **age** rather than by "everything in the test database" is deliberate. A truncate
/// would be simpler but would depend on cargo running test binaries sequentially — true today,
/// nowhere promised, and it would corrupt a concurrent run silently rather than loudly.
async fn sweep_stale_test_tenants(db: &PgPool, qdrant: &Qdrant) {
    let rows = sqlx::query(&format!(
        "SELECT id FROM tenants WHERE created_at < now() - interval '{STALE_AFTER}'"
    ))
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for row in rows {
        let tenant_id: String = row.get("id");
        let _ = delete_tenant_points(qdrant, &tenant_id).await;
        let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(&tenant_id)
            .execute(db)
            .await;
    }
}

/// Remove every vector belonging to one tenant from the shared collection.
async fn delete_tenant_points(
    qdrant: &Qdrant,
    tenant_id: &str,
) -> Result<(), qdrant_client::QdrantError> {
    qdrant
        .delete_points(
            // `common::COLLECTION`, never a literal: this said "documents" and kept saying it after
            // phase 10 renamed the collection, so teardown silently deleted from nothing.
            DeletePointsBuilder::new(api::COLLECTION)
                .points(Filter::must([Condition::matches(
                    "tenant_id",
                    tenant_id.to_string(),
                )]))
                .wait(true),
        )
        .await?;
    Ok(())
}

/// A running API — real router, real state, real database — plus everything a test needs to drive it.
pub struct TestApp {
    router: Router,
    /// Connected as `app_user`, for setup and inspection. Subject to RLS, like the handlers.
    pub db: PgPool,
    pub admin_key: String,
    pub gateway: FakeGateway,
    qdrant: Arc<Qdrant>,
    /// Tenant ids this fixture created, cleaned out of the shared Qdrant collection on teardown.
    tenants: std::sync::Mutex<Vec<String>>,
    /// **Must outlive `router`.** Dropping the Connection closes the Channel inside `AppState`, and
    /// the only symptom is /health reporting rabbitmq down — nothing else fails, loudly or at all.
    _amqp: lapin::Connection,
}

impl TestApp {
    pub async fn new() -> Self {
        dotenvy::dotenv().ok();
        ensure_test_database().await;

        let gateway = FakeGateway::start().await;

        // Built as a struct literal rather than from_env(): mutating process env from parallel
        // tests is a data race, and adding a builder to Config would be production churn for tests.
        let app_url = test_app_url();
        guard_test_database(&app_url);
        let config = Config {
            database_url: test_admin_url(),
            app_database_url: app_url,
            qdrant_url: env("QDRANT_URL"),
            bind_addr: "127.0.0.1:0".to_string(),
            // Both gateways point at the in-process stub: these tests must never make a billed call.
            llm_base_url: gateway.base_url(),
            embedding_base_url: gateway.base_url(),
            // Deliberately not a real credential shape. If a misconfiguration ever escaped this
            // harness to a real gateway, it 401s instead of spending money.
            llm_api_key: "test-key-not-a-real-credential".to_string(),
            embedding_api_key: "test-key-not-a-real-credential".to_string(),
            llm_model: "fake-model".to_string(),
            embedding_model: "fake-embedding-model".to_string(),
            s3_endpoint: env("S3_ENDPOINT"),
            s3_public_endpoint: std::env::var("S3_PUBLIC_ENDPOINT")
                .unwrap_or_else(|_| env("S3_ENDPOINT")),
            presign_ttl_secs: 900,
            max_upload_bytes: 25 * 1024 * 1024,
            s3_bucket: std::env::var("S3_BUCKET").unwrap_or_else(|_| "documents".to_string()),
            s3_access_key: env("S3_ACCESS_KEY"),
            s3_secret_key: env("S3_SECRET_KEY"),
            s3_region: std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
            rabbitmq_url: env("RABBITMQ_URL"),
            redis_url: env("REDIS_URL"),
            // The fake embedder scores an exact match at ~1.0 and unrelated text near 0.0, so this
            // floor is nowhere near either. It is not tuned; it is just out of the way.
            rag_score_threshold: 0.5,
            // A table-driven auth test fires many requests as one tenant and must not 429 itself.
            rate_limit_per_minute: 100_000,
            admin_api_key: format!("admin-{}", uuid::Uuid::new_v4().simple()),
            session_ttl_secs: 3600,
        };

        let (state, _amqp) = api::build_state(&config)
            .await
            .expect("failed to build AppState — are the five compose services up?");

        let db = state.db.clone();
        let qdrant = state.qdrant.clone();
        guard_not_superuser(&db).await;

        // Once per test binary: clear debris left by runs that panicked before their teardown.
        static SWEPT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
        SWEPT
            .get_or_init(|| sweep_stale_test_tenants(&db, &qdrant))
            .await;

        Self {
            router: api::app(state),
            db,
            admin_key: config.admin_api_key.clone(),
            gateway,
            qdrant,
            tenants: std::sync::Mutex::new(Vec::new()),
            _amqp,
        }
    }

    /// Drive one request through the real router. No socket, no port, no teardown race — but every
    /// layer still runs, which is the whole point: the extractors are where the bugs are.
    pub async fn request(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = self
            .router
            .clone() // oneshot consumes; Router is Clone
            .oneshot(req)
            .await
            .expect("router failed to respond");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("failed to read response body");
        // Not every response is JSON (204, text/plain errors); those become Null rather than a panic.
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    /// Create a tenant with a unique id and return `(tenant_id, sk_key)`.
    ///
    /// Unique per test because `cargo test` runs in parallel threads against one database and one
    /// Qdrant collection — never assume an empty table or an empty collection.
    pub async fn create_tenant(&self) -> (String, String) {
        let tenant_id = format!("t{}", uuid::Uuid::new_v4().simple());
        let (status, body) = self
            .request(
                json_request("POST", "/admin/tenants", &self.admin_key)
                    .body(Body::from(
                        serde_json::json!({"id": tenant_id, "name": "Test Tenant"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "create_tenant failed: {body}");

        self.tenants.lock().unwrap().push(tenant_id.clone());
        let key = body["api_key"]
            .as_str()
            .expect("no api_key in response")
            .to_string();
        (tenant_id, key)
    }

    /// Mint an additional key for a tenant.
    pub async fn mint_key(&self, tenant_id: &str, kind: &str, origins: &[&str]) -> String {
        let (status, body) = self
            .request(
                json_request(
                    "POST",
                    &format!("/admin/tenants/{tenant_id}/keys"),
                    &self.admin_key,
                )
                .body(Body::from(
                    serde_json::json!({
                        "kind": kind,
                        "label": "test",
                        "allowed_origins": origins,
                    })
                    .to_string(),
                ))
                .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "mint_key failed: {body}");
        body["api_key"].as_str().expect("no api_key").to_string()
    }

    /// Register a fresh tenant + owner account and return `(tenant_id, sess_ token)`.
    ///
    /// Self-serve registration creates its *own* tenant — it is the one path that does so without
    /// the admin key — so this cannot attach a session to a tenant made by `create_tenant`. For
    /// gate tests that is irrelevant: `require_management()` asks what kind of principal this is,
    /// never which tenant it belongs to.
    pub async fn register_session(&self) -> (String, String) {
        let tenant_id = format!("t{}", uuid::Uuid::new_v4().simple());
        let (status, body) = self
            .request(
                Request::builder()
                    .method("POST")
                    .uri("/auth/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "email": format!("owner@{tenant_id}.test"),
                            "password": "correct horse battery staple",
                            "tenant_name": "Test Tenant",
                            // Explicit: the derived slug would come from tenant_name and collide
                            // with every other run.
                            "slug": tenant_id,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "register failed: {body}");

        self.tenants.lock().unwrap().push(tenant_id.clone());
        let token = body["session_token"]
            .as_str()
            .expect("no session_token")
            .to_string();
        (tenant_id, token)
    }

    /// Plant an indexed chunk directly, as the worker would have written it.
    ///
    /// **Deliberately not via `POST /ingest`.** Since phase 11 that route stores an object and lets
    /// the worker index it, and the harness runs no worker — but more importantly, a test of the
    /// *search filter* should not depend on the *ingestion path* at all. Writing the point here is
    /// both what makes the test work and what makes it test one thing.
    ///
    /// The vector comes from the same content-addressed function the fake gateway uses, so a query
    /// for this exact text scores ~1.0 and anything else ~0.0 — which is what makes a denial
    /// unambiguous rather than merely empty.
    pub async fn plant_chunk(&self, tenant_id: &str, text: &str) -> String {
        self.plant_chunk_for(tenant_id, &uuid::Uuid::new_v4().to_string(), text)
            .await
    }

    /// The same, for a document that already has a row — so `DELETE /documents/{id}` can reach it.
    ///
    /// Needed because the harness runs no worker: `/ingest` creates the row and stores the object,
    /// but nothing indexes it, so a test about *retrieval and erasure together* has to stand in for
    /// the indexing step.
    pub async fn plant_chunk_for(&self, tenant_id: &str, document_id: &str, text: &str) -> String {
        let document_id = document_id.to_string();
        self.qdrant
            .upsert_points(
                qdrant_client::qdrant::UpsertPointsBuilder::new(
                    api::COLLECTION,
                    vec![qdrant_client::qdrant::PointStruct::new(
                        uuid::Uuid::new_v4().to_string(),
                        fake_embedding(text),
                        [
                            ("text", text.into()),
                            ("tenant_id", tenant_id.into()),
                            ("document_id", document_id.clone().into()),
                            ("chunk_index", 0i64.into()),
                        ],
                    )],
                )
                .wait(true),
            )
            .await
            .expect("failed to plant a chunk");
        document_id
    }

    /// Run a read against a tenant-scoped (RLS) table.
    ///
    /// **`self.db` alone is not enough.** `documents`, `conversations` and `messages` are RLS-forced
    /// and the harness connects as `app_user`, so a query without `app.current_tenant` set matches
    /// zero rows and *reports success* — the corollary trap, arriving in a test rather than in
    /// production. A test that read directly got 0 and looked like a missing feature.
    pub async fn count_as_tenant(&self, tenant_id: &str, sql: &str, bind: Value) -> i64 {
        use sqlx::Row;
        let mut tx = self.db.begin().await.unwrap();
        sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
            .bind(tenant_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let n: i64 = sqlx::query(sql)
            .bind(bind)
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .get(0);
        tx.commit().await.unwrap();
        n
    }

    /// `POST /search` with the given credential.
    pub async fn search(&self, token: &str, query: &str) -> (StatusCode, Value) {
        self.request(
            json_request("POST", "/search", token)
                .body(Body::from(
                    serde_json::json!({"query": query, "limit": 10}).to_string(),
                ))
                .unwrap(),
        )
        .await
    }

    /// Remove this fixture's vectors from the shared collection.
    ///
    /// Not merely tidiness: `/ingest` writes points with random ids and no `document_id` payload,
    /// so **nothing in the product can ever delete them** (the largest known piece of debt in the
    /// system). Without this, every run would leak points into the shared collection permanently.
    ///
    /// Call it at the end of a test, but do not rely on it alone — a panicking test never reaches
    /// this line. `sweep_stale_test_tenants` is the backstop that makes the leak self-healing.
    pub async fn cleanup(&self) {
        let tenants = self.tenants.lock().unwrap().clone();
        for tenant_id in tenants {
            // Loud on failure: a teardown that silently stopped working would leak indefinitely,
            // and the only visible symptom would be a slowly growing collection nobody attributes
            // to the test suite.
            delete_tenant_points(&self.qdrant, &tenant_id)
                .await
                .unwrap_or_else(|e| panic!("failed to clean up tenant {tenant_id}: {e}"));
        }
    }
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} is not set — is the root .env present?"))
}

/// A JSON request carrying a bearer token.
pub fn json_request(method: &str, uri: &str, token: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
}
