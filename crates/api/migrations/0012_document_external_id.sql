-- Phase 11: `/ingest` becomes a real document, so its vectors are erasable by id.
--
-- `external_id` is the caller's own identifier for the content — a CMS page id, a row key in
-- whatever system the text came from. It exists so that re-syncing the same source is an OVERWRITE
-- rather than a duplicate: the API reuses the matching row's document_id and object_key, MinIO
-- reports a different etag, and invariant 10's "a different fingerprint means the client overwrote
-- the file, so it is re-indexed" does the rest. Idempotency through machinery that already exists.
--
-- Nullable, and the index is partial, because NULL means "always create a new document" — which is
-- today's behaviour for uploads, and must stay the default for a caller who does not care.
alter table documents add column external_id text;

-- Scoped to the tenant, not global: two tenants may legitimately use the same id from their own
-- systems, and a globally-unique constraint would leak that fact across the boundary (one tenant's
-- insert failing because of another's row is an existence oracle — the rule invariants 8, 18 and 26
-- all draw). `documents` has RLS, but a unique index is enforced beneath it.
create unique index idx_documents_external_id
    on documents (tenant_id, external_id) where external_id is not null;
