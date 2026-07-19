# Feature: Seeing the system fail, and getting it back (phase 13)

> Status: **built.** A token-gated `/metrics`, a live `GET /admin/ops/tenants`, backup/restore
> scripts, and a restore drill that was **actually run** — output below. Closes
> [production blocker 3](../production-readiness.md), with its residues named.

## Context — why

Phases 11 and 12 closed the two *correctness* blockers: the system can erase a document and erase a
tenant, and prove it. Blocker 3 was what still argued against self-serve, and it had become the one
that mattered most — **the system could erase data correctly and could not tell you when it was
failing.**

That matters more here than in most systems, because almost everything that goes wrong in this one
goes wrong *quietly*. A refusal is a `200`. A half-re-indexed collection answers nothing while
`/health` stays green. A cross-tenant `UPDATE` under RLS matches zero rows and reports success. The
blocker named four failures that nobody would notice, and it closed on backups being
**restore-tested** plus one alert each for worker death and dead-letter depth.

## The trap: a metric label is a store erasure cannot reach

**`botflow_ask_total{tenant="acme"}` would have put a tenant's identity and activity into a store
that `DELETE /admin/tenants/{id}` cannot touch** — days after building erasure across three stores
with an audit trail that outlives its subject.

And this is not "we would have to remember to clean it up." Prometheus is *designed* not to be
deleted from: the admin delete API is off by default, a delete is a tombstone until compaction, and a
remote-write copy may have no delete path you control. So the one store a compliance question is
about would be the one we could not answer for — which is invariant 29's own failure mode, a partial
erasure that looks like diligence.

So: **no tenant labels, ever** (invariant 30), enforced by a rule that is checkable by reading code
rather than data — *every label value is a variant of a closed enum or a `const`*. That same rule
keeps invariant 16 out of the label space and bounds cardinality.

The cost is real and accepted: you cannot ask who caused last Tuesday's spike. `GET /admin/ops/tenants`
answers who is causing *this* one, live from Postgres — and it is safe precisely because it retains
nothing, so a tenant erased by phase 12 vanishes from it in the same statement.

## What was built

**`/metrics`**, registered **only** when `METRICS_TOKEN` is set — not "registered and 401", because
an endpoint that refuses still confirms it exists. Deliberately not `ADMIN_API_KEY`: that key can now
*erase a tenant*, and a scrape config is a low-trust, widely-readable artifact.

The four named failures, mapped to things that actually move:

| Silent failure | Metric |
| --- | --- |
| Retrieval degrading | `botflow_ask_refused_total / botflow_ask_total` — **the canary**, because a refusal is a `200` and has no other observable |
| Gateway 429s, dead-lettering | `botflow_queue_messages{queue="…dlq"}`, `botflow_embed_requests_total{outcome}` |
| Spend inside a rate limit | `botflow_ask_total` + `botflow_rate_limited_total`, with attribution via the ops endpoint |
| The reaper failing | `botflow_documents_overdue{kind}` — the *effect* the reaper exists to prevent, not a liveness ping |

**Worker death, with zero worker code.** A passive `queue_declare` returns `consumer_count` as well
as message count, so `botflow_queue_consumers{queue="document_events"} == 0` **is** worker death —
reported by the broker rather than asserted by the worker, which is strictly better evidence than a
heartbeat (a wedged worker deregisters; a heartbeat thread keeps ticking). No listener, no port, no
heartbeat table.

**Backups of the two stores that cannot be rebuilt.** Qdrant is deliberately excluded because phase
10's `worker reindex` reconstructs it from MinIO + Postgres — a real payoff from earlier work that
halves the backup surface. The cost is stated rather than buried: recovery is a full, billed re-embed,
and pre-phase-11 `/ingest` points cannot be rebuilt at all.

## The drill — run, not assumed

The blocker says *restore-tested*, and the gap between tested and assumed is the entire point.
Executed 2026-07-19 against the dev stack:

1. Seeded a tenant, ingested two documents, asked a question (grounded answer, 1 source), deleted one
   document to create an `erasures` row.
2. `./scripts/backup.sh` → `tenants: 1, documents: 1, erasures: 2, objects: 1`.
3. **`./scripts/reset.sh -y`** — the repo's real destroy button. Confirmed dead: the pre-backup `sk_`
   returned `401`, `tenants: 0`.
4. `./scripts/restore.sh` → counts matched the manifest exactly.
5. Restarted the API, stopped the worker, `worker reindex` → `1 document, 1 chunk, 0 failed`.

Three acceptance checks, and only the second one proves anything:

| | Check | Result |
| --- | --- | --- |
| (a) | Rows + credentials restored — old `sk_` lists its documents | ✅ `policy.md / ready` |
| (b) | **A real question gets a grounded answer** | ✅ *"The warranty period for the Zephyr-9 unit is 37 months from delivery"*, 1 source |
| (c) | The audit trail survived the disaster | ✅ the phase-12 `doomed` row, naming a tenant that no longer exists |

**(b) is load-bearing and must never be replaced by row counts.** A restore with perfect counts and
an empty collection refuses every question and looks entirely healthy from the dashboard — which is
exactly what step 5 would produce if someone skipped it.

### What the drill caught that review had not

**The backup silently contained zero objects.** `docker compose cp` failed, a `|| echo` fallback
masked it, and the manifest reported `objects: 1` — because it counted the *store's inventory* rather
than the archive. A backup that looks like it worked and contains nothing is the single worst outcome
in this phase, and only running it found it. Three fixes: mirror via the **S3 API** rather than
copying MinIO's `xl.meta` backend (which round-trips only into a byte-identical MinIO and is useless
against real S3), count from what was actually written, and **refuse** to produce a backup that has
document rows and no objects.

**A running API does not recreate a dropped collection.** `ensure_collection` runs at startup, so
leaving the API up through a restore leaves the collection missing and the reindex fails with
*"Collection doesn't exist"*. The restore script now says *restart*, and says why.

## Verification of the instruments themselves

Every metric an alert reads was watched moving:

| Action | Metric | Observed |
| --- | --- | --- |
| Kill the worker | `botflow_queue_consumers{queue="document_events"}` | 1 → 0 ✅ |
| Seed a row 45m past its lease | `botflow_documents_overdue{kind="stuck_processing"}` | 0 → 1 → 0 ✅ |
| Ask with nothing indexed | `botflow_ask_refused_total` | +1, while an answered question left it unchanged ✅ |
| Upload a document | `botflow_documents{status="uploading"}` | appears — proving the `SECURITY DEFINER` path dodges RLS ✅ |

Plus integration tests: `/metrics` 401s on a wrong token **and on `ADMIN_API_KEY`**, the gauges see
across tenants, and the refusal counter moves only on refusals.

**The RLS trap this last one guards** is worth restating: `documents` is RLS-forced and the API is
`app_user`, so a plain `SELECT count(*) … GROUP BY status` matches zero rows and *reports success*.
Every gauge would read 0 and the dashboard would be permanently, beautifully green — in the one
endpoint whose entire job is to say something is wrong.

## Known debt & traps

The Traps table in CLAUDE.md carries eleven new rows from this phase. The ones most likely to bite:
tenant labels, scraping with the admin key, aggregating over an RLS table, passive-declaring on the
shared channel, backing up MinIO before Postgres, and `docker cp`-ing MinIO's backend.

**Left standing, deliberately:**

- **Alert *delivery*.** `doc/ops/alerts.yml` is a rules file. There is no Prometheus and nowhere to
  send to; the file says which rules have been fired (none) and which metrics have (all of them).
- **No per-tenant history** — the invariant-30 trade, paid knowingly.
- **No latency histograms**, with the stopping point written into `metrics.rs`: take the `metrics`
  crate rather than extending that module.
- **No structured error reporting** (Sentry and similar). The blocker mentions it; this phase does
  not close it, and the blockers doc says so rather than letting "CLOSED" imply otherwise.
- **Backups are manual, local, unencrypted, unrotated, with no PITR.** The scripts are the mechanism;
  scheduling and offsite copies are deployment decisions this repo does not make.
- **The drill is manual and billed** (step 5 re-embeds), so it is not in CI. Its date going stale is
  itself information.
- **`/metrics` on a second port** is the stronger isolation for a shared cluster, and is not built.
