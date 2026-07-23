# Feature: a dashboard that only claims what exists (phase 18)

> Status: **built.** The mock nav is gone, `/dashboard` and `/` have real content, there is a root
> error page, and `mapKeyError` handles 429. Closes
> [production blocker 8](../production-readiness.md) — the last one on that list.

## Context — why

This was the only blocker that never lost data, hid a failure, or stopped a deploy. It is also the
one a customer meets first.

A tenant who signed up got: a sidebar offering **Billing** and **Team** pages that do not exist, a
tenant switcher listing *Acme Inc*, *Acme Corp.* and **Evil Corp.** on three fictional plans above
their own workspace name, a photograph of a stranger as their avatar, a breadcrumb reading *Build
Your Application / Data Fetching* on every page, and — as the landing spot after onboarding — a page
whose entire content was the text `dashboard tenant`.

None of that is a bug in the sense the rest of this repo uses the word. It is worse in a specific
way: **a tenant who clicks Billing and finds nothing learns that this UI does not mean what it
says**, and after that the parts that *are* true have to earn belief separately. The refusal path,
the citations, the isolation — all the things earlier phases went to real trouble to make honest —
get read through that.

## What replaced what

| Was | Now |
| --- | --- |
| Tenant switcher: *Acme Inc* / *Acme Corp.* / **Evil Corp.**, plans Enterprise/Startup/Free | The tenant's real name and slug |
| **Config** group: Design Engineering, Sales & Marketing, Travel — all `url: '#'` | Deleted, with its per-item dropdown and "More" row |
| User menu: **Upgrade to Pro**, **Account**, **Billing**, **Notifications**, all inert | One real entry: *Change password* |
| `/avatars/shadcn.jpg` as every tenant's avatar | Initials from the signed-in email |
| Breadcrumb: *Build Your Application / Data Fetching*, `href="##"` | Derived from the route |
| `/dashboard`: the text `dashboard tenant` | A readiness checklist |
| `/`: an unstyled `<h1>` and two bare links | A landing page |
| `(auth)` shell branded **"Acme Inc."** | BotFlow |
| No `+error.svelte` at any level | A styled 404/5xx page with a way back |

**The switcher deserves its own note, because "populate it with real data" was the obvious fix and
it is wrong.** `accounts` carries `unique index idx_accounts_tenant` (migration 0009): an account
belongs to exactly one tenant. A switcher can never have anything to switch to, so filling it in
would have produced a dropdown with one item and a chevron promising more. It became a static
identity block instead.

## The dashboard, and the temptation to invent numbers

The obvious replacement for a stub dashboard is a wall of charts. The risk in this codebase is
specific: **this system has no metric a tenant can see**, by design — invariant 30 keeps tenant
identity out of Prometheus entirely. So any "requests this week" tile would have had to be invented
or built from a store that does not exist.

It answers one question instead — *will my bot answer a question right now?* — because that is what
someone actually has on the day they sign up, and it cannot be answered without visiting three
pages. Three steps, each gating the next:

1. **Index a document.** Invariant 4: with nothing indexed the bot declines every question.
2. **Create a publishable key.** The widget cannot authenticate without a `pk_`.
3. **Ask it something.** Check the answers before embedding it anywhere.

Steps 2 and 3 render `blocked` rather than `pending` until a document is answerable, so the page can
say *do this one next* rather than being a list of links.

**Counting only `pk_` keys is load-bearing.** Every tenant gets an `sk_` at registration, so
counting all keys would mark step 2 done for everyone on day one, forever — and an `sk_` cannot
drive the widget. A test pins this specifically.

### Never state a total we do not have

`GET /documents` returns **no `total`**, deliberately: a count is the full table scan keyset
pagination exists to avoid (phase 15). So the dashboard counts over one page — the API's maximum,
200 — and when `next_cursor` is non-null it renders `200+` rather than `200`.

That matters more than it looks. A bare `200` for a tenant with 5,000 documents is a number that
quietly means "the first page", which is this system's characteristic failure mode in miniature: not
an error, just a plausible figure that is wrong. `formatCount` exists for that one distinction and
is tested for it.

## The one real inconsistency

`mapKeyError` had no 429 branch. Once phase 15 metered key minting, hitting the limit fell through
to *"Something went wrong. Please try again"* — advice that invites exactly the retry that keeps the
caller limited, and the one message guaranteed not to help. `RATE_LIMITED` already existed in three
sibling maps.

## Breadcrumbs: the prefix that is not a page

Deriving a breadcrumb from the URL is four lines, and the obvious version has a bug: it links every
prefix. `/settings/password` exists; **`/settings` does not**. Linking it puts a guaranteed 404
inside the one component whose entire job is orientation.

So a crumb is a link only if its path is in an explicit `LINKABLE` set, and everything else renders
as plain text. Hand-listing routes is the cost, and the failure direction is the safe one: a real
page shown as text rather than a dead link offered as navigation. Pinned by a test that fails if the
guard is removed.

## Verification

222 web tests (21 files), `svelte-check` clean. Three break-verifications, each watched failing:

| Break | What went red |
| --- | --- |
| Link every breadcrumb prefix | `does not link an intermediate segment that has no route` |
| Remove the 429 branch from `mapKeyError` | `names the rate limit instead of inviting a retry` |
| Mark the key step done regardless of key count | three readiness tests, including `a secret key alone does not satisfy the key step` |

And in a browser, against a live stack — a fresh tenant seeded to 2 ready / 1 processing / 1 failed:

```
/dashboard (no documents)      -> "Get your bot answering", all three steps, none done
/dashboard (2 ready, sk_ only) -> "2 ready to answer from", "Still indexing", "Needs attention",
                                  key step still PENDING   (an sk_ must not satisfy it)
/dashboard (after minting pk_) -> "Your bot is ready"
breadcrumbs                    -> Dashboard | Documents | API keys | Settings / Password
/no-such-page                  -> 404 with "Page not found" and a way back
/                              -> real landing copy, zero "Acme" references
grep the rendered shell        -> no Design Engineering, Evil Corp, Acme Inc, Upgrade to Pro,
                                  Billing, Notifications, Data Fetching, shadcn.jpg
                                  and no href="#" anywhere
```

## What is deliberately still open

- **No security response headers.** `hooks.server.ts` still returns `resolve(event)` untouched, so
  `/onboarding/api-key` — which renders a live `sk_` — is framable by any origin. Named in phase 17
  and still true; it is now the most worthwhile small thing left in `web/`.
- **The dashboard's `load` is not tested**, only its pure readiness logic. That includes the
  `kind === 'publishable'` filter, which decides whether a tenant is told their bot is live — a
  behaviour verified by hand in a browser rather than by CI. This is the concrete cost of the repo's
  "pure functions only" test line, and it is the first time that line has hidden something worth
  testing.
- **The landing page makes no claim about pricing, availability or customers**, because none would
  be true. It is a description, not marketing.
- **`keyHash` is still interpolated into a URL path unencoded**, and the session-expiry fetch
  surfaces still render a message rather than driving a re-login. Both were on the audit list; both
  are unrelated to what this blocker was about.
