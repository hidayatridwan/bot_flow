-- Phase 8: Row-Level Security on documents — the DB-enforced leg of tenant isolation.
-- Even a query that forgets `WHERE tenant_id = ...` cannot see or modify other tenants' rows.
alter table documents enable row level security;
-- FORCE so the policy applies to the table OWNER too (our app connects as the owner in dev).
alter table documents force row level security;

-- The app sets `app.current_tenant` per transaction via set_config(..., is_local => true).
-- current_setting(..., true) returns NULL when unset -> the comparison is false -> deny by default.
create policy documents_tenant_isolation on documents
    using (tenant_id = current_setting('app.current_tenant', true))
    with check (tenant_id = current_setting('app.current_tenant', true));
