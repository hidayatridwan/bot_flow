# Production readiness — what blocks going live

> Assessed 2026-07-19, against the phase-10 tree. Every claim below was checked against the code, not
> recalled; each carries the file it lives in so it can be re-checked rather than believed. CLAUDE.md's
> *Known state & debt* is the running inventory — **this document is narrower**: it asks only "what
> stops real customers using this", and orders by that.

## At a glance

| # | Blocker | Severity | Closed by |
| --- | --- | --- | --- |
| 1 | ~~`/ingest` vectors cannot be attributed to a document~~ | ~~blocking~~ | **CLOSED** — [phase 11](feature/phase-11-ingest-gdpr.md), invariant 29 |
| 2 | ~~No tenant-level erasure, no audit trail, history unredacted~~ | ~~blocking~~ | **CLOSED** — [phase 12](feature/phase-12-tenant-erasure.md); one window left open, stated below |
| 3 | ~~No metrics, no alerting, no backups~~ | ~~blocking~~ | **CLOSED** — [phase 13](feature/phase-13-observability.md), invariant 30; alert *delivery* and error reporting remain |
| 4 | ~~`failed` cannot tell a tenant whether to re-upload or wait~~ | ~~high~~ | **CLOSED** — [phase 14](feature/phase-14-failure-classification.md) |
| 5 | ~~`GET /documents` unpaginated; `/auth/keys` unmetered; `/ask/stream` unbounded~~ | ~~high~~ | **CLOSED** — [phase 15](feature/phase-15-bounded-reads.md) |

**Every blocker on this list is now closed**, by phases 11–15. What remains is in *Not blockers* below
and in CLAUDE.md's *Known state & debt*, which is the superset this document filters.

Re-verified 2026-07-20 after phase 15: `GET /documents` bounds every caller — including one sending
no parameters — and pages by keyset cursor; `POST /auth/keys` returns 429 past its own bucket's
limit; and `/ask/stream` carries a 300s wall clock that ends the stream with `done` rather than
discarding the answer. There is still **no** endpoint exposing `documents.error`.

## Verdict

**Every blocker this document raised is closed. That is not the same as "ready", and the difference
is now a deployment question rather than a code one.**

Phases 11 and 12 closed the correctness blockers — the system can erase a document and erase a
tenant, and prove it. Phase 13 closed the operational one: instruments that move, backups that have
actually been restored, alert rules ready to wire. Phase 14 closed the UX one: a failed document says
whether to re-upload or to wait. Phase 15 closed the scaling one: no unbounded read, no unmetered
write, no unbounded stream.

**What stands between this and self-serve signups is no longer on this list**, and saying so plainly
matters more than declaring victory:

- **Nobody is paged.** `doc/ops/alerts.yml` exists and **no rule in it has ever fired** — there is no
  Prometheus in this repo to fire them. Until that is wired, the system is observable but not
  monitored, and the difference is whoever notices first.
- **Backups are manual, local, unencrypted, unrotated, with no PITR.** The restore drill was real,
  and it was run by hand.
- **CI is report-only.** A red run does not stop a merge.
- **`app_user` ships the dev password** from migration 0005 unless the role is pre-created.

None of those is a code blocker, which is exactly why they are easy to carry into production
unnoticed. A design-partner or internal pilot is well served today. Strangers handling real customer
policy need the four above done first.

The distinction is not polish. Most of what goes wrong in this system goes wrong *quietly* — a
plausible answer, a silent refusal, a partially re-indexed collection. That is the specific reason
this list exists rather than a general sense of "needs hardening".

## What is genuinely solid

Worth stating, because the list below is otherwise unbalanced and would misrepresent the system.

- **Tenant isolation is three-layered and *tested*** — Qdrant filter, Postgres RLS forced on a
  non-superuser runtime, and the key/session → tenant resolution. 14 integration tests, each verified
  to go red against a deliberate break before being trusted (`crates/api/tests/`, phase 9).
- **Credentials are hashed and never logged** — API keys SHA-256, passwords Argon2id, session tokens
  SHA-256. A database dump is not a credential dump.
- **Ingestion is idempotent under redelivery**, with a tested claim/fence state machine and a tested
  deletion saga across all three stores (phase 8, 9b).
- **Every outgoing gateway call is bounded**, with the stall-vs-deadline distinction handled correctly
  (invariant 28).
- **Retrieval quality is measured**, not asserted — `cargo run -p eval`, with a sabotage table that
  must move before any number is believed (phase 10).

## Blockers

### 1. ~~`POST /ingest` writes vectors that cannot be attributed to a document~~ — CLOSED (phase 11)

**Closed.** `/ingest` now writes the caller's text to MinIO as an ordinary object and lets the worker
index it, so it produces a real `documents` row and inherits the deletion saga. Verified live: after
`DELETE /documents/{id}`, a search for the document's distinctive text returns zero hits and the
collection holds zero points. Invariant 29 states the property; `crates/api/tests/ingest_erasure.rs`
pins it, and the assertions were watched failing first.

**One residue, and it is not fixable by code:** vectors written by the *old* path remain
unattributed, because nothing ever recorded which call produced which point.
`cargo run -p worker -- purge-unattributed [tenant] [--yes]` erases them — dry-run by default,
scoped to one tenant, reporting counts before it deletes. Until an operator runs it, per-document
erasure is impossible for that data specifically.

The original assessment follows, for the record.

`ingest` writes points with a random v4 id and a payload of exactly `text` + `tenant_id`
(`crates/api/src/handlers.rs`). No `document_id`, no `chunk_index`, no `created_at`, and **no
`documents` row**. Consequences, in order of seriousness:

- **There is no per-document erasure.** `DELETE /documents/{id}` cannot reach these points because
  nothing ties them to an id. If a tenant's customer exercises a right to erasure and the relevant
  text arrived via `/ingest`, the only instrument is a tenant-wide wipe.
- **Be precise about what is and is not possible.** Bulk erasure *does* work: the points carry
  `tenant_id`, and `Condition::is_empty("document_id")` exists in the client, so "delete everything
  unattributed for this tenant" is one filtered call. The gap is **attribution**, not deletion —
  which matters, because it means the remedy for existing data is *destructive but available*.
- **It also breaks two invariants that are now written down.** Invariant 9 (re-indexing overwrites)
  does not hold — re-ingesting the same text duplicates it. And phase 10's widening of invariant 6
  says a collection may not hold text cut two different ways; `/ingest` does not chunk at all, so it
  stores whole strings beside 500-character boundary-aware chunks in the same collection.
- **These points cannot be migrated.** The phase-10 re-index driver walks `documents` rows; rows are
  exactly what these lack.

*Closed by:* [phase 11](feature/phase-11-ingest-gdpr.md).

### 2. ~~No erasure guarantees beyond the happy path~~ — CLOSED (phase 12)

**Closed.** `DELETE /admin/tenants/{id}` erases a tenant across Postgres, Qdrant and MinIO, revoking
access first and sweeping vectors twice (a worker mid-index holds no lock and can write between the
sweeps). Every erasure — document and tenant — is recorded in `erasures`, which has **no foreign key
to `tenants`**, so the audit row outlives the thing it audits. Verified live: erasing one tenant took
its vector and its object, left the other tenant intact, and left an audit row naming a tenant that
no longer exists.

**One claim here was wrong and is corrected.** This document said `messages` "retains the passage
text the model was shown". It does not — `append_turn` stores the question and the answer, nothing
else. The real gap was narrower and is also closed: an *answer* quotes the passages, so assistant
turns now carry their sources in `metadata` and deleting a document **redacts** the turns that cite
it (redacts, not deletes — removing the row would leave a question answering itself).

**What is deliberately still open**, and why it is no longer blocking:

- **The ~30-minute deferred-delete window** on a *document* deletion racing an active index. Phase
  8's known trade, pinned in both directions by phase 9b — a bounded, tested window rather than an
  unknown one.
- **Turns written before phase 12 carry no provenance** and cannot be found by redaction. Not
  recoverable: nothing ever recorded which document an old answer quoted.
- **The audit is not tamper-evident** — a table an operator with database access can edit. Making it
  append-only is a different phase with a different threat model.
- **No retention policy**, and **`purge-unattributed` writes no audit row**.

### 3. ~~Operational blindness~~ — CLOSED (phase 13)

**Closed.** A token-gated `/metrics` measures the four failures this section named, `GET
/admin/ops/tenants` answers "which tenant" live, `scripts/backup.sh` + `restore.sh` cover the two
stores that cannot be rebuilt, and `doc/ops/alerts.yml` carries rules for worker death and DLQ depth.

**The restore was tested, not assumed** — that was the section's own bar. Seed → back up →
`reset.sh -y` → restore → reindex → **ask a real question and get the same grounded answer**. Row
counts alone would not have proved it: a restore with perfect counts and an empty collection refuses
everything while looking healthy. The drill also caught a backup that silently contained **zero
objects** while reporting success, which review had not.

**Worker death** is `botflow_queue_consumers{queue="document_events"} == 0` — the broker reporting
the consumer's absence, which beats a heartbeat and needed no worker code. Verified 1 → 0 on kill.

**What remains open, and why it is no longer blocking:**

- **Alert delivery.** The rules file exists; there is no Prometheus in this repo and nothing to send
  to. Wiring it up is a deployment task of maybe fifteen minutes, and the file states plainly that no
  rule here has ever fired.
- **No structured error reporting** (Sentry or equivalent) — this section mentioned it and phase 13
  does not close it. `tracing` is already structured; shipping it elsewhere is deployment.
- **Backups are manual, local, unencrypted, unrotated, no PITR**, and the drill is manual and billed
  so it is not in CI. Its last-run date going stale is itself a signal.
- **No per-tenant metric history**, deliberately: invariant 30 keeps tenant identity out of any store
  the erasure saga cannot reach.

### 4. ~~`failed` cannot tell a tenant what to do~~ — CLOSED (phase 14)

**Closed on its own stated condition:** the worker writes a classified reason code beside the raw
text, and the API exposes the code, not the text. `failure_reason` is a closed enum
(`unreadable_file` / `unsupported_type` / `too_large` / `system_error`) cut by **what the tenant
should do**, and the dashboard renders re-upload advice or wait advice accordingly.

**The assessment above under-described the problem.** It framed this as the UI being vague. The
sharper version is that the vagueness was *load-bearing*: the copy had to cover a dead worker and a
corrupt PDF at once, so a tenant whose worker died was told their file might be damaged and sent to
re-upload a good file into a system that was down. That is not a missing detail, it is wrong advice
delivered politely — which is why `system_error` is the one branch tested never to say "upload".

**One finding worth recording, because it would have inverted the fix.** This repo's own documented
sidecar contract (`2` = unreadable, `3` = unsupported) was wrong: `2` is argv misuse the worker
cannot trigger, and a genuinely unreadable PDF was an uncaught traceback on **exit 1** —
indistinguishable from our own sidecar being broken. Classifying on the documented contract would
have reported every deployment fault to tenants as their damaged document. The sidecar gained an
explicit exit `4`; verified against the real interpreter, not the docs.

**What is deliberately still open:** rows that failed before phase 14 carry no reason and render as
the old both-causes copy (nothing recorded a cause; a backfill would be inventing one). The enum
lives in three places and only the DB `CHECK` fails closed. And the reason is coarse for *operators*
— it answers "re-upload or wait", not "which store was down", which still means reading logs.

### 5. ~~Unbounded reads and unmetered writes~~ — CLOSED (phase 15)

**Closed, all three.** `GET /documents` pages by keyset cursor with a default that bounds callers who
send no parameters at all; `POST /auth/keys` is metered on its own bucket; `/ask/stream` carries a
300s wall clock.

**Two things the original assessment got wrong, both found by measuring rather than reading.**

The pagination entry framed this as purely a scaling problem. It was also a *plan* problem that
predated pagination: the listing had sorted `ORDER BY created_at DESC` since migration 0003 **with no
index behind it**, so every call was a sequential scan plus a sort. Worse, `created_at::text AS
created_at` shadows the column — Postgres resolves a bare ORDER BY name against the output list
first, so the sort ran on the *text rendering*. Measured on 5k rows: cost 371 with a `Seq Scan`,
versus 0.29 and no sort node once the reference was qualified and migration 0016's index existed.

And `created_at` is **not unique** — it defaults to `transaction_timestamp()`, so documents created
in one transaction share a timestamp to the microsecond. A cursor without the `id` tiebreaker
silently loses rows on a page boundary; deleting it from the query drops **5 of 9 documents** while
the listing still looks perfectly normal.

**What is deliberately still open:**

- **`STREAM_DEADLINE` firing has no test.** At 300s a real one would be a five-minute test, and a
  faked clock would assert against a stub rather than the code. The frames around it are now covered
  — `/ask/stream` had no tests at all before this phase.
- **No `total` and no "previous page".** Both are consequences of the keyset choice: a count is the
  full scan this replaced, and a cursor moves one way. The dashboard uses browser history; an API
  client must keep its own cursors.
- **A truncated answer is persisted mid-sentence**, so the next rewrite reasons over it. That is the
  better half of the trade against losing the answer entirely, but it is a trade.

## Not blockers, but do them before you forget

- **No `.env.example`**, though `.gitignore` expects one. Pure friction for the next contributor.
- **`app_user`'s password is hardcoded** as `'app_user'` in migration `0005`, and is only set on
  first creation — so a production deployment ships the dev password unless the role is pre-created
  out of band. Not exploitable from outside the network, but it is a credential in a tracked file.
- **CI is report-only** — no branch protection, so a red run does not stop a merge.
- **The migration driver is exercised by hand**, not by a test.
- **The `uploaded` document status is unreachable** — no code path assigns it. A trap for the next
  reader.
- **`POST /documents` (multipart) is still present** and still buffers whole files in API memory.
  Deprecated; delete it with `queue.rs` and the worker's `consume_legacy`.

## The one to be most careful about

**Everything in this system that goes wrong, goes wrong quietly.** A too-high relevance floor refuses
every question and looks identical to a working bot with nothing to say. A partially re-indexed
collection degrades retrieval with no error anywhere. A cross-tenant `UPDATE` under RLS matches zero
rows and *reports success*. A superseded policy answers alongside the current one, confidently.

That is why the instruments — the integration suite, the retrieval bench, the sabotage table — are
load-bearing rather than nice-to-have, and why the operational blindness in (3) is a heavier blocker
here than it would be in a system that fails loudly.

## Re-checking this document

Everything above is a claim about code that changes. To re-verify:

```bash
cargo test --workspace                # unit tests, offline
cargo test --workspace -- --ignored   # isolation, auth matrix, claim, deletion saga
cargo run -p eval                     # retrieval quality + the sabotage table
```

and read CLAUDE.md's *Known state & debt*, which is the superset this document filters.
