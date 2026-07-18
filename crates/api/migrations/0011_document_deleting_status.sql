-- Phase 8: document deletion is a tombstone-guarded saga across three stores (Postgres, Qdrant,
-- MinIO) with no transaction spanning them. `deleting` is the tombstone: a TRANSIENT state a row
-- enters at the start of deletion and leaves only by being removed entirely. It is not a resting
-- state — a row must never settle here. Its whole job is to (a) drop the document from listings
-- immediately, and (b) fence the worker out, so a redelivered event or a late-finishing index
-- cannot resurrect a document mid-erasure. See lifecycle.rs (`claim` skips it; the post-index
-- transitions require `processing`) and invariant 10.
--
-- The vestigial `uploaded` status is left in the set unchanged — an unreachable state that predates
-- this change, and folding its cleanup in here would muddy the diff. It is its own todo.

alter table documents drop constraint documents_status_valid;

alter table documents add constraint documents_status_valid check (status in
    ('uploading','uploaded','processing','ready','failed','expired','quarantined','deleting'));
