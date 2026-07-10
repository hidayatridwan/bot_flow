-- Direct-to-MinIO uploads: the API mints a presigned PUT and never sees the bytes.
-- The document row is created BEFORE the object exists, so it needs a lifecycle richer
-- than pending/processing/done: an upload can be abandoned, expire, or arrive oversize.

alter table documents
    add column content_type          text,
    add column size_bytes           bigint,
    add column etag                 text,
    add column upload_expires_at    timestamptz,
    add column uploaded_at          timestamptz,
    add column processing_started_at timestamptz,  -- lease: lets the reaper reclaim a dead worker's row
    add column processed_at         timestamptz,
    add column error                text,
    add column attempts             integer not null default 0;

-- Existing rows predate the new lifecycle; map them onto it before the constraint lands.
update documents set status = 'ready'     where status = 'done';
update documents set status = 'uploading' where status = 'pending';

alter table documents
    add constraint documents_status_valid check (status in
        ('uploading','uploaded','processing','ready','failed','expired','quarantined')),
    alter column status set default 'uploading';

-- The key is the identity of the object; two rows must never claim the same one.
alter table documents add constraint documents_object_key_unique unique (object_key);

-- Serves the reaper's only query. Partial: it never looks at settled rows.
create index idx_documents_expiring on documents (upload_expires_at)
    where status = 'uploading';
create index idx_documents_processing on documents (processing_started_at)
    where status = 'processing';

-- SECURITY: tenant_id is interpolated into the object key, which is the boundary a presigned
-- URL is bound to. A slug like 'a/../b' would let one tenant's key escape its own prefix.
-- Enforce it in the database so no future code path can bypass the check.
alter table tenants add constraint tenants_id_slug
    check (id ~ '^[a-z0-9][a-z0-9-]{0,62}$');
