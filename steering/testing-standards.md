# Testing Standards

> **Scope:** what is tested here, how, and — just as importantly — what is not.

**There is no jest, vitest, playwright, or `tests/` directory.** Tests are Rust unit tests,
co-located with the code they cover, run with `cargo test`.

---

## The pattern

An inline `#[cfg(test)] mod tests` block at the bottom of the module:

```rust
// crates/worker/src/main.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_ids_are_stable_across_runs() {
        let doc = uuid::Uuid::parse_str("7045945d-3a0e-4b69-9749-326871ef7516").unwrap();
        assert_eq!(point_id(&doc, 0), point_id(&doc, 0));
        assert_ne!(point_id(&doc, 0), point_id(&doc, 1));
        // A different document never collides with this one's chunks.
        let other = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap();
        assert_ne!(point_id(&doc, 0), point_id(&other, 0));
    }
}
```

`#[cfg(test)]` means the block is compiled only under `cargo test` — no cost in the shipped binary.
`use super::*` gives access to the module's private items, which is why `point_id` and
`empty_string_as_none` can be tested without being made `pub`. **Do not widen visibility just to
test something.**

Async tests use `#[tokio::test]` (see `crates/api/src/conversation.rs`).

---

## What gets a test

The existing suite is small and every test in it earns its place. The pattern to imitate:

**1. Pure functions over strings or numbers.** `crates/common/src/key.rs` is the model — six tests,
no I/O, exhaustive. Its module doc says why:

> Everything here is a pure function over strings so it can be tested exhaustively without MinIO or
> Postgres.

If a piece of logic can be made pure, make it pure, and then test it hard. `event.rs` and `chunk.rs`
follow the same shape.

**2. Security boundaries.** `path_traversal_slugs_are_rejected` and `traversal_key_does_not_parse`
exist because a tenant slug containing `..` would let one tenant's object escape into another's
prefix. These are not coverage padding; they are the executable form of an invariant.

**3. Deserialization edge cases.** `crates/api/src/handlers.rs` tests that `conversation_id` accepts
`""`, whitespace and `null` as "new conversation" while still rejecting `"not-a-uuid"`. Every case
came from a real client sending something the naive `#[serde(default)]` would have 400'd.

**4. Determinism claims.** `point_ids_are_stable_across_runs` pins the property the whole
re-indexing story depends on. If a refactor swaps UUIDv5 for v4, this test is what catches it —
nothing else would, until documents silently duplicated in production.

**Rule of thumb:** *if the code makes a promise that a reader would have to take on faith, write the
test that makes it a fact.* Round-trips, rejections, stability, boundaries.

---

## What does not get a test, today

Anything needing Postgres, Qdrant, RabbitMQ, MinIO or Redis. There is **no integration test suite, no
HTTP-level harness, and no coverage tooling**. Stating that plainly is more useful than inventing a
coverage target nobody enforces.

Those paths are exercised manually against the local stack, using
`postman/bot_flow.postman_collection.json` and `widget/demo.html`.

**This is a real gap**, not a stance. The most valuable missing tests, roughly in order:

1. RLS actually denies cross-tenant reads (a bug here is the worst bug this system can have).
2. `lifecycle::claim` deduplicates concurrent events on the same document.
3. `AuthTenant` rejects a `pk_` key from a disallowed `Origin`.
4. The `Fatal` / `Retryable` classification acks vs. nacks correctly.

---

## Running tests

```bash
cargo test                  # whole workspace
cargo test -p common        # one crate
cargo test point_id         # by name substring
cargo test -- --nocapture   # see println!/tracing output
```

Tests must pass without any of the five backing services running. If a test you write needs
`docker compose up`, it is an integration test and does not belong in a `#[cfg(test)]` block — see
below.

---

## If you add integration tests

**Not yet adopted. Discuss before introducing.** The two conventional Rust options:

- **`#[sqlx::test]`** — provisions a throwaway database per test and runs migrations into it.
  Natural fit for the RLS and lifecycle tests above, since those *are* database behaviour. Requires
  a reachable Postgres.
- **testcontainers** — spins up real Qdrant / RabbitMQ / MinIO containers per test. Heavier, slower,
  but the only way to test the event pipeline end to end.

Either way they go in `crates/<crate>/tests/` (Cargo's integration-test directory), never in a
`#[cfg(test)]` block, because they need external services and must be skippable in a plain
`cargo test` run.

Whichever is chosen, update this file in the same change. A steering file that describes a testing
strategy nobody follows is worse than one that admits the gap.

---

## Maintenance

Review at sprint planning and whenever the testing setup changes. Treat edits as production changes.
If integration tests land, rewrite the "does not get a test" section rather than leaving it stale —
an out-of-date claim here will send Claude looking for a harness that does not exist.

Related: [code-conventions.md](code-conventions.md), [deployment-workflow.md](deployment-workflow.md).
