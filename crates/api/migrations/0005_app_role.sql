-- Phase 8: runtime connects as a NON-superuser so RLS applies (superusers bypass it).
-- Migrations still run as the admin role (bot_flow).
do $$
begin
   if not exists (select 1 from pg_roles where rolname = 'app_user') then
      create role app_user login password 'app_user';  -- dev-only password
   end if;
end
$$;

grant usage on schema public to app_user;
grant select, insert, update, delete on all tables in schema public to app_user;
grant usage, select on all sequences in schema public to app_user;

-- Apply to objects future migrations create (default-privs are tied to bot_flow).
alter default privileges in schema public grant select, insert, update, delete on tables to app_user;
alter default privileges in schema public grant usage, select on sequences to app_user;
