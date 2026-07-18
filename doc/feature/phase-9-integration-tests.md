# Feature: An integration harness for the guarantees unit tests cannot reach (phase 9)

> Status: **design for review. No code.** CLAUDE.md asks for exactly this conversation before any of
> it is built — *"Anything needing Postgres or Qdrant belongs in `crates/<crate>/tests/`, which does
> not exist yet — **discuss before introducing one**."* This document is that discussion.
>
> **The fork, stated up front:** the other candidate for phase 9 is retrieval quality (chunking +
> hybrid search), which is the bigger *product* win. This is proposed first because retrieval work
> means re-indexing every vector in the system, and it is better to be able to *prove* isolation still
> holds before rewriting the ingest path underneath it. If you would rather the product feel better
> sooner, swap the order — the argument is a preference about sequencing, not a claim that this is
> more valuable.

## Context — why

**The system's most important promise is the one nothing tests.** Invariant 1 says every Qdrant search
is filtered by tenant; invariant 2 says every row query goes through `tenant_tx`. One forgotten filter
in one handler leaks another company's support documents. Today that rests entirely on code review —
there is no `crates/*/tests`, and (verified) **no `.github/workflows`**, so nothing runs even the tests
that do exist except a human remembering to type `cargo test`.

That was tolerable while the code was still. It is not now. In the last few phases alone the codebase
gained a new auth principal reaching `/ask`, a gate on `/search`, and a deletion saga that issues a
**Qdrant delete-by-filter**. I scoped that delete on `document_id` *and* `tenant_id` deliberately —
but had I got it wrong, it would silently delete another tenant's vectors, and **no test in this repo
would have gone red.** Every phase adds another such surface, and the cost of not having a harness
compounds rather than staying flat.

## The trap that decides the whole design

**A test that connects as the database owner proves nothing, and passes.**

Postgres superusers bypass RLS entirely. That is precisely why migration `0005` created `app_user` and
why the runtime connects as it (isolation layer 3) — the admin pool runs migrations and is then
*closed* so a well-meaning refactor cannot reach for it.

I demonstrated the hazard by accident all through phase 8: every diagnostic query I ran this session
used `psql -U bot_flow`, and it happily read across tenants. That is not a bug — `bot_flow` owns the
tables and is a superuser, so `FORCE ROW LEVEL SECURITY` does not apply to it.

So a harness that connects with the convenient credential would assert "tenant B cannot see tenant A's
document", watch it pass, and have tested **nothing at all** — because the query it ran was never
subject to the policy. Green, meaningless, and permanently reassuring in the worst way.

**Every test touching `documents` / `conversations` / `messages` must connect as `app_user`.** This is
the single non-negotiable in the design.

## Decisions to settle (open — recommendations given)

### D1 — how do integration tests coexist with "tests pass with no backing services"?

This is the first real tension, because CLAUDE.md states a rule this phase would otherwise break:
*"Tests must pass with **no backing services running**."* Anything in `crates/api/tests/` is compiled
and run by a bare `cargo test`, so a contributor with no Docker would suddenly see failures.

Options: mark them `#[ignore]` (a bare `cargo test` skips them; `cargo test -- --ignored` runs them);
or gate them behind a cargo feature (`--features integration`); or have them detect a missing service
at runtime and skip.

Recommendation: **`#[ignore]`**. It is the standard mechanism, needs no Cargo wiring, and keeps
`cargo test` honest and offline exactly as the rule promises. Reject runtime-detect-and-skip outright:
a test that silently passes when its dependencies are absent is the same failure mode as the
superuser trap — it converts "untested" into "green", which is worse than a visible gap. CLAUDE.md's
rule then gains one clause naming the second command rather than being contradicted.

### D2 — where do services come from?

Options: (a) the existing `docker compose` stack, against a **separate test database**; (b)
`testcontainers`, spinning fresh containers per run; (c) assume something is listening.

Recommendation: **(a), with a dedicated `bot_flow_test` database on the same Postgres container**, and
Qdrant/MinIO shared. It reuses the environment the repo already documents as the dev setup, adds no
dependency, and is what CI will do with service containers anyway. Testcontainers is cleaner in
principle but adds a heavy dependency and a Docker-in-CI wrinkle for a repo that has no CI at all yet
— worth revisiting if test pollution becomes real. **Never point the harness at the dev database**:
these tests create and delete tenants, and a stray `documents` truncation against real dev data is a
bad afternoon.

### D3 — drive the HTTP layer in-process, or spawn a server?

The interesting bugs live in extractors: `Actor::from_request_parts` needs a database, which is
exactly why the auth matrix has never been unit-testable. So tests must exercise the real `Router`.

Recommendation: **build the real `Router` and drive it with `tower::ServiceExt::oneshot`** — no
socket, no port, no teardown race, and every layer (extractors, gates, RLS) still runs. Spawn a real
listener only if something needs genuine network behaviour, and SSE streaming is the likely exception,
since `oneshot` gives a body you must drive yourself.

### D4 — test isolation, given a shared database and a shared Qdrant collection

`cargo test` runs tests in parallel threads against one database, and **Qdrant has no RLS at all** —
one collection, isolation enforced only by the payload filter under test.

Recommendation: **every test provisions its own tenants with unique ids** (a random suffix), asserts
only about its own ids, and never assumes an empty collection or table. Transaction-rollback isolation
is tempting and wrong here: the code under test manages its own transactions, and the worker's claim
path depends on real commits.

### D5 — does CI land in this phase?

Recommendation: **yes, and it is half the value.** A harness nobody runs is a harness that rots. One
GitHub Actions workflow with Postgres and Qdrant service containers, running `cargo test`,
`cargo test -- --ignored`, `cargo clippy`, `cargo fmt --check`, and `web/`'s `bun run test` +
`bun run check`. Note the repo's existing lint caveat: `bun run lint` fails on ~208 pre-existing
vendored files, so CI must not gate on it until that is dealt with separately.

## The tests worth writing, in order

1. **Cross-tenant denial (the reason this phase exists).** Tenant A uploads; tenant B asks for A's
   document by id, searches for its text, and tries to delete it. Every path must return
   nothing/404 — and A's document must still be intact afterwards, which is the half a naive test
   forgets. Must run as `app_user`.
2. **The auth matrix**, which no unit test can reach because the extractor needs a database: `pk_`
   refused by `require_management()` on `/search` and the document routes, `pk_` accepted on `/ask`,
   origin rejection for a `pk_` from an un-allow-listed `Origin`, `sess_` accepted where invariant 23
   says it should be. This has been verified by hand with curl in three separate phases; that is
   exactly the sort of thing that should stop being manual.
3. **Concurrent claim.** Two workers, one document, one `Proceed` and one `Skip`. The row lock plus
   status check is invariant 10's entire deduplication story and it has never been executed
   concurrently in anger.
4. **The deletion saga**, including the case that took a live stack to check: a delete landing while a
   document is `processing` must tombstone, defer, and end fully erased with no orphaned vectors —
   and the sweep must **not** touch a row whose worker still holds the lease.

**One item in CLAUDE.md's list is already done and should be struck:** it names "correct
fatal-versus-retryable classification" as missing, but `common/src/embedding.rs` has unit tests for
`EmbedError::is_fatal` covering the retryable statuses, the `413`, and the ambiguous `400`. That entry
is stale.

## Verification — how do we know the harness itself works?

**A passing test proves nothing until you have watched it fail.** This is the whole risk of a phase
whose deliverable is tests: it is entirely possible to write a green suite that asserts nothing, and
nobody would notice for a year.

So each of the four above must be demonstrated to **go red against deliberately broken code**, and the
break must be reverted in the same sitting:

| Break | Test that must fail |
| --- | --- |
| Drop `.filter(tenant_filter(...))` from the search | cross-tenant search denial |
| Swap `tenant_tx` for the plain pool in `list_documents` | cross-tenant document denial |
| Connect the harness as `bot_flow` instead of `app_user` | **all RLS tests must fail** — this is the superuser trap, and proving the harness detects it is the most important check here |
| Remove `require_management()` from `/search` | the `pk_` refusal case |

If a test stays green through its corresponding break, that test is decoration.

## Known debt & traps

| Don't | Do | Why |
| --- | --- | --- |
| Connect tests as `bot_flow` because it is already in `.env` | Connect as `app_user` | Superusers bypass RLS, so every isolation assertion would pass without testing anything. Green and meaningless |
| Point the harness at the dev database | A separate `bot_flow_test` DB | These tests create/delete tenants and documents; sharing is one truncation away from wiping dev data |
| Let integration tests run under a bare `cargo test` | `#[ignore]`, run explicitly and in CI | CLAUDE.md promises `cargo test` works with no services; breaking that punishes every contributor without Docker |
| Skip a test when its service is missing | Fail, or don't run it at all | Silent skip turns "untested" into "green" — the same lie as the superuser trap |
| Assume an empty Qdrant collection | Unique tenant ids per test | One shared collection, no RLS; parallel tests and leftover data are both normal |

**Left standing, deliberately:** this phase tests *guarantees*, not *quality*. It says nothing about
whether retrieval returns good answers — that is the retrieval-quality phase, and today's
`RAG_SCORE_THRESHOLD` tuning (0.20, fitted to a three-chunk corpus with a 0.22-vs-0.13 margin) is a
reminder that it is waiting.

## Open questions for review

1. **Is the sequencing right** — harness before retrieval, or retrieval first? See the header.
2. **Should CI gate merges** (branch protection) or just report? Gating is the point, but it is a
   workflow change for whoever else pushes to this repo.
3. **Does the worker get integration coverage in this phase**, or only the API? The concurrent-claim
   and deletion-sweep tests are worker behaviour and need its binary or its functions driven directly;
   that is a meaningfully larger harness than testing the API's `Router`.
