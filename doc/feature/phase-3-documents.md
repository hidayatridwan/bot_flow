# Feature: Documents — session auth + the dashboard library (phase 3)

> Status: **implemented.** Both halves shipped: the `Actor` extractor that lets a session reach the
> document routes, and the `web/` documents slice (list + upload + status). CLAUDE.md invariants
> 15/17/23/24/25, its *Traps* table, *Where things live* and *Known state & debt* were updated in the
> same change, as were README's endpoint table and upload section. 89 web unit tests + 41 Rust tests
> pass; the flow was driven end to end in a real browser (15/15 assertions, including the
> invariant-20 one).
>
> **Delete is deliberately not here** — see [Not in this phase](#not-in-this-phase).

## Context — why

Phase 2 shipped auth. Nothing else. A tenant could register, log in, hold a session, and land on a
dashboard whose entire content was the literal string `dashboard tenant`. The product could not do
the thing it exists to do.

And it *could not be built*, because of a contract problem rather than a UI one:

- `GET /documents` was guarded by `AuthTenant` + `require_secret()`. `AuthTenant` hashes the bearer
  and looks it up in **`api_keys`**.
- `SessionAuth` hashes the bearer and looks it up in **`sessions`**. Disjoint tables.
- A `sess_` sent to `/documents` found no `api_keys` row → **401**.
- The BFF holds no other credential. The one-time `sk_` is flashed to `/onboarding/api-key` and
  dropped (invariant 22); `GET /auth/keys` returns hashes. There is no recovery path short of
  minting a new key.

Three ways out: make the routes session-authable; store an `sk_` server-side (contradicts invariant
14's stated trade — *hash, don't encrypt*, and its cost is that we can never show a key again); or
mint a key per page load. Only the first is real. This was **decision D3**, deferred on purpose in
[`phase-1-tenant-auth.md`](./phase-1-tenant-auth.md): *"Extending `SessionAuth` onto data routes is a
clean phase-3 step once the account model is proven."* It is proven.

**Outcome:** a tenant uploads a PDF from the dashboard and watches it reach `Ready`. Bytes go browser
→ MinIO directly; the session never leaves Node.

## Part 1 — `Actor`, the union principal

CLAUDE.md's security section says *"Two auth principals, do not conflate them."* This does not
conflate them: both extractors stay intact and independently usable. `Actor` exists only for the
routes both may legitimately reach, and it widens nothing by itself — `require_management()` decides.

```rust
pub enum ActorKind { Secret, Publishable, Session }
pub struct Actor { pub tenant_id: String, pub kind: ActorKind }
```

Its `FromRequestParts` peeks the bearer's **prefix** and delegates: `sess_…` → `SessionAuth`,
anything else → `AuthTenant`. One table, one query. Delegation is literal — both impls have the same
signature and parse the same header — so the `pk_` Origin check inside `AuthTenant` is inherited
untouched. An `api_keys.kind` the DB CHECK should have made impossible fails closed with the same 401
as an unknown key, so it is not an oracle either.

**This made the `sess_` prefix load-bearing.** It was decoration before — neither extractor read it.
Now it selects the table, so renaming it would route every session into `api_keys`, miss, and 401 the
whole dashboard at once. Invariant 17 says so now.

| Route | Before | After |
| --- | --- | --- |
| `GET /documents` | `AuthTenant` + `require_secret` | `Actor` + `require_management` |
| `POST /documents/upload-url` | `AuthTenant` + `require_secret` | `Actor` + `require_management` |
| `POST /documents/{id}/upload-url` | `AuthTenant` + `require_secret` | `Actor` + `require_management` |

Unchanged on purpose: `POST /documents` (deprecated multipart) and `/ingest` — both are paths we are
not extending; `/ask`, `/ask/stream`, `/search` — `pk_` reaches these by design; `/admin/*`.

**Isolation is untouched**, and this is the whole reason the change is small: the handlers only ever
read `.tenant_id` and hand it to `db::tenant_tx()`. **RLS is keyed on the string, not on how the
string was obtained.** Verified: `sess_` and `sk_` return byte-identical rows, and tenant B's session
sees an empty list where tenant A's document sits.

Rate limiting needed no change — `rate_limit::check` is keyed on `tenant_id`, so dashboard traffic
lands in the tenant's existing bucket. That is correct: spend is per tenant, not per credential.

## Part 2 — the `web/` documents slice

Phase 2 predicted this would need no refactor of the plumbing. It didn't: `client.ts`, `parse.ts`,
`ApiResult`, the cookies and `hooks.server.ts` were untouched. That was the test of the layering.

### The upload flow

```
1. Client validates extension + size                            [advisory only]
2. POST /documents/upload-url {filename}   → same-origin, BFF
   bf_session rides along (httpOnly). The browser cannot read it.
3. +server.ts: token = locals.session.token                     [server memory only]
   → API, Authorization: Bearer sess_…                          [server → API only]
4. Browser PUTs the File to the presigned URL                   [browser → MinIO :9000]
   No Authorization header. No cookies. Signature in the query string.
5. MinIO → AMQP → worker. No callback (invariant 13).
6. invalidate('documents:list') → the load re-runs server-side.
```

**Why the browser holding a storage URL is not an invariant-20 leak** (invariant 24): it authorises
one object key, one method, for 15 minutes. A capability, not a credential.

**Why upload is JS-only, and it is architectural.** A multipart form action proxies bytes through
Node — precisely the deprecated `POST /documents`, one layer up. A presigned POST policy *is*
form-native and could even enforce `content-length-range`, but it lands the user on MinIO's XML with
no route home; `success_action_redirect` returns them but makes completion a client-controlled query
param — a forgeable callback, which invariant 13 exists to forbid. So: `<noscript>` on the card. The
read path keeps the no-JS guarantee.

**CSRF**, the app's first non-form-action surface: `SameSite=Lax` alone closes it — a cross-site
`fetch` POST does not carry the cookie. Note `csrf.checkOrigin` does **not** help here; it only
covers form-encodable content types and never sees a JSON POST. Explicit origin + content-type checks
are the belt to Lax's braces.

**One endpoint, not two.** A discriminated body (`{filename}` → mint, `{documentId}` → re-mint)
rather than mirroring the API 1:1: one guard, one origin check, one place to forget the session, and
an identical response shape so the client has one code path. It also keeps the UUID out of a route
param — it is validated in exactly one place before being interpolated into a path.

**The endpoint must not use `requireUser`.** That throws `redirect(303, '/login')`, and `fetch`
follows redirects — the caller would get the login page's HTML with a 200 and choke parsing JSON.
`error(401)` instead. Right for a page, wrong for a JSON endpoint.

### Polling

No completion callback exists (by design), so the only way to see `uploading → processing → ready` is
to ask again. `invalidate()` on a backoff, gated three ways:

1. **Only when a row is transient** (`uploading | processing`). All-`ready` tenants start **no timer
   at all**.
2. **Only while the tab is visible.** Better than a cutoff: an unwatched tab costs nothing.
3. **Backoff ×1.5, floor 3s, ceiling 15s**, reset on change. A small `.txt` reaches `ready` in single
   digit seconds (3s feels live, which is when someone is watching), but an abandoned `uploading` row
   sits ~20 min (presign TTL 15 + `UPLOAD_GRACE` 5) — at a flat 3s that is ~400 identical responses
   over an unpaginated table.

Honest cost, stated in the code: each tick is **two** API calls — `hooks.server.ts` runs
`GET /auth/me` before the load runs `GET /documents`. That is the number that would justify a
dedicated JSON endpoint later. Rejected for now because the BFF cannot make the API return less, so
the expensive hop is identical either way.

### Status copy

Six reachable statuses. `uploaded` is in the DB CHECK with no writer — no branch was built for it;
`toStatus` sends it to `unknown`.

| status | badge | detail |
| --- | --- | --- |
| `uploading` | Uploading + spinner | "Waiting for the file to arrive." |
| `processing` | Processing + spinner | "Reading and indexing this file…" |
| `ready` | Ready (green) | "Ready to answer questions." |
| `failed` | Failed | "…damaged, or something went wrong on our side. Try uploading it again." |
| `expired` | Upload expired (`outline`) | "The upload didn't finish in time." |
| `quarantined` | Too large | "…over the 25 MB limit…" |

**`failed` names both causes deliberately.** It conflates a broken file with a dead worker of ours,
and we cannot tell them apart without the `error` column — which invariant 16 forbids exposing, since
it holds parser stderr and `'processing lease expired; worker presumed dead'`. Blaming the user for
our outage is wrong; excusing a genuinely broken PDF is wrong. "Try uploading it again" is correct
under both, which is what makes the ambiguity survivable. `status.test.ts` asserts no copy contains
`worker`, `stderr`, `lease`, `parser`, `rls`…

`quarantined` is the one failure we can be specific about: the oversize check is its only writer
today. If a second ever appears, that copy becomes a lie — the test is the tripwire.

**`expired` is `outline`, not `destructive`.** Nothing broke; the user closed a tab. Red would lie.

**A load failure is not an empty table.** `+page.server.ts` returns `{documents: [], loadError: true}`
and the page renders an alert. Telling a tenant their library is empty because our API blinked would
invite them to re-upload everything — the direct analogue of invariant 21: *an API outage is not an
empty library.*

## Two bugs this phase found

### `refresh_session` cannot re-mint for a different file (invariant 25)

`upload/mod.rs` takes **no filename** — it re-signs the row's existing `object_key`, whose extension
was fixed at `create_session`. So the obvious UI (*"your upload expired, pick a file again"* →
`refreshUploadUrl`) would store markdown bytes at `original.pdf`; the sidecar dispatches on the
suffix and the row goes `failed`. **The user is told their good file is broken, and it is our fault.**

So the client re-mints in exactly one place: a mid-flight `403` where it still holds the same `File`,
so the extension provably cannot have changed. An `expired` row on a cold load mints a fresh row.
README's curl instructions carried the same trap and now carry the warning.

### `extension_of` is `Path::extension()`, and the naive mirror is wrong

Rust returns `None` for `.pdf` (leading dot ⇒ all stem) but `Some("pdf")` for `..pdf`.
`split('.').pop()` accepts `.pdf`, so the client says valid and the API 400s. The phase-2
`passwordByteLength` trap in a new costume.

**`key.rs` did not pin this** — its test stopped at `cv.pdf` / `NOTES.MD` / `a.b.txt` /
`resume.docx` / `noext`. A mirror of unpinned behaviour is not a mirror, so
`dotfiles_have_no_extension` was added to the Rust side first, then ported to TS.

## Verification

**Unit** — 89 web tests across 9 files; 41 Rust. The important ones:

- **`upload.test.ts`** — pins **invariant 20**: the cross-origin PUT carries no `authorization`, no
  cookie, `credentials: 'omit'`; the mint goes to our own origin, relatively. A leak there would
  still upload fine and nothing else would notice.
- **`schema.test.ts`** — both Rust extension tests, ported verbatim.
- **`status.test.ts`** — the invariant-16 leak sweep over every status's copy.
- **`format.test.ts`** — Postgres `timestamptz::text` is `2026-07-16 11:39:20.470205+00`: a space,
  and a 2-digit offset. `new Date()` rejects both **silently**, so the raw string reached the UI. Only
  a screenshot caught it.
- **`poll.test.ts`** — floor, ceiling, reset-on-change.

**End to end**, driven in a real browser against the live stack — 15/15:

| Check | Result |
| --- | --- |
| logged-out `/documents` → `/login?redirectTo=%2Fdocuments` | ✅ |
| `document.cookie` on the dashboard | ✅ empty |
| `.docx` and `.pdf` (dotfile) rejected with **zero** network calls | ✅ |
| upload a `.txt` → **Ready** with no manual reload | ✅ |
| the PUT went to `:9000` directly | ✅ |
| **the PUT carried no `authorization` and no `cookie`** | ✅ ← invariant 20, observed |
| no `sess_` in any request to storage | ✅ |
| polling stops when nothing is transient | ✅ 0 polls in 9s |
| tenant B sees none of tenant A's documents | ✅ |

## Not in this phase

- **Delete.** Spans Postgres + Qdrant + MinIO and needs a partial-failure ordering decision. A design
  problem, not a UI task. The dashboard now makes the gap *visible*, which raises its priority.
- **Failure classification.** The fix for `failed`'s ambiguity is a reason code written by the worker,
  not the `error` column exposed raw. That is a worker + API + invariant change.
- **Pagination.** `GET /documents` is unbounded and the dashboard polls it. The 15s ceiling is a
  mitigation, not a fix.
- **Upload progress.** `fetch` emits no progress events; real progress needs `XMLHttpRequest`. An
  indeterminate spinner instead.
- **Key management UI.** `/auth/keys` list/mint/revoke — fully unblocked, needs no plumbing changes.
  The natural next slice.
  **Shipped in phase 4** — see [`phase-4-keys.md`](./phase-4-keys.md). "Needs no plumbing changes" held
  for `web/`, but the API did need fixing first: `allowed_origins` had no validation, so a mint form
  would have produced keys that 403 forever.
- **The "Queued" refinement.** After the PUT resolves the row is still `uploading`, so the badge is
  briefly a lie. A client-held `Set` of ids could render those as "Queued" — but it is the only client
  state that would override a server status, and that kind of thing rots.
