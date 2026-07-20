# Production readiness — what blocks going live

> Assessed 2026-07-19, against the phase-10 tree. Every claim below was checked against the code, not
> recalled; each carries the file it lives in so it can be re-checked rather than believed. CLAUDE.md's
> *Known state & debt* is the running inventory — **this document is narrower**: it asks only "what
> stops real customers using this", and orders by that.
>
> **Extended 2026-07-20 to cover `web/`, which it had never assessed.** Blockers 1–5 were all about
> the Rust API; the dashboard appeared only twice, in passing, as "the dashboard". That was a scoping
> error, not a judgement that the web tier was fine — nobody had looked. Blockers 6–8 are the result
> of looking, and two of them are worse than anything left on the API side.

## At a glance

| # | Blocker | Severity | Closed by |
| --- | --- | --- | --- |
| 1 | ~~`/ingest` vectors cannot be attributed to a document~~ | ~~blocking~~ | **CLOSED** — [phase 11](feature/phase-11-ingest-gdpr.md), invariant 29 |
| 2 | ~~No tenant-level erasure, no audit trail, history unredacted~~ | ~~blocking~~ | **CLOSED** — [phase 12](feature/phase-12-tenant-erasure.md); one window left open, stated below |
| 3 | ~~No metrics, no alerting, no backups~~ | ~~blocking~~ | **CLOSED** — [phase 13](feature/phase-13-observability.md), invariant 30; alert *delivery* and error reporting remain |
| 4 | ~~`failed` cannot tell a tenant whether to re-upload or wait~~ | ~~high~~ | **CLOSED** — [phase 14](feature/phase-14-failure-classification.md) |
| 5 | ~~`GET /documents` unpaginated; `/auth/keys` unmetered; `/ask/stream` unbounded~~ | ~~high~~ | **CLOSED** — [phase 15](feature/phase-15-bounded-reads.md) |
| 6 | **No account recovery** — a forgotten password is unrecoverable, and no email is ever sent | blocking | not designed |
| 7 | **`web/` has no deployment path, and 403s everything behind TLS as configured** | blocking | not designed |
| 8 | **The dashboard advertises features that do not exist**, and has no error page | high | not designed |

**Blockers 1–5 (the API) are closed** by phases 11–15. **Blockers 6–8 are new, and they are all in
`web/`.** They are not regressions — they were never assessed, because this document was written
about the Rust half and quietly stayed that way.

Re-verified 2026-07-20 after phase 15: `GET /documents` bounds every caller — including one sending
no parameters — and pages by keyset cursor; `POST /auth/keys` returns 429 past its own bucket's
limit; and `/ask/stream` carries a 300s wall clock that ends the stream with `done` rather than
discarding the answer. There is still **no** endpoint exposing `documents.error`.

Also verified 2026-07-20, by reading the files rather than recalling them: `grep -ri "password_reset\|
reset_token\|smtp\|mailer" crates/` returns **nothing**; the root `Dockerfile` never mentions `web`,
`node` or `bun`; `docker-compose.yml` has no `web` service; `web/package.json` has no `start` script;
`hooks.server.ts` sets **no** response headers; and `web/src/routes/(authenticated)/dashboard/+page.svelte`
is one line containing the text `dashboard tenant`.

## Verdict

**The API is in good shape. The product is not, and the gap is `web/`.**

An earlier version of this section said every blocker was closed and the rest was "a deployment
question". That was true of the Rust half and wrong about the system, because the web tier had never
been examined. Correcting it is the point of this revision.

Phases 11–15 closed the API blockers: erasure that can be proved, instruments that move, backups
that have actually been restored, a failure a tenant can act on, and no unbounded read, unmetered
write or unbounded stream.

**`web/` is roughly two-thirds of a product.** The path a new tenant walks — sign up, see the key
once, upload a document, watch it index, ask a question in the playground, copy the embed snippet —
is complete, and unusually carefully built: the login error is form-level so the API's non-oracle
survives into the UI (invariant 19), the one-time `sk_` moves in a read-and-delete httpOnly cookie
rather than a query param (invariant 22), an API outage renders an alert rather than an empty
library, and both JSON endpoints hand-roll an `Origin` check because SvelteKit's CSRF guard never
sees a JSON POST. Cookies are `httpOnly`, `secure: !dev`, `sameSite: 'lax'`.

**Everything around that path is missing**, and three pieces of it block going live — see blockers
6, 7 and 8. The shortest statement of the problem: *a user who forgets their password cannot get back
in, the app cannot be deployed by any means in this repo, and the sidebar offers them Billing.*

Standing deployment gaps, unchanged and still not code blockers:

- **Nobody is paged.** `doc/ops/alerts.yml` exists and **no rule in it has ever fired** — there is no
  Prometheus in this repo to fire them. Until that is wired, the system is observable but not
  monitored, and the difference is whoever notices first.
- **Backups are manual, local, unencrypted, unrotated, with no PITR.** The restore drill was real,
  and it was run by hand.
- **CI is report-only.** A red run does not stop a merge — and the `web` job runs `test` and `check`
  but **never `build`**, so a build-breaking change ships green.
- **`app_user` ships the dev password** from migration 0005 unless the role is pre-created.

A design-partner or internal pilot, with accounts created by an operator, is well served today.
Self-serve signup is blocked on 6 and 7.

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

### 6. No account recovery — a forgotten password is the end of the account

`grep -ri "password_reset\|reset_token\|forgot_password\|smtp\|mailer" crates/` returns **nothing**.
This is not a missing screen. There is no reset endpoint, no reset token, no email transport, and no
UI — the capability is absent at every layer, infrastructure included.

The registered auth surface is six routes (`lib.rs:208-219`): `register`, `login`, `logout`, `me`,
and three key routes. There is no change-password, no email verification (`accounts.rs:48` says so
outright — *"the real proof an address exists would be a confirmation email, which this MVP does not
send"*), no self-serve account deletion, and no way to change a tenant name after registration.

Why this is *the* blocker for self-serve rather than a gap: **every lockout becomes a manual database
edit by an operator**, and the operator cannot verify the requester owns the address, because nothing
ever proved the address was real at signup. That is a support burden and an account-takeover vector
arriving together. It also interacts badly with invariant 22 — the one-time `sk_` is genuinely
unrecoverable, so a locked-out tenant loses both their login *and* their ability to mint a new key.

Sequencing note: email verification and password reset want the same transport, so they are one
piece of work, not two.

*Closes when:* an email transport exists, `POST /auth/password-reset` issues a single-use expiring
token (hashed at rest, like every other credential here — invariants 14 and 17), and the reset page
consumes it. Verification of the address at signup should land with it.

### 7. `web/` cannot be deployed, and would 403 everything if it were

Two independent problems, both invisible in development.

**There is no deployment path.** The root `Dockerfile` builds only the Rust binaries — it never
mentions `web`, `node` or `bun`. `docker-compose.yml` has no `web` service. `web/package.json` has no
`start` script, so nothing invokes the `build/index.js` that `adapter-node` emits. The app builds
locally (verified: `bun run build` succeeds and produces `handler.js`/`index.js`) and there is no
artifact, process manager or documented command to run it anywhere.

**And behind TLS termination it would refuse every write.** `adapter-node` derives `url.origin` from
the request, so a proxy terminating TLS leaves the app seeing `http://…` while the browser sends
`Origin: https://…`. That mismatch fails SvelteKit's `csrf.checkOrigin` **and** both hand-rolled
guards in `documents/upload-url/+server.ts:26-31` and `playground/ask/+server.ts:28-36`. Every form
post, every upload, every playground question: 403.

The fix is the `ORIGIN` env var, and it appears in this repo exactly once — **commented out**, in
`web/.env.example:26`. `web/src/lib/server/env.ts` never reads it, never requires it, never warns.
The single variable whose absence breaks production hardest is the one with no enforcement, while
`API_BASE_URL` — which does have `required()` — throws on the *first request* rather than at boot, so
a port-open healthcheck reports a healthy server that 500s every page.

Adjacent, same area, cheap: `.dockerignore` excludes `widget/` but not `web/`, and its `.env` pattern
is root-anchored — so `web/.env` and `web/node_modules` are copied into the Rust builder context.
They do not reach the final images, which copy only the binaries, but they are in the build cache.

*Closes when:* a `web` image (or documented run command) exists, `ORIGIN` is required at boot beside
`API_BASE_URL`, and CI runs `bun run build`.

### 8. The dashboard advertises features that do not exist

The sidebar is still the shadcn sample, and `app-sidebar.svelte:14-17` admits it: *"Part sample
data… the teams, the Config group and every remaining `#` are mocked — nothing backs them."* What a
paying signup actually sees:

- A **Settings** submenu — General, **Team**, **Billing**, Limits — every one `url: '#'`
  (`app-sidebar.svelte:62-84`). These are exactly the account-management pages blocker 6 says do not
  exist, presented as though they do.
- A **tenant switcher** offering *Acme Inc*, *Acme Corp.* and **Evil Corp.** on plans
  *Enterprise/Startup/Free* (`:22-38`), rendered above the user's real tenant.
- **Upgrade to Pro**, **Account**, **Billing**, **Notifications** in the user menu, all inert
  (`nav-user.svelte:67-85`). Only **Log out** works.
- A breadcrumb reading *Build Your Application / Data Fetching* with `href="##"`, on every
  authenticated page (`(authenticated)/+layout.svelte:27-31`).

Then the destinations. **`/dashboard` — where onboarding sends every new user — is one line
containing the text `dashboard tenant`.** The marketing root `/` is an unstyled `<h1>` and two bare
links, which is what a cold self-serve visitor lands on.

**There is no `+error.svelte` at any level**, so a 404, a 500, or any thrown `error()` renders
SvelteKit's default black-on-white chrome: a status code, no styling, no navigation, no way back.

Finally one real inconsistency rather than a cosmetic one: `keys/error-map.ts:9-17` has **no 429
branch**, so now that phase 15 meters key minting, hitting the limit shows *"Something went wrong.
Please try again"* — advice that invites the retry that keeps them limited. `RATE_LIMITED` copy
already exists in three sibling maps.

This is filed as high rather than blocking because nothing here loses data or leaks anything. It is
filed at all because **offering Billing to someone who just entered a credit-card-free signup, and
landing them on the word `dashboard tenant`, does more damage to trust than a missing feature does.**
Deleting the mock nav is honest and takes minutes; building it is a product decision.

*Closes when:* the mock nav is removed or built, `/dashboard` and `/` have real content, a root
`+error.svelte` exists, and `mapKeyError` handles 429.

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

From the `web/` audit — none of these blocks, all are small:

- **No security response headers at all.** `hooks.server.ts` returns `resolve(event)` untouched: no
  `X-Frame-Options`/`frame-ancestors`, no CSP, HSTS, `nosniff` or `Referrer-Policy`. The clickjacking
  one is worth doing first and is a two-line change — `/onboarding/api-key` renders a live `sk_` into
  an input and is framable by any origin. CSP is less urgent than it looks: there is **no `{@html}`
  anywhere** in `src/`, so the stored-XSS surface is small.
- **`keyHash` is interpolated into a URL path unencoded** (`server/api/keys.ts:52,55`), and the
  `delete`/`revoke`/`updateOrigins` actions skip validation while `isUuid` exists two files away. The
  blast radius is bounded — the caller is authenticated and the API authorises per tenant — but it is
  an unnecessary primitive and inconsistent with the discipline everywhere else.
- **Env numbers are parsed with bare `Number()`** (`env.ts:33,35,53`), so `SESSION_TTL_SECS=30d`
  silently becomes `NaN` and then a malformed cookie `maxAge`. Fails quietly, which is this system's
  characteristic failure mode.
- **`dependencies` is empty — everything is a devDependency.** It works because the bundle inlines
  what it needs, but `bun install --production` yields a broken tree, which will matter the moment
  blocker 7 gets a Dockerfile.
- **Session expiry is a message, not a flow.** `ask.ts` and `upload.ts` render "Your session has
  expired. Please log in again" and then do nothing — no redirect, no re-login prompt. Navigation
  *is* handled correctly (`guard.ts` redirects with `redirectTo`); it is only the fetch surfaces.
- **Five pages have no `<title>`** — including `/`, `/login`, `/signup` and `/dashboard`.
- **No component, route or E2E tests.** All 18 web test files are pure unit tests, which is a
  deliberate and defensible line (`vite.config.ts` says so: the UI is shadcn primitives we do not
  own) — but it means no test exercises a `load`, a form action, or a rendered page.

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

cd web && bun run test && bun run check && bun run build   # note: CI does NOT run build
```

The `web/` blockers are claims about absence, which is the kind that rots quietly — a file appearing
does not make this document wrong loudly. Each is a one-line check:

```bash
grep -ri "password_reset\|reset_token\|smtp\|mailer" crates/   # blocker 6: expect no output
grep -n "web\|bun\|node" Dockerfile                            # blocker 7: expect no output
grep -n "ORIGIN" web/src/lib/server/env.ts                     # blocker 7: expect no output
grep -rn "url: '#'" web/src/lib/components/app-sidebar.svelte  # blocker 8: expect many
find web/src -name "+error.svelte"                             # blocker 8: expect no output
```

and read CLAUDE.md's *Known state & debt*, which is the superset this document filters.
