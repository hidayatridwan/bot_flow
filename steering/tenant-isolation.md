# Tenant Isolation

> **Scope:** the one invariant that must never break. Read this before touching any query, any
> Qdrant call, or any new endpoint.

`bot_flow` is multi-tenant. Every document, chunk, conversation and message belongs to exactly one
tenant. A single leak across that boundary is a company-ending bug: tenant A reading tenant B's
support documents. Nothing in this repo is more important.

The defense is **three independent layers**. Each one alone would be sufficient on a good day. They
exist together because a good day is not something to depend on.

---

## Layer 1 — Mandatory filter at the app layer (Qdrant)

Every Qdrant query is filtered by `tenant_id`. There is one helper and it is not optional.

```rust
// crates/api/src/handlers.rs
/// MANDATORY filter: restrict the query to points owned by this tenant only.
fn tenant_filter(tenant_id: &str) -> Filter {
    Filter::must([Condition::matches("tenant_id", tenant_id.to_string())])
}
```

The `tenant_id` payload field is registered as a **keyword index with `is_tenant(true)`**, created
in `ensure_collection()` *before any ingest happens*.

**Why `is_tenant=true` and why so early:** it makes Qdrant's HNSW graph filter-aware, so the vector
search is structured per-tenant instead of scanning globally and discarding foreign hits afterwards.
Adding the index after data exists does not retroactively restructure the graph. The ordering is
correctness for performance, not a style choice.

### Before / After

```rust
// ❌ BEFORE — searches every tenant's vectors. Returns other customers' documents.
let response = state.qdrant.query(
    QueryPointsBuilder::new(COLLECTION)
        .query(vector)
        .limit(req.limit)
        .with_payload(true),
).await?;

// ✅ AFTER — the filter is not decoration.
let response = state.qdrant.query(
    QueryPointsBuilder::new(COLLECTION)
        .query(vector)
        .limit(req.limit)
        .filter(tenant_filter(&tenant.tenant_id))   // <-- mandatory
        .with_payload(true),
).await?;
```

**Rule:** if you write `QueryPointsBuilder::new(COLLECTION)` and there is no `.filter(...)` on it,
the change is wrong. There is no read path that legitimately spans tenants.

---

## Layer 2 — Row-Level Security in Postgres

The `documents`, `conversations` and `messages` tables have RLS enabled *and forced*. A query that
forgets `WHERE tenant_id = $1` still cannot see another tenant's rows — the database refuses.

```sql
-- crates/api/migrations/0004_rls_documents.sql
alter table documents enable row level security;
-- FORCE so the policy applies to the table OWNER too.
alter table documents force row level security;

create policy documents_tenant_isolation on documents
    using (tenant_id = current_setting('app.current_tenant', true))
    with check (tenant_id = current_setting('app.current_tenant', true));
```

`current_setting(..., true)` returns `NULL` when the variable is unset, so the comparison is false
and the policy **denies by default**. Forgetting to set the tenant yields zero rows, never all rows.

The variable is set by `db::tenant_tx()`:

```rust
// crates/api/src/db.rs
pub async fn tenant_tx<'a>(db: &'a PgPool, tenant_id: &str)
    -> Result<Transaction<'a, Postgres>, AppError>
{
    let mut tx = db.begin().await?;
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
```

**Why `set_config(_, _, true)` and not `SET LOCAL`:**

1. Only `set_config` accepts a **bound parameter**. `SET LOCAL app.current_tenant = '...'` would
   require string interpolation of the tenant id into SQL — an injection vector in the one place
   that must never have one.
2. The third argument `true` means *transaction-local*: the setting resets automatically on commit
   or rollback. That is what makes it **safe with a connection pool**. A session-level setting would
   leak onto the next request that borrowed the same pooled connection, silently handing it the
   previous tenant's identity.

### Before / After

```rust
// ❌ BEFORE — runs outside a tenant transaction. RLS denies by default, so this returns
//    zero rows and looks like "no documents" instead of failing loudly. Worse, if the
//    connection happens to carry a stale setting, it returns the WRONG tenant's rows.
let rows = sqlx::query("SELECT id, filename FROM documents")
    .fetch_all(&state.db)
    .await?;

// ✅ AFTER — the tenant is bound for the life of the transaction.
let mut tx = db::tenant_tx(&state.db, &tenant.tenant_id).await?;
let rows = sqlx::query("SELECT id, filename FROM documents")
    .fetch_all(&mut *tx)
    .await?;
tx.commit().await?;
```

**Rule:** any query touching `documents`, `conversations` or `messages` goes through `tenant_tx()`.
The tables `tenants` and `api_keys` are *global* — they are the tenancy registry itself and are
correctly queried on the plain pool (see `create_tenant`, `mint_key`, and the `AuthTenant`
extractor).

---

## Layer 3 — The runtime connects as a non-superuser

**Postgres superusers bypass RLS entirely.** A policy on a table is invisible to a superuser
connection. So the process holds two pools with different roles:

| Pool | Role | Connection string | Used for |
| --- | --- | --- | --- |
| Admin | superuser (`bot_flow`) | `DATABASE_URL` | migrations only, then **closed** |
| Runtime | `app_user` (non-superuser) | `APP_DATABASE_URL` | every request query |

```rust
// crates/api/src/main.rs
let admin_db = PgPoolOptions::new().max_connections(1)
    .connect(&config.database_url).await?;
sqlx::migrate!().run(&admin_db).await?;
admin_db.close().await; // done with admin privileges

let db = PgPoolOptions::new().max_connections(5)
    .connect(&config.app_database_url).await?;   // <-- app_user; RLS applies
```

The admin pool is closed immediately after migrations *on purpose*, so it cannot be reached for
later by a well-meaning refactor. `app_user` is created in `0005_app_role.sql`.

**Rule:** never put a runtime query on the admin pool, and never "temporarily" widen `app_user` to
superuser to debug something. That silently disables Layer 2 across the whole application.

---

## The worker isolates too

`crates/worker/src/lifecycle.rs` has its own `tenant_tx` and every status transition runs inside it.
`crates/worker/src/reaper.rs` sweeps **per tenant in a loop**, and its header comment explains why
you cannot shortcut that:

> `documents` has FORCE ROW LEVEL SECURITY and the worker connects as the non-superuser `app_user`,
> so a single cross-tenant UPDATE would match zero rows and appear to succeed.

A cross-tenant bulk `UPDATE` does not error. It reports success and does nothing. If you write a
maintenance query that "works" but changes no rows, suspect RLS before suspecting your `WHERE`.

---

## Layer 0 — the object key

Before any of the above, the storage key binds an upload to a tenant:

```
tenants/{tenant_id}/documents/{document_id}/original.{ext}
```

A presigned URL authorises exactly one key, so the key *is* the authorisation boundary. This is why
`common::key::is_valid_slug` rejects `/`, `..`, uppercase and underscores, and why the same rule is
duplicated as a DB `CHECK` constraint. A tenant slug of `a/../b` would let one tenant's object escape
into another's prefix. `crates/common/src/key.rs` tests this explicitly
(`path_traversal_slugs_are_rejected`, `traversal_key_does_not_parse`).

---

## Checklist for any change

- [ ] New Qdrant search? It has `.filter(tenant_filter(&tenant.tenant_id))`.
- [ ] New query on `documents` / `conversations` / `messages`? It goes through `tenant_tx()`.
- [ ] New table holding tenant data? Add `enable` **and** `force row level security` plus a policy
      in a migration, mirroring `0004`.
- [ ] Touching tenant slugs? `is_valid_slug` in the app **and** the DB `CHECK` stay in sync.
- [ ] Bulk/maintenance query? Loop per tenant, as `reaper.rs` does.

---

## Maintenance

Review this file whenever the schema, the Qdrant collection layout, or the connection-role setup
changes — and at sprint planning. Treat edits here as production changes: they describe a security
boundary. After any refactor, confirm the referenced paths still resolve.

Related: [security-policies.md](security-policies.md), [api-standards.md](api-standards.md),
[rag-pipeline.md](rag-pipeline.md).
