-- An account's rows outlive the account.
--
-- Venues are shared: a member's fixtures, groups and scores live in someone
-- else's venue, under the member's `uid`. Every one of those `uid`s was
-- `references auth.users (id) on delete cascade`, so deleting the member's
-- auth user would have deleted their work out of everyone's venue, and
-- deleting an owner would have deleted the venue from under its members.
--
-- Authored rows keep their author. The foreign keys from every table anyone
-- but the author can read drop `on delete cascade` and become `no action`:
-- deleting an auth user who authored anything is refused, so an admin has to
-- decide what happens to those rows first. `uid` stays `not null` and keeps
-- naming the author, so no policy, `can_access_*` helper, history trigger or
-- sync rule that reads it changes. What still cascades is what only the
-- account itself can see: its venue memberships, agent conversations and
-- drafts.
--
-- `history`'s comment and `record_history` still mention a cascade from an
-- account deletion; there no longer is one.
--
-- Server only: the local database has no `auth.users`.
--
-- Idempotent.

begin;

do $$
declare
    fk record;
    -- Rows nobody but their own account can read. Deleting the account may
    -- take these with it.
    private text[] := array[
        'venue_members', 'agent_threads', 'agent_thread_messages',
        'agent_thread_transcript_heads', 'drafts'
    ];
begin
    for fk in
        select c.conname, c.conrelid::regclass as owner, pg_get_constraintdef(c.oid) as definition
        from pg_constraint c
        join pg_class t on t.oid = c.conrelid
        join pg_namespace n on n.oid = t.relnamespace
        where c.contype = 'f'
          and c.confrelid = 'auth.users'::regclass
          and c.confdeltype = 'c'
          and n.nspname = 'public'
          and t.relname <> all (private)
    loop
        execute format('alter table %s drop constraint %I', fk.owner, fk.conname);
        execute format('alter table %s add constraint %I %s', fk.owner, fk.conname,
                       replace(fk.definition, ' ON DELETE CASCADE', ''));
    end loop;

    if exists (
        select 1
        from pg_constraint c
        join pg_class t on t.oid = c.conrelid
        join pg_namespace n on n.oid = t.relnamespace
        where c.contype = 'f'
          and c.confrelid = 'auth.users'::regclass
          and c.confdeltype = 'c'
          and n.nspname = 'public'
          and t.relname <> all (private)
    ) then
        raise exception 'a shared table still cascades from auth.users';
    end if;
end
$$;

commit;
