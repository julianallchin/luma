-- The server is the permanent record; a device is a cache of live rows.
--
-- Until now a delete was a delete on both sides, and what a row used to be
-- survived only in `public.history`, for some tables. This file makes every
-- table one of two kinds and gives the first kind one contract.
--
--   source   what people or agents made: venues and their content, tracks,
--            scores, clips, beat grids and their validations, conversations.
--            Never deleted by the app: a delete sets `deleted_at`, the sync
--            rules stop shipping the row, and it leaves every device.
--   derived  computed from audio: stems, roots, drum onsets, bar
--            classifications, genres. A cache. No flag, no history; deleted
--            for real and rebuilt when wanted.
--
-- Drafts are scratch and stay as they were.
--
-- The contract for a source table:
--   * `id` is random and opaque. It names no parent and is never used twice.
--     Rows whose id was built from other columns (`score:key`, `node:key`,
--     `folder:track`, a track's id reused for its grid) get a new one here;
--     `public.legacy_ids` keeps what each used to be, so a history entry from
--     before this file can still be read.
--   * `deleted_at`, `deleted_by`. Uniqueness holds among live rows only.
--   * Inserting an id that is flagged clears the flag. That is undo, and the
--     only way a row comes back.
--   * Flagging a row flags its source children and deletes its derived ones
--     in the same transaction, so a parent and its children leave a device in
--     one checkpoint.
--   * An edit to a flagged row changes nothing, so the client that made it is
--     told "not saved" instead of writing to a row nobody can see.
--   * The app's role cannot `DELETE`.
--
-- A beat grid is never edited: reanalysis writes a new row, and a score names
-- the grid it was written against (`scores.grid_id`).
--
-- History is one append-only log over every source table but the
-- conversation ones, whose messages are their own record. A version of a
-- score or a venue is a position in it (`seq`), so entries within one scope
-- are written in commit order.
--
-- Format 2. A build that reads format 1 is refused and told to update, and
-- the build that ships with this clears its copy and downloads again.
--
-- contract: after 0.7.0

begin;

create or replace function public.data_format() returns integer
language sql
stable
set search_path = ''
as $$ select 2 $$;

-- ---------------------------------------------------------------------------
-- History stops listening while ids are reissued: `legacy_ids` is the record
-- of that, and eleven thousand "the id changed" entries would say less.
-- ---------------------------------------------------------------------------

do $$
declare
    listener record;
begin
    for listener in
        select t.tgname, c.relname
        from pg_trigger t
        join pg_class c on c.oid = t.tgrelid
        join pg_proc p on p.oid = t.tgfoid
        where p.proname = 'record_history' and p.pronamespace = 'public'::regnamespace
          and not t.tgisinternal
    loop
        execute format('drop trigger %I on public.%I', listener.tgname, listener.relname);
    end loop;
end
$$;

-- Drafts hold a score as JSON keyed by the old clip ids. They are scratch.
delete from public.drafts;

-- ---------------------------------------------------------------------------
-- Ids.
-- ---------------------------------------------------------------------------

create table public.legacy_ids (
    table_name text not null,
    old_id text not null,
    new_id text not null,
    primary key (table_name, old_id)
);
alter table public.legacy_ids enable row level security;
revoke all on public.legacy_ids from public, anon, authenticated;

-- A clip's readable handle, for the ones an agent named. The key the old id
-- carried is that handle unless it was itself random.
alter table public.clips add column label text;
update public.clips
set label = substr(id, length(score_id) + 2)
where substr(id, length(score_id) + 2)
      !~ '^(new:)?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$';

-- A laser's limits were keyed by their fixture's id.
alter table public.laser_limits add column fixture_id text;
update public.laser_limits set fixture_id = id;
alter table public.laser_limits alter column fixture_id set not null;
alter table public.laser_limits drop constraint if exists laser_limits_id_fkey;
alter table public.laser_limits
    add constraint laser_limits_fixture_id_fkey foreign key (fixture_id)
    references public.fixtures (id) on delete cascade deferrable initially deferred;

alter table public.folder_tracks drop constraint if exists folder_tracks_check;

create or replace function pg_temp.reissue(name text, every boolean) returns void
language plpgsql
as $$
begin
    execute format(
        'insert into public.legacy_ids (table_name, old_id, new_id)
         select %L, id, gen_random_uuid()::text from public.%I
         where $1 or id !~ ''^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$''',
        name, name) using every;
    execute format(
        'update public.%I row set id = was.new_id from public.legacy_ids was
         where was.table_name = %L and was.old_id = row.id',
        name, name);
end
$$;

-- Built from other columns, every one of them.
select pg_temp.reissue('clips', true);
select pg_temp.reissue('folder_tracks', true);
select pg_temp.reissue('venue_node_params', true);
select pg_temp.reissue('venue_constraints', true);
select pg_temp.reissue('venue_members', true);
select pg_temp.reissue('laser_limits', true);
select pg_temp.reissue('perform_layouts', true);
select pg_temp.reissue('track_beats', true);
select pg_temp.reissue('track_beat_validations', true);
select pg_temp.reissue('agent_thread_transcript_heads', true);
-- Only the ones that name a parent: a venue's root node, a venue's folder.
select pg_temp.reissue('venue_nodes', false);
select pg_temp.reissue('folders', false);

-- What pointed at a reissued row. Every one of these foreign keys is checked
-- at the commit.
update public.venue_nodes row set parent_id = was.new_id
from public.legacy_ids was
where was.table_name = 'venue_nodes' and was.old_id = row.parent_id;
update public.venue_node_params row set node_id = was.new_id
from public.legacy_ids was
where was.table_name = 'venue_nodes' and was.old_id = row.node_id;
update public.venue_constraints row set node_id = was.new_id
from public.legacy_ids was
where was.table_name = 'venue_nodes' and was.old_id = row.node_id;
update public.venue_constraints row set target_node = was.new_id
from public.legacy_ids was
where was.table_name = 'venue_nodes' and was.old_id = row.target_node;
update public.folder_tracks row set folder_id = was.new_id
from public.legacy_ids was
where was.table_name = 'folders' and was.old_id = row.folder_id;

-- Check them now: a table with a check still pending cannot be altered.
set constraints all immediate;

-- The grid a score was written against. Not a foreign key: a grid is a
-- versioned input, and a score that names one is not its child.
alter table public.scores add column grid_id text;
update public.scores score set grid_id = grid.id
from public.track_beats grid where grid.track_id = score.track_id;

-- ---------------------------------------------------------------------------
-- The flag, and uniqueness among live rows.
-- ---------------------------------------------------------------------------

do $$
declare
    name text;
begin
    foreach name in array array[
        'venues', 'venue_members', 'fixtures', 'fixture_groups', 'fixture_group_members',
        'venue_nodes', 'venue_node_params', 'venue_constraints', 'folders', 'folder_tracks',
        'laser_zones', 'laser_limits', 'perform_patterns', 'perform_layouts',
        'perform_controllers', 'control_bindings', 'tracks', 'track_beats',
        'track_beat_validations', 'scores', 'clips', 'agent_threads',
        'agent_thread_messages', 'agent_thread_transcript_heads'
    ] loop
        execute format(
            'alter table public.%I add column deleted_at timestamptz, add column deleted_by uuid',
            name);
        -- What a policy or a helper function reads when it asks about a parent.
        execute format(
            'create view public.%I with (security_invoker = true) as
             select * from public.%I where deleted_at is null',
            name || '_live', name);
        execute format('revoke all on public.%I from public, anon, authenticated', name || '_live');
        execute format('revoke delete, truncate on public.%I from authenticated, anon', name);
    end loop;
end
$$;

alter table public.fixture_groups drop constraint if exists fixture_groups_venue_id_name_key;
create unique index fixture_groups_live_name
    on public.fixture_groups (venue_id, name) where deleted_at is null;

alter table public.control_bindings
    drop constraint if exists control_bindings_controller_id_control_address_json_key;
create unique index control_bindings_live_input
    on public.control_bindings (controller_id, control, address_json) where deleted_at is null;

alter table public.folder_tracks drop constraint if exists folder_tracks_folder_id_track_id_key;
create unique index folder_tracks_live_link
    on public.folder_tracks (folder_id, track_id) where deleted_at is null;

alter table public.venue_members drop constraint if exists venue_members_venue_id_uid_key;
create unique index venue_members_live_member
    on public.venue_members (venue_id, uid) where deleted_at is null;

alter table public.venue_node_params drop constraint if exists venue_node_params_node_id_key_key;
create unique index venue_node_params_live_key
    on public.venue_node_params (node_id, key) where deleted_at is null;

alter table public.venue_constraints
    drop constraint if exists venue_constraints_node_id_my_socket_key;
create unique index venue_constraints_live_socket
    on public.venue_constraints (node_id, my_socket) where deleted_at is null;

drop index if exists public.venue_nodes_root_idx;
create unique index venue_nodes_root_idx
    on public.venue_nodes (venue_id) where kind = 'venue' and deleted_at is null;

drop index if exists public.venues_share_code_key;
create unique index venues_share_code_key
    on public.venues (share_code) where share_code is not null and deleted_at is null;

-- A track has many grids now; the current one is its newest live row.
alter table public.track_beats drop constraint if exists track_beats_track_id_key;
create index track_beats_track_idx on public.track_beats (track_id, created_at desc);

alter table public.track_beat_validations
    drop constraint if exists track_beat_validations_track_id_key;
create unique index track_beat_validations_live_track
    on public.track_beat_validations (track_id) where deleted_at is null;

alter table public.agent_thread_transcript_heads
    drop constraint if exists agent_thread_transcript_heads_thread_id_key;
create unique index agent_thread_transcript_heads_live_thread
    on public.agent_thread_transcript_heads (thread_id) where deleted_at is null;

create unique index laser_limits_live_fixture
    on public.laser_limits (fixture_id) where deleted_at is null;

-- A venue's Perform layout was keyed by its venue's id.
create unique index perform_layouts_live_venue
    on public.perform_layouts (venue_id) where deleted_at is null;

create unique index clips_live_label
    on public.clips (score_id, label) where label is not null and deleted_at is null;

-- ---------------------------------------------------------------------------
-- What a flag does.
-- ---------------------------------------------------------------------------

-- Who flagged it, and what a write to a flagged row means.
--
-- Outside a request (a migration, the SQL editor) nothing is refused: a
-- migration runs over every row, flagged ones included.
create or replace function public.keep_flag() returns trigger
language plpgsql
set search_path = ''
as $$
begin
    if new.deleted_at is null then
        new.deleted_by := null;
        return new;
    end if;
    if old.deleted_at is null then
        new.deleted_by := coalesce(new.deleted_by, auth.uid());
        return new;
    end if;
    if nullif(current_setting('request.headers', true), '') is null then
        return new;
    end if;
    -- Still flagged. An edit is to a row nobody can see: it changes nothing,
    -- and the client reads "no row changed". A second delete is the first.
    if new.deleted_at is not distinct from old.deleted_at then
        return null;
    end if;
    new.deleted_at := old.deleted_at;
    new.deleted_by := old.deleted_by;
    return new;
end
$$;

-- A flagged row takes what hangs off it: source children are flagged with
-- the same stamp, derived and scratch ones are deleted. Each flagged child
-- fires this again for its own.
create or replace function public.flag_children() returns trigger
language plpgsql
security definer
set search_path = ''
as $$
declare
    child record;
begin
    if old.deleted_at is not null or new.deleted_at is null then
        return null;
    end if;
    for child in
        select link.conrelid::regclass as relation, held.attname as reference,
               exists (
                   select 1 from pg_catalog.pg_attribute flag
                   where flag.attrelid = link.conrelid and flag.attname = 'deleted_at'
                     and not flag.attisdropped) as source
        from pg_catalog.pg_constraint link
        join pg_catalog.pg_attribute id
          on id.attrelid = link.confrelid and id.attname = 'id'
        join lateral unnest(link.conkey, link.confkey) as pair (child_column, parent_column)
          on pair.parent_column = id.attnum
        join pg_catalog.pg_attribute held
          on held.attrelid = link.conrelid and held.attnum = pair.child_column
        where link.contype = 'f' and link.confrelid = tg_relid
    loop
        if child.source then
            execute format(
                'update %s set deleted_at = $1, deleted_by = $2
                 where %I = $3 and deleted_at is null',
                child.relation, child.reference)
            using new.deleted_at, new.deleted_by, new.id;
        else
            execute format('delete from %s where %I = $1', child.relation, child.reference)
            using new.id;
        end if;
    end loop;
    return null;
end
$$;
revoke all on function public.flag_children() from public, anon, authenticated;

-- A beat grid is written once.
create or replace function public.keep_written() returns trigger
language plpgsql
set search_path = ''
as $$
begin
    if nullif(current_setting('request.headers', true), '') is not null
       and to_jsonb(new) - array['deleted_at', 'deleted_by', 'updated_at']
           is distinct from to_jsonb(old) - array['deleted_at', 'deleted_by', 'updated_at']
    then
        raise exception '% rows are never edited; write a new one', tg_table_name
            using errcode = '42501';
    end if;
    return new;
end
$$;

do $$
declare
    name text;
begin
    foreach name in array array[
        'venues', 'venue_members', 'fixtures', 'fixture_groups', 'fixture_group_members',
        'venue_nodes', 'venue_node_params', 'venue_constraints', 'folders', 'folder_tracks',
        'laser_zones', 'laser_limits', 'perform_patterns', 'perform_layouts',
        'perform_controllers', 'control_bindings', 'tracks', 'track_beats',
        'track_beat_validations', 'scores', 'clips', 'agent_threads',
        'agent_thread_messages', 'agent_thread_transcript_heads'
    ] loop
        execute format(
            'create trigger %I before update on public.%I
             for each row execute function public.keep_flag()',
            name || '_keep_flag', name);
        execute format(
            'create trigger %I after update of deleted_at on public.%I
             for each row execute function public.flag_children()',
            name || '_flag_children', name);
    end loop;
end
$$;

create trigger track_beats_keep_written before update on public.track_beats
    for each row execute function public.keep_written();

-- ---------------------------------------------------------------------------
-- Who may reach a row. A policy on a table never asks whether its own row is
-- live: flagging a row and bringing it back are both writes to it. It asks
-- whether the row's *parents* are, through these.
-- ---------------------------------------------------------------------------

create or replace function public.is_venue_member(venue_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.venue_members_live m
        where m.venue_id = is_venue_member.venue_id and m.uid = auth.uid()
    );
$$;

create or replace function public.can_access_venue(venue_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.venues_live v
        where v.id = venue_id
          and (v.uid = auth.uid() or public.is_venue_member(v.id))
    );
$$;

create or replace function public.owns_venue(venue_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.venues_live v where v.id = venue_id and v.uid = auth.uid()
    );
$$;

create or replace function public.can_access_score(score_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.scores_live s
        where s.id = score_id
          and (s.uid = auth.uid() or public.can_access_venue(s.venue_id))
    );
$$;

create or replace function public.can_access_fixture_group(group_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select public.can_access_venue(
        (select g.venue_id from public.fixture_groups_live g where g.id = group_id));
$$;

create or replace function public.can_access_venue_node(node_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select public.can_access_venue(
        (select n.venue_id from public.venue_nodes_live n where n.id = node_id));
$$;

create or replace function public.track_is_shared(track_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.scores_live s
        where s.track_id = track_is_shared.track_id and public.can_access_venue(s.venue_id)
    );
$$;

create or replace function public.can_read_venue_track(
    track_id text,
    metadata_owner uuid,
    include_members boolean default true
)
returns boolean
language sql
stable security definer
set search_path to ''
as $function$
    select auth.uid() is not null and exists (
        select 1 from public.tracks_live track
        join public.scores_live score on score.track_id = track.id and score.uid = track.uid
        join public.venues_live venue on venue.id = score.venue_id
        where track.id = $1 and track.uid = $2
          and (venue.uid = auth.uid() or ($3 and exists (
              select 1 from public.venue_members_live member
              where member.venue_id = venue.id and member.uid = auth.uid())))
    );
$function$;

create or replace function public.can_read_track_media(bucket text, object_name text)
returns boolean
language sql
stable security definer
set search_path to ''
as $function$
    select auth.uid() is not null and bucket in ('track-audio', 'track-stems') and (
        split_part(object_name, '/', 1) = auth.uid()::text
        or exists (
            select 1 from public.tracks_live track
            where split_part(object_name, '/', 1) = track.uid::text
              and public.can_read_venue_track(track.id, track.uid)
              and ((bucket = 'track-audio'
                    and bucket || '/' || object_name
                        in (track.storage_path, track.album_art_storage_path))
                or (bucket = 'track-stems' and exists (
                    select 1 from public.track_stems stem
                    where stem.track_id = track.id and stem.uid = track.uid
                      and stem.storage_path = bucket || '/' || object_name)))
        )
    );
$function$;

-- Joining again after leaving is a new membership, under a new id.
create or replace function public.join_venue(code text)
returns text
language plpgsql
security definer
set search_path = public, pg_temp
as $$
declare
    target text;
    member uuid := auth.uid();
begin
    if member is null then
        raise exception 'not authenticated';
    end if;
    select v.id into target from public.venues_live v where v.share_code = code;
    if target is null then
        raise exception 'no venue for share code';
    end if;
    if exists (select 1 from public.venues_live v where v.id = target and v.uid = member)
       or public.is_venue_member(target) then
        return target;
    end if;
    insert into public.venue_members (id, uid, venue_id, role)
    values (gen_random_uuid()::text, member, target, 'member');
    return target;
end;
$$;

-- ---------------------------------------------------------------------------
-- History.
-- ---------------------------------------------------------------------------

alter table public.history rename column id to seq;
alter table public.history
    add column actor_type text,
    add column thread_id text,
    add column message_id text,
    add column change_group_id text,
    add column scope_id text,
    add column schema_version integer;

update public.history set
    schema_version = 1,
    actor_type = case
        when actor like 'user:%' then 'person'
        when actor like 'migration:%' then 'migration'
        when actor like 'app:%' then 'app'
        when actor in ('postgres', 'supabase_admin', 'service_role') then 'admin'
        else 'agent'
    end,
    scope_id = case
        when table_name in ('venues', 'scores') then row_id
        when table_name = 'clips' then coalesce(after, before) ->> 'score_id'
        else coalesce(after, before) ->> 'venue_id'
    end;

alter table public.history
    alter column actor_type set not null,
    alter column schema_version set not null,
    add constraint history_actor_type_check
        check (actor_type in ('person', 'agent', 'migration', 'app', 'admin'));

create index history_scope_idx on public.history (scope_id, seq);
revoke insert, update, delete, truncate on public.history from public, anon, authenticated;

-- One entry per change to a source row. The scope is the score or venue (or
-- track) the row belongs to, read off the column the trigger names.
--
-- A version is a `seq`, so within one scope `seq` has to be commit order, and
-- a sequence alone is not: two writers can commit in the other order. The
-- lock makes writers to one scope take turns until they commit.
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

    insert into public.history (
        uid, table_name, row_id, op, before, after, actor, actor_type,
        thread_id, message_id, change_group_id, scope_id, schema_version)
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
        public.data_format()
    );
    return null;
end;
$$;
revoke all on function public.record_history() from public, anon, authenticated;

do $$
declare
    listened text[];
begin
    foreach listened slice 1 in array array[
        ['venues', 'id'], ['venue_members', 'venue_id'], ['fixtures', 'venue_id'],
        ['fixture_groups', 'venue_id'], ['fixture_group_members', 'venue_id'],
        ['venue_nodes', 'venue_id'], ['venue_node_params', 'venue_id'],
        ['venue_constraints', 'venue_id'], ['folders', 'venue_id'],
        ['folder_tracks', 'venue_id'], ['laser_zones', 'venue_id'],
        ['laser_limits', 'venue_id'], ['perform_patterns', 'venue_id'],
        ['perform_layouts', 'venue_id'], ['perform_controllers', 'venue_id'],
        ['control_bindings', 'venue_id'], ['tracks', 'id'], ['track_beats', 'track_id'],
        ['track_beat_validations', 'track_id'], ['scores', 'id'], ['clips', 'score_id']
    ] loop
        execute format(
            'create trigger %I after insert or update or delete on public.%I
             for each row execute function public.record_history(%L)',
            listened[1] || '_history', listened[1], listened[2]);
    end loop;
end
$$;

-- A row as format 1 wrote it, as format 2 reads it: its id and what it
-- pointed at are what they were reissued as, and it carries no flag.
create or replace function public.row_from_format_1(table_name text, old_row jsonb)
returns jsonb
language sql
stable
set search_path = ''
as $$
    select old_row
        || jsonb_build_object('deleted_at', null, 'deleted_by', null)
        || coalesce((
            select jsonb_object_agg(pointer.key, was.new_id)
            from (values
                ('id', table_name),
                ('parent_id', 'venue_nodes'),
                ('node_id', 'venue_nodes'),
                ('target_node', 'venue_nodes'),
                ('folder_id', 'folders')
            ) as pointer (key, target)
            join public.legacy_ids was
              on was.table_name = pointer.target and was.old_id = old_row ->> pointer.key
        ), '{}'::jsonb);
$$;
revoke all on function public.row_from_format_1(text, jsonb) from public, anon, authenticated;

-- Left over from the schema before the row model; nothing calls them.
drop function if exists public.is_group_owner(uuid);
drop function if exists public.is_venue_owner(uuid);
drop function if exists public.cascade_soft_delete_track_children();
drop function if exists public.search_patterns(text, text, integer, integer);

notify pgrst, 'reload schema';

commit;
