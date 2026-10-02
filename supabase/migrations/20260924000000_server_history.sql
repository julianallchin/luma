-- History moves to the server.
--
-- Devices used to keep a `changes` log of every synced row, before and after,
-- and sync it up here and back down to every device. That included the large
-- analysis rows and the agent transcripts, and it was most of the data
-- PowerSync hosted. Nothing on a device reads history, so none of it syncs
-- now: this trigger records authored rows as they arrive, and `changes` goes
-- with its data.
--
-- Who made a change is `actor`: `user:<uid>` for a person, or a model id (or
-- an MCP client's label) for an agent. The client sends an agent's actor as
-- the `x-luma-actor` header on the PostgREST request that carries the write;
-- without one the write is the signed-in user's. A header cannot claim to be
-- another user: a `user:` value is replaced by the caller's own.
--
-- A score's "last edited" time is `scores.authored_at`, which the client
-- writes when a save moves something.
--
-- Order:
--   1. Redeploy `deploy/powersync/sync-config.yaml` without `changes`: a sync rule over a
--      dropped table fails its whole stream.
--   2. Apply this file.
--   3. Ship the client that uploads `scores.authored_at`.
--
-- Idempotent.

begin;

drop table if exists public.changes;

alter table public.scores add column if not exists authored_at timestamptz;

-- `uid` is whoever made the change, so the history a person reads is their
-- own edits. No foreign key: deleting an account cascades through its rows,
-- and the history of those deletes would reference the user being deleted.
create table if not exists public.history (
    id bigserial primary key,
    uid uuid not null,
    table_name text not null,
    row_id text not null,
    op text not null check (op in ('insert', 'update', 'delete')),
    before jsonb,
    after jsonb,
    actor text not null,
    at timestamptz not null default now()
);

create index if not exists history_row_idx on public.history (table_name, row_id, at desc);
create index if not exists history_at_idx on public.history (at);

-- Read-only to its owner. Only the trigger writes, as the definer.
alter table public.history enable row level security;
revoke all on public.history from public, anon, authenticated;
grant select on public.history to authenticated;
drop policy if exists history_select on public.history;
create policy history_select on public.history
    for select to authenticated
    using (uid = auth.uid());

create or replace function public.record_history() returns trigger
language plpgsql
security definer
set search_path = public, pg_temp
as $$
declare
    before_row jsonb;
    after_row jsonb;
    claimed text;
    who uuid;
begin
    if tg_op <> 'INSERT' then
        before_row := to_jsonb(old);
    end if;
    if tg_op <> 'DELETE' then
        after_row := to_jsonb(new);
    end if;
    -- The touch of `updated_at` alone, or an upsert that re-sends the row
    -- as it is, is not an edit.
    if tg_op = 'UPDATE' and before_row - 'updated_at' = after_row - 'updated_at' then
        return null;
    end if;

    claimed := left(nullif(current_setting('request.headers', true), '')::json ->> 'x-luma-actor', 200);
    if claimed = '' or claimed like 'user:%' then
        claimed := null;
    end if;
    -- Outside a request (the SQL editor, a cascade from an account deletion)
    -- there is no caller; the row's owner stands in.
    who := coalesce(auth.uid(), (coalesce(after_row, before_row) ->> 'uid')::uuid);

    insert into public.history (uid, table_name, row_id, op, before, after, actor)
    values (
        who,
        tg_table_name,
        coalesce(after_row, before_row) ->> 'id',
        lower(tg_op),
        before_row,
        after_row,
        coalesce(claimed, 'user:' || auth.uid()::text, current_user::text)
    );
    return null;
end;
$$;

revoke all on function public.record_history() from public, anon, authenticated;

-- The authored tables. Not tracks or their analysis, which the app computes;
-- not agent transcripts or drafts, which are their own record.
do $$
declare
    name text;
begin
    foreach name in array array[
        'venues', 'venue_members', 'fixtures', 'fixture_groups', 'fixture_group_members',
        'venue_nodes', 'venue_edges', 'venue_node_params', 'venue_constraints',
        'scores', 'clips', 'midi_modifiers', 'midi_bindings'
    ] loop
        execute format('drop trigger if exists %I on public.%I', name || '_history', name);
        execute format(
            'create trigger %I after insert or update or delete on public.%I
             for each row execute function public.record_history()',
            name || '_history', name);
    end loop;
end
$$;

commit;

notify pgrst, 'reload schema';
