# Feature: getting back in (phase 16)

> Status: **built.** Password reset by email, password change while logged in, and a Mailpit sink so
> a live reset link can never leave a dev box. Closes
> [production blocker 6](../production-readiness.md).

## Context — why

`grep -ri "password_reset\|reset_token\|smtp\|mailer" crates/` used to return nothing. Not a missing
screen — no endpoint, no token, no email transport, no UI. The capability was absent at every layer.

The consequence was not "users are inconvenienced". It was that **every lockout became a manual
database edit by an operator who could not verify the requester owned the address**, because signup
never proved the address was real. A support burden and an account-takeover vector arriving
together. It also compounded invariant 22: the one-time `sk_` is genuinely unrecoverable, so a
locked-out tenant lost their login *and* their ability to mint a key.

That is why this was the blocker for self-serve rather than a gap.

## The rule everything here is shaped by

**A reset link is the most dangerous credential in the system.** Redeeming one *takes* an account
rather than merely opening it, and unlike every other credential we mint, it travels through a
channel nobody controls after handoff — an inbox, forwardable, archived, sometimes shared.

So the design is the conservative reading at every branch:

| Decision | Why the obvious alternative is worse |
| --- | --- |
| Stored SHA-256, never raw | A dump of `password_reset_tokens` would otherwise be a set of live account-takeover links (invariants 14, 17) |
| Single use, enforced by one atomic `UPDATE … WHERE used_at IS NULL … RETURNING` | A read-then-write races, and the loser is a second password change nobody asked for |
| One hour | Long enough for a slow relay and a phone read twenty minutes later; short enough that an archived email is not a permanent key |
| Redeeming revokes **every** session | The person resetting may be recovering *from* a compromise. A reset that leaves the attacker's session alive is theatre |
| Redeeming burns the account's **other** links | Ask three times, use one, and the two older emails stay live for the rest of the hour otherwise |
| No session issued on success | Redeeming proves control of an inbox, not knowledge of a password. Issuing one lets a leaked link become a live login without the password ever being typed |
| `rst_` prefix, never `sess_` | `Actor` dispatches on `sess_`. A reset token wearing it would be resolved against `sessions` — offering a password-changing credential to the path that grants access |

## The non-oracle, and its second half

`/auth/password/forgot` answers **`202` for everything**: registered, unregistered, malformed,
empty. This is invariant 18's rule arriving at a third public endpoint — `/auth/login` refuses to
reveal which emails exist, and a `404` here would hand that back on a new URL.

It goes further than the status code in two ways worth stating.

**It does not validate the address shape.** A `422` for a malformed address is a free "this shape is
accepted" signal for a well-formed one, and the two paths then differ in more than their body.
Garbage simply matches no row.

**And the rule extends to timing.** If a known address cost an SMTP conversation while an unknown
one returned immediately, the response *time* would be the oracle the status code is not. Delivery
is therefore spawned and never awaited.

Be precise about what that buys, because "constant time" would be an overclaim. It removes the SMTP
round trip — the only difference large enough to read over a network — but a known address still
costs one extra `INSERT`. Measured over eight interleaved pairs after warm-up:

```
known:   2.3 – 3.3 ms
unknown: 1.5 – 2.5 ms      (ranges overlap)
```

Sub-millisecond, and swamped by jitter on any real link. It is a difference, not an oracle. Closing
it entirely would mean inserting a throwaway row for addresses that do not exist — trading a
lab-measurable signal for a table anyone can grow at will.

## Change-password, and the 403

`POST /auth/password` requires the current password *even though the caller already holds a valid
session*. A session is a bearer token; one that has been stolen must not be enough to take the
account. This is the check that keeps a stolen `sess_` a temporary problem.

**The refusal is a 403, not a 401**, and that is not a stylistic choice. `hooks.server.ts` clears
the session cookie on a 401 (invariant 21) — so a 401 here would sign the user out for mistyping
their own password, on the one page where they are proving they are still themselves.

It also revokes every *other* session while keeping the current one — the opposite of the reset
path, deliberately. The user is here, authenticated; logging them out of the tab they are using
would be punishing the person doing the right thing.

## Mailpit, and why development gets a sink rather than a relay

`docker-compose.yml` gains a sixth service. The reasoning is one line: **a reset link is a live
credential, so a dev box must not be able to deliver one to a real inbox.** Mailpit accepts
everything, delivers nothing, and shows it at <http://localhost:8025>.

`SMTP_URL`, `MAIL_FROM` and `APP_BASE_URL` are **required at boot** — the process exits naming the
missing variable. That is deliberately unlike `METRICS_TOKEN`'s "absent config, absent surface": a
missing metrics token costs you a dashboard, while a missing mailer costs a locked-out user their
account *silently*, because `forgot` answers `202` either way. A silent feature is acceptable for
observability and not for account recovery.

`APP_BASE_URL` points at the **web app**, not the API. A user opens the link in a browser; the API
is not something an end user visits. Getting this wrong produces a link that 404s at exactly the
moment someone is already locked out.

## Verification

Six integration tests, each **watched failing** against a deliberate break before being trusted:

| Break | What went red |
| --- | --- |
| Remove the `DELETE FROM sessions` on reset | `a_reset_revokes_every_existing_session` |
| Drop `used_at IS NULL` from the redemption guard | `a_reset_token_cannot_be_replayed`, `redeeming_one_link_burns_the_others` |
| Make `forgot` 404 an unknown address | `forgot_password_is_not_an_existence_oracle` |

Plus the whole flow by hand, against real Mailpit and a real browser — no JavaScript:

```
POST /forgot-password (form)      -> 200, "Check your email"  (and one message in Mailpit)
open the emailed link             -> 200, "Choose a new password", token in a hidden field
POST /reset-password (form)       -> 303 -> /login?reset=1
GET  /login?reset=1               -> "Your password has been changed."
GET  /login                       -> banner absent
login with the new password       -> 200
login with the old password       -> 401
```

And the boot guard proved itself unprompted: the first run failed with
`Error: MAIL_FROM is not set` because `dotenvy` will not parse an unquoted value containing `<`.
That is the designed behaviour catching a real misconfiguration on its first outing.

## What is deliberately still open

- **No email verification.** An address is still unproven at signup — `accounts.rs` has said so
  since phase 2 — which means recovery is only as reliable as the address someone typed. Reset and
  verification want the same transport, so this is now much cheaper than it was; it is scope, not
  difficulty.
- **No durable outbox.** The send is a spawned task, so an email lost to a crash between the `202`
  and delivery is lost silently and the user must ask again.
- **Spent and expired token rows are never swept.** Harmless — both guards live in the redemption
  query — but the table only grows. The `expires_at` index exists so a future sweep is cheap.
- **Delivery has no automated test.** The harness points `SMTP_URL` at a dead port on purpose: a
  mail assertion there would be an assertion about a stub. The suite covers redemption, revocation
  and the non-oracle; delivery is the manual drill above.
- **The harness mints reset tokens itself**, since a token is only ever emailed and only its hash is
  stored. The duplication is safe in the direction that matters: if the handler ever hashed
  differently it would match no row, and every test using the helper would go **red**.
- **No self-serve account deletion**, and no way to change the tenant name. Blocker 6 named those;
  they are not recovery, and they are not built.
