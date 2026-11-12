-- A score is written by the account that owns it.
--
-- Everything else in a venue is every member's to edit. A score is one
-- person's work: whoever can open its venue reads it, and copies it into a
-- score of their own to change it. Until this file the write policies let any
-- member through (`can_access_venue` on `scores`, `can_access_score` on
-- `clips`), so only the app held the rule.
--
-- Reading does not change: the select policies and `can_access_score` are as
-- they were. A clip is written by whoever owns its score, whatever `uid` the
-- clip row carries.
--
-- Policies and one helper only; the local schema does not change. Any time
-- before shipping the client that opens other people's scores read-only: an
-- older client's edit of someone else's score then uploads as a PATCH the
-- update policy filters to no rows, which PostgREST answers with success, and
-- the next download puts the owner's value back; a clip it adds there is
-- refused and shows as a sync rejection.
--
-- Idempotent.

begin;

create or replace function public.owns_score(score_id text)
returns boolean
language sql
stable security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.scores_live s
        where s.id = score_id and s.uid = auth.uid()
    );
$$;

revoke all on function public.owns_score(text) from public, anon;
grant execute on function public.owns_score(text) to authenticated;

drop policy if exists scores_insert on public.scores;
create policy scores_insert on public.scores
    for insert to authenticated
    with check (uid = auth.uid());

drop policy if exists scores_update on public.scores;
create policy scores_update on public.scores
    for update to authenticated
    using (uid = auth.uid())
    with check (uid = auth.uid());

drop policy if exists scores_delete on public.scores;
create policy scores_delete on public.scores
    for delete to authenticated
    using (uid = auth.uid());

drop policy if exists clips_insert on public.clips;
create policy clips_insert on public.clips
    for insert to authenticated
    with check (public.owns_score(score_id));

drop policy if exists clips_update on public.clips;
create policy clips_update on public.clips
    for update to authenticated
    using (public.owns_score(score_id))
    with check (public.owns_score(score_id));

drop policy if exists clips_delete on public.clips;
create policy clips_delete on public.clips
    for delete to authenticated
    using (public.owns_score(score_id));

commit;

notify pgrst, 'reload schema';
