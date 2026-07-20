# Feature: bounding the three things that had no ceiling (phase 15)

> Status: **built.** `GET /documents` pages by keyset cursor, `POST /auth/keys` is metered, and
> `/ask/stream` carries a wall clock. Closes [production blocker 5](../production-readiness.md) —
> the last one on that list.

## Context — why

Blocker 5 was three unrelated gaps filed together because they share a shape: **a request whose cost
is chosen by the caller, or by nobody.**

- `GET /documents` returned the tenant's entire table, fully materialised, on a route the dashboard
  polls every few seconds.
- `POST /auth/keys` let one logged-in session mint unbounded API keys.
- `/ask/stream` had no maximum duration — only a *stall* bound, which is a different thing.

None loses data, which is why they outlived the correctness work. All three get worse with success.

## Pagination: the part that was not about pagination

The obvious change is `LIMIT`. Two things underneath it were not obvious, and both were found by
measuring rather than reading.

**The sort had no index, and was not even sorting the column it appeared to.** `ORDER BY created_at
DESC` has been in this query since migration 0003. The `SELECT` renders `created_at::text AS
created_at` — and Postgres resolves a *bare* name in `ORDER BY` against the **output list first**, so
the alias wins and the sort runs on the text rendering, not the timestamp. Measured on 5k rows:

| Query | Plan | First-page cost |
| --- | --- | --- |
| `ORDER BY created_at` | `Seq Scan` + `Sort Key: ((created_at)::text)` | 371.70 |
| `ORDER BY documents.created_at` | `Index Scan using idx_documents_tenant_created`, **no sort node** | 0.29 |

So migration 0016 adds `(tenant_id, created_at desc, id desc)` — `tenant_id` leading because RLS
applies its predicate as a filter — and the `ORDER BY` is **qualified**, which looks like clutter and
is the only reason the index gets used.

It reads like a correctness bug too, since the keyset `WHERE` compares `timestamptz` while the bare
`ORDER BY` compared text. **It is not**, and the reason is worth recording so nobody fixes it twice:
`timestamptz` normalises to UTC on storage and renders in the session's TimeZone, so every row's text
carries the same offset and lexicographic order agrees with chronological order. *The bug is the
plan, not the result.* An earlier draft of this document claimed otherwise, and a test written to
prove the correctness half passed with the bug deliberately reintroduced — which is what exposed the
overclaim. That test was deleted rather than kept: a test that passes either way is worse than none.

**`created_at` is not unique, and the table is built to make that common.** It defaults to `now()` —
`transaction_timestamp()` — so every row written in one transaction shares a byte-identical
timestamp. A cursor naming only the timestamp cannot say where a page ended. Deleting the `id`
tiebreaker from the query and re-running the suite:

```
paging across identical timestamps returned 4 of 9 rows
```

Five documents gone, and the listing still renders perfectly. That is the failure mode this whole
feature had to be designed against, and it is why the cursor is `(created_at, id)` and the predicate
is the row-value comparison `(created_at, id) < ($1, $2)` — one expression rather than the
`a < x OR (a = x AND b < y)` it expands to, precisely so it cannot be written subtly wrong.

**Keyset, not `OFFSET`, because the list is polled.** With an offset, a document created between two
polls shifts every later row by one, so the reader silently sees a row twice or misses one.

### The `+` that decodes to a space

The cursor carries a UTC offset — `+00` — and a bare `+` in a query string decodes to a **space**.
So a hand-concatenated URL sends a timestamp with a space where the offset belongs. Three defences,
because this fails *only* at a page boundary and never on page one:

- `encode_cursor` emits ISO-8601 with a `T`, so the date/time separator is not a second space.
- The web client builds the query with `URLSearchParams`, pinned by `documents.test.ts`, which parses
  the result back and asserts it round-trips.
- A cursor we did not mint is a **422**, not a 500. `is_our_timestamp` validates shape *and range*
  before the value reaches a `::timestamptz` cast, because `9999-99-99T99:99:99` passes any
  "looks like a date" check and still fails the cast — and a failed cast is a database error that `?`
  turns into a 500 for what is plainly the caller's malformed input.

### The default is the feature

`limit` is optional and defaults to 50. That is deliberate and is the part that actually closes the
blocker: an **un-updated client**, sending no parameters, is bounded on deploy. Requiring the
parameter would have left every existing caller exactly as unbounded as before.

No `total` (counting is the full scan this replaces) and no "previous" (a keyset cursor moves one
way). The dashboard gets "previous" from browser history, which is why each page is a real URL —
and why the pager is anchors rather than buttons, keeping the no-JavaScript read guarantee that
invariant 24 spends on *uploading* only.

## Metering `/auth/keys`

One line, and the interesting part is the bucket string. `rate_limit::check` keys on whatever it is
handed, so the bare `tenant_id` would have put key-minting in the **same** 60/min window as `/ask` —
a tenant provisioning keys would spend their own question budget, and their widget would start 429ing
for a reason no log connects to the dashboard tab that caused it. Hence `keys:{tenant_id}`.

Verified live at `RATE_LIMIT_PER_MINUTE=5`:

```
mint 1..5 -> 201    mint 6,7 -> 429
upload-url on the same session, after -> 201   (separate bucket)
```

`GET`/`PATCH`/`DELETE` stay unmetered: they create no rows, and their cost is bounded by the keys
that already exist.

## The stream deadline, and the trap it had to avoid

`READ_TIMEOUT` bounds *silence between reads*, which is what a hung gateway looks like. It says
nothing about total duration — a gateway emitting one token every 59 seconds streams forever while
never once looking unhealthy. That is the hole.

Two ways to close it wrong, both of which the invariant-28 write-up predicted:

**On the HTTP client.** A reqwest client `.timeout()` is a total deadline *including the body*, and
this body *is* the answer — it would cap how long an answer may be rather than how long a gateway may
hang. `llm.rs` has two tests that go red if someone adds it. So the bound lives in the handler's
loop, as an absolute `Instant` taken **once before** the loop: a per-token deadline resets on every
delta and bounds nothing.

**By treating it as a failure.** The obvious implementation sets `failed = true`. That emits an
`error` frame to a client that has already rendered three good paragraphs, *and* skips `append_turn`,
so invariant 7 drops from history a turn that only our own ceiling truncated. The user loses the
answer they watched arrive **and** the record of having asked. So the deadline emits a normal `done`
and **persists what arrived**.

The cost is real and accepted: an answer cut mid-sentence becomes history the next rewrite reasons
over. A slightly awkward follow-up beats a vanished conversation.

## `/ask/stream` got its first tests

The route had none — a standing entry in CLAUDE.md's coverage inventory — and it seemed wrong to
restructure the loop that emits every frame a widget consumes without changing that. `ask_stream.rs`
covers frame order, the **data-less `done`** (the reason a browser `EventSource` cannot be used
here), the persisted turn, a refusal costing no LLM call at all, and a `pk_` reaching the route
(invariant 27, previously pinned only for `/ask`).

## What is deliberately still open

- **The deadline firing is untested.** At 300s a real test would take five minutes, and a faked clock
  would assert against a stub rather than the code. Reasoned and reviewed, not proven — stated here
  rather than papered over.
- **No `total`, no backwards cursor.** Consequences of the keyset choice, above.
- **A truncated answer is stored mid-sentence.**
- **`?limit=` is not exposed in the dashboard**, which always uses the default 50.
- **`GET /documents` still returns `deleting` rows by exclusion, not by index** — the partial filter
  is not part of 0016's index, which is fine at current selectivity and would not be at scale.

## Verification

```
cargo test --workspace              # 91 unit tests
cargo test --workspace -- --ignored # 32 integration tests, incl. pagination.rs and ask_stream.rs
bun run test                        # 196 web tests
```

Both new suites were watched failing first. The tie test drops to `4 of 9 rows` without the `id`
tiebreaker; the web cursor test fails if the query string is concatenated rather than encoded.

Live, against the running stack: seven documents inserted in **one transaction** (confirmed
`count(distinct created_at) = 1`) paged at `limit=3` returned 3 + 3 + 1 = **7 unique ids**; a raw
pasted cursor returned `422 invalid cursor`; `?limit=100000` returned 422; `?limit=lots` returned
400; and no parameters at all returned a bounded 50. The probe tenants were deleted afterwards.

One incidental confirmation: adding migration 0016 made every **worker** integration test fail with
`VersionMissing(16)` until `cargo clean -p worker`. That is the documented `sqlx::migrate!`
compile-time trap firing exactly as CLAUDE.md's trap table says it does — the worker embeds
`../api/migrations` and still held the old set.
