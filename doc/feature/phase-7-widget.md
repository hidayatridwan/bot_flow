# Feature: Serving the widget, and making it tell the truth (phase 7)

> Status: **planned, decisions locked.** Audited against `widget/widget.js` (all 173 lines of it),
> `widget/demo.html`, `web/src/lib/features/keys/embed.ts` and the API's router before writing. All
> four decisions (D1–D4) are settled below; the reasoning is retained as the spec the implementation is
> held to, not as an open question. The headline of the audit is in *Does this break existing
> integrations?* — the short answer is **no, and it cannot**, for a reason more specific than "it is
> additive". The real risk this phase carries is not breakage; it is doing the work and delivering
> nothing — which is why D1, the cache header, *is* the feature rather than a detail of it.

## Context — why

### The funnel ends at a placeholder

`embed.ts` builds the snippet the `/keys` page hands a tenant. Its first line is, literally:

```
<script src="/path/to/widget.js"></script>
```

Every phase so far has been building the staircase to that line. Register, upload, wait for `Ready`,
mint a `pk_`, get the origins canonicalised, confirm the answer in the playground — and then go-live
stops at a placeholder the tenant cannot resolve without finding `widget.js` in a repo they do not
have, hosting it somewhere, and editing the path by hand.

### Self-hosting means no fix ever reaches anyone — and `demo.html` already says so

`demo.html` carries this comment above its script tag:

> *"Bump the version query whenever the widget changes: browsers cache an unversioned script
> aggressively, so visitors keep running a stale copy long after a fix ships."*

That is the disease, diagnosed in our own repo, and `?v=2` is the symptom. It is a workaround a
*tenant* would have to perform, on their own page, for a fix *we* shipped — which means in practice it
never happens. A tenant onboarded today takes a permanent snapshot of `widget.js`. Every bug in it is
theirs forever, and no fix we write can reach them.

**This is the only item on the roadmap with a deadline.** Document delete, the `/dashboard` stub, the
isolation tests — all cost the same whenever they are done. This one gets strictly more expensive with
every tenant who pastes the snippet, and it is free right now.

### The playground promises what the widget does not deliver

Invariant 24 justifies the playground's JavaScript spend on exactly one claim: *"this is what your end
users see."* It renders citations. `widget.js:147` says, in as many words:

> *"The server still emits a `sources` event — we ignore it rather than render it."*

So the claim is false on precisely the surface phase 5 added. Invariant 5 forbids the model from
writing citation markers into its prose **because** citations come back as structured data — the system
has been paying that cost since day one and, in the one place a real customer sees, collecting none of
the benefit.

## Does this break existing integrations?

**No, and the reason is stronger than "the change is additive".**

**The snippet has never produced a working integration.** `/path/to/widget.js` is a placeholder, not a
URL. Nobody has ever pasted it verbatim and had a widget appear. So every integration that exists in
the world today is self-hosted at a path the tenant chose themselves — and this phase does not touch
their file, their path, or their page. Their copy keeps loading and keeps working.

The API contract is likewise untouched: `POST /ask/stream`, `Authorization: Bearer pk_`, and the SSE
frame shape all stay exactly as they are. An old self-hosted copy talks to the same endpoint tomorrow
as today. Phase 6 already proved that `pk_` on `/ask/stream` is unmoved, in the live containment
matrix.

Three things worth stating because they *look* like breakage and are not:

| Looks like a risk | Actually |
| --- | --- |
| CORS on the `<script>` tag | `<script src>` is not subject to CORS. No `crossorigin` attribute, no preflight, nothing for `allow_origin(Any)` to be involved in. The widget's *fetch* to `/ask/stream` is the CORS surface, and it is unchanged |
| A new unauthenticated route | `GET /widget.js` must be public — it is the file that has not authenticated yet. It joins `/health` as the second unauthenticated route. It makes no gateway call, so it is not spend, and there is no `tenant_id` to key `rate_limit::check` on. It is a bandwidth surface, no more, and that should be said plainly rather than papered over |
| Old copies not getting citations | Not breakage — *non-repair*. They render answers today and will render answers after. They simply stay as they are, which is the entire problem this phase exists to end for everyone who comes next |

**The actual risk is non-delivery, and there are two ways to spend this whole phase and change
nothing.** Both are cache decisions, which is why D1 is the most important decision here and not a
detail:

1. **A long `Cache-Control`.** Serving the file but letting browsers hold it for a year rebuilds
   self-hosting inside our own API. `demo.html`'s comment would still be true — we would just own the
   stale copy instead of the tenant.
2. **A versioned URL** (`/widget/v1.js`). The CDN-standard pattern, and exactly wrong here: it makes a
   fix require a snippet edit *on the tenant's page*, which is the thing we are trying to abolish. It
   is `?v=2` with extra steps.

The cache header is not a detail of this feature. **It is the feature.**

## The thing the audit turned up that I did not expect

**Serving the widget changes what `widget.js` *is*.**

Today it is a file we publish for people to copy. Its bugs are, in a real sense, theirs — they took a
snapshot. After this phase it is **our deployed code, shipped by us, at our cadence, into strangers'
browsers**. Every bug in it becomes ours the moment we serve it.

So the audit re-read all 173 lines with that in mind, and found three things that are tolerable in a
sample and not tolerable in a product we ship:

**1. The empty-answer bug is still live in the widget.** This is the one the user hit in Bahasa
Indonesia. Phase 5 fixed the root cause (`MAX_TOKENS`) for everyone — but the *client-side* guard went
into `ask.ts` only. `widget.js` counts nothing: `_onEvent` appends `token` data to `answer` and sets
`bot.textContent`. Zero token frames means an empty grey bubble, forever, with no error and nothing in
any log. The playground now says *"the bot found relevant passages but didn't produce an answer"*; the
widget says nothing at all. We fixed the demo and left the product.

**2. The widget never handles `done`.** `_onEvent` switches on `token`, `conversation` and `error` —
there is no `done` case. It works by accident: the loop ends when the reader does, because the
connection closes. But it means the widget cannot distinguish a completed answer from a socket that
died mid-sentence, so it has no `CUT_SHORT` equivalent and never can. A truncated answer and a finished
one look identical to a visitor.

**3. `Error ${res.status}` is what a stranger sees.** On a 403 — invariant 15's *"403s forever with
nothing in any log to say why"* — a tenant's customer reads `Error 403`. Not an invariant-16 violation
(a status code is not internal detail), but it is useless to the person reading it and it is the exact
failure the playground admits it cannot reproduce.

None of this is an argument against serving the widget. It is an argument that the phase which serves
it is the phase that owns it, and should not take ownership of known bugs.

## Decisions taken

All four are settled — this is the Plan of Record, not a proposal. The reasoning is kept because it is
what the audit and the implementation must hold the code against, not because the choice is still open.

**D1 — cache header: stable URL (`/widget.js`) + `Cache-Control: no-cache` + a strong `ETag`.** This is
the whole feature; the other three are its passengers.

`no-cache` does not mean "do not cache" — it means "revalidate before use", which is exactly right. A
browser keeps its copy and asks; unchanged, it gets a ~200-byte `304` and reuses it. A fix is live for
every visitor of every tenant the moment the API restarts, with no snippet edit anywhere.

The ETag is free: `include_str!` fixes the content at compile time, so the hash can be computed once at
startup and compared per request. It must be a **strong** ETag (no `W/` prefix) — the bytes are
identical, not merely equivalent, and a strong tag is what lets a future CDN serve `304`s itself.

The trade, accepted knowingly: one conditional GET per visitor page load. Cheap per request, real in
aggregate. If it ever bites, the answer is a CDN in front — a deployment change, not a redesign,
precisely *because* the URL is stable. `max-age=300` was the considered alternative (five minutes of
staleness for zero revalidation traffic) and was **rejected**: the entire reason this phase exists is
that a stale widget cannot be fixed remotely, and any staleness window reintroduces exactly that, in
miniature. `no-cache` is the only header that makes "a fix reaches every visitor immediately" literally
true.

**D2 — render citations, by porting `chat/sources.ts`.** One widget change, one deploy, and it ends
invariant 24's contradiction (the playground claims *"this is what your end users see"* while the
widget drops the `sources` event). Porting rather than reinventing is load-bearing: `sources.ts` is the
tested surface, and its one rule the widget must not break is that a citation's number is `index` from
the field, **never** the array position (invariant 5). A hand-rolled `#{i+1}` would pass every casual
test — the API sends `1..n` in order today — and silently violate the invariant the moment it does not.

**D3 — fix the empty-answer bug and handle `done`.** Serving the widget is taking ownership of it (see
the section above); shipping it with known bugs means shipping *our* bugs, deployed, into strangers'
browsers on day one. Two concrete changes:

- **Count token frames; report zero-with-a-completed-stream.** Mirror `ask.ts`: if `done` arrives and
  no `token` ever did, the bubble must say something rather than sit empty. The message is the widget's
  own copy — it has no `error-map.ts` — but the *condition* is identical, and counting frames (not
  inspecting the accumulated string) is the same discipline for the same reason: a whitespace-only
  answer is still the model speaking.
- **Add a `done` case to `_onEvent`.** Today the loop ends only when the reader closes, so the widget
  cannot tell a finished answer from a dropped socket. An explicit `done` makes completion observable,
  which is what a "the answer was cut short" state would later hang off — the widget's `CUT_SHORT`
  equivalent is out of scope here, but it is *impossible* without this, so this is the enabling half.

**D4 — `include_str!`; keep the two SSE parsers separate.** The unification is tempting and rejected on
purpose. `sse.ts` is the contract's *only tested* parser and `widget.js`'s `_parseEvent` is a second,
untested one — CLAUDE.md flags exactly this — so building the widget from `web/`'s modules would delete
the duplication. It would also make the **Rust build depend on a `bun` build**, in a repo whose first
architectural rule is *"two projects, one repo, and the root is the Rust one."* That is a large,
permanent change to how everything builds, traded for one deduplicated function.

`include_str!` keeps the widget a zero-build-step artifact and the API self-contained: no `ServeDir`,
no filesystem path, no traversal surface, the binary carrying its own asset. The duplication stays, and
is recorded in *Known debt* rather than solved by accident. **Consequence the implementation must
respect:** D2 and D3 change `widget.js`'s `_parseEvent`/`_onEvent` and citation rendering, so `sse.ts`
and `sources.ts` are now definitions the widget is *ported from* and must not silently diverge from —
the duplication is accepted, drift is not.

## Design sketch

- `GET /widget.js` — public, `include_str!("../../../widget/widget.js")`, content type
  `application/javascript; charset=utf-8`, `X-Content-Type-Options: nosniff`, plus D1's caching.
  Sits beside `/health` as the router's other unauthenticated route.
- `embed.ts` — `<script src="{apiBase}/widget.js">`. The snippet becomes copy-pasteable for the first
  time, which is the entire point, and `embedSnippet` already refuses to carry an `sk_` (keep that).
- `widget.js` — render `sources` (port `chat/sources.ts`'s rules: `index` from the field, **never** the
  array position, invariant 5); handle `done`; count token frames and say something when zero.
- `demo.html` — drop `?v=2` and point at the served URL. Its cache-busting comment should not be
  deleted quietly: it is the reason this phase exists, and it wants rewriting into a note about what
  replaced it.

## Verification plan

**Live, and the point is not that the file downloads:**

| | |
| --- | --- |
| `GET /widget.js`, no auth | 200, `application/javascript` |
| A second GET with `If-None-Match` | **304** — the ETag works, so the cache story is real |
| Change the file, rebuild, GET again | **200 with new bytes** ← the whole feature. If this is a 304, the phase delivered nothing |
| `demo.html` at `localhost:5500`, snippet from `/keys` verbatim | widget appears, answers, cites |
| The same page, `pk_` origin removed via PATCH | 403 — and now check what the *visitor* reads |
| A tenant's existing self-hosted copy | still works, untouched — the non-breakage claim, actually run |

**Browser:** citations render with filenames and their own `index`; a zero-token answer says something
rather than showing an empty bubble; a mid-stream kill is distinguishable from a finished answer.

**Regression:** `/ask/stream` with a `pk_` is unchanged — phase 6's matrix rows re-run, because this
phase touches the one route every deployed widget flows through.

## Known debt & traps

| Don't | Do | Why |
| --- | --- | --- |
| Serve `widget.js` with a long `max-age` | `no-cache` + `ETag` (D1) | It rebuilds self-hosting inside the API. `demo.html`'s comment stays true, we just own the stale copy now |
| Version the URL (`/widget/v1.js`) | Keep `/widget.js` stable | A fix would need a snippet edit on the tenant's page — `?v=2` with extra steps, and the exact thing this phase abolishes |
| Reach for `ServeDir` | `include_str!` | The API would gain a filesystem dependency and a traversal surface to serve one known file that never changes at runtime |
| Renumber citations in the widget | `index` from the field | Invariant 5. The widget is a second renderer of the same contract, and it will drift from `sources.ts` unless it is ported rather than reinvented |
| Assume old self-hosted copies break | They cannot | The snippet was a placeholder — no integration ever came from it. Their file, their path, our unchanged API |

**Left standing, deliberately:**

- **Two SSE parsers, one tested.** D4 keeps `_parseEvent` untested and duplicated. Unchanged by this
  phase — but its *stakes* rise, because the untested one becomes code we deploy rather than code we
  publish.
- **`GET /widget.js` is unauthenticated and unmetered.** It cannot be tenant-keyed — there is no tenant
  yet. Bandwidth only, no gateway call, no spend.
- **The widget still cannot show an `allowed_origins` mismatch usefully.** It can say something better
  than `Error 403`, but the playground's admission stands: a green playground is not a green widget.
