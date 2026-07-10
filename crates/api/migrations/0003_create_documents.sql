-- Phase 5: every uploaded file gets a row here so we can track processing status
-- (pending -> processing -> done/failed) and tie chunks back to their source document.
create table documents (
    id          uuid primary key,
    tenant_id   text not null references tenants(id) on delete cascade,
    filename    text not null,
    object_key  text not null,                 -- key in the MinIO bucket
    status      text not null default 'pending',
    created_at  timestamptz not null default now()
);
create index idx_documents_tenant on documents (tenant_id);
