# Production readiness — what blocks going live

> Assessed 2026-07-19, against the phase-10 tree. Every claim below was checked against the code, not
> recalled; each carries the file it lives in so it can be re-checked rather than believed. CLAUDE.md's
> *Known state & debt* is the running inventory — **this document is narrower**: it asks only "what
> stops real customers using this", and orders by that.

## At a glance

| # | Blocker | Severity | Closed by |
| --- | --- | --- | --- |
| 1 | `/ingest` vectors cannot be attributed to a document — no per-document erasure | **blocking** | [phase 11](feature/phase-11-ingest-gdpr.md) — designed |
| 2 | No tenant-level erasure, no erasure audit trail, history retains passage text | **blocking** | phase 12 (D8 of phase 11) — not designed |
| 3 | No metrics, no alerting, no backups | **blocking** | not designed |
| 4 | `failed` cannot tell a tenant whether to re-upload or wait | high | not designed |
| 5 | `GET /documents` unpaginated; `/auth/keys` unmetered; `/ask/stream` unbounded | high | not designed |

Verified against the tree on 2026-07-19: `/ingest` makes **zero** chunker calls and writes a payload of
exactly `text` + `tenant_id`; there is **no** tenant-deletion endpoint, **no** metrics endpoint, **no**
endpoint exposing `documents.error`, and **no** `LIMIT`/`OFFSET` in `list_documents`.

## Verdict

**Ready for a design-partner or internal pilot. Not ready for self-serve signups from strangers
handling real customer policy.**

The distinction is not polish. It is that a pilot has a named operator who can avoid `/ingest`, watch
the logs, and re-upload when something breaks — and self-serve has none of those. Three of the five
blockers below are invisible to the person they hurt: they produce a *plausible* answer, a silent
refusal, or an erasure that did not happen. That is the specific reason this list exists rather than
a general sense of "needs hardening".

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

### 1. `POST /ingest` writes vectors that cannot be attributed to a document — GDPR

**Severity: blocking. This is the one to fix first.**

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

### 2. No erasure guarantees beyond the happy path

Even for properly-uploaded documents, the erasure story has holes a compliance review would find:

- **A delete racing an active index defers for up to one `PROCESSING_LEASE` (~30 min)** during which
  the row is gone from listings but its vectors still answer searches (CLAUDE.md; tested in both
  directions by phase 9b, so it is a *known* window rather than an unknown one).
- **There is no "delete this tenant" operation.** Removing a tenant means `DELETE FROM tenants`
  cascading in Postgres and *nothing* in Qdrant or MinIO — the vectors and objects survive. For a
  processor obligation this is the gap that matters most after (1).
- **There is no audit trail of erasures.** Nothing records that a deletion happened, when, or by
  which principal.
- **Conversation history is not covered by document deletion.** `messages` retains the passage text
  the model was shown; deleting the source document does not redact it.

*Closes when:* a tenant-erasure endpoint plus an audit log exist. Partly in scope for phase 11.

### 3. Operational blindness

There is **no metrics endpoint, no structured error reporting, and no alerting**. `/health` reports
reachability of five dependencies and nothing about correctness. Concretely, none of these would be
noticed today without someone reading logs:

- retrieval quality degrading after a corpus grows;
- the embedding gateway returning 429s and documents dead-lettering;
- a tenant's spend running away inside their own rate limit;
- the reaper failing every sweep.

There are also **no backups** and no restore procedure for Postgres, MinIO or Qdrant. Qdrant is
rebuildable from MinIO + Postgres via `worker reindex`; Postgres is not rebuildable from anything.

*Closes when:* backups exist and are restore-tested, and there is at least one alert that fires on
worker death and on dead-letter depth.

### 4. `failed` cannot tell a tenant what to do

`mark_failed` writes the parser's stderr; the reaper writes `'processing lease expired; worker
presumed dead'`. Both land in the same `error` column, which **no endpoint exposes** — correctly,
since invariant 16 forbids shipping either string to a client. So the UI says "failed" and names both
possible causes, and the tenant cannot tell whether to re-upload a broken PDF or wait for us.

*Closes when:* the worker writes a classified reason code beside the raw text and the API exposes the
code (not the text).

### 5. Unbounded reads and unmetered writes

- **`GET /documents` has no pagination** (`handlers.rs`) — the whole table, fully materialised, every
  call, and the dashboard polls it. Fine at 10 documents; a real problem at 10,000, and the polling
  back-off is a mitigation rather than a fix.
- **`/auth/keys` is unmetered** — a logged-in session can mint unbounded keys. Session-gated and not
  a spend multiplier, so this is an audit-surface problem, not a cost one.
- **`/ask/stream` has no maximum duration.** Bounds are stalls, not deadlines (deliberately —
  invariant 28), so a gateway trickling one token just inside `READ_TIMEOUT` streams indefinitely.
  `MAX_TOKENS` bounds it only for a well-behaved gateway.

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
