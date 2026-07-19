# Feature: Ending a tenant, and proving it (phase 12)

> Status: **built.** `DELETE /admin/tenants/{id}`, an `erasures` audit trail that outlives its
> subject, and redaction of answers that quoted a deleted document. Closes
> [production blocker 2](../production-readiness.md) except for one window it deliberately leaves
> open (see *Left standing*).

## Context — why

Phase 11 made a *document* erasable. This is the other half of the same obligation, and the half a
processor is actually asked for: **"delete everything you hold about us."**

Until now the answer was that we could not. `DELETE FROM tenants` cascades through `api_keys`,
`documents`, `conversations`, `messages`, `accounts` and `sessions` — and does **nothing** in Qdrant
or MinIO. Every vector and every uploaded byte survived, tagged with the id of a tenant that no
longer existed: unreachable through the product (no key, no session, no row) and therefore invisible,
which is worse than merely present. There was also no way to demonstrate that any erasure had
happened at all — no record of what was removed, when, or at whose request.

**One claim in the blockers document was wrong, and correcting it changed the design.** It said
`messages` "retains the passage text the model was shown". It does not: `append_turn` stores the
user's question and the assistant's answer, and nothing else. The real gap is narrower and still
real — an *answer* is derived from the passages and routinely recites them, so deleting a document
while leaving the answers that quote it is an erasure with a hole in it. That is a different problem
with a different fix, and it is fixed here.

## The trap that decides the design

**An audit trail that cascades with the thing it audits is worse than no audit trail, because it
looks like diligence.**

Every tenant-scoped table in this schema carries `references tenants(id) on delete cascade`. That is
correct for data — it is the whole reason a tenant deletion is one statement — and it is exactly
wrong for the record of that deletion. Give `erasures` the foreign key every neighbouring table has,
and deleting a tenant destroys the evidence that the tenant was deleted, in the same statement, with
nothing anywhere to indicate it happened.

So `erasures` has **no foreign key and no RLS**, and `tenant_id` is plain text. It is an operator's
record rather than a tenant's, and the row for an erased tenant has no tenant left to scope it to.
This is the single decision the phase turns on, and the one a reasonable implementation gets wrong by
being consistent with its neighbours.

The property is pinned by a test that was watched failing: adding the foreign key makes
`the_erasure_record_survives_the_erasure` go red. Worth noting how the break behaved — Postgres
*refused* to add the constraint at first, because rows already referenced tenants that no longer
existed. The schema disagreeing with itself is the property, stated by the database.

## What was built

**`DELETE /admin/tenants/{tenant_id}`** — admin-gated, like tenant *creation*, because these are the
operations that make and unmake the tenancy registry itself. It returns `200` with counts rather than
`204`: a caller acting on an erasure request needs evidence, and "1 vector, 1 object" is evidence.
An unknown tenant is `404` — not for non-oracle reasons (the admin key already sees every tenant) but
so a typo cannot read as a completed erasure.

The saga, in order, mirroring phase 8's document deletion for the same reason — a crash partway must
leave the least-bad orphan:

1. **Revoke access first**, in its own committed transaction. From that moment nothing can
   authenticate as the tenant, so no new work can begin while the erasure runs. Without it a client
   could mint an upload URL into a tenant being erased.
2. **Vectors**, one filtered call.
3. **Objects**, listed under `tenants/{id}/` and deleted. Listed rather than derived from document
   rows: a row is not the only thing that can put an object there, and an erasure that removes only
   what the database remembers has a blind spot.
4. **Rows** — one `DELETE`, and the cascade does the rest.
5. **Vectors again.** A worker already mid-index holds no database lock (invariant 10) and can upsert
   after step 2. Its `mark_ready` will find no row and stop, correctly — but the chunks it wrote are
   already in the collection. The second sweep costs one filtered call and converts an unbounded race
   into a bounded one.

**Message redaction.** Assistant turns now carry `{"document_ids": [...]}` in `metadata` — the
documents whose passages were in the model's context. Deleting a document redacts the turns that cite
it. **Redaction, not deletion**: removing the row would leave a user's question answering itself and
renumber a conversation the client still holds.

**The `erasures` table**, written for document *and* tenant erasures, recording scope, actor kind,
what was removed, and `completed_at` separately from `requested_at` — because a row that never
completed is the interesting one, and one timestamp cannot say that.

## Verification

Break table, executed, each reverted in the same sitting:

| Break | Result |
| --- | --- |
| Drop the `tenant_id` filter from the vector erasure | the **control** went red — erasing one tenant took the other's vectors ✅ |
| Add `references tenants(id) on delete cascade` to `erasures` | the audit-survival test went red ✅ (and Postgres first *refused* the constraint, which is the same fact) |

**Verified live**, full stack, two tenants with a document each: erasing `doomed` reported
`1 vector, 1 object`, left `keeper`'s data intact, removed the tenant row — and the `erasures` row
for `doomed` **remained, naming a tenant that no longer exists.**

22 integration tests green.

## Known debt & traps

| Don't | Do | Why |
| --- | --- | --- |
| Give `erasures` a foreign key to `tenants` | Leave it unreferenced, `tenant_id` as plain text | It would cascade away in the same statement as the erasure it records. Consistency with neighbouring tables is exactly the wrong instinct here |
| Erase vectors before revoking access | Revoke keys and sessions first, in their own transaction | Otherwise a client can mint an upload URL into a tenant mid-erasure, and the bytes land after the sweep |
| Sweep vectors once | Sweep again after the rows are gone | A worker mid-index holds no lock and can upsert between the two. The second call is cheap and bounds the race |
| Delete conversation turns whose source was erased | Redact the content, keep the turn | Removing the row leaves the user's question answering itself and renumbers a conversation the client is still holding |
| Assume `messages` stores the retrieved passages | It stores the question and the answer | The answer *quotes* them, which is the actual problem — narrower than "we store the passages", and it needed provenance in `metadata` to fix rather than a schema change |

**Left standing, deliberately:**

- **The ~30-minute deferred-delete window** when a *document* deletion races an active index (phase
  8's known trade, pinned in both directions by phase 9b). Tenant erasure's double sweep bounds the
  equivalent race; the document path still waits out the lease.
- **Messages written before this phase carry no provenance** and cannot be found by redaction. There
  is no way to reconstruct which document an old answer quoted — the same shape of problem as
  phase 11's unattributed vectors, and the same honest answer: it is not recoverable.
- **No retention policy.** Nothing expires on its own; erasure is always something someone asks for.
- **The audit is not tamper-evident.** It is a table an operator with database access can edit. Making
  it append-only (a trigger, or shipping it off-box) is a different phase with a different threat
  model — this one assumes the operator is trusted and the auditor is asking in good faith.
- **`purge-unattributed` is not audited.** It erases pre-phase-11 vectors and writes no `erasures`
  row, because it operates below the document model this table describes. Worth closing.

## Invariants this creates or touches

- **Invariant 29 widens from a document to a tenant.** It said every indexed point is erasable by
  document. It now also says a tenant can be erased entirely — vectors, objects and rows — and that
  **the record of an erasure outlives its subject**. The second clause is a property of the schema,
  not of a handler, and it is why `erasures` has no foreign key.
- **Invariant 3 is load-bearing here in a new way.** Object erasure works by prefix, so the tenant
  slug regex — enforced in the application *and* a DB `CHECK` — is what stops one tenant's erasure
  reaching into a neighbour's prefix. It was written to stop an upload escaping; it now also stops a
  deletion escaping.
- **Invariant 7 is unchanged but newly visible.** A turn is recorded only once an answer exists, which
  is why redaction has something coherent to redact: there are no dangling half-turns to reason about.
