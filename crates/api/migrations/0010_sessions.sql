-- A session proves a login. It is a bearer token, stored ONLY as its SHA-256 hash — exactly
-- like api_keys.key_hash — so a database dump is not a session dump. The raw token is returned
-- once at login/register and never again.
--
-- Global table, NO row-level security: the session lookup is what ESTABLISHES tenant context,
-- so it necessarily precedes it (same category as tenants / api_keys / accounts).
create table sessions (
    token_hash text primary key,
    account_id uuid not null references accounts(id) on delete cascade,
    -- Denormalised so resolving a session yields tenant context in a single query. On delete
    -- of either the account or the tenant, the session goes with it.
    tenant_id  text not null references tenants(id) on delete cascade,
    created_at timestamptz not null default now(),
    -- Checked on every resolve (`expires_at > now()`); an expired token is indistinguishable
    -- from an unknown one to the caller — both 401.
    expires_at timestamptz not null
);

create index idx_sessions_account on sessions (account_id);
