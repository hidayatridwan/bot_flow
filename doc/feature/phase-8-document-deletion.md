# Feature: Document deletion — the design (phase 8)

> Status: **design for review. No code.** This document exists to settle the decisions *before* a
> line is written, because deletion spans three data stores with no transaction across them, and the
> failure modes live entirely in the ordering. Audited against the code: the three delete primitives
> already exist, so this is not a "how do I delete from each store" problem — it is an *ordering,
> atomicity, and concurrency* problem, and that is what the decisions below are about.

## Context — why this is the biggest gap

CLAUDE.md states it plainly: *"There is no delete path for a document — record, vectors and bytes
persist forever. A 'delete my data' request cannot presently be honoured."* For a product holding
other companies' support documents, that is not a missing feature — it is a compliance position you
cannot sell past. GDPR/erasure requests are not optional, and today the honest answer to one is "we
can't."

Two phases made it visible rather than merely true. The dashboard's `/documents` lists every row with
no way to remove one, so `expired`/`failed`/`quarantined` rows accumulate in the tenant's own view.
And the playground renders citations, so a document that should be gone keeps answering questions in
full view of the tenant.

## What already exists — this reframes the whole problem

Every primitive deletion needs is already in the codebase, used on other paths:

| Store | Delete primitive | Where it is used today |
| --- | --- | --- |
| **Qdrant** vectors | `delete_points` with `Filter::must([Condition::matches("document_id", id)])` | `worker/src/main.rs` — invariant 9 clears old chunks before re-indexing |
| **MinIO** object | `bucket.delete_object(object_key)` | `worker/src/main.rs` — `mark_quarantined` deletes an oversize file's bytes |
| **Postgres** row | a `DELETE` under `tenant_tx` (RLS-scoped) | nowhere yet — the one new primitive |

And the **API already holds all three handles** — `state.qdrant`, `state.s3`, `state.db` — so a
delete need not involve the worker or the queue at all. The building blocks are done. The design is
about the order they run in and what a crash between any two of them leaves behind.

## The three stores, and what an orphan in each one *means*

This table is the whole basis for the ordering decision. "Orphan" = this store still holds the
document after the others let go.

| Orphaned store | Consequence | Severity |
| --- | --- | --- |
| **Vectors** (row + object gone, vectors remain) | The document **still answers questions** — search filters by `tenant_id`, and the vectors still carry it. The tenant sees a deleted document reply in the playground/widget. | **Worst.** A functional data-retention failure, visible and query-reachable. |
| **Object** (row + vectors gone, bytes remain) | Invisible to search and to the tenant. Raw bytes at rest in MinIO under `tenants/{id}/…`. | **Bad.** The customer's file still exists — the exact thing "delete my data" is about — but it is inert. |
| **Row** (vectors + object gone, row remains) | A "ghost": listed as a document that answers nothing and has no bytes. Annoying, not a retention failure. | **Least bad.** |

The ordering falls out of this: **delete what makes the document *queryable* first (vectors), then
the bytes (object), then the record (row).** A crash then always fails toward the less-bad orphan,
never toward "it still answers."

## The two hard problems

### 1. There is no transaction across the three stores

Postgres, Qdrant and MinIO cannot commit together. Any multi-store delete has crash windows between
steps. So the design cannot be "delete all three and hope" — it needs a **durable intent** that a
sweep can resume, and every step must be **idempotent** (deleting already-deleted vectors, or an
absent object, must be a no-op, not an error — both primitives already behave this way).

### 2. Deletion races the worker, which resurrects rows

This is the subtle one, and it is specific to how the worker indexes. `claim()` takes
`SELECT … FOR UPDATE`, flips the row to `processing`, and **commits — releasing the lock — before it
parses, embeds and upserts.** So for the whole duration of an index, the worker holds *no* database
lock. Two failure modes follow if a delete lands during that window:

- The worker calls `upsert_points` **after** our vector-delete ran → **orphaned vectors**, the worst
  orphan, re-created moments after we cleared them.
- The worker then calls `mark_ready`, whose SQL today is `UPDATE … SET status='ready' WHERE id=$1`
  with **no status guard** → it would flip our tombstone back to `ready`, **resurrecting the whole
  document.**

So deletion is not just "remove from three stores." It must *fence the worker out*, and the worker
must learn to check whether the ground moved under it. This is the heart of the design.

## Decisions to settle (open — recommendations given)

### D1 — synchronous, asynchronous, or hybrid?

The concurrency analysis answers this, and the answer is **hybrid, keyed on the row's status at
delete time:**

- **Not `processing`** (`ready`, `failed`, `expired`, `uploading`, `quarantined`): the worker is not
  touching this document, so delete all three stores **synchronously in the request** and return
  `204`. This is the overwhelmingly common case.
- **`processing`**: the worker is mid-index and may still write vectors. **Tombstone the row now**
  (so it leaves listings and the worker is fenced — see D3), return `202 Accepted`, and let a
  **sweep** finish the store-deletions once the worker has provably stopped.

Recommendation: **hybrid.** A purely synchronous design is wrong because it cannot safely delete a
`processing` document; a purely asynchronous design adds latency and a compliance-clock ("deleted
within N minutes") to the 99% case that could have been instant. The hybrid is more moving parts than
either, and that cost is real — but it is the only one that is both immediate when it can be and
correct when it can't.

### D2 — deletion order

**Vectors → object → row**, per the orphan-severity table. Recommendation: as stated; it is not a
preference, it is the order that fails safe.

### D3 — how the worker is fenced, and how it learns the ground moved

Two code changes to the worker, both small, both required for correctness:

1. **A tombstone status the worker refuses to claim.** Add `deleting` to the status set. `claim()`
   gains a branch: a `deleting` row returns `Skip` (a redelivered `ObjectCreated` event must never
   resurrect a document being erased). A gone row already `Skip`s ("no such document").
2. **`mark_ready` must guard on status.** Change its `WHERE id=$1` to `WHERE id=$1 AND
   status='processing'`. If it matches **zero rows**, the document was tombstoned mid-index: the
   worker logs and stops, and it does **not** own those vectors — the sweep will delete them. This is
   the fence. Without this guard, `mark_ready` silently resurrects a `deleting` row to `ready`.

Recommendation: both. Note this touches invariant 10's state machine, so per *Working here* the
invariant is edited in the same commit as the code.

### D4 — soft delete forever, or transient tombstone then hard delete?

A `deleted_at` tombstone that lives forever is itself **retained tenant data** (the row still holds
`filename`, `object_key`, timestamps) — which is the very thing an erasure request forbids. So the
end state must be a **hard-deleted row**. The `deleting` status is *transient*: it exists only for
the life of the saga, and the final step removes the row entirely.

Recommendation: **transient `deleting` → hard `DELETE`.** Consequence to respect: the row's
`object_key` is needed to delete the MinIO object, so the object deletion must happen **before** the
row is hard-deleted (which the D2 order already guarantees).

### D5 — is the Qdrant delete tenant-scoped?

`document_id` is a server-minted UUIDv4, globally unique, so `Filter::must([document_id])` alone
cannot touch another tenant's vectors. But the isolation philosophy is *three layers, because a good
day is not something to depend on*. Recommendation: filter on **`document_id` AND `tenant_id`** — one
extra condition, and it means a bug that ever made `document_id`s collide still could not cross a
tenant boundary. (The worker's existing re-index delete filters `document_id` only; leave it — this
is a new path and can be stricter without touching a working one.)

### D6 — this is a constraint, not a choice: delete by the **stored** `object_key`

There are **two object-key schemes** in the `documents` table. The live path
(`create_upload_url` → `key::object_key`) writes
`tenants/{tenant_id}/documents/{document_id}/original.{ext}`. The **deprecated multipart path** writes
`{tenant_id}/{document_id}` — no prefix, no extension. So deletion must read the `object_key` column
and delete **that**, never reconstruct a key from `(tenant_id, document_id)` — a reconstruction would
miss the object for every legacy row and silently orphan its bytes. Pin this with a note; it is
invisible until a legacy row is deleted.

### D7 — who may delete, and the non-oracle 404

Deletion is a management operation: `actor.require_management()` — `sk_` or `sess_`, never `pk_`,
exactly like `GET /documents` (invariant 23). The row `DELETE` runs under `tenant_tx`, so RLS scopes
it; **check `rows_affected`** and return `404` when it is zero. Another tenant's `document_id` and an
unknown one must both `404` identically (invariants 8, 26) — never a `403`, or the endpoint becomes
an oracle for which ids exist. Beware the corollary trap: a cross-tenant `DELETE` under RLS **matches
zero rows and reports success**, so the `rows_affected` check is load-bearing, not decoration.

### D8 — what about conversation history that cited the document?

**Nothing needs scrubbing, and this is worth stating because it looks like it might.** Messages
persist only `role` and `content` (verified in `conversation.rs`); the `sources` array is ephemeral,
sent over SSE and never written to the database. So a deleted document leaves **no copy of its text**
in message history. A past answer may quote it — but that is the tenant's own assistant output, not
our stored copy of their file, and rewriting history to remove it would be dishonest about what was
said. The one visible trace is that an old citation's `document_id` no longer resolves to a filename;
the UI already renders that as `Unattributed passage` (`sources.ts`). Recommendation: **out of scope,
by design** — say so in the doc so the next reader does not "fix" it.

### D9 — the sweep, and whether the reaper owns it

The `processing`-case saga (D1) needs a background finisher, and `reaper.rs` is already exactly this
shape: a per-tenant loop (because a cross-tenant `UPDATE` matches zero rows and reports success —
same trap as D7), settling rows no event will arrive for. Recommendation: **extend the reaper** with
a third sweep that finds `deleting` rows whose worker has provably released (the row is no longer
being processed — the lease is gone) and runs vectors → object → hard-delete for each. A crash
anywhere in a synchronous delete *also* leaves a `deleting` row, so this same sweep is the recovery
path for **both** cases. One mechanism, both crash stories.

## The recommended design, end to end

```
DELETE /documents/{id}          actor.require_management()
  │
  ├─ tenant_tx:  SELECT status FROM documents WHERE id=$1 FOR UPDATE
  │     row missing / other tenant ──────────────► 404  (non-oracle)
  │     status = 'deleting'      ─────────────────► 404 or 204 (idempotent; already going)
  │
  ├─ status = 'processing'?
  │     YES ─► UPDATE … SET status='deleting'; COMMIT ─► 202  (sweep finishes it)
  │     NO  ─► UPDATE … SET status='deleting'; COMMIT
  │            ├─ Qdrant delete_points  (document_id AND tenant_id)   [idempotent]
  │            ├─ MinIO  delete_object  (the STORED object_key)       [idempotent]
  │            └─ tenant_tx: DELETE FROM documents WHERE id=$1        ─► 204
  │
  └─ any store step fails / process crashes ─► row stays 'deleting' ─► reaper sweep resumes
```

Worker changes (D3): `claim()` skips `deleting`; `mark_ready` guards `WHERE status='processing'`.

Reaper change (D9): a third per-tenant sweep resumes `deleting` rows.

Why tombstone-first even on the synchronous path: it makes the row leave listings **atomically and
immediately** (the list query filters out `deleting`), fences the worker the instant we commit, and
turns every subsequent step into a resumable, idempotent cleanup rather than a point of no return
with three separate failure windows.

## Failure matrix — where a crash can land, and what it leaves

| Crash point (synchronous path) | State left | Recovered by |
| --- | --- | --- |
| Before the tombstone commits | Nothing changed — the delete never happened | Client retries |
| After tombstone, before vectors | `deleting` row, vectors + object intact | Sweep: vectors → object → row |
| After vectors, before object | `deleting` row, object intact, vectors gone | Sweep: object → row (vector re-delete is a no-op) |
| After object, before row delete | `deleting` row, vectors + object gone | Sweep: row delete (both store deletes are no-ops) |
| The `processing`-race case | `deleting` row, worker may still upsert | Sweep runs *after* the worker releases; its by-filter vector delete catches whatever the worker wrote |

Every row recovers to fully deleted, and no crash leaves the worst orphan (queryable vectors) as a
*terminal* state — only as a transient one a sweep clears.

## Invariants this creates or touches

- **New invariant (deletion is a fenced saga).** Deletion tombstones the row first, then removes
  stores in the order vectors → object → row; the worker never resurrects a `deleting` row, and a
  crash resumes rather than stranding. Draft it before the code (per *Working here*).
- **Invariant 10 (the claim state machine) gains a `deleting` skip**, and `mark_ready` gains a status
  guard. Edit invariant 10 in the same commit.
- **The vestigial `uploaded` status** (CLAUDE.md debt) is untouched here, but adding `deleting` to
  the same `CHECK` constraint is the moment someone will ask about it — note it, do not fold it in.
- **Invariants 8/23/26 (non-oracle 404, management gate)** are *applied*, not changed.

## Verification plan (when built — none of this is unit-testable)

The core properties need a live stack: RLS denial, the worker race, and cross-store idempotency all
require Postgres + Qdrant + MinIO + the running worker. `Actor::from_request_parts` needs a database,
as every prior phase's matrix has.

- **Happy path, all states:** delete a `ready` doc → gone from `/documents`, `/search` returns none
  of its chunks, the MinIO object is 404, the row is gone. Repeat for `failed`, `expired`,
  `quarantined` (bytes already gone — object delete must no-op), and `uploading` (no object yet).
- **The worker race — the important one:** upload a large document; issue the delete **while it is
  `processing`**; assert it ends fully deleted with **no orphaned vectors**, and that the worker's
  `mark_ready` found zero rows and did not resurrect it.
- **Idempotency:** delete the same id twice; the second is a clean `404`/`204`, not a 500.
- **Crash recovery:** kill the API mid-saga (after the tombstone); assert the reaper sweep completes
  the deletion.
- **Isolation (invariant 1/8):** tenant B's `DELETE` on tenant A's `document_id` → `404`, and A's
  document is **untouched** — the cross-tenant `DELETE`-matches-zero-rows trap, actually exercised.
- **Non-oracle:** unknown id and other-tenant id return the identical `404`.

## Out of scope, deliberately

- **Scrubbing conversation history** (D8) — nothing to scrub; the text was never stored.
- **Bulk / "delete all my data" (tenant offboarding)** — a related but larger operation (it also
  removes `tenants`/`api_keys`/`accounts`/`sessions` and the whole MinIO prefix). This phase is
  single-document; offboarding can build on the same saga per document but is its own design.
- **Undo / trash / retention window before hard delete** — an erasure request wants the data *gone*,
  not recoverable. A "trash" feature is the opposite requirement and a separate product decision.
- **The vestigial `uploaded` status** — name it or drop it in its own change, not this one.

## Open questions for review

1. **`202` vs blocking on the `processing` case.** The hybrid returns `202` and defers. Alternative:
   the request *waits* for the worker to release (bounded by the lease) and then completes
   synchronously. Simpler contract (always `204`), worse tail latency. Recommendation stands at
   `202`, but it is a UX call worth a second opinion.
2. **Sweep cadence vs the compliance clock.** If erasure must complete within a stated window, the
   reaper interval and the `processing` lease together bound the worst case. What is the window?
3. **Should `DELETE` on an already-gone document be `404` or an idempotent `204`?** HTTP allows
   either; `404` is the more common and stays non-oracle. Minor, but pin it before building.
