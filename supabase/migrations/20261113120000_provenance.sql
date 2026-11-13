-- What a later reader of the record needs and cannot work out afterwards:
-- which build made a write, what a score was copied from, what its author
-- thought of what an agent made, and who that author is.
--
--   * `history.client_build`: the app version that sent the write, from the
--     `x-luma-build` header. A prompt, a tool list and an analysis version
--     all ship in a build, so this one field stands for them, and a build
--     that wrote something wrong can be left out by name.
--   * `history.venue_seq`: on an entry for a score or a clip, the newest
--     entry of the score's venue when it was written. A clip names its
--     venue's groups by id, and what those ids held is the venue at that
--     `seq`. Entries from before this file are filled in the same way.
--   * `scores.copied_from_score_id`: the score a duplicate was made from.
--     Not a foreign key: a copy is not its source's child, and outlives it.
--   * `feedback`: a person's verdict on an agent's turn (`message_id`) or on
--     one clip (`clip_id`). A source table, under the contract of
--     docs/design/data-layer.md. Neither id is a foreign key: a verdict on a
--     clip is still a verdict once the clip is deleted, which is often the
--     next thing its author does.
--   * `profiles.roles`, `profiles.role_other`, `profiles.experience`: what
--     the account said about
--     itself, once, after accepting the terms. Reached through the two
--     functions below, like the terms. A change is a history entry.
--
-- Additive: no row changes shape, so the data format stays where it is and a
-- build that knows none of this keeps working.
--
-- Symmetric with backend/migrations/20261113120000_provenance.sql, which has
-- the score column and the feedback table.

begin;

-- ---------------------------------------------------------------------------
-- A score's source.
-- ---------------------------------------------------------------------------

alter table public.scores add column copied_from_score_id text;

-- ---------------------------------------------------------------------------
-- Feedback.
-- ---------------------------------------------------------------------------

create table public.feedback (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    -- The score the turn or the clip belongs to. A conversation about a venue
    -- or a pattern has none.
    score_id text references public.scores (id) on delete cascade deferrable initially deferred,
    thread_id text,
    message_id text,
    clip_id text,
    rating smallint not null check (rating in (-1, 1)),
    note text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    deleted_by uuid,
    check (message_id is not null or clip_id is not null)
);

create index feedback_score_idx on public.feedback (score_id);
-- One verdict a person per turn, and one per clip.
create unique index feedback_live_message
    on public.feedback (uid, message_id)
    where clip_id is null and deleted_at is null;
create unique index feedback_live_clip
    on public.feedback (uid, clip_id)
    where clip_id is not null and deleted_at is null;

create view public.feedback_live with (security_invoker = true) as
    select * from public.feedback where deleted_at is null;
revoke all on public.feedback_live from public, anon, authenticated;

alter table public.feedback enable row level security;
revoke all on public.feedback from anon, public;
grant select, insert, update on public.feedback to authenticated;
grant select on public.feedback to powersync_role;

-- A verdict is its author's and nobody else reads it.
create policy feedback_select on public.feedback for select to authenticated
    using (uid = auth.uid());
create policy feedback_insert on public.feedback for insert to authenticated
    with check (uid = auth.uid()
                and (score_id is null or public.can_access_score(score_id)));
create policy feedback_update on public.feedback for update to authenticated
    using (uid = auth.uid())
    with check (uid = auth.uid()
                and (score_id is null or public.can_access_score(score_id)));

create trigger feedback_keep_flag before update on public.feedback
    for each row execute function public.keep_flag();
create trigger feedback_flag_children after update of deleted_at on public.feedback
    for each row execute function public.flag_children();
create trigger feedback_live_parents before insert or update on public.feedback
    for each row execute function public.live_parents();

alter publication powersync add table public.feedback;

-- A build behind the server's data format writes to no synced table
-- (20261111120000_client_format.sql).
create trigger feedback_client_format before insert or update or delete on public.feedback
    for each statement execute function public.require_current_client();

-- ---------------------------------------------------------------------------
-- History: the build, and the venue a score's entry was written against.
-- ---------------------------------------------------------------------------

alter table public.history
    add column client_build text,
    add column venue_seq bigint;

-- The venue of the score an entry belongs to, read off the entry itself for a
-- score and off the score for a clip.
update public.history entry set venue_seq = (
    select max(venue.seq) from public.history venue
    where venue.seq < entry.seq
      and venue.scope_id = case entry.table_name
          when 'scores' then coalesce(entry.after, entry.before) ->> 'venue_id'
          else (select score.venue_id from public.scores score where score.id = entry.scope_id)
      end)
where entry.table_name in ('scores', 'clips');

create or replace function public.record_history() returns trigger
language plpgsql
security definer
set search_path = public, pg_temp
as $$
declare
    before_row jsonb;
    after_row jsonb;
    headers json := nullif(current_setting('request.headers', true), '')::json;
    claimed text;
    scope text;
    venue text;
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

    claimed := left(nullif(headers ->> 'x-luma-actor', ''), 200);
    if claimed like 'user:%' then
        claimed := null;
    end if;
    scope := coalesce(after_row, before_row) ->> tg_argv[0];
    perform pg_advisory_xact_lock(hashtextextended('luma.history:' || coalesce(scope, ''), 0));

    -- The venue a score's groups are read in. A feedback entry is in a
    -- score's scope too, and says nothing about the venue.
    if tg_table_name = 'scores' then
        venue := coalesce(after_row, before_row) ->> 'venue_id';
    elsif tg_table_name = 'clips' then
        select score.venue_id into venue from public.scores score where score.id = scope;
    end if;

    insert into public.history (
        uid, table_name, row_id, op, before, after, actor, actor_type,
        thread_id, message_id, change_group_id, scope_id, schema_version,
        client_build, venue_seq)
    values (
        -- Outside a request there is no caller; the row's owner stands in.
        coalesce(auth.uid(), (coalesce(after_row, before_row) ->> 'uid')::uuid),
        tg_table_name,
        coalesce(after_row, before_row) ->> 'id',
        lower(tg_op),
        before_row,
        after_row,
        coalesce(claimed, 'user:' || auth.uid()::text, current_user::text),
        case
            when claimed like 'migration:%' then 'migration'
            when claimed like 'app:%' then 'app'
            when claimed is not null then 'agent'
            when auth.uid() is not null then 'person'
            else 'admin'
        end,
        nullif(headers ->> 'x-luma-thread', ''),
        nullif(headers ->> 'x-luma-message', ''),
        -- Changes made together. A client names the group; without one, the
        -- transaction is the group.
        coalesce(nullif(headers ->> 'x-luma-group', ''), 'tx:' || txid_current()::text),
        scope,
        public.data_format(),
        left(nullif(headers ->> 'x-luma-build', ''), 100),
        (select max(entry.seq) from public.history entry where entry.scope_id = venue)
    );
    return null;
end;
$$;
revoke all on function public.record_history() from public, anon, authenticated;

create trigger feedback_history after insert or update or delete on public.feedback
    for each row execute function public.record_history('score_id');

-- ---------------------------------------------------------------------------
-- What an account says about itself.
-- ---------------------------------------------------------------------------

-- `public.profiles` predates this folder (20261111235859_profiles_private.sql),
-- so a database built from it alone has none, and the functions below find
-- no table there.
do $$
begin
    if to_regclass('public.profiles') is null then
        return;
    end if;

    alter table public.profiles
        -- Every word that fits: plenty of people are a DJ and a lighting
        -- designer.
        add column roles text[] check (roles <@ array[
            'lighting_designer', 'dj', 'musician', 'venue_tech', 'hobbyist', 'other']),
        -- What "other" is, in the account's own words.
        add column role_other text check (char_length(role_other) <= 80),
        add column experience text check (experience in (
            'none', 'some', 'regular', 'professional')),
        -- When the account was asked. Set by an answer and by a skip, so the
        -- question is put once.
        add column background_asked_at timestamptz;

    -- Only what the functions below write: a sign-up makes this row outside
    -- a request, where history has nobody to name.
    create trigger profiles_history
        after update of roles, role_other, experience on public.profiles
        for each row execute function public.record_history('id');
end
$$;

-- What the caller has said, or null where it has not been asked.
create function public.profile_background () returns jsonb
language plpgsql
stable
security definer
set search_path = ''
as $$
begin
    return (
        select jsonb_build_object(
            'roles', coalesce(held.roles, '{}'), 'role_other', held.role_other,
            'experience', held.experience)
        from public.profiles held
        where held.id = auth.uid() and held.background_asked_at is not null);
end
$$;

-- Record the caller's answer. Nothing in it is a skip, and still counts as
-- asked.
create function public.set_profile_background (roles text[], role_other text, experience text)
returns void
language plpgsql
volatile
security definer
set search_path = ''
as $$
begin
    insert into public.profiles as held (id, roles, role_other, experience, background_asked_at)
    values (auth.uid(), $1, $2, $3, now())
    on conflict (id) do update
        set roles = excluded.roles, role_other = excluded.role_other,
            experience = excluded.experience,
            background_asked_at = now(), updated_at = now();
end
$$;

revoke all on function public.profile_background (), public.set_profile_background (text[], text, text)
    from public, anon;
grant execute on function public.profile_background (), public.set_profile_background (text[], text, text)
    to authenticated;

notify pgrst, 'reload schema';

commit;
