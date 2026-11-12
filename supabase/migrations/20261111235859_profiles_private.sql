-- A profile is read by its own account and nobody else.
--
-- `public.profiles` predates the row model and is not created by this folder:
-- it holds one row per account, with the display name the account chose. Its
-- read policy was `using (true)` for every role, signed out included, so the
-- key the app ships with was enough to list every account id and name.
--
-- Nothing in the app reads another account's profile. The sign-up trigger
-- that inserts the row is `security definer` and is unaffected.
--
-- Server only. Idempotent, and a no-op on a project that never had the table.

do $$
begin
    if to_regclass('public.profiles') is null then
        return;
    end if;

    drop policy if exists "Anyone can read profiles" on public.profiles;
    drop policy if exists profiles_select_own on public.profiles;
    create policy profiles_select_own on public.profiles
        for select to authenticated
        using (id = auth.uid());

    revoke all on public.profiles from anon;
    revoke all on public.profiles from authenticated;
    grant select, insert, update on public.profiles to authenticated;
end
$$;
