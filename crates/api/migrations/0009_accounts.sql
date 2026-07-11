-- Self-serve tenant accounts: a human login (email + password) that owns a tenant.
-- This is the credential a person uses in the dashboard; API keys (api_keys) remain the
-- credential a tenant's server and widget use. Registration mints both.
--
-- Global table, NO row-level security: like tenants and api_keys, an account is resolved
-- BEFORE any tenant context exists (a session lookup is how tenant context is established).
-- app_user's CRUD grants come from the default privileges set in 0005 — no explicit grant here.
create table accounts (
    id            uuid primary key,
    tenant_id     text not null references tenants(id) on delete cascade,
    email         text not null,
    -- Argon2id PHC string (algorithm + salt + params + hash). Never logged, like a key hash.
    password_hash text not null,
    created_at    timestamptz not null default now()
);

-- Email is the login identity: globally unique, case-insensitive. Indexing lower(email)
-- (rather than depending on the citext extension) keeps the migration superuser-free.
create unique index idx_accounts_email_lower on accounts (lower(email));

-- MVP is one owner account per tenant; multi-user/invite is a later extension that would
-- drop this constraint.
create unique index idx_accounts_tenant on accounts (tenant_id);
