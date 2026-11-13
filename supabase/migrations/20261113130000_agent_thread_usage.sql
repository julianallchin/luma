-- What a conversation cost, kept: tokens by kind, the price where the writer
-- was told one, how long it ran and how many subagents it started.
--
-- A source table under the contract of docs/design/data-layer.md. One live
-- row a thread, holding the thread's totals so far: a device writes it after
-- each step of a turn, so its history in the thread's scope is the spend
-- step by step, each entry naming the model that ran.
--
-- `thread_id` is not a foreign key. A client that hosts a session deletes
-- its thread when the session ends and only then learns the price, and a
-- row under a deleted parent would be refused.
--
-- `row_model.sql` dropped an older table of this name that nothing wrote.
--
-- Additive: the data format does not change.
--
-- Symmetric with backend/migrations/20261113130000_agent_thread_usage.sql.
--
-- Order: apply this file, redeploy `deploy/powersync/sync-config.yaml`, which
-- now names the table, then ship the client that syncs it.

begin;

create table public.agent_thread_usage (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    thread_id text not null,
    model text,
    turns bigint not null default 0,
    input_tokens bigint not null default 0,
    output_tokens bigint not null default 0,
    cache_creation_tokens bigint not null default 0,
    cache_read_tokens bigint not null default 0,
    cost_usd double precision,
    duration_ms bigint not null default 0,
    subagents bigint not null default 0,
    recorded_at timestamptz not null default now(),
    deleted_at timestamptz,
    deleted_by uuid
);

create unique index agent_thread_usage_live_thread
    on public.agent_thread_usage (thread_id) where deleted_at is null;

create view public.agent_thread_usage_live with (security_invoker = true) as
    select * from public.agent_thread_usage where deleted_at is null;
revoke all on public.agent_thread_usage_live from public, anon, authenticated;

alter table public.agent_thread_usage enable row level security;
revoke all on public.agent_thread_usage from anon, public;
grant select, insert, update on public.agent_thread_usage to authenticated;
grant select on public.agent_thread_usage to powersync_role;

create policy agent_thread_usage_select on public.agent_thread_usage for select to authenticated
    using (uid = auth.uid());
create policy agent_thread_usage_insert on public.agent_thread_usage for insert to authenticated
    with check (uid = auth.uid());
create policy agent_thread_usage_update on public.agent_thread_usage for update to authenticated
    using (uid = auth.uid())
    with check (uid = auth.uid());

create trigger agent_thread_usage_keep_flag before update on public.agent_thread_usage
    for each row execute function public.keep_flag();
create trigger agent_thread_usage_flag_children
    after update of deleted_at on public.agent_thread_usage
    for each row execute function public.flag_children();
create trigger agent_thread_usage_live_parents
    before insert or update on public.agent_thread_usage
    for each row execute function public.live_parents();
create trigger agent_thread_usage_client_format
    before insert or update or delete on public.agent_thread_usage
    for each statement execute function public.require_current_client();
create trigger agent_thread_usage_history
    after insert or update or delete on public.agent_thread_usage
    for each row execute function public.record_history('thread_id');

alter publication powersync add table public.agent_thread_usage;

notify pgrst, 'reload schema';

commit;
