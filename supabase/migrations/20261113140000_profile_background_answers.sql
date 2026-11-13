-- `set_profile_background` answers its caller.
--
-- It returned nothing, which PostgREST sends as an empty body, and the app
-- reads every function's reply as JSON: 0.9.0 recorded an account's answers
-- and then told it they could not be recorded. It returns `true` now, which
-- that build reads as success.
--
-- A function's return type cannot be replaced in place, so it is dropped and
-- made again. Server only. Idempotent: production had this run by hand
-- the day 0.9.0 shipped.

begin;

drop function if exists public.set_profile_background (text[], text, text);

create function public.set_profile_background (roles text[], role_other text, experience text)
returns boolean
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
    return true;
end
$$;

revoke all on function public.set_profile_background (text[], text, text) from public, anon;
grant execute on function public.set_profile_background (text[], text, text) to authenticated;

notify pgrst, 'reload schema';

commit;
