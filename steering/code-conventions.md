# Code Conventions

> **Scope:** how code in this repo is shaped. Rust idiom, module layout, error handling, comments.

**This is a Rust Cargo workspace.** There is no `package.json`, no npm, no eslint, no prettier. The
only lockfile is `Cargo.lock`. If you find yourself reaching for a JS tool, stop.

---

## Workspace layout

```
crates/
  api/       Axum HTTP server        (binary)
  worker/    RabbitMQ consumer       (binary)
  common/    shared object-key contract (library)
sidecar/     Python text extractor   (pypdf)
widget/      embeddable chat widget  (vanilla JS, no build step)
```

`crates/common` exists for exactly one reason: the API writes object keys and the worker reads them.
A shared crate makes drift a compile error rather than a silent mismatch in production. Anything both
binaries must agree on belongs there.

### Dependencies are declared once

The root `Cargo.toml` holds `[workspace.dependencies]`. Member crates reference them:

```toml
# ❌ BEFORE — crates/worker/Cargo.toml pins its own version.
[dependencies]
uuid = { version = "1", features = ["v4", "serde"] }   # note: no "v5" — worker needs it

# ✅ AFTER — one source of truth, no version skew between binaries.
[dependencies]
uuid = { workspace = true }
```

**Why:** the API and the worker embed with the same model and hash the same keys. Two versions of
`uuid` or `fastembed` across the two binaries would produce different ids or different vectors, and
the failure would surface as bad search results, not as a build error.

New dependency? Add it to the root `[workspace.dependencies]` first.

---

## Naming

Standard Rust, no local dialect:

| Thing | Style | Examples |
| --- | --- | --- |
| modules / files | `snake_case` | `rate_limit.rs`, `conversation.rs`, `lifecycle.rs` |
| functions / variables | `snake_case` | `tenant_tx`, `hash_key`, `point_id` |
| types / traits / variants | `CamelCase` | `AppError`, `AuthTenant`, `Claim::Proceed` |
| constants | `SCREAMING_SNAKE_CASE` | `COLLECTION`, `RAG_SYSTEM_PROMPT`, `NO_ANSWER`, `HISTORY_LIMIT` |

**One concern per module.** `rate_limit.rs` is the rate limiter and nothing else. `handlers.rs` is
the exception by convention — all HTTP handlers live together — but their helpers (`conversation`,
`upload`, `llm`, `storage`) do not.

A magic value used more than once becomes a named constant. `NO_ANSWER` and `HISTORY_LIMIT` exist so
that the string a customer reads and the number of remembered turns each have exactly one definition.

---

## Error handling

**Internally:** `anyhow::Error` with `.context(...)` and `?` propagation.

```rust
let admin_db = PgPoolOptions::new().max_connections(1)
    .connect(&config.database_url).await
    .context("failed to connect to Postgres (admin)")?;
```

`.context()` on every fallible boundary. A bare `?` on a connect call produces `Connection refused`
with no indication of *which* of the five services refused.

**At the HTTP edge:** `AppError`. `?` yields a 500; caller mistakes are constructed explicitly with
`AppError::client(status, msg)`. Full rules in [api-standards.md](api-standards.md).

**In the worker:** every failure is classified.

```rust
/// Distinguishes "this will never work" from "try again later".
enum Failure {
    Fatal(anyhow::Error),      // ack — discard the poison message
    Retryable(anyhow::Error),  // nack + requeue — the delivery limit dead-letters it
}
```

```rust
// ❌ BEFORE — an unparseable event nacks, requeues, and cycles until the delivery
//    limit burns down. Five wasted attempts at something that can never succeed.
let obj = event::parse(body)?;

// ✅ AFTER — the classification is the point.
let Some(obj) = event::parse(body).map_err(Fatal)? else {
    return Ok(()); // not an ObjectCreated event
};
```

**An unclassified error in the worker is a bug.** If you cannot say which of the two it is, that is
the design question to answer before writing the code.

---

## Comments explain *why*, never *what*

This is the strongest convention in the repo. Read any file: nearly every non-obvious decision
carries a rationale. Preserve it. Restating the code in English is noise; recording the reasoning is
the only way the next reader knows whether they may change it.

```rust
// ❌ BEFORE — describes what the line already says.
// Set the tenant config for this transaction.
sqlx::query("SELECT set_config('app.current_tenant', $1, true)")

// ✅ AFTER — the actual comment in db.rs. Tells you why it is not `SET LOCAL`,
//    and why the `true` matters. Now you know what you'd break by "simplifying" it.
/// `set_config(_, _, true)` is transaction-local (auto-resets on commit/rollback) — the
/// pool-safe way to scope RLS. We use set_config (not `SET LOCAL`) because only it accepts a
/// bound parameter, so the tenant id can never be SQL-injected.
sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
```

Comment the trap, the trade-off, and the thing you almost got wrong. Module-level `//!` docs state
what the module is *for* and what invariant it holds — see `lifecycle.rs`, `event.rs`, `key.rs`,
`upload/mod.rs`, `reaper.rs`.

Deprecated code is labelled `DEPRECATED` with what replaces it and what it gets deleted alongside.

---

## Async and concurrency

**CPU-bound work goes in `spawn_blocking`.** Embedding is the only such work here, and it is always
wrapped:

```rust
let vectors = tokio::task::spawn_blocking(move || {
    let mut model = embedder.lock().expect("embedder lock poisoned");
    embedding::embed_passages(&mut model, &to_embed)
}).await??;
```

The `??` is not a typo — one for the `JoinError`, one for the inner `anyhow::Result`.

**Independent awaits run concurrently.** `health` probes all five dependencies with `tokio::join!`
rather than sequentially, so one slow service does not serialize behind the others.

**`lapin` is forced onto our Tokio runtime** via `tokio-executor-trait` / `tokio-reactor-trait`. It
would otherwise spawn a second runtime. Do not remove those two dependencies because they look
unused — they are wired in at `Connection::connect`.

---

## Formatting and lints

No `rustfmt.toml`, no `clippy.toml`. **Stock defaults are the standard**, which means there is
nothing to argue about:

```bash
cargo fmt            # before every commit
cargo clippy         # warnings are worth fixing, not suppressing
cargo test
```

Do not add `#[allow(...)]` to silence a lint without a comment saying why the lint is wrong here.

---

## Anti-patterns

Each of these exists in, or nearly slipped into, this codebase.

| Don't | Do | Why |
| --- | --- | --- |
| Query `documents` on `state.db` directly | Use `db::tenant_tx()` | RLS denies by default → silent empty results |
| `QueryPointsBuilder` without `.filter()` | `.filter(tenant_filter(&tenant.tenant_id))` | cross-tenant data leak |
| `?` on a caller-caused failure | `AppError::client(...)` | turns a 400 into a 500 and a page |
| Slice text by byte offset | index over `chars` | panics on any non-ASCII document |
| Embed on an async task | `spawn_blocking` | stalls the whole runtime |
| Extend `POST /documents` (multipart) | use `POST /documents/upload-url` | deprecated; buffers whole files in API memory |
| Log or store a raw API key | `auth::hash_key()` | see [security-policies.md](security-policies.md) |
| Bulk `UPDATE` across tenants | loop per tenant | RLS matches zero rows and *reports success* |
| Version a dep in a member crate | `[workspace.dependencies]` | version skew between the two binaries |

---

## Maintenance

Review at sprint planning and after any architectural change. Treat edits as production changes.
When a module moves, fix the paths referenced here in the same commit.

Related: [api-standards.md](api-standards.md), [testing-standards.md](testing-standards.md),
[tenant-isolation.md](tenant-isolation.md).
