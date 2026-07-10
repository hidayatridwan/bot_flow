-- Phase 9: distinguish secret keys (full access, server-side) from publishable keys
-- (chat-only, embeddable in browsers, restricted to a tenant's allowed origin domains).
alter table api_keys
    add column kind text not null default 'secret' check (kind in ('secret', 'publishable')),
    add column allowed_origins text[] not null default '{}';
