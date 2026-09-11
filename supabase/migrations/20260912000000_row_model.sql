-- Row model: PowerSync moves rows between local SQLite and this schema.
--
-- There is no write service, no revision blobs, no projection layer. Every
-- synced table here mirrors the SQLite table of the same name, minus the
-- local-only columns listed in docs/design/sync.md and minus the retired
-- `version` / `synced_at` / `origin` sync bookkeeping.
--
-- This file is self-sufficient: it drops every object the old sync created
-- (the deployed project may be reset from nothing) and then creates the whole
-- row model. It is written to run on a database that is empty as well as on
-- one that still carries the old schema.
--
-- Type mapping from SQLite:
--   TEXT                        -> text
--   REAL                        -> double precision
--   INTEGER                     -> bigint
--   INTEGER used as 0/1 flag    -> boolean
--     (venues.groups_initialized, fixtures.address_pinned,
--      patterns.is_verified, midi_bindings.exclusive)
--     The client must upload JSON `true`/`false` for these four columns;
--     PostgREST will not coerce a JSON `0` into a boolean.
--   TEXT timestamps             -> timestamptz (created_at / updated_at)
--   *_json / *_document columns stay `text`, never `jsonb`, so the client's
--     serialized JSON round-trips byte-for-byte through PowerSync.
--
-- Composite-key SQLite tables get a real `id text primary key` that the client
-- supplies, because PowerSync addresses every row by a single `id`:
--   venue_edges.id        = child_id
--   venue_node_params.id  = node_id || ':' || key
--   venue_constraints.id  = node_id || ':' || my_socket
--   track_stems.id        = track_id || ':' || stem_name
--   track_* analysis.id   = track_id
--   agent_thread_transcript_heads.id = thread_id
--
-- Foreign keys between synced tables are `deferrable initially deferred` and
-- `on delete cascade` so a PowerSync upload transaction may present rows in
-- any order.

begin;

-- ---------------------------------------------------------------------------
-- Drop the old sync
-- ---------------------------------------------------------------------------

drop publication if exists powersync;
drop publication if exists luma_authored;

-- Old authored-revision / write-service entry points.
drop function if exists public.submit_authored_head_proposal(jsonb) cascade;
drop function if exists public.submit_authored_head_proposal cascade;
drop function if exists public.integrate_authored_head_proposal cascade;
drop function if exists public.archive_authored_document cascade;
drop function if exists public.check_pattern_score_scope cascade;
drop function if exists public.claim_agent_thread_run cascade;
drop function if exists public.release_agent_thread_run cascade;
drop function if exists public.join_venue_by_code cascade;
drop function if exists public.leave_venue cascade;
drop function if exists public.remove_venue_member cascade;
drop function if exists public.can_access_venue cascade;
drop function if exists public.can_access_venue_node cascade;
drop function if exists public.can_access_fixture_group cascade;
drop function if exists public.can_access_score cascade;
drop function if exists public.can_read_pattern cascade;
drop function if exists public.can_read_track cascade;
drop function if exists public.track_is_shared cascade;
drop function if exists public.is_venue_member cascade;
drop function if exists public.owns_venue cascade;
drop function if exists public.join_venue cascade;

-- Everything the old sync kept out of sight.
drop schema if exists private cascade;

-- Old synced tables. `cascade` removes their policies, triggers and indexes.
drop table if exists public.authored_head_integrations cascade;
drop table if exists public.authored_head_proposals cascade;
drop table if exists public.authored_operation_outcomes cascade;
drop table if exists public.authored_document_archives cascade;
drop table if exists public.authored_document_heads cascade;
drop table if exists public.authored_revision_files cascade;
drop table if exists public.authored_revision_parents cascade;
drop table if exists public.authored_revisions cascade;
drop table if exists public.authored_documents cascade;
drop table if exists public.authored_subagent_workspaces cascade;
drop table if exists public.authored_turn_outcomes cascade;
drop table if exists public.authored_turn_preparations cascade;
drop table if exists public.authored_device_identity cascade;
drop table if exists public.agent_thread_message_appends cascade;
drop table if exists public.agent_thread_deletions cascade;
drop table if exists public.agent_thread_runs cascade;
drop table if exists public.agent_thread_usage cascade;
drop table if exists public.agent_thread_transcript_heads cascade;
drop table if exists public.agent_thread_messages cascade;
drop table if exists public.agent_threads cascade;
drop table if exists public.track_scores cascade;
drop table if exists public.track_beat_validations cascade;
drop table if exists public.track_genres cascade;
drop table if exists public.track_bar_classifications cascade;
drop table if exists public.track_drum_onsets cascade;
drop table if exists public.track_stems cascade;
drop table if exists public.track_roots cascade;
drop table if exists public.track_beats cascade;
drop table if exists public.track_waveforms cascade;
drop table if exists public.track_mert cascade;
drop table if exists public.midi_bindings cascade;
drop table if exists public.midi_modifiers cascade;
drop table if exists public.cues cascade;
drop table if exists public.implementations cascade;
drop table if exists public.pattern_categories cascade;
drop table if exists public.patterns cascade;
drop table if exists public.score_definitions cascade;
drop table if exists public.clips cascade;
drop table if exists public.drafts cascade;
drop table if exists public.changes cascade;
drop table if exists public.scores cascade;
drop table if exists public.venue_constraints cascade;
drop table if exists public.venue_node_params cascade;
drop table if exists public.venue_edges cascade;
drop table if exists public.venue_nodes cascade;
drop table if exists public.venue_implementation_overrides cascade;
drop table if exists public.fixture_group_overrides cascade;
drop table if exists public.fixture_group_members cascade;
drop table if exists public.fixture_groups cascade;
drop table if exists public.fixtures cascade;
drop table if exists public.stage_pieces cascade;
drop table if exists public.universe_outputs cascade;
drop table if exists public.venue_memberships cascade;
drop table if exists public.venue_members cascade;
drop table if exists public.venues cascade;
drop table if exists public.tracks cascade;
drop table if exists public.settings cascade;
drop table if exists public.sync_state cascade;
drop table if exists public.sync_tombstones cascade;
drop table if exists public.sync_push_failures cascade;
drop table if exists public.auth_write_admission cascade;

-- The old write service ran as its own role with its own policies.
do $$
begin
    if exists (select 1 from pg_roles where rolname = 'luma_sync_service') then
        execute 'drop owned by luma_sync_service cascade';
        execute 'drop role luma_sync_service';
    end if;
end
$$;

-- ---------------------------------------------------------------------------
-- Venues
-- ---------------------------------------------------------------------------

create table public.venues (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    name text not null,
    description text,
    share_code text,
    role text not null default 'owner',
    environment text not null default '{"mode":"indoor","houseLevel":1.0}',
    groups_initialized boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create unique index venues_share_code_key on public.venues (share_code) where share_code is not null;
create index venues_uid_idx on public.venues (uid);

create table public.venue_members (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    role text not null default 'member',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (venue_id, uid)
);

create index venue_members_uid_idx on public.venue_members (uid);

-- ---------------------------------------------------------------------------
-- Venue content
-- ---------------------------------------------------------------------------

create table public.fixtures (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    universe bigint not null default 1,
    address bigint not null,
    num_channels bigint not null,
    manufacturer text not null,
    model text not null,
    mode_name text not null,
    fixture_path text not null,
    label text,
    pos_x double precision default 0,
    pos_y double precision default 0,
    pos_z double precision default 0,
    rot_x double precision default 0,
    rot_y double precision default 0,
    rot_z double precision default 0,
    address_pinned boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index fixtures_venue_idx on public.fixtures (venue_id);

create table public.fixture_groups (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text,
    axis_lr double precision,
    axis_fb double precision,
    axis_ab double precision,
    movement_config text,
    display_order bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (venue_id, name)
);

create index fixture_groups_venue_idx on public.fixture_groups (venue_id);

create table public.fixture_group_members (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    fixture_id text not null references public.fixtures (id) on delete cascade deferrable initially deferred,
    group_id text not null references public.fixture_groups (id) on delete cascade deferrable initially deferred,
    head_index bigint not null default -1,
    display_order bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index fixture_group_members_group_idx on public.fixture_group_members (group_id);
create index fixture_group_members_fixture_idx on public.fixture_group_members (fixture_id);

create table public.venue_nodes (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    kind text not null,
    catalog_ref text,
    label text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index venue_nodes_venue_idx on public.venue_nodes (venue_id);
create unique index venue_nodes_root_idx on public.venue_nodes (venue_id) where kind = 'venue';

-- id = child_id: a node has at most one parent edge.
create table public.venue_edges (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    child_id text not null references public.venue_nodes (id) on delete cascade deferrable initially deferred,
    parent_id text not null references public.venue_nodes (id) on delete cascade deferrable initially deferred,
    my_socket text not null,
    their_socket text not null,
    roll double precision not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (child_id)
);

create index venue_edges_parent_idx on public.venue_edges (parent_id);

-- id = node_id || ':' || key
create table public.venue_node_params (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    node_id text not null references public.venue_nodes (id) on delete cascade deferrable initially deferred,
    key text not null,
    value double precision not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (node_id, key)
);

-- id = node_id || ':' || my_socket
create table public.venue_constraints (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    node_id text not null references public.venue_nodes (id) on delete cascade deferrable initially deferred,
    my_socket text not null,
    target_node text not null,
    target_socket text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (node_id, my_socket)
);

create index venue_constraints_target_idx on public.venue_constraints (target_node);

-- ---------------------------------------------------------------------------
-- Tracks and their analysis
-- ---------------------------------------------------------------------------

create table public.tracks (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_hash text not null,
    title text,
    artist text,
    album text,
    track_number bigint,
    disc_number bigint,
    duration_seconds double precision,
    storage_path text,
    album_art_mime text,
    album_art_storage_path text,
    source_type text,
    source_id text,
    source_filename text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index tracks_uid_idx on public.tracks (uid);
create index tracks_hash_idx on public.tracks (track_hash);

-- id = track_id
create table public.track_beats (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    beats_json text not null,
    downbeats_json text not null,
    bpm double precision,
    downbeat_offset double precision,
    beats_per_bar bigint,
    processor_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id)
);

-- id = track_id
create table public.track_roots (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    sections_json text not null,
    logits_storage_path text,
    processor_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id)
);

-- id = track_id || ':' || stem_name
create table public.track_stems (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    stem_name text not null,
    storage_path text,
    processor_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id, stem_name)
);

-- id = track_id
create table public.track_drum_onsets (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    onsets_json text not null,
    processor_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id)
);

-- id = track_id
create table public.track_bar_classifications (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    classifications_json text not null,
    tag_order_json text not null,
    processor_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id)
);

-- id = track_id
create table public.track_genres (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    genres_json text not null,
    labels_json text not null,
    processor_version bigint not null default 1,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id)
);

-- id = track_id
create table public.track_beat_validations (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    track_hash text not null,
    grid_json text not null,
    processor_version bigint not null,
    verdict text not null,
    reason text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (track_id)
);

-- ---------------------------------------------------------------------------
-- Scores
-- ---------------------------------------------------------------------------

create table public.scores (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    venue_id text references public.venues (id) on delete cascade deferrable initially deferred,
    name text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index scores_track_idx on public.scores (track_id);
create index scores_venue_idx on public.scores (venue_id);

-- `seed` and `selection_seed` are decimal u64 strings: they do not fit a
-- bigint on the SQLite side either, so they stay text on both ends.
create table public.clips (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    score_id text not null references public.scores (id) on delete cascade deferrable initially deferred,
    graph text not null,
    "start" double precision not null,
    duration double precision not null,
    seed text not null,
    selection_seed text,
    selection_json text not null,
    z_index bigint not null default 0,
    blend_mode text not null default 'replace',
    inputs_json text not null default '{}',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index clips_score_idx on public.clips (score_id);

create table public.score_definitions (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    score_id text not null references public.scores (id) on delete cascade deferrable initially deferred,
    definition_json text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index score_definitions_score_idx on public.score_definitions (score_id);

-- ---------------------------------------------------------------------------
-- Patterns, implementations and live MIDI cues
-- ---------------------------------------------------------------------------

create table public.patterns (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    name text not null,
    description text,
    category_id text,
    category_name text,
    is_verified boolean not null default false,
    author_name text,
    forked_from_id text,
    score_id text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index patterns_uid_idx on public.patterns (uid);
create index patterns_score_idx on public.patterns (score_id);
create index patterns_verified_idx on public.patterns (is_verified) where is_verified;

create table public.implementations (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    pattern_id text not null references public.patterns (id) on delete cascade deferrable initially deferred,
    name text,
    graph_json text not null default '{"nodes":[],"edges":[]}',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index implementations_pattern_idx on public.implementations (pattern_id);

create table public.cues (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text not null,
    pattern_id text not null,
    args_json text not null default '{}',
    z_index bigint not null default 1,
    blend_mode text not null default 'Replace',
    default_target_json text not null default '"All"',
    execution_mode_json text not null default '"Loop"',
    display_order bigint not null default 0,
    display_x bigint not null default 0,
    display_y bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index cues_venue_idx on public.cues (venue_id);

create table public.midi_modifiers (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text not null,
    input_json text not null,
    groups_json text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index midi_modifiers_venue_idx on public.midi_modifiers (venue_id);

create table public.midi_bindings (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    trigger_json text not null,
    required_modifiers_json text not null default '[]',
    exclusive boolean not null default false,
    mode_json text not null default '"Toggle"',
    action_json text not null,
    target_override_json text,
    display_order bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index midi_bindings_venue_idx on public.midi_bindings (venue_id);

-- ---------------------------------------------------------------------------
-- Conversations
-- ---------------------------------------------------------------------------

create table public.agent_threads (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    agent_kind text not null,
    subject_kind text,
    subject_id text,
    venue_id text,
    score_id text,
    title text,
    lifecycle_state text not null default 'active',
    implementation_id text,
    forked_from_thread_id text,
    forked_at_message_id text,
    actor text,
    parent_thread_id text,
    parent_call_id text,
    engine text not null default 'api',
    model text,
    provider text,
    effort text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index agent_threads_uid_updated_idx on public.agent_threads (uid, updated_at desc);
create index agent_threads_subject_idx on public.agent_threads (subject_kind, subject_id);
create index agent_threads_parent_idx on public.agent_threads (parent_thread_id);

create table public.agent_thread_messages (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    principal_key text not null,
    created_in_thread_id text not null,
    parent_message_id text,
    depth bigint not null,
    role text not null,
    parts_json text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index agent_thread_messages_uid_created_idx on public.agent_thread_messages (uid, created_at, id);
create index agent_thread_messages_thread_idx on public.agent_thread_messages (created_in_thread_id, created_at, id);
create index agent_thread_messages_parent_idx on public.agent_thread_messages (parent_message_id);

-- id = thread_id
create table public.agent_thread_transcript_heads (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    thread_id text not null references public.agent_threads (id) on delete cascade deferrable initially deferred,
    head_message_id text,
    message_count bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (thread_id)
);

create index agent_thread_transcript_heads_uid_idx on public.agent_thread_transcript_heads (uid, updated_at desc);

-- ---------------------------------------------------------------------------
-- Drafts and history
-- ---------------------------------------------------------------------------

create table public.drafts (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    score_id text not null references public.scores (id) on delete cascade deferrable initially deferred,
    thread_id text,
    base_json text not null,
    state_json text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index drafts_score_idx on public.drafts (score_id);
create index drafts_thread_idx on public.drafts (thread_id);

-- The local trigger set appends one row here per insert, update and delete on
-- a synced table. It is a log: no foreign keys, nothing cascades into it.
create table public.changes (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    table_name text not null,
    row_id text not null,
    op text not null,
    before_json text,
    after_json text,
    actor text,
    at timestamptz not null default now(),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index changes_uid_at_idx on public.changes (uid, at desc);
create index changes_row_idx on public.changes (table_name, row_id, at desc);

commit;

-- ---------------------------------------------------------------------------
-- Access helpers
--
-- These are `security definer` so a policy on venues may ask about venues
-- without recursing through the policy it is evaluating, and so a child table
-- may resolve its venue through a parent the caller cannot select directly.
-- ---------------------------------------------------------------------------

-- A policy on a table must never resolve access by re-reading that same
-- table: `insert ... returning` evaluates the select policy against a row the
-- function's snapshot cannot see yet, and the insert would be refused. So
-- membership and score-sharing get their own helpers that only read the
-- table on the far side of the relationship.
create function public.is_venue_member (venue_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.venue_members m
        where m.venue_id = is_venue_member.venue_id and m.uid = auth.uid()
    );
$$;

create function public.can_access_venue (venue_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (select 1 from public.venues v where v.id = venue_id and v.uid = auth.uid())
        or public.is_venue_member(venue_id);
$$;

create function public.owns_venue (venue_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (select 1 from public.venues v where v.id = venue_id and v.uid = auth.uid());
$$;

create function public.can_access_fixture_group (group_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select public.can_access_venue((select g.venue_id from public.fixture_groups g where g.id = group_id));
$$;

create function public.can_access_venue_node (node_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select public.can_access_venue((select n.venue_id from public.venue_nodes n where n.id = node_id));
$$;

create function public.can_access_score (score_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.scores s
        where s.id = score_id
          and (s.uid = auth.uid() or public.can_access_venue(s.venue_id))
    );
$$;

-- A track and its analysis are readable by the owner and by anyone who can
-- read a score that uses the track, so a venue member sees the beat grid
-- behind a shared score.
create function public.track_is_shared (track_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.scores s
        where s.track_id = track_is_shared.track_id and public.can_access_venue(s.venue_id)
    );
$$;

-- Verified patterns are the shared library; everyone may read them and their
-- implementations.
create function public.can_read_pattern (pattern_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.patterns p
        where p.id = pattern_id and (p.uid = auth.uid() or p.is_verified)
    );
$$;

-- Joining is the one operation that must see a row the caller cannot select:
-- the venue is found by its share code and the membership is written for the
-- caller only.
create function public.join_venue (code text) returns text
language plpgsql
volatile
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
    select v.id into target from public.venues v where v.share_code = code;
    if target is null then
        raise exception 'no venue for share code';
    end if;
    if exists (select 1 from public.venues v where v.id = target and v.uid = member) then
        return target;
    end if;
    insert into public.venue_members (id, uid, venue_id, role)
    values (target || ':' || member::text, member, target, 'member')
    on conflict (venue_id, uid) do nothing;
    return target;
end;
$$;

-- ---------------------------------------------------------------------------
-- Row-level security
--
-- Every table is readable and writable by `authenticated` only, through
-- `auth.uid()`. The policies come in five shapes:
--
--   owner        uid = auth.uid()
--   track        owner, plus read when a readable score uses the track
--   venue        the venue is owned by the caller or the caller is a member
--   venue child  the same, resolved through the parent row
--   library      owner, plus read when the pattern is verified
--
-- The `can_*` helpers above are `security definer`, so a child policy may look
-- at a parent the caller cannot select and a policy on `venues` does not
-- recurse into itself.
-- ---------------------------------------------------------------------------

begin;

do $$
declare
    entry record;
    tables text[] := array[
        'venues', 'venue_members', 'fixtures', 'fixture_groups', 'fixture_group_members',
        'venue_nodes', 'venue_edges', 'venue_node_params', 'venue_constraints',
        'tracks', 'track_beats', 'track_roots', 'track_stems', 'track_drum_onsets',
        'track_bar_classifications', 'track_genres', 'track_beat_validations',
        'scores', 'clips', 'score_definitions', 'patterns', 'implementations', 'cues',
        'midi_modifiers', 'midi_bindings', 'agent_threads', 'agent_thread_messages',
        'agent_thread_transcript_heads', 'drafts', 'changes'
    ];
    name text;
begin
    foreach name in array tables loop
        execute format('alter table public.%I enable row level security', name);
        execute format('revoke all on public.%I from anon, public', name);
        execute format('grant select, insert, update, delete on public.%I to authenticated', name);
    end loop;

    -- read expression, write expression
    for entry in
        select * from (values
            -- owner
            ('agent_threads', 'uid = auth.uid()', 'uid = auth.uid()'),
            ('agent_thread_messages', 'uid = auth.uid()', 'uid = auth.uid()'),
            ('agent_thread_transcript_heads', 'uid = auth.uid()', 'uid = auth.uid()'),
            ('drafts', 'uid = auth.uid()', 'uid = auth.uid()'),
            ('changes', 'uid = auth.uid()', 'uid = auth.uid()'),
            -- tracks and their analysis
            ('tracks', 'uid = auth.uid() or public.track_is_shared(id)', 'uid = auth.uid()'),
            ('track_beats', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            ('track_roots', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            ('track_stems', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            ('track_drum_onsets', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            ('track_bar_classifications', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            ('track_genres', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            ('track_beat_validations', 'uid = auth.uid() or public.track_is_shared(track_id)', 'uid = auth.uid()'),
            -- venues and their content
            ('venues', 'uid = auth.uid() or public.is_venue_member(id)', 'uid = auth.uid()'),
            ('fixtures', 'public.can_access_venue(venue_id)', 'public.can_access_venue(venue_id)'),
            ('fixture_groups', 'public.can_access_venue(venue_id)', 'public.can_access_venue(venue_id)'),
            ('venue_nodes', 'public.can_access_venue(venue_id)', 'public.can_access_venue(venue_id)'),
            ('cues', 'public.can_access_venue(venue_id)', 'public.can_access_venue(venue_id)'),
            ('midi_modifiers', 'public.can_access_venue(venue_id)', 'public.can_access_venue(venue_id)'),
            ('midi_bindings', 'public.can_access_venue(venue_id)', 'public.can_access_venue(venue_id)'),
            ('fixture_group_members', 'public.can_access_fixture_group(group_id)', 'public.can_access_fixture_group(group_id)'),
            ('venue_edges', 'public.can_access_venue_node(child_id)', 'public.can_access_venue_node(child_id)'),
            ('venue_node_params', 'public.can_access_venue_node(node_id)', 'public.can_access_venue_node(node_id)'),
            ('venue_constraints', 'public.can_access_venue_node(node_id)', 'public.can_access_venue_node(node_id)'),
            -- scores live in a venue but may be drafted without one
            ('scores', 'uid = auth.uid() or public.can_access_venue(venue_id)', 'uid = auth.uid() or public.can_access_venue(venue_id)'),
            ('clips', 'public.can_access_score(score_id)', 'public.can_access_score(score_id)'),
            ('score_definitions', 'public.can_access_score(score_id)', 'public.can_access_score(score_id)'),
            -- the shared pattern library
            ('patterns', 'uid = auth.uid() or is_verified', 'uid = auth.uid()'),
            ('implementations', 'uid = auth.uid() or public.can_read_pattern(pattern_id)', 'uid = auth.uid()')
        ) as t(name, readable, writable)
    loop
        execute format('create policy %I on public.%I for select to authenticated using (%s)',
                       entry.name || '_select', entry.name, entry.readable);
        execute format('create policy %I on public.%I for insert to authenticated with check (%s)',
                       entry.name || '_insert', entry.name, entry.writable);
        execute format('create policy %I on public.%I for update to authenticated using (%s) with check (%s)',
                       entry.name || '_update', entry.name, entry.writable, entry.writable);
        execute format('create policy %I on public.%I for delete to authenticated using (%s)',
                       entry.name || '_delete', entry.name, entry.writable);
    end loop;
end
$$;

-- Membership is the one table a member may not edit freely: the share code is
-- checked inside `join_venue`, which runs as the definer, and the row it
-- writes is always the caller's own. The insert policy exists so a PowerSync
-- upload may replay that same row idempotently.
create policy venue_members_select on public.venue_members
    for select to authenticated
    using (uid = auth.uid() or public.owns_venue(venue_id));

create policy venue_members_insert on public.venue_members
    for insert to authenticated
    with check (uid = auth.uid() and public.can_access_venue(venue_id));

create policy venue_members_delete on public.venue_members
    for delete to authenticated
    using (uid = auth.uid() or public.owns_venue(venue_id));

revoke all on function
    public.can_access_venue(text), public.owns_venue(text),
    public.can_access_fixture_group(text), public.can_access_venue_node(text),
    public.can_access_score(text), public.track_is_shared(text),
    public.is_venue_member(text),
    public.can_read_pattern(text), public.join_venue(text)
    from public, anon;

grant execute on function
    public.can_access_venue(text), public.owns_venue(text),
    public.can_access_fixture_group(text), public.can_access_venue_node(text),
    public.can_access_score(text), public.track_is_shared(text),
    public.is_venue_member(text),
    public.can_read_pattern(text), public.join_venue(text)
    to authenticated;

commit;

-- ---------------------------------------------------------------------------
-- Replication
--
-- https://docs.powersync.com/installation/database-setup
--
-- `powersync_role` is created without a usable password; the operator sets one
-- and hands it to the PowerSync instance:
--
--     alter role powersync_role with password '<generated>';
-- ---------------------------------------------------------------------------

begin;

drop publication if exists powersync;
create publication powersync for table
    public.venues, public.venue_members, public.fixtures, public.fixture_groups,
    public.fixture_group_members, public.venue_nodes, public.venue_edges,
    public.venue_node_params, public.venue_constraints, public.tracks,
    public.track_beats, public.track_roots, public.track_stems,
    public.track_drum_onsets, public.track_bar_classifications, public.track_genres,
    public.track_beat_validations, public.scores, public.clips,
    public.score_definitions, public.patterns, public.implementations, public.cues,
    public.midi_modifiers, public.midi_bindings, public.agent_threads,
    public.agent_thread_messages, public.agent_thread_transcript_heads,
    public.drafts, public.changes;

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'powersync_role') then
        create role powersync_role with replication bypassrls login password 'set-me-before-connecting';
    end if;
end
$$;

-- Replication must see every row, not the rows the policies would show a
-- signed-in user: PowerSync applies the sync rules itself.
alter role powersync_role with bypassrls;

grant usage on schema public to powersync_role;
grant select on all tables in schema public to powersync_role;
alter default privileges in schema public grant select on tables to powersync_role;

commit;
