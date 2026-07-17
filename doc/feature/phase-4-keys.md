# Feature: Key management + the widget embed (phase 4)

> Status: **implemented.** `normalize_origin` + `checked_origins` + `PATCH /auth/keys/{hash}` in the
> API; a `keys` slice and `/keys` route in `web/`. CLAUDE.md invariants 15/22 gained the origin rules
> and 26 is new; two *Traps* rows, *Where things live*, README and `widget/demo.html` were corrected
> in the same change. 114 web + 21 API unit tests; verified against a live stack by curl (the
> containment matrix) and in a real browser (18/18).

## Context — why

**The app made a promise it could not keep.** `/onboarding/api-key` told the tenant, in a destructive
alert: *"If you lose it, leave this page, or refresh, you will need to create a new key from your
dashboard."* Its own CTA then delivered them to `/dashboard` — whose complete contents were the words
`dashboard tenant`. There was no key UI anywhere. A tenant who refreshed that page had **permanently
lost their only credential**, while `POST /auth/keys` sat implemented and unreachable.

Invariant 22 asserts that endpoint *is* the recovery path. Until this phase it was a claim about an
endpoint no user could call.

**And no tenant could deploy the bot.** Going live needs a `pk_` whose `allowed_origins` holds their
site's exact origin, plus the embed snippet. Both were curl-only. A tenant could register, upload
documents, watch them reach `Ready` — and stop. The dashboard was a file uploader.

## The trap this phase existed to close

`allowed_origins` had **zero server-side validation**. The request body went straight into the INSERT.
So:

- `kind:"publishable"` + `allowed_origins: []` minted happily — and `auth.rs` can never match an empty
  list, so that key was **403 on every request, forever**. It was also the **default**
  (`#[serde(default)]`), i.e. what a mint form omitting the field would produce.
- Matching is **exact string equality**. `https://acme.com/`, `https://ACME.com` and
  `https://acme.com:443` all minted fine and never matched.

Both failures are silent, permanent, and invisible until a real visitor hits the widget and sees
`Error 403`. A dashboard mint form is the most likely place on earth to produce exactly them — so the
API was fixed first and the UI built on a sound one.

## Design

### `normalize_origin` sits next to the check it must agree with

`crates/api/src/auth.rs`, directly beside the `allowed_origins` comparison. That adjacency *is* the
design: a validator that lives in another file drifts from the matcher, and the drift is undetectable
because a wrong value looks exactly like a right one until traffic arrives.

A browser sends `scheme://host[:port]` — lowercased, no trailing slash, default port omitted. So:

- **canonicalise** what a human would plausibly type: `HTTPS://Acme.COM:443/` → `https://acme.com`
- **reject** what can never match: no scheme, a path/query/fragment, credentials, a non-http(s)
  scheme, and `null` — which is what every `file://` page and sandboxed iframe sends, so allow-listing
  it would admit all of them at once.

Unit-tested including **idempotency**, because a PATCH re-normalises what a mint already stored.

### Enforced in `insert_api_key`, so the two mint paths cannot diverge

`handlers::checked_origins` is called inside `insert_api_key` — already the shared choke point
(CLAUDE.md: *"Shared by the admin `mint_key` and the self-serve `/auth/keys` handler so the two mint
paths cannot drift"*). `provision_tenant` right below it set the precedent exactly: the pure validator
lives elsewhere, the enforcement lives in the shared helper. An admin cannot mint a dead key either.

A publishable key with an empty allow-list is a **422**, not a permissive key. Secret keys keep
accepting origins and ignoring them — they are never origin-checked, so requiring one there would
break the admin path for nothing.

### `PATCH /auth/keys/{key_hash}` — invariant 26

Adding `www.` must not mean minting a new key and re-editing every page's `<script>`. A `pk_` is
public and expected to be stolen, so rotating it to add a domain buys nothing — the allow-list *is*
the containment, and if editing it is expensive, tenants will ask for a wildcard, which deletes
invariant 15.

`SELECT kind` → validate against *that* kind → `UPDATE`. Two queries rather than one clever one, so
"not found" and "invalid origin" stay distinct answers instead of collapsing into an ambiguous
zero-row result. Only `allowed_origins` is mutable: `kind` would silently turn a published key secret
(or a secret key public) under an unchanged snippet. The `tenant_id` in the `WHERE` is the isolation
boundary — `api_keys` has no RLS — and a foreign hash **404s like an unknown one**.

### The web slice

`server/api/keys.ts` (wire), `features/keys/{schema,embed,error-map}.ts`, three components, and
`/keys`. No plumbing changed: `client.ts`, `parse.ts`, the cookies and `hooks.server.ts` were
untouched, for the third phase running.

**`schema.ts` is the TS mirror of `normalize_origin`**, ported test-for-test. The stakes are higher
than the usual mirror: drift here produces a key that mints and 403s forever.

**`embed.ts` throws on a non-`pk_`.** The snippet exists to be pasted into a public page; an `sk_`
there is invariant 15 inverted — the key that may do everything, printed where anyone can read it.
Refusing is the only safe behaviour, and it is the slice's most important test.

**The reveal is a form action, not a flash cookie.** The raw key rides back in the action result and
renders once; a reload loses it, which is invariant 22 holding rather than being worked around. The
flash-cookie dance in `onboarding/api-key` exists only because register *redirects* — a dashboard mint
does not, so it needs no cookie. For a `pk_` the reveal also renders the pre-filled snippet, because
mint time is the only moment a complete one can exist.

**`WIDGET_API_BASE_URL`** (defaulting to `API_BASE_URL`): the snippet needs the URL a *tenant's
visitors* reach, which may differ from the BFF's own (`http://api:3000`). This does not breach phase
2's *"nothing in `PUBLIC_*`"* rule — the browser still gets no API config for its own use; this is
display text the tenant copies elsewhere.

## Verification

**Unit**: 21 API tests (`normalize_origin` canonicalisation, rejection, the `null` case, idempotency);
114 web tests including the Rust port and `embed.test.ts`'s refusal of an `sk_`.

**The containment matrix, against a live stack** — the point is not that the page renders, it is that
the minted key *works*:

| | |
| --- | --- |
| `POST /auth/keys` publishable, no origins | **422** — "cannot answer from anywhere" |
| `POST /auth/keys` with `acme.com` | **422**, naming the offending origin |
| Minted with `HTTPS://Acme.COM:443/` | stored `https://acme.com` |
| `/ask/stream` + an allow-listed Origin | **200** ← the loop closes |
| `/ask/stream` + `https://evil.example` | **403** |
| `/ask/stream` + no Origin at all | **403** |
| PATCH to add a domain | the new origin **200**, the removed one **403**, **same key** |
| PATCH publishable → `[]` | **422** |
| Tenant B lists / PATCHes / revokes A's key | invisible; **404**, **404** — not 403, so no oracle |
| Revoke, then reuse | **401** |

**Browser, 18/18**: the reveal alert links to `/keys`; publishable-with-no-origins is refused before
any network call; the snippet is pre-filled and contains no `sk_`; the typed origin appears
canonicalised in the list; reloading loses the key; a secret key gets **no** snippet; revoke confirms
first.

## Not in this phase

- **Serving `widget.js`.** Tenants self-host a copy, so a widget fix can never reach them — the
  cache-busting comment in `demo.html` already worries about it. Serving it from the API
  (`include_str!` + one route — no `ServeDir`, no traversal surface) would fix that *and* make the
  snippet fully copy-pasteable rather than leaving `/path/to/widget.js` for the tenant to fill in.
  The most valuable remaining piece of this feature.
- **The `/dashboard` stub** — still the words `dashboard tenant`, still where every login lands.
- **Rate-limiting `/auth/keys`** — a session can mint unbounded keys. Session-gated, and it does not
  multiply LLM spend (`rate_limit` buckets on `tenant_id`), so it is an audit and revocation-surface
  problem rather than a spend one.
- **Widget citations** — the server emits `sources` and `widget.js` ignores it. The README claimed
  otherwise and has been corrected; making it true is a widget change, not a docs one.
- ~~**The chat playground**~~ — done in phase 5. `/ask` and `/ask/stream` now take `Actor` and gate
  nothing (invariant 27), and the spend question was answered by the limiter that already existed:
  `rate_limit::check` keys on `tenant_id`, not on the credential.
