-- Password reset: the one credential flow that must work for a user who has lost their credential.
--
-- Global and RLS-free, for the same reason as `accounts` and `sessions` (0009, 0010): this table is
-- read *before* tenant context exists. A reset is resolved from a token alone — the requester has no
-- session, and by construction cannot prove which tenant they belong to.
--
-- The token is stored **hashed**, never raw. Same rule as API keys (invariant 14) and session tokens
-- (invariant 17), and for the same reason: a database dump must not be a credential dump. A reset
-- token is a *bearer credential that changes a password*, so it is if anything more dangerous than
-- the session it replaces — leaking one is account takeover.
create table password_reset_tokens (
    token_hash text primary key,
    account_id uuid not null references accounts(id) on delete cascade,
    created_at timestamptz not null default now(),
    expires_at timestamptz not null,
    -- Single use. Set when redeemed, and never cleared: a redeemed token must stay redeemed even
    -- though the row lingers. Nullable rather than a boolean because *when* it was used is the only
    -- forensic trail this flow leaves.
    used_at    timestamptz
);

-- Redeeming a token invalidates every other outstanding token for that account, so the consume path
-- updates by `account_id` as well as by hash. Without this index that is a sequential scan on a
-- table that only ever grows.
create index idx_password_reset_account on password_reset_tokens (account_id);

-- Expired and used rows are dead weight; this index is what lets a future sweep find them cheaply.
-- There is no sweep yet, which is stated in the phase doc rather than implied by an unused index —
-- but the rows are harmless: a used token fails the `used_at is null` guard and an expired one fails
-- the `expires_at > now()` guard, so retention is a housekeeping question, not a security one.
create index idx_password_reset_expires on password_reset_tokens (expires_at);
