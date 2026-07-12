# Feature: Tenant login / register (self-serve accounts)

> Status: **phase 1 (REST API) implemented.** The recommended options were taken: D1 = 1:1 owner
> account per tenant, D2 = DB-backed opaque `sess_` sessions, D3 = data routes stay on `AuthTenant`.
> Migrations `0009`/`0010`, `crates/api/src/accounts.rs`, and the `SessionAuth` extractor are live;
> CLAUDE.md invariants 17–18, README's *Self-serve accounts* section, and the Postman *Auth* folder
> were updated in the same change. The SvelteKit `web/` integration (phase 2, sketched below) is
> **not** done. The admin path (`POST /admin/tenants`) is retained as the operator escape hatch and
> now shares `provision_tenant` with register.

## Context — why

Today a tenant cannot sign up or log in. A tenant is **admin-provisioned**: an operator holding
`ADMIN_API_KEY` calls `POST /admin/tenants`, which inserts the `tenants` row and mints one `sk_`
secret key, shown once. Additional keys come from `POST /admin/tenants/{id}/keys`. There is no
concept of a *human account* anywhere — no email, no password, no session. The whole auth model is
stateless Bearer API keys (`crates/api/src/auth.rs`).

That is fine for us bootstrapping tenants by hand, but it blocks the product: a tenant can't
self-register, can't log into a dashboard, can't see or rotate their own keys, can't manage their
documents through a UI. The `web/` app already has empty `(admin)` and `(tenant)` route groups and a
dead "Log out" menu item waiting for exactly this.

**Intended outcome:** a tenant can register (creating their tenant + an owner account), log in, hold
a session, and self-serve the things that today require the admin key — starting with key management
and their document list. API keys (`sk_`/`pk_`) do **not** go away: they remain the credential the
tenant's *server* and *widget* use. The account/session is the credential a *human* uses in the
dashboard.

## What exists today (grounding)

| Concern | Where |
| --- | --- |
| `AuthTenant` / `AdminAuth` extractors, `hash_key`, `generate_key`, `require_secret` | `crates/api/src/auth.rs` |
| `create_tenant`, `mint_key` handlers (admin-gated) | `crates/api/src/handlers.rs` |
| Route table, permissive CORS (`allow_origin(Any)`) | `crates/api/src/main.rs` |
| `tenant_tx()`, admin vs `app_user` pools | `crates/api/src/db.rs` |
| `tenants`, `api_keys` schema (global, no RLS) | `crates/api/migrations/0001`, `0002`, `0006` |
| `AppError::client` vs `Internal` (500) | `crates/api/src/error.rs` |
| Env vars incl. `ADMIN_API_KEY`, `RATE_LIMIT_PER_MINUTE` | `crates/api/src/config.rs` |

Facts that shape the design:

- `tenants` and `api_keys` are **global** (not under RLS); they are queried on the plain `state.db`
  pool *before* any tenant context exists. New account/session tables will live in the same category.
- Raw keys are shown **exactly once** at mint and only their SHA-256 hash is stored (invariant 14).
  A logged-in tenant therefore **cannot** be handed their old `sk_` back — the dashboard has to reach
  tenant data some other way (see "Session as a principal" below).
- CORS is deliberately `allow_origin(Any)` and this is safe *because auth is Bearer-only with no
  cookies* — "no cookies ⇒ no CSRF surface" (CLAUDE.md security section). Any cookie we introduce
  must not break that reasoning. → We keep **cookies out of the API**; the SvelteKit BFF owns the
  cookie on its own origin (see web phase).

## Design — REST API (phase 1)

### New principal: the account + session

- **Account** = a human login: email + password, belonging to a tenant.
- **Session** = a bearer token proving a login, revocable and expiring, stored **hashed** exactly like
  `api_keys` ("hash, don't encrypt").

**MVP relationship:** one registration creates **one new tenant + one owner account** (1 account : 1
tenant). Multi-user-per-tenant and invite flows are a deliberate later extension — the schema leaves
room (`tenant_id` on `accounts` is not globally unique-forced beyond MVP convenience) but phase 1
ships single-owner.

### New tables (migrations `0009`, `0010`)

`accounts`:

| Column | Type | Notes |
| --- | --- | --- |
| `id` | uuid PK | |
| `tenant_id` | text NOT NULL FK → `tenants(id)` ON DELETE CASCADE | the tenant this account owns |
| `email` | citext (or text + `lower()` unique index) NOT NULL | **globally unique**, case-insensitive |
| `password_hash` | text NOT NULL | Argon2id PHC string — never logged |
| `created_at` | timestamptz NOT NULL DEFAULT now() | |

`sessions`:

| Column | Type | Notes |
| --- | --- | --- |
| `token_hash` | text PK | SHA-256 hex of the raw session token (mirrors `api_keys.key_hash`) |
| `account_id` | uuid NOT NULL FK → `accounts(id)` ON DELETE CASCADE | |
| `tenant_id` | text NOT NULL | denormalised so a session lookup yields tenant context in one query |
| `created_at` | timestamptz NOT NULL DEFAULT now() | |
| `expires_at` | timestamptz NOT NULL | checked on every resolve; expired ⇒ 401 |

Both tables are **global, no RLS** — same category as `tenants`/`api_keys`, queried on `state.db`
before tenant context exists. Grant CRUD to `app_user` in the migration (follow `0005_app_role.sql`).

### New dependencies / config

- **`argon2`** crate for password hashing (Argon2id, default params). Declare the version in
  `[workspace.dependencies]`, not in the member crate (workspace-dep trap). Used only by `crates/api`.
- **No new signing secret.** DB-backed opaque sessions need none — a point in their favour over JWT
  (see decision D2). Add `SESSION_TTL` (e.g. default 30 days) to `config.rs`.

### New routes

Public (no auth) — these are the front door, so they **must be rate limited** (see invariant note):

| Method | Path | Body | Returns |
| --- | --- | --- | --- |
| POST | `/auth/register` | `{ email, password, tenant_name, slug? }` | 201: `{ session_token, tenant_id, api_key }` — the `api_key` is the **one-time** `sk_` reveal |
| POST | `/auth/login` | `{ email, password }` | 200: `{ session_token, tenant_id }` (never re-reveals a key) |

Session-authenticated (new `SessionAuth` extractor):

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/auth/logout` | delete the current session row |
| GET | `/auth/me` | `{ account: { email }, tenant: { id, name } }` — used by the BFF to hydrate `locals` |
| GET | `/auth/keys` | list key **metadata** (`kind`, `label`, `created_at`, `allowed_origins`) — never raw |
| POST | `/auth/keys` | mint a key for this tenant (`{ kind, label, allowed_origins }`) → raw shown once |
| DELETE | `/auth/keys/{key_hash}` | revoke a key |

This key-management set is the bridge that makes register usable: registration hands you an owner
account + your first `sk_`; the dashboard then lets you mint a `pk_` for your widget and rotate keys —
things that today only the admin key can do.

### `SessionAuth` extractor (new, in `auth.rs`)

An axum `FromRequestParts<AppState>` alongside `AuthTenant`/`AdminAuth`:

1. Read `Authorization: Bearer <token>`; session tokens use a distinct prefix (e.g. **`sess_`**) via
   `generate_key`-style minting so they're never confused with `sk_`/`pk_`.
2. `hash_key(token)` → `SELECT account_id, tenant_id, expires_at FROM sessions WHERE token_hash=$1`.
3. Missing/expired/unknown → **401**, uniform message (no enumeration).
4. Yields `{ account_id, tenant_id }`. Because it produces a `tenant_id`, it can drive `tenant_tx()`
   exactly like `AuthTenant` does — so session-authed handlers get RLS for free.

### Reuse, not reinvention

- Register's tenant+key creation is the same logic as `create_tenant` (`handlers.rs:641`) and
  `mint_key` (`handlers.rs:691`). **Extract a shared helper** (e.g. `provision_tenant` /
  `mint_key_row`) so `/admin/tenants` and `/auth/register` don't drift. `mint_key`'s validation
  (kind check, `allowed_origins` binding) is reused verbatim by `POST /auth/keys`.
- Slug validation reuses `upload::key::is_valid_slug` (already called by `create_tenant`).
- All caller errors go through `AppError::client(...)`; bare `?` stays reserved for real 500s
  (`error.rs`). 4xx here: 409 email/slug taken, 422 for a parsed-but-invalid field, 401 bad creds.

## Invariants this feature introduces / touches

Per CLAUDE.md's rule *"a behaviour change starts here — edit the invariant, then write the code, in
the same commit,"* implementation must land these in CLAUDE.md alongside the code:

- **Passwords are Argon2id-hashed and never logged**, extending invariant 14's "hash, don't encrypt"
  from keys to passwords.
- **Session tokens are stored as SHA-256 hashes**, like API keys — a DB dump is not a session dump.
- **`/auth/register` and `/auth/login` are public and MUST be rate limited.** Register *creates
  tenants*; login is a password oracle. This is the same unmetered-spend exposure already flagged in
  "Known state & debt" for `/search` — a public endpoint that costs money/rows per call. Reuse
  `rate_limit::check`; register likely also wants stricter throttling / a captcha.
- **Login and account-lookup failures are uniform** — "invalid email or password", 401 regardless of
  whether the email exists. Mirrors invariant 8 (unknown vs foreign conversation both 404): never be
  an existence oracle.
- **Sessions/accounts are global tables queried on `state.db`, never under RLS** — they precede
  tenant context, same as `tenants`/`api_keys`.
- **Cookies stay out of the API.** Sessions are Bearer tokens at the API boundary; the CORS
  `allow_origin(Any)` invariant depends on there being no cookie/CSRF surface. The BFF owns the cookie.

## Open decisions (recommend first option)

- **D1 — account ↔ tenant cardinality.** *Recommend:* 1:1 owner account for MVP; multi-user later.
  Alt: model `accounts` as many-per-tenant from day one (more schema now, less churn later).
- **D2 — session mechanism.** *Recommend:* opaque **DB-backed** token (revocable, no signing secret,
  consistent with `api_keys`). Alt: stateless JWT (no DB read per request, but needs a signing secret
  and can't be revoked without a denylist — against the grain of this codebase).
- **D3 — does `SessionAuth` also authorise existing data routes** (`/documents`, `/search`, `/ask`)?
  *Recommend:* phase 1 keeps those on `AuthTenant` (keys); the dashboard reads documents via
  `/auth/*` + the tenant's own key. Extending `SessionAuth` onto data routes is a clean phase-3 step
  once the account model is proven.

## Web integration (phase 2)

**Designed in full in [`phase-2-web-auth.md`](./phase-2-web-auth.md).** Not implemented.

The shape, in one paragraph: the API stays cookie-free; SvelteKit is a **BFF** that turns a login into
an httpOnly cookie on its own origin and forwards it to the API as `Authorization: Bearer`, so the
browser never holds the token and the CORS posture of invariant 18 is untouched. `hooks.server.ts`
resolves the cookie through `GET /auth/me` into `event.locals`; `+layout.server.ts` guards the
authenticated route group; form actions drive `/auth/register|login|logout`.

Read the phase-2 doc before touching `web/` — it pins the details that are easy to get wrong: the two
**409s** that share a status and must land on different fields, the login **401** that must stay
form-level (rendering it under a field rebuilds the account-enumeration oracle this phase deliberately
destroyed), and the one-time `sk_` reveal, which is unrecoverable if the BFF drops it.

## Implementation order

1. Migrations `0009_accounts.sql`, `0010_sessions.sql` (+ `app_user` grants). Update CLAUDE.md
   invariants in the same commit.
2. `argon2` in `[workspace.dependencies]`; `SESSION_TTL` in `config.rs`.
3. `SessionAuth` extractor + session mint/hash helpers in `auth.rs`.
4. Extract `provision_tenant` / key-minting helper shared by admin + auth handlers.
5. `/auth/register`, `/auth/login`, `/auth/logout`, `/auth/me`, `/auth/keys*` handlers + routes,
   behind rate limiting.
6. Unit tests (below). Then phase 2 web integration.

## Verification

Unit tests (must pass with **no backing services**, per CLAUDE.md — pure functions only):

- Argon2 hash/verify round-trip; a wrong password fails.
- Session token minting has the `sess_` prefix and hashes deterministically.
- Register request validation: bad email → 422; bad slug → 400; duplicate handling shape.
- Login uniform-error: same 401 body for unknown email and wrong password.

End-to-end (needs Postgres, so `crates/api/tests/` — discuss before adding, per CLAUDE.md):

```bash
docker compose up -d && cargo run -p api
# register → expect 201 with session_token + one-time api_key
curl -sX POST localhost:3000/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"owner@acme.test","password":"correct horse battery staple","tenant_name":"Acme","slug":"acme"}'
# login → 200 session_token, NO api_key
curl -sX POST localhost:3000/auth/login \
  -H 'content-type: application/json' \
  -d '{"email":"owner@acme.test","password":"correct horse battery staple"}'
# me → tenant + account, using the session
curl -s localhost:3000/auth/me -H 'authorization: Bearer sess_...'
# wrong password → 401, identical body to unknown email (no enumeration)
# self-serve mint a pk_ for the widget
curl -sX POST localhost:3000/auth/keys -H 'authorization: Bearer sess_...' \
  -H 'content-type: application/json' -d '{"kind":"publishable","allowed_origins":["https://acme.example"]}'
```

Cross-tenant check worth asserting once the extractor exists: a session for tenant A must never read
tenant B's documents through any `/auth/*` route — the same RLS denial that protects `AuthTenant`.
