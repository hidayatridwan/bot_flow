# Feature: Web auth — the SvelteKit BFF (phase 2)

> Status: **implemented.** Phase 1 (the REST API) is live — see
> [`phase-1-tenant-auth.md`](./phase-1-tenant-auth.md). This document covers the `web/` half: turning
> the auth mockup into a working Backend-for-Frontend. **Scope is the auth slice only** — register,
> login, logout, the session guard, and the one-time API-key reveal. Documents, key management and
> chat are later phases; the structure below is shaped so they slot in without a refactor.
>
> The design below was built as written. CLAUDE.md invariants 19–22 and its *Where things live*
> table were updated in the same change. 42 unit tests cover the validation mirror, the error map
> and the API client; the verification matrix at the bottom was walked end to end against a live
> API. Deviations from the plan: `formsnap` + `sveltekit-superforms` + `zod` came in via
> `shadcn-svelte add form` (the `form` component *does* exist — it is documented under
> [/docs/forms](https://www.shadcn-svelte.com/docs/forms), not in the components index), and
> `eslint.config.js` had to be repaired — it pointed at a `web/.gitignore` that has never existed,
> so eslint threw before linting anything.

## Context — why

Phase 1 shipped the auth **API**: `/auth/register`, `/auth/login`, `/auth/logout`, `/auth/me`,
`/auth/keys*`, backed by Argon2id passwords, SHA-256-hashed `sess_` tokens and the `SessionAuth`
extractor. **Nothing consumes it.**

`web/` is a pure mockup. Every route is a client-only `.svelte` file: there is no `hooks.server.ts`,
no `+page.server.ts`, no `.env`, and not one line of code anywhere that calls the API. The login and
signup forms are markup with no `method`, no `action` and — critically — **no `name=` attributes**, so
a real submit would post an empty `FormData`. The sidebar's "Log out" item has no handler, and the
user it shows is the hardcoded string `shadcn / m@example.com`.

**Intended outcome:** a tenant can register, log in, hold a session, see their real identity in the
dashboard, and log out. The API stays cookie-free; SvelteKit exchanges the login for an httpOnly
cookie **on its own origin** and forwards it as `Authorization: Bearer` server-side. The browser never
holds the token, so invariant 18's reasoning — `allow_origin(Any)` is safe *because* there is no
cookie/CSRF surface on the API — is untouched. **The BFF owns the cookie. The API never sees one.**

## Constraints

- **Every UI component comes from the [shadcn-svelte registry](https://www.shadcn-svelte.com/docs/components).**
  Composing installed primitives (`Card` + `Field` + `Input`) into a page is composition. Writing a new
  `.svelte` file under `lib/components/ui/` is not allowed — that directory is regenerable and must
  stay so.
- **Existing mockup copy stays.** The Terms-of-Service / Privacy-Policy line, the "Login with Apple" /
  "Login with Google" buttons, "Forgot your password?", the mock teams, nav items, breadcrumbs and the
  `/avatars/shadcn.jpg` avatar all remain — even where no backend supports them. They are decoration
  and they are what makes the product look finished. Do not delete a label because it is not wired up.
- **The Rust API does not change.** If the web app wants something the API does not offer, that is a
  new phase, not a patch.

## What exists today (grounding)

| Concern | Where |
| --- | --- |
| Login / signup markup — no `method`, no `action`, no `name=` | `web/src/lib/components/{login,signup}-form.svelte` |
| Centred card shell for the auth pages (brand: "Acme Inc.") | `web/src/routes/(auth)/+layout.svelte` |
| Sidebar shell; hardcoded breadcrumb | `web/src/routes/(authenticated)/+layout.svelte` |
| **All** mock user/team/nav data, in a module script; rendered with zero props | `web/src/lib/components/app-sidebar.svelte` |
| The dead "Log out" `DropdownMenu.Item`; hardcoded `CN` avatar fallback | `web/src/lib/components/nav-user.svelte` |
| SvelteKit config — **inline in the Vite plugin; there is no `svelte.config.js`** | `web/vite.config.ts` |
| Tailwind v4 entry (per `components.json`, *not* `app.css`) | `web/src/routes/layout.css` |
| `App.Locals` — still commented out | `web/src/app.d.ts` |

Installed shadcn components: `avatar breadcrumb button card collapsible dropdown-menu field input
label separator sheet sidebar skeleton tooltip`. Runes mode is forced on. Package manager is **bun**.
Adapter is **`adapter-node`**. `dependencies` is currently **empty** — everything is a devDependency.

### Facts the design is pinned to

Verified against the source. Getting any of these wrong produces a bug that is invisible until a real
user hits it.

| Fact | Detail |
| --- | --- |
| Register `201` | `{session_token, tenant_id, api_key, note}`. `api_key` is a **one-time** `sk_` reveal — only its hash is stored (invariant 14). If the BFF drops it, it is gone forever. |
| **The two 409s** | `"an account with this email already exists"` (`accounts.rs`) and `"tenant already exists"` (`handlers.rs`) share status **409**. The *message string is the only discriminator*, and they must land on different fields. |
| **Login `401`** | `"invalid email or password"` — **identical** for an unknown email and a wrong password, deliberately (invariant 18: the endpoint is not an existence oracle). |
| Password rule | `req.password.len() < 8` on a Rust `String` is **bytes**, not characters. `z.string().min(8)` counts UTF-16 code units. They diverge. |
| Slug rule | `^[a-z0-9][a-z0-9-]{0,62}$` — `common::key::is_valid_slug`, mirrored by a DB `CHECK`. |
| Email rule | `accounts::is_plausible_email`: needs `@`, a non-empty local part, a domain containing a `.` that neither starts nor ends with one, and no whitespace. `owner@localhost` is **rejected**. |
| Error body | `{"error": "..."}` JSON from `AppError` — **but** axum's own extractor rejections (415/400/422 on a malformed body) return **`text/plain`**. The client must not assume JSON. |
| Session | `sess_` + 64 hex. TTL from `SESSION_TTL_SECS`, default 30d. **No refresh endpoint** — expiry means re-login. |
| Rate limits | `429 "rate limit exceeded, slow down"`. Register is one **global** bucket; login is **per-email**. |
| Logout | **`204`, empty body.** The response parser must handle a bodyless success or logout breaks. |
| API bind | `BIND_ADDR`, default `0.0.0.0:3000`. There is no `PORT` var. |

## Architecture

**Layered at the boundary, feature-sliced above it.** Pure feature-slicing would duplicate the
HTTP/session plumbing in every slice; pure layering (`services/`, `stores/`, `models/`) makes "delete
the documents feature" a grep. So: the stable, generic machinery lives in layers
(`$lib/server/api`, `$lib/server/auth`), and the volatile, product-shaped code lives in slices
(`$lib/features/*`).

```
web/src/
├── app.d.ts                       # App.Locals typing
├── hooks.server.ts                # NEW — cookie → GET /auth/me → event.locals
│
├── lib/
│   ├── server/                    # SvelteKit HARD-FAILS any browser import of this tree.
│   │   ├── env.ts                 #   That build error IS the enforcement. Lean on it.
│   │   ├── api/
│   │   │   ├── client.ts          # fetch wrapper → ApiResult<T>. Imports no $env → unit-testable.
│   │   │   ├── parse.ts           # Response → data | ApiError   (JSON *or* text/plain)
│   │   │   ├── index.ts           # api(event, token) factory — the only place env is read
│   │   │   ├── auth.ts            # /auth/register|login|logout|me         ← THIS PHASE
│   │   │   ├── keys.ts            # /auth/keys*                            ← later
│   │   │   └── documents.ts       # /documents*, /ask                      ← later
│   │   └── auth/
│   │       ├── cookies.ts         # session cookie + the one-time api_key flash cookie
│   │       └── guard.ts           # requireUser(locals, url)
│   │
│   ├── features/auth/             # one slice; features/documents/ clones this shape
│   │   ├── components/            # login-form.svelte, signup-form.svelte (moved here)
│   │   ├── schema.ts              # zod schemas — the TS mirror of the Rust rules
│   │   ├── error-map.ts           # ApiError → which field gets the message
│   │   └── display.ts             # initialsFromEmail()
│   │
│   ├── components/
│   │   ├── ui/                    # shadcn ONLY. Never hand-edited. Regenerable.
│   │   └── layout/                # app-sidebar, nav-*, team-switcher (moved out of the root)
│   │
│   ├── types/api.ts               # ApiResult / ApiError — pure types, browser-safe
│   ├── types/auth.ts              # SessionUser — browser-safe, contains NO token
│   └── utils/redirect.ts          # safeRedirectTo() — open-redirect guard
│
└── routes/
    ├── (auth)/
    │   ├── +layout.server.ts      # NEW — bounce already-logged-in users to /dashboard
    │   ├── login/    +page.server.ts  +page.svelte
    │   └── signup/   +page.server.ts  +page.svelte
    └── (authenticated)/
        ├── +layout.server.ts      # NEW — guard; returns { user } and nothing else
        ├── dashboard/
        ├── onboarding/api-key/    # NEW — the one-time sk_ reveal
        └── logout/+page.server.ts # NEW — action-only route, no +page.svelte
```

**The `$lib/server` rule.** Anything that touches `API_BASE_URL`, a raw `sess_`/`sk_` token, or
`cookies` lives under `$lib/server/`. SvelteKit throws a build error if any of it becomes reachable
from a `.svelte` file — *that error is the enforcement mechanism*, and it is why the token lives there
rather than in a "conveniently shared" module. Everything else (`types/`, `schema.ts`, `error-map.ts`,
`display.ts`) is pure and safe to import from a component.

**`locals.session.token` is never returned from a `load`.** `session` (the credential) and `user` (the
identity) are separate fields on `Locals` precisely so that nesting the token inside `user` cannot put
it one careless `return { user }` away from the wire.

**How `documents` slots in later, with no refactor:** add `$lib/server/api/documents.ts` (reusing
`client.ts`, `parse.ts` and `ApiResult`), add `$lib/features/documents/`, add
`routes/(authenticated)/documents/` — already guarded by the existing layout. `hooks.server.ts`, the
cookies, the guard and the client are untouched. That is the test of the layering, and it passes.

## Design

### 1. The API client — a Result, not a throw

```ts
// $lib/types/api.ts — shared, pure
export type ApiErrorKind = 'client' | 'server' | 'transport' | 'malformed';
export interface ApiError { status: number; message: string; kind: ApiErrorKind; path: string }
export type ApiResult<T> =
  | { ok: true;  status: number; data: T }
  | { ok: false; error: ApiError };
```

```ts
// $lib/server/api/client.ts — takes fetch as an option and reads no env, so vitest can import it
export function createApiClient(opts: {
  baseUrl: string; fetch: typeof globalThis.fetch; token?: string; timeoutMs?: number;
}): ApiClient;
```

A form action then reads `if (!res.ok) …` with no `try`/`catch`, and TypeScript narrows `res.data` on
the happy path. Returning a Result rather than throwing is what keeps the actions flat.

`parse.ts` handles the body in this order, **and the order is the design**:

1. **`204` or zero-length → `{ ok: true, data: undefined }`.** This is what makes logout work.
2. `await res.text()` — read the body **as text, once, always**. Calling `res.json()` first would throw
   on an axum text/plain rejection and lose the body with it.
3. Branch on `content-type: application/json`. A non-JSON error body (the 415/422 extractor rejections)
   becomes `kind: 'malformed'` and is **never shown to the user** — it leaks axum internals
   ("Failed to deserialize the JSON body…"), which invariant 16 forbids in the other direction and
   which is simply useless to a human in this one.

Every call carries `AbortSignal.timeout(…)`. A hung Rust API must not hang SSR.

Env is read from **`$env/dynamic/private`** (not `static`: `adapter-node` builds one artifact and must
be re-pointable at a different API without a rebuild), in **`$lib/server/env.ts` only**, through
**functions, not top-level consts** — a top-level `required('API_BASE_URL')` evaluates during
`vite build` and breaks any CI that has no `.env`.

Wire DTOs stay `snake_case` (`tenant_name`, `session_token`) and are confined to
`$lib/server/api/auth.ts`. Everything above that speaks `camelCase` `SessionUser`. That boundary is why
a future rename in the Rust API touches exactly one file.

### 2. The session cookie

`bf_session` — `httpOnly` (the browser must never read the token: that is the entire point of the BFF),
`secure` outside dev, `path: '/'`, `maxAge` = `SESSION_TTL_SECS`.

`sameSite: 'lax'`, not `strict`: `strict` would break the `/login?redirectTo=…` flow from an emailed
link, and SvelteKit's `csrf.checkOrigin` (on by default) already rejects cross-origin form posts.

**The cookie is only a hint.** The `sessions` row in Postgres is the authority and `GET /auth/me` is the
check. A cookie that outlives its session costs exactly one 401.

`hooks.server.ts` reads the cookie, calls `GET /auth/me`, and populates `locals`:

- **`401` → delete the cookie.** The session is dead; stop re-asking on every request.
- **`5xx` / transport failure → leave `locals` null but *keep* the cookie.** A blip in the Rust API must
  not silently log every user out. Once it recovers, a reload just works. This distinction is the whole
  reason `ApiError.kind` exists.
- A visitor with no cookie costs **zero** API calls.
- **No cross-request caching of `/auth/me`.** It would make logout eventually-consistent. If it ever
  shows up in a profile, it goes behind one function (`resolveSession`) so the change stays local.

`handleFetch` is deliberately **not** used. Its job is rewriting URLs and forwarding credentials for
`event.fetch` — but a `handleFetch` that attaches `Bearer ${token}` would attach it to *every* outbound
fetch, third-party URLs included. Passing the token explicitly through `api(event, token)` is the safer
contract.

### 3. Forms — the official shadcn-svelte stack

Per <https://www.shadcn-svelte.com/docs/forms>, the recommended stack is **`form` + formsnap +
sveltekit-superforms + zod**, installed by `shadcn-svelte add form`. `form` is a registry component, so
this satisfies the shadcn-only rule; formsnap/superforms/zod are validation and binding libraries with
no DOM surface of their own.

Per route: `schema.ts` (zod) → `+page.server.ts` (`superValidate` + the `zod4` adapter, `fail(400, {
form })`) → `+page.svelte` (`superForm` with `zod4Client` validators) → the form component using
`Form.Field` / `Form.Control` / `Form.Label` / `Form.FieldErrors`.

**`Field.Group`, `Field.Separator` and `Field.Description` stay** as the layout shell, so every mockup
label survives verbatim: the OAuth buttons, "Or continue with", "Forgot your password?", "Don't have an
account?", and the Terms/Privacy line. Only the *bound inputs* become `Form.Field`. The page looks
identical; it just now submits.

The zod schema is **the TS mirror of the Rust rules**, and must be written as one:

```ts
// The Rust check is `password.len() < 8` on a String — that is BYTES.
// z.string().min(8) counts UTF-16 code units. For an 8-char string containing an emoji they disagree,
// and the user sees a client-side "valid" that the server then 422s.
password: z.string().refine((v) => new TextEncoder().encode(v).length >= 8, {
  message: 'Password must be at least 8 characters.',
}),

// Mirrors common::key::is_valid_slug and the tenants_id_slug DB CHECK.
slug: z.string().regex(/^[a-z0-9][a-z0-9-]{0,62}$/, { … }),
```

`confirmPassword` is a **client-only concept** — `RegisterRequest` has no such field. It is checked with
a `.refine()` on the object and then **dropped**. Serde would ignore it, but sending a secret twice for
no reason is gratuitous.

**Passwords are never echoed back.** `superValidate` puts the submitted data in the `fail()` payload,
which is serialised into the SSR HTML and visible in the network panel. Blank `form.data.password` (and
`confirmPassword`) before returning. Repopulating a password field is a security regression wearing the
costume of a UX nicety — and the browser's password manager refills it anyway.

### 4. Mapping API errors onto fields — three rules

`$lib/features/auth/error-map.ts` turns an `ApiError` into a `setError()` target.

1. **The two 409s must be told apart by message, not status.**
   `.includes('account with this email already exists')` → the **email** field;
   `.includes('tenant already exists')` → the **slug** field. Matching on the status alone would put the
   error under the wrong input. Use `.includes`, not `===`, so a future punctuation tweak in Rust
   degrades to a generic form-level message instead of throwing.
2. **`429` is form-level, never a field.** Register's bucket is *global* — it has nothing to do with
   anything the user typed, so attaching it to a field would be a lie.
3. **A login `401` is form-level, uniform, always. Never a field.** The API returns the identical message
   for an unknown email and a wrong password precisely so that it is not an existence oracle. Render it
   under **Email** and it reads as "this email is wrong"; render it under **Password** and it reads as
   "this email exists, but the password is wrong". **The UI would reconstruct the exact oracle the API
   deliberately destroyed.** This is the most important line in the file and it gets a comment saying so.

Everything else — `5xx`, `transport`, `malformed` — collapses to one generic form-level message. The raw
string is logged, never shown.

### 5. The one-time `sk_` reveal

The signup action sets the session cookie, sets a **5-minute httpOnly flash cookie** holding the key, and
redirects to `/onboarding/api-key`. That page's `load` **reads and deletes** the flash in the same
request; if it is empty (a refresh, or a direct visit) it redirects to `/dashboard`.

The page is composed from registry primitives only: `Card` + `Alert` (destructive, carrying the API's own
*"store the api_key now; it won't be shown again"*) + `InputGroup` (a readonly `Input` with a trailing
copy `Button`) + a `Checkbox` gating the continue button.

**Why a page and not a `Dialog` on the dashboard:** a modal is dismissible by `Esc` or an outside click,
and this key is **unrecoverable**. A stray keypress would destroy a value the user can never get back.

The key is **never in a URL** (it would land in browser history, in the `Referer` of every outbound link
on the page, and in every proxy access log), **never in `localStorage`** (any XSS exfiltrates it, and it
persists forever), and **never in a browser-readable cookie** (the flash is `httpOnly`; only the BFF's
`load` can read it). It appears in the SSR HTML of exactly one response, which is unavoidable — that is
what "show it to the user" means.

**Honest trade-off:** because the flash is deleted on read, **refreshing the page loses the key.** That is
correct. It mirrors the API's own semantics rather than papering over them, and `POST /auth/keys` already
exists as the recovery path: mint a new one. The `Alert` says exactly that.

### 6. Guards and redirects

`(authenticated)/+layout.server.ts` calls `requireUser(locals, url)`, which redirects to
`/login?redirectTo=…` and otherwise returns `{ user }` — **and nothing else**. No token.

`(auth)/+layout.server.ts` bounces an already-logged-in user to `/dashboard`.

`redirectTo` is consumed in the **login action**, from `event.url.searchParams`. A bare `method="POST"`
form (no `action` attribute) posts to `location.href`, query string included, so no hidden input is
needed — and a hidden input would only add another attacker-controllable field.

It goes through `safeRedirectTo()`, which rejects anything not starting with a single `/`. **This is an
open-redirect guard, and it is not optional** — `?redirectTo=https://evil.example` would otherwise turn
the login page into a phishing launcher.

The logout action clears the cookie **even when the API call fails**. Layout `load` functions do not run
before an action, so the `(authenticated)` guard does not protect it — which is fine: logout is
idempotent, and clearing a cookie is never harmful.

### 7. The sidebar

`(authenticated)/+layout.svelte` passes `data.user` into `AppSidebar`, which today takes **no props at
all** and sources its user from a module-level mock.

`NavUser` shows the real tenant name and email. **The avatar image and its `CN` fallback stay mocked** —
the API returns no display name and no avatar, and a broken image would look worse than a placeholder.
Teams, nav items and breadcrumbs stay mocked too.

The dead "Log out" item becomes a real `POST /logout`. **`DropdownMenu.Content` is portalled to
`<body>`**, so a `<form>` wrapping the *trigger* would not contain it. The form goes *inside* the content,
with the Item rendered through its `child` snippet as a submit button. (Fallback, if the portal fights it:
a root-level `<form id="logout-form">` plus `<button type="submit" form="logout-form">` — HTML form
association works across any DOM distance, portals included.)

### 8. shadcn components to add

```bash
cd web && bunx shadcn-svelte@latest add form alert input-group spinner checkbox
```

`form` also pulls `formsnap`, `sveltekit-superforms` and `zod`. `alert` is the form-level error banner and
the "copy this key now" warning; `input-group` is the readonly-input-plus-copy-button composition;
`spinner` is the pending state on the submit buttons (the v1 `Button` has no `loading` prop); `checkbox`
gates the onboarding continue button.

**Not now:** `dialog` / `alert-dialog` (superseded by the dedicated onboarding page); `table`, `badge`,
`empty` (those belong to the documents and key-management phases).

### 9. Env

New `web/.env.example` — the root `.gitignore` already whitelists it, and this closes part of the
`.env.example` debt CLAUDE.md flags:

```dotenv
API_BASE_URL=http://localhost:3000   # server-side only; the browser needs NO api config
SESSION_TTL_SECS=2592000             # must match the API's SESSION_TTL_SECS; sets the cookie max-age only
API_TIMEOUT_MS=10000                 # a stuck API must not hang SSR
# ORIGIN=https://app.example.com     # required by adapter-node in production
```

**Nothing goes in `PUBLIC_*`.** Every call to the Rust API is server-side; the browser needs no API
configuration at all. `SESSION_TTL_SECS` is duplicated across the API and the BFF — acceptable, because
the cookie is only a hint, but it is a drift risk and is commented as such in both files.

## Mockup breakages this phase must fix

Each of these is currently broken and bites the moment a real submit happens.

1. **No `name=` attributes on any input, in either form.** `await request.formData()` returns **empty**.
   The single biggest breakage.
2. **No `method="POST"` on either `<form>`.** It defaults to GET — which would put the password in the URL.
3. `login-form.svelte` uses `$props.id()`-generated ids while `signup-form.svelte` uses **static** ones
   (`id="name"`, `id="email"`) — a duplicate-id collision waiting to happen. `name` must be a stable
   hardcoded string; only `id`/`for` may be generated. Do not reuse the generated id as the `name`.
4. `signup-form.svelte` **nests `Field.Field` inside `Field.Field`** for the two-column password grid.
   `Field` carries `group/field` and `data-[invalid=true]` styling, so nesting makes the error styling and
   `Field.Error` placement ambiguous. The outer wrapper becomes a `Field.Group`. Still 100% registry
   components.
5. `confirm-password` has no counterpart in `RegisterRequest` — validate it, then drop it.
6. The Apple/Google buttons sit **inside** the `<form>` and are correctly `type="button"`. **Keep them that
   way.** Dropping the attribute would make them submit the login form.
7. `nav-user.svelte`'s "Log out" is dead, and `app-sidebar.svelte` is rendered with zero props.
8. `app.d.ts` has `App.Locals` commented out — nothing type-checks against `event.locals` today.

## Implementation order

1. `app.d.ts`, `$lib/types/{api,auth}.ts`, `$lib/server/env.ts`, `web/.env.example` + `web/.env`.
   Types and config first, so everything downstream type-checks.
2. `$lib/server/api/{client,parse,index,auth}.ts`, `$lib/server/auth/{cookies,guard}.ts`,
   `$lib/utils/redirect.ts`.
3. `hooks.server.ts` and both `+layout.server.ts` guards. **Verify here:** `/dashboard` bounces to
   `/login`. Nothing renders yet, but the skeleton is live.
4. `bunx shadcn-svelte@latest add form alert input-group spinner checkbox`.
5. `$lib/features/auth/{schema,error-map,display}.ts` + vitest + the three test files. **Green tests
   before any UI.**
6. Move the two form components into `$lib/features/auth/components/`; wire `method` / `name` /
   `superForm` / `Form.FieldErrors` / `Alert` / `Spinner`; fix the nested-`Field` quirk. Add the login and
   signup actions.
7. The `/onboarding/api-key` route.
8. Move `app-sidebar` + `nav-*` + `team-switcher` into `$lib/components/layout/`; thread `user` through;
   make Log out a real POST; add `/logout/+page.server.ts`.
9. Run the verification matrix below, then `bun run check && bun run lint && bun run test`.

## Verification

```bash
docker compose up -d && cargo run -p api                        # → :3000
cd web && cp .env.example .env && bun install && bun run dev    # → :5173
```

With devtools open on **Application → Cookies** and **Network**:

| Check | Expected |
| --- | --- |
| `/dashboard` while logged out | 303 → `/login` |
| Signup with password `short` | inline field error, and **no** network call (client-side zod) |
| Signup, valid | 303 → `/onboarding/api-key`; `bf_session` present, **HttpOnly ✓**, `Max-Age=2592000` |
| On the reveal page | `document.cookie` in the console shows **neither** cookie; the copy button works |
| Refresh the reveal page | 303 → `/dashboard`. The flash was consumed — **correct, not a bug** |
| Signup again, same email | 409 → error under **Email** |
| Signup, new email, taken slug | 409 → error under **Slug** ← *proves the two 409s are disambiguated* |
| Login, wrong password | form-level alert only — **assert no field-level error appears** ← the non-oracle check |
| Login ×60, fast | 429 → form-level alert |
| Log out | cookie gone → 303 `/login`; the back button re-redirects |
| Tamper `bf_session` to `sess_deadbeef` | `/auth/me` 401 → cookie deleted → `/login` |
| Kill the API, then load `/dashboard` | no 500; redirected to `/login`; **the cookie is still there** (a 5xx must not log you out) |
| Restart the API, reload | logged back in without re-entering credentials — proves the row above |

`bun run check` (svelte-check, strict) and `bun run lint` must be clean.

### Tests — `bun add -d vitest`, three files

Only the correctness contracts, all of them pure functions. No component or browser tests: the UI is
shadcn primitives we do not own, and the matrix above covers the wiring far more cheaply.

1. **`schema.test.ts`** — port the Rust unit tests *verbatim*. `accounts.rs` already has
   `email_validation_accepts_and_rejects` and `slugify_produces_valid_slugs` with exact expected values.
   A TS↔Rust drift shows the user a client-side "valid" that the server then 422s — invisible until a
   real user with an umlaut in their business name hits it. **The highest-value test in the app.**
2. **`error-map.test.ts`** — pin the two 409s to different fields, and pin a login 401 to a **form-level**
   error with no field keys. A regression here silently rebuilds the account-enumeration oracle.
3. **`client.test.ts`** — with a stubbed `fetch` (which is exactly why `createApiClient` takes `fetch` as
   an option and reads no env): a `204` → `ok` with no body; a `{"error":…}` 409 → `kind: 'client'`; a
   **`text/plain` 415** → `kind: 'malformed'` and **does not throw**; a rejecting fetch →
   `kind: 'transport'`.

## Invariants this feature touches

Per CLAUDE.md's *"a behaviour change starts here — edit the invariant, then write the code, in the same
commit"*, the implementation commit must also:

- **Extend invariant 18** with the concrete BFF contract: the session lives in an **httpOnly cookie on the
  web origin**, is forwarded to the API as `Authorization: Bearer`, and **never reaches the browser's
  JavaScript**. The API remains cookie-free, which is what keeps `allow_origin(Any)` sound.
- **Record the non-oracle rule at the UI layer.** Invariant 18 already says login failures are uniform;
  the BFF can undo that by rendering the uniform message under a specific field. A login 401 is
  form-level. Always.
- **Add `web/` to the *Where things live* table** — at minimum `hooks.server.ts` (the BFF hinge),
  `$lib/server/api/client.ts` (the shape every future feature depends on), `$lib/server/auth/cookies.ts`
  and `$lib/features/auth/error-map.ts`.

## Later phases (explicitly out of scope)

- **Key management** — `/auth/keys` list / mint / revoke. Needs `table`, `badge`, `alert-dialog`. The
  `keys.ts` API module and a `features/keys/` slice; no change to the plumbing.
- **Documents** — list, upload via `POST /documents/upload-url`, status. Needs `empty` for the
  zero-documents state.
  **Shipped in phase 3** — see [`phase-3-documents.md`](./phase-3-documents.md). The prediction above
  held exactly: `client.ts`, `parse.ts`, `ApiResult`, the cookies and `hooks.server.ts` were all
  untouched, and the slice was additive. Two things this doc did not foresee: the API had to change
  first (`/documents` would not accept a session, and the BFF has no key to offer instead), and the
  upload path is the app's **first route with no form action** — the bytes go browser → MinIO
  directly, so there is nothing to progressively enhance towards.
- **Chat** — the `/ask/stream` SSE surface.
- **Not backed by any API, and the mockup lies about them:** OAuth ("Login with Apple/Google"), password
  reset ("Forgot your password?"), team switching, and everything under the sidebar's nav. They stay as
  decoration; they are not commitments.
