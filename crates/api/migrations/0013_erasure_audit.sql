-- Phase 12: tenant-level erasure, and a record that erasure happened.
--
-- **`erasures` deliberately has NO foreign key to `tenants`.** Every other tenant-scoped table
-- cascades on tenant deletion, which is exactly right for data and exactly wrong for an audit
-- record: an erasure log that is destroyed by the erasure it documents proves nothing. `tenant_id`
-- is plain text here for the same reason a receipt outlives the transaction.
--
-- It also has no RLS. It is an operator's record, not a tenant's — and the row for a deleted tenant
-- has no tenant left to scope it to.
create table erasures (
    id            uuid primary key,
    tenant_id     text not null,
    -- 'document' | 'tenant'. What was asked to disappear.
    scope         text not null check (scope in ('document', 'tenant')),
    -- The document's id, or null for a whole-tenant erasure.
    document_id   uuid,
    -- Who asked. 'admin' for the deployment key, 'secret'/'session' for a tenant's own principal.
    -- Not the credential itself: invariant 14 forbids a key reaching any log, and a hash here would
    -- be a fingerprint linking rows to a key we cannot otherwise identify.
    actor         text not null,
    -- What was actually removed, for a reviewer who asks "did it work?" rather than "did you try?".
    vectors_deleted bigint,
    objects_deleted bigint,
    -- Set when the saga finished. A row with `completed_at` null is an erasure that started and did
    -- not finish — which is the interesting case, and the reason this is two columns not one.
    requested_at  timestamptz not null default now(),
    completed_at  timestamptz
);

create index idx_erasures_tenant on erasures (tenant_id, requested_at desc);

-- Phase 12: an answer may quote a document, and deleting the document must not leave the quote
-- behind. Nothing linked a message to its sources, so nothing could find them.
--
-- `metadata` already existed and was unused. Assistant messages now carry
-- `{"document_ids": ["…"]}` — the documents whose passages were in the model's context — and
-- document deletion redacts the messages that cite it.
--
-- jsonb_path_ops: this index answers exactly one question, "which messages cite this document",
-- and that operator class is both smaller and faster for containment than the default.
create index idx_messages_document_ids on messages
    using gin ((metadata -> 'document_ids') jsonb_path_ops);
