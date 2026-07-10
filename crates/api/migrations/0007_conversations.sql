-- Conversation memory: the history a follow-up question is resolved against.
-- Both tables carry tenant_id directly so the RLS policy is a plain column comparison,
-- exactly like documents (0004) — no join back to conversations to decide visibility.
create table conversations (
    id         uuid primary key,
    tenant_id  text not null references tenants(id) on delete cascade,
    created_at timestamptz not null default now()
);

create table messages (
    id              uuid primary key,
    conversation_id uuid not null references conversations(id) on delete cascade,
    tenant_id       text not null references tenants(id) on delete cascade,
    role            text not null check (role in ('user', 'assistant')),
    content         text not null,
    metadata        jsonb not null default '{}',
    -- Order by `seq`, not `created_at`: now() is the transaction timestamp, so two messages
    -- written in one transaction would tie and the history could come back out of order.
    seq             bigserial not null,
    created_at      timestamptz not null default now()
);

-- Reads are always "latest N of one conversation, in order" — this index serves exactly that.
create index idx_messages_conversation on messages (conversation_id, seq);

alter table conversations enable row level security;
alter table conversations force row level security;
create policy conversations_tenant_isolation on conversations
    using (tenant_id = current_setting('app.current_tenant', true))
    with check (tenant_id = current_setting('app.current_tenant', true));

alter table messages enable row level security;
alter table messages force row level security;
create policy messages_tenant_isolation on messages
    using (tenant_id = current_setting('app.current_tenant', true))
    with check (tenant_id = current_setting('app.current_tenant', true));
