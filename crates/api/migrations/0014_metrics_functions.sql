-- Phase 13: fleet-wide counts for /metrics.
--
-- **Why these exist at all, and it is not performance.** `documents` has FORCE ROW LEVEL SECURITY
-- and the API connects as the non-superuser `app_user`. A plain
-- `SELECT status, count(*) FROM documents GROUP BY status` from the API therefore matches **zero
-- rows and reports success** — the corollary trap, arriving in the one endpoint whose job is to say
-- when something is wrong. Every gauge would read 0 and the dashboard would be permanently,
-- beautifully green. This is the single most likely way to get phase 13 wrong and not notice.
--
-- The alternative is looping per tenant with `tenant_tx`, as `reaper.rs` does. That is correct and
-- honest, and it is N transactions *per scrape* — 2,000 a minute at 500 tenants on a 15s interval,
-- for a monitoring endpoint. Rejected on cost, but it remains the right fallback if the power
-- granted below is ever felt to be too much.
--
-- **Why SECURITY DEFINER is safe here, precisely.** These functions step around RLS, and what makes
-- that acceptable is that **their return type cannot express a tenant**. They return aggregates over
-- the fleet; there is no row and no identity in the result. That is invariant 30 enforced by a
-- signature rather than by discipline — see the trap note in CLAUDE.md about never adding a
-- `tenant_id` column here.
--
-- `search_path` is pinned (standard SECURITY DEFINER hygiene: without it, a caller could shadow
-- `documents` with a temp table and have the definer read that instead). `STABLE` because they only
-- read.

create or replace function metrics_document_counts()
returns table (status text, n bigint)
language sql
security definer
stable
set search_path = pg_catalog, public
as $$
    select status, count(*) from documents group by status;
$$;

-- The reaper's job is to drive every one of these to zero within 60s of a threshold elapsing. So a
-- persistently non-zero value means the reaper is dead, or its sweep is erroring — learned from the
-- outcome it exists to produce rather than from a liveness ping, which would be green while every
-- sweep threw.
--
-- **The thresholds live here and only here.** They mirror `UPLOAD_GRACE` (5 minutes) and
-- `PROCESSING_LEASE` (30 minutes) in `crates/worker/src/reaper.rs`. Copying them into Rust as well
-- would give the gauge its own opinion, and the failure would be a metric that quietly stops
-- matching the sweep it is watching.
create or replace function metrics_overdue_counts()
returns table (kind text, n bigint)
language sql
security definer
stable
set search_path = pg_catalog, public
as $$
    select 'stuck_processing'::text, count(*) from documents
      where status = 'processing'
        and processing_started_at < now() - interval '30 minutes'
    union all
    select 'stuck_uploading'::text, count(*) from documents
      where status = 'uploading'
        and upload_expires_at < now() - interval '5 minutes'
    union all
    select 'stuck_deleting'::text, count(*) from documents
      where status = 'deleting'
        and (processing_started_at is null
             or processing_started_at < now() - interval '30 minutes');
$$;

grant execute on function metrics_document_counts() to app_user;
grant execute on function metrics_overdue_counts() to app_user;
