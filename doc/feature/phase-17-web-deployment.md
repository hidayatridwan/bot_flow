# Feature: making `web/` deployable, and startable-or-not (phase 17)

> Status: **built.** A `web/Dockerfile`, a `start` script, an opt-in compose profile, and a startup
> config check that refuses to run a deployment that would 403 every write. Closes
> [production blocker 7](../production-readiness.md).

## Context — why

Blocker 7 was two unrelated problems filed together because both are invisible in development.

**There was no deployment path.** The root `Dockerfile` builds only the two Rust binaries — it never
mentions `web`, `node` or `bun`. `docker-compose.yml` had no `web` service. `package.json` had no
`start` script, so the `build/index.js` that `adapter-node` emits was an artifact nothing ever ran.
`bun run build` worked; nothing consumed the result.

**And behind TLS termination it would have refused every write.** That half is worse, because the app
would have looked deployed.

## The `ORIGIN` trap

`adapter-node` derives `url.origin` from the incoming connection. Behind a TLS-terminating proxy the
app sees plain HTTP, so `url.origin` is `http://app.example.com` while the browser sends
`Origin: https://app.example.com`.

That single mismatch fails three checks at once:

- SvelteKit's own `csrf.checkOrigin` — every form action
- the hand-rolled guard in `documents/upload-url/+server.ts`
- the hand-rolled guard in `playground/ask/+server.ts`

So **every form post, every upload and every playground question returns 403**, while `GET` pages
render perfectly. Login is a form post: nobody can even sign in. And nothing in any log names the
cause — the app is behaving exactly as designed, on a fact about the world that is wrong.

The fix has existed all along: the `ORIGIN` env var. It appeared in this repo exactly once —
**commented out**, in `web/.env.example`. `env.ts` never read it, never required it, never warned.
The one variable whose absence breaks production hardest was the one with no enforcement.

## Fail at boot, not at first request

`env.ts`'s getters are lazy for a good reason, stated in its own comment: a top-level
`required('API_BASE_URL')` is evaluated while Vite walks the module graph during `build`, so it
would fail any CI that has no `.env`.

The cost of that laziness was never paid back. A deployment missing `API_BASE_URL` would **start,
bind its port, pass a TCP healthcheck, and then 500 every page**. An orchestrator calls that
healthy and moves on.

`assertRuntimeEnv()` pays it back, called from `hooks.server.ts` at module load — which is server
start — guarded by `!building` so it still never runs during a build. It checks `API_BASE_URL`, the
numeric vars, and (outside dev) `ORIGIN` or the `PROTOCOL_HEADER`/`HOST_HEADER` pair.

Verified against the real build output, in an empty directory:

```
no API_BASE_URL, no ORIGIN   -> exit 1, "Missing required env var API_BASE_URL"
API_BASE_URL set, no ORIGIN  -> exit 1, "Missing ORIGIN. Behind a TLS-terminating proxy…"
SESSION_TTL_SECS=30d         -> exit 1, "must be a positive number of seconds, got \"30d\""
all set                      -> starts, GET /login -> 200
```

That last numeric case was a quieter bug of the same family: `Number('30d')` is `NaN`, which becomes
a cookie `maxAge` of `NaN`, which browsers drop — sessions would simply stop persisting, with
nothing anywhere saying why.

## The image

bun installs and builds; **node runs it**. bun owns `bun.lock` and is what CI uses, so the graph it
resolves is the one that was tested — and `adapter-node` targets Node specifically, so that is what
executes the output. Using bun for both would be tidier and would put production on a path the
adapter is not tested against.

The runtime stage copies **only `build/`**, which is worth stating because it looks like an
oversight. `adapter-node`'s output is self-contained — verified by running `node build/index.js` in
an empty directory with **no `node_modules` present at all**, which served `/login` and `/`
correctly. So the image carries no package manager, no lockfile and no dependency tree to audit,
and the "empty `dependencies`" concern the earlier audit raised turned out not to be a deployment
problem at all. It runs as the non-root `node` user, and its healthcheck uses Node's built-in
`fetch` against `/login` — a real `load`, so a healthy answer means the server is actually
rendering, not merely listening.

## Compose: an opt-in profile

`docker compose up -d` must keep starting **only** the backing services, because that is the
documented dev loop — the binaries and `bun run dev` run on the host for fast rebuilds. A plain
service would rebuild the app on every `up` and shadow `bun run dev` on another port.

So `web` sits behind `profiles: ["full"]`. It exists at all so that something routinely builds the
image: a Dockerfile nobody builds is one that has already rotted, which is precisely how this repo
went so long with no web deploy path.

Verified end to end — container on `:5173`, API on the host:

```
docker compose --profile full up -d --build web   -> healthy
GET  /  /login  /signup  /forgot-password         -> 200
POST /forgot-password (form, no JS)               -> 200, "Check your email"
   -> API -> Mailpit: 1 message to the registered address
   -> link points at APP_BASE_URL (the web app), not the API
docker compose config --services                  -> web absent without the profile
```

## Also fixed here, same area

- **`.dockerignore` was root-anchored.** `.env` matched only the repo-root file, so `web/.env` and
  `web/node_modules` were being copied into the Rust builder context on every image build. They
  never reached the final api/worker images (those copy only the binaries out of the builder) but
  they sat in the build cache. Now `**/.env*` with an exception for `.env.example`, plus `web/`.
- **CI never ran `bun run build`.** A build-breaking change shipped green — and the adapter lives in
  `vite.config.ts` (there is no `svelte.config.js`), which is exactly the kind of config nothing
  else exercises.
- **A root `.env.example`**, which `.gitignore` has expected since the first commit and which never
  existed. Generated from `config.rs` rather than from memory; secrets blank, non-secret values
  lined up with `docker-compose.yml`.

## The bug this phase found in phase 15's work

`ASK_TIMEOUT_MS` defaulted to **120s** while the API's `STREAM_DEADLINE` is **300s**. The BFF's
ceiling fired first — and the two are not equivalent. The API's is deliberately graceful: it ends
the stream with a normal `done` and *persists what arrived*, so the user keeps the prose they
watched appear. The BFF's just aborts the fetch, losing the answer **and** the recorded turn.

So phase 15's careful design was being defeated one layer up, by a default that predated it. Now
330s, with the ordering stated in both `env.ts` and `.env.example`.

Its doc comment had also rotted into three false claims — that the API had no timeouts (invariant 28
gave every gateway call one), that `max_tokens` was 512 (it is 4096), and that this was the only
bound in the chain. All three were true when written.

## What is deliberately still open

- **No security response headers.** `hooks.server.ts` still returns `resolve(event)` untouched: no
  `X-Frame-Options`, CSP, HSTS or `Referrer-Policy`. The `/onboarding/api-key` page renders a live
  `sk_` and is framable by any origin. That is blocker 8's territory and a small change; it is named
  here because *this* is the phase that made the app deployable, and shipping it framable is a
  choice rather than an oversight.
- **The `ASK_TIMEOUT_MS` > `STREAM_DEADLINE` ordering has no enforcement.** Two codebases, no
  compiler between them — the same shape of risk as `SESSION_TTL_SECS`.
- **The image is not published anywhere**, and there is no CI job that builds it. CI builds the app,
  not the container.
- **No non-root filesystem hardening** beyond `USER node` — no read-only root, no dropped
  capabilities.
