-- A classified, tenant-safe companion to `documents.error`.
--
-- `error` holds raw parser stderr and the reaper's "worker presumed dead" post-mortem, so no
-- endpoint may expose it (invariant 16). That left the UI able to say only "failed", naming both a
-- broken file and a dead worker, and a tenant could not tell whether to re-upload or wait.
-- `failure_reason` is the half that *can* be shipped: a closed set describing what the tenant
-- should do, never where in our stack the fault was.
--
-- Nullable, and not backfilled. NULL means "failed before this column existed" — nothing ever
-- recorded which cause produced those rows, and deriving one by grepping `error` for the reaper's
-- string would be inventing a fact from free text that was never a contract. The UI renders NULL
-- as the old both-causes copy, which is exactly as much as is actually known.
alter table documents add column failure_reason text;

-- The enum lives in three places that must agree: `crates/worker/src/failure.rs`, this CHECK, and
-- the TypeScript union in `web/src/lib/types/documents.ts`. This constraint is the one that fails
-- closed, so a worker writing a variant the others do not know about aborts its transaction rather
-- than storing a value the UI would render as 'unknown'.
alter table documents
  add constraint documents_failure_reason_check
  check (
    failure_reason is null
    or failure_reason in ('unreadable_file', 'unsupported_type', 'too_large', 'system_error')
  );
