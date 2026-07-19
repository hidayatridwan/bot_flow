# Feature: Making `/ingest` a document, so erasure can be honest (phase 11)

> Status: **built. Two of its own decisions reversed during implementation — see [Outcome](#outcome).**
>
> This closes the entry CLAUDE.md has called *"the largest single piece of debt in the system"* since
> it was written, and which [production-readiness.md](../production-readiness.md) lists as blocker 1.
> It is the first phase driven by an obligation rather than a preference: a processor that cannot
> erase a named document on request has a compliance problem, not a backlog item.

## Context — why

`POST /ingest` writes Qdrant points with a random v4 id and a payload of exactly `text` and
`tenant_id`. There is no `document_id`, no `chunk_index`, no `created_at`, and — the root of it — **no
`documents` row anywhere**. CLAUDE.md has never been coy about this:

> *"they belong to no record, so they can never be listed, re-indexed or removed. **They are
> permanent.** … The largest single piece of debt in the system. Demo and testing convenience; not a
> supported path. Do not build on it."*

Two things have changed since that was written, and together they turn a documented shortcut into a
blocker.

**Phase 10 made the breakage visible rather than merely stated.** Invariant 6 now covers the whole
index recipe, and invariant 9 says a chunk carries its provenance — `document_id`, `chunk_index`,
`char_start`, `char_end`, `created_at`. `/ingest` writes none of them and does not chunk at all, so
`documents_v2` currently holds whole verbatim strings sitting beside 500-character boundary-aware
chunks, scored against each other as though they were the same kind of thing. The path does not merely
lack a feature; it violates two invariants the rest of the system is now built on.

**And erasure stopped being hypothetical.** The system stores tenant-supplied text that routinely
contains personal data — the repo's own fixture corpus includes a CV, because that is what it was
developed against. A tenant whose customer exercises a right to erasure needs us to remove a *named
document*. Today, for anything that arrived via `/ingest`, we cannot.

## The trap that decides the whole design

**The gap is not that these vectors cannot be deleted. It is that they cannot be *attributed* — and
the remedy for data already written is therefore destructive, not corrective.**

This distinction decides the phase, and getting it backwards produces the wrong plan.

Deletion of existing `/ingest` points is entirely possible: they carry `tenant_id`, and
`Condition::is_empty("document_id")` exists in the pinned client, so *"delete everything unattributed
for this tenant"* is one filtered call. What is impossible is saying **which** of them came from which
upload — because the only thing that ever knew was the caller, and we did not write it down.

So a new contract fixes the future and **cannot fix the past**. There is no migration that
reconstructs attribution: no row to join, no key to derive, no order to rely on. The choice for
existing data is between:

1. **Delete it.** Honest, immediately compliant, and destroys retrieval for any tenant who used
   `/ingest` as their real path.
2. **Leave it.** Keeps those tenants working and means the system still cannot honour a per-document
   erasure request for that data — which is precisely the blocker this phase exists to close.

**There is no third option, and a design that implies one is wrong.** The recommendation (D6) is to
delete, loudly and per tenant, with the count reported — but the decision belongs to whoever knows
what is in those collections.

**The second half of the trap:** this is a *contract* change on a route that is already deployed and
already called. `/ingest` currently accepts `{"texts": [...]}` and returns `{"ingested": n}`. Anything
that mandates a `document_id` breaks every existing caller. That is acceptable — CLAUDE.md says the
path is unsupported — but it must be a decision rather than a discovery (D2).

## Decisions to settle (open — recommendations given)

### D1 — is `/ingest` fixed, or deleted?

The honest fork, stated first because everything else depends on it. CLAUDE.md's standing position is
*"not a supported path. Do not build on it."* Deleting the route closes the blocker completely and
costs nothing but the convenience of seeding a demo without a file.

**Recommendation: fix it, and promote it to a supported path.** Inline text is a legitimate ingestion
mode — an FAQ pasted from a CMS, a policy synced from another system, content that never existed as a
file. The reason to keep it is not the demo; it is that a tenant with content and no file currently
has to invent one. But the fix must make it a *document* in every sense, not a document-shaped
exception — which is what the rest of these decisions are about.

**Risk:** promoting it means it inherits every obligation the upload path has. If that is not wanted,
delete the route instead; a half-supported path is how this debt was created in the first place.

### D2 — who supplies the `document_id`?

Options: the client supplies one; the server mints one and returns it; the client supplies an opaque
`external_id` and the server maps it.

**Recommendation: the server mints the `document_id`, and the client may supply an optional
`external_id` for idempotency.**

A client-supplied `document_id` is a UUID in a globally-unique primary key, so one tenant could name a
row belonging to another — the id would collide before RLS ever sees it, and a `409` on a foreign id
becomes an existence oracle for other tenants' documents (the rule invariants 8, 18 and 26 all draw).
Minting server-side removes that surface entirely.

`external_id` is what actually gives callers what they want from a mandated id: re-sync the same FAQ
and it *replaces* rather than duplicates. Unique per `(tenant_id, external_id)`, so it cannot collide
across tenants. Absent means "always create new", which preserves today's behaviour for a caller that
does not care.

### D3 — does the row look like an upload's row, and what about `object_key`?

`documents.object_key` is `text not null` with a **unique** constraint (`0003`, `0008`). An inline
document has no object, so this is a schema decision, not a detail.

Options: make `object_key` nullable; write a synthetic key; give inline documents their own table.

**Recommendation: make `object_key` nullable, and add a `source` column (`upload` | `inline`).**

Reject the synthetic key: a value shaped like an object key that addresses no object is a lie the
deletion saga, the reaper and every future reader would have to be told about individually. Reject a
separate table: two tables means every query, every RLS policy and every listing forks, and the
deletion saga — the thing this phase exists to make work — would need two implementations of what is
one idea.

`source` earns its place by making the difference *queryable* instead of inferable from a null. The
reaper's expiry sweep must skip inline documents (they can never receive an object, so `uploading` and
`expired` are meaningless for them), and "where `object_key is null`" is a subtler way of saying that
than "where `source = 'inline'`".

### D4 — synchronous, or through the worker?

The upload path is asynchronous by necessity: bytes land in MinIO, storage announces it, the worker
picks it up. Inline text has none of that — the content is in the request body.

**Recommendation: synchronous.** The API creates the row, chunks, embeds and upserts, then returns
`201` with the `document_id` and the chunk count. A caller who just handed us the text should not have
to poll to learn whether we accepted it.

The cost is honest: `/ingest` becomes a request that makes a billed `/embeddings` call and can take
seconds for a long text. It is already rate-limited and `sk_`-only, and it already makes that call
today. The alternative — publish a job and return `202` — buys asynchrony the caller did not ask for
and re-introduces the "did it work?" polling the upload path only tolerates because it must.

**Risk:** a very long text becomes a very long request. Cap the input (D7).

### D5 — inline text goes through the same chunker. Non-negotiable.

Today `/ingest` stores each string verbatim as one point. Under phase 10's widened invariant 6 that is
already a violation: `documents_v2` holds whole strings beside 500-character chunks, and a cosine score
between a 40-character string and a 500-character chunk compares two different kinds of thing.

**Recommendation: `common::chunk::chunk_with_spans`, the same call the worker makes, with the same
constants.** Every provenance field follows — `chunk_index`, `char_start`, `char_end`, `created_at` —
and a deterministic `uuid_v5(document_id, chunk_index)` point id, which is what makes re-sync an
overwrite rather than a duplication (invariant 9, which this path has never satisfied).

**This changes retrieval for existing `/ingest` users**, and they will not be expecting it: a
previously-whole string may now be two chunks, and scores will move. It is a correction, not a
regression, but it is a behaviour change on a live path and belongs in the release note.

### D6 — what happens to the data already written?

Per the trap: attribution cannot be reconstructed. Only deletion or retention is available.

**Recommendation: an explicit, per-tenant, opt-in purge — `worker purge-unattributed` — that reports
what it *would* delete before deleting anything.** Not automatic on deploy, and not silent.

The sweep is one filtered call per tenant: `tenant_id = X` **and** `is_empty("document_id")`. It must
loop tenant by tenant rather than filter globally, for the reason `reaper.rs` already loops — and it
must print counts per tenant so an operator can see whose data is about to go.

Rejected: purging automatically at startup, which would delete a tenant's working corpus because they
deployed on a Tuesday. Also rejected: leaving it undocumented and hoping nobody asks — that is exactly
the shape of the decision that produced this debt.

**And note what this does not fix.** Until an operator runs it, per-document erasure is impossible for
that data. The purge is the compliance instrument; the new contract only stops the problem growing.

### D7 — the request contract, and its bounds

**Recommendation:**

```
POST /ingest
{
  "filename":    "faq.md",          // required — it is a document; it needs a name
  "text":        "…",               // required
  "external_id": "cms-faq-42"       // optional; unique per tenant, makes re-sync an overwrite
}
→ 201 { "document_id": "…", "chunks": 12, "status": "ready" }
```

`texts: [...]` becomes `text: "…"`, singular. The array was the shape of "write these vectors"; a
document is one thing with a name. A caller with several documents makes several calls, which is also
what makes each one separately erasable — the entire point of the phase.

Bounds, all of which are currently absent: cap the body (`MAX_UPLOAD_BYTES` is the obvious ceiling and
already exists, though it is enforced by the worker for uploads — this path can enforce it directly,
which is the one place the "there is no earlier" of invariant 11 does not apply); reject empty or
whitespace-only text with `422`; validate `filename` through `common::key::extension_of` so the
extension rules match the upload path.

### D8 — does deleting a tenant erase its stores?

Out of scope for the strict reading of this phase, but it is the neighbouring half of the same
obligation and the design should not pretend otherwise. `DELETE FROM tenants` cascades in Postgres and
does **nothing** in Qdrant or MinIO.

**Recommendation: state it as the next phase, and do not quietly imply this one covers it.** Phase 11
makes per-document erasure possible for every path; tenant-level erasure is a separate endpoint with
its own saga, and listing it here is how it stops being forgotten.

## Verification

Following phase 9's rule — *a passing test proves nothing until you have watched it fail* — and phase
10's, that a metric must be sabotaged before it is believed. The deliverable here is an **erasure
guarantee**, so the tests must attack erasure.

| Break | Test that must fail |
| --- | --- |
| Drop `document_id` from the `/ingest` payload write | "an ingested document is deletable by id" |
| Make the deletion saga skip a null `object_key` by erroring rather than continuing | "deleting an inline document returns 204" |
| Remove the `(tenant_id, external_id)` unique constraint | "re-syncing the same external_id overwrites, not duplicates" |
| Chunk inline text with a different size than the worker uses | a bench run — the collection now holds two recipes (invariant 6) |
| Scope the purge sweep to `is_empty(document_id)` without `tenant_id` | "purging tenant A leaves tenant B's unattributed points" |

Beyond the break table, the property worth asserting directly, because it is the phase's whole claim:

> **After `DELETE /documents/{id}` on an inline document, a search for its distinctive text returns
> nothing, for that tenant and every other.**

That is one integration test, it needs the real stack, and it is the thing a compliance reviewer would
actually ask to see demonstrated.

## Known debt & traps

| Don't | Do | Why |
| --- | --- | --- |
| Let the client choose the `document_id` | Mint it; accept an optional `external_id` | A client-chosen UUID collides in a global primary key before RLS is consulted, and the resulting `409` is an existence oracle for other tenants' rows |
| Write a synthetic `object_key` for inline documents | Make the column nullable and add `source` | A value shaped like an object key that addresses nothing is a lie the deletion saga, the reaper and every future reader must each be told about separately |
| Store inline text verbatim as one point | Chunk it with `common::chunk`, same constants | The collection would hold whole strings beside 500-char chunks and score them against each other — invariant 6, which this path already violates today |
| Purge unattributed vectors automatically on deploy | An explicit per-tenant command that reports before it deletes | It is someone's working corpus. Automatic and silent is how this debt was created |
| Filter the purge on `is_empty(document_id)` alone | Add `tenant_id`, and loop per tenant | Same reason `reaper.rs` loops: a cross-tenant operation here deletes a stranger's data, and under RLS the Postgres half would report success having matched nothing |
| Claim this phase makes the system "GDPR compliant" | Say it closes per-document erasure | Erasure is one obligation. Tenant deletion (D8), conversation-history redaction, audit trails and retention policy are all still open |

**Left standing, deliberately:** tenant-level erasure across all three stores (D8 — the next phase);
`messages` retaining passage text after its source document is deleted; any audit trail of erasures;
retention policy; and the `~30-minute` deferred-deletion window when a delete races an active index
(phase 8's known trade, pinned in both directions by phase 9b).

## Invariants this creates or touches

Per *Working here* — edit the invariant, then write the code, in the same commit.

- **Invariant 9 finally holds on every path.** "Indexing the same document twice is a no-op, not a
  duplication" has always carried an unwritten exception for `/ingest`. With a `document_id` and a
  UUIDv5 point id, the exception goes away and the sentence becomes true as stated.
- **Invariant 6's provenance clause becomes universal.** "Every indexed point carries `document_id`,
  `chunk_index`, `char_start`, `char_end` and `created_at`" currently ends with "`POST /ingest` writes
  none of them". That clause is deleted, and its deletion is the phase.
- **New — every indexed point is erasable by document.** The one the phase exists for: for every point
  in the collection there is a `documents` row whose deletion removes it. Stated as an invariant
  because it is exactly the property a compliance question asks about, and because the only way it
  breaks is by someone adding a *second* write path that skips the row — which is how it broke the
  first time.
- **Invariant 11 is untouched but worth re-reading.** "Upload size cannot be enforced at upload time —
  there is no earlier" is about presigned PUTs. Inline text arrives in the request body, so this path
  *does* have an earlier, and D7 uses it. The invariant is about a mechanism, not a wish.

## Open questions for review

1. **Fix or delete (D1)?** Everything here assumes fix. If `/ingest` is genuinely only ever a demo
   convenience, deleting the route closes the blocker faster and with less surface.
2. **What is actually in the existing unattributed vectors?** D6's recommendation is to purge, and
   that is easy to recommend when it is not your data. Whoever knows what those collections hold
   should make that call — and if the answer is "nothing important", the whole of D6 becomes a
   one-line command rather than a decision.
3. **Does `external_id` need to be exposed on `GET /documents`?** It is the caller's join key back to
   their own system, so probably yes — but it is also tenant-supplied text in a listing that other
   code renders, which deserves a moment's thought rather than a reflex.
4. **Should this phase absorb tenant-level erasure (D8)?** It is the other half of the same
   obligation, and splitting them means the system spends another phase unable to answer "delete
   everything you hold about us". Against: it is a distinct saga with its own failure modes, and this
   phase is already a contract break plus a schema migration.


## Outcome

Built and verified live. `POST /ingest` now creates a real document; invariant 29 (*every indexed
point is erasable by document*) is written down and true.

**Two decisions in this document were reversed while implementing it, and the design reads
persuasively either way — which is why they are recorded rather than quietly corrected.**

**D4 was wrong: `/ingest` is asynchronous, not synchronous.** The doc argued the API should chunk,
embed and upsert directly, so a caller who just handed us the text need not poll. That design stores
the text *only* as Qdrant vectors — and phase 10's `worker reindex` walks `documents` rows reading
`object_key`. **On the next collection version bump every inline document would have vanished
silently**, which is the same class of unaccountable data this phase exists to eliminate. Writing the
bytes to MinIO instead makes them re-indexable and, more importantly, collapses the work: the row,
the lifecycle, chunking with provenance, the deletion saga, the reaper and the re-index driver all
already existed and all now serve this path unchanged. The cost is the `202` the doc wanted to avoid.

**D3 was wrong, and falls out of D4: `object_key` stays `NOT NULL` and there is no `source` column.**
The key is real because the object is real. Implementation also found the trap that would have made
the nullable version expensive: `Row::get::<String, _>` **panics** on a SQL NULL, and `object_key` is
read that way in three places (`handlers.rs`, `reaper.rs`, `worker/main.rs`). `source` was redundant
in any case — it duplicated a fact the key already implied.

Net effect: **one migration** (`external_id` plus a partial unique index), where the doc implied
schema surgery.

**Two pre-existing bugs surfaced, both worth more than the feature.**

1. **`ensure_collection` was not idempotent under concurrency.** `collection_exists` and
   `create_collection` are two calls; two processes starting together both see "absent" and both
   create, and the loser died. Three parallel tests found it — but **two API instances booting
   simultaneously is normal production behaviour**, so this was a real availability bug hiding
   behind a single-instance dev setup. It now tolerates a concurrent creator.
2. **The integration harness's teardown named the wrong collection.** It used a `"documents"`
   literal, and phase 10 renamed the collection to `documents_v2` — so teardown silently deleted
   from nothing for an entire phase, and only surfaced now because the old collection no longer
   exists to absorb the call. It uses `api::COLLECTION` now. A literal that was correct when written
   is exactly the drift `common` exists to prevent.

**And one gap in this document's own plan**, found by using the thing: `purge-unattributed` purged
*every* tenant, when a right-to-erasure request concerns exactly one. It takes an optional tenant
argument now. The design said "per tenant, never globally" and meant it about the filter; it did not
notice that the operator needs to say *which*.

**Verified live**, on a full stack: inline text → object → `ObjectCreated` → worker → `ready`;
payload carries `document_id`, `chunk_index`, `char_start`, `char_end`, `created_at`; `/ask` answers
from it; `DELETE` returns `204` and a search for the document's distinctive text returns **zero
hits**, with **zero points** left in the collection. `purge-unattributed p11 --yes` erased three
planted orphans and left the other tenant's two untouched.

**Break table, executed** — each reverted in the same sitting:

| Break | Result |
| --- | --- |
| Write the object, skip the `documents` row | erasure + listing tests red; validation stayed green ✅ |
| Ignore `external_id` on lookup | re-sync test red, others green ✅ |
| Drop the `tenant_id` leg of the purge filter | verified manually (no automated coverage — recorded as a gap) |

**Still open, and deliberately not claimed:** tenant-level erasure (D8) erases nothing outside
Postgres; `messages` retains passage text after its source document is deleted; there is no audit
trail of erasures and no retention policy. This phase closes **per-document** erasure. Calling it
"GDPR compliant" would be the kind of claim CLAUDE.md exists to prevent.
