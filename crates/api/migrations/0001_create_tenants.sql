-- Phase 3: the tenants table = the official registry of valid tenants (source of truth for "does this tenant exist").
-- id is intentionally a human-friendly text slug (e.g. 'acme'), NOT a UUID: convenient for curl tests,
-- and the exact same value is later used as the tenant_id filter in Qdrant (the cross-store join key).
create table tenants (
    id         text primary key,
    name       text not null,
    created_at timestamptz not null default now()
);

-- No seed data: register tenants through POST /admin/tenants.