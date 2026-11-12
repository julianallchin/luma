-- Accepting the terms: one row per account, saying which version it accepted
-- and when.
--
-- The app shows the terms after a sign-in and asks here whether the account
-- has accepted them. The row is reached only through the two functions below,
-- so the server stamps the time and a client can neither write nor backdate
-- one. Accepting again keeps the first row; a newer version replaces it.
--
-- Server only: nothing here syncs, so there is no local migration and no
-- sync rule.

create table public.terms_acceptances (
    uid uuid primary key references auth.users (id) on delete cascade,
    version integer not null,
    accepted_at timestamptz not null default now()
);

alter table public.terms_acceptances enable row level security;
revoke all on public.terms_acceptances from anon, public, authenticated;

-- The version the caller has accepted, or null when it has accepted none.
create function public.terms_accepted () returns integer
language sql
stable
security definer
set search_path = ''
as $$
    select held.version from public.terms_acceptances held where held.uid = auth.uid();
$$;

-- Record that the caller accepts `accepted_version`, and return the version
-- it now holds.
create function public.accept_terms (accepted_version integer) returns integer
language sql
volatile
security definer
set search_path = ''
as $$
    insert into public.terms_acceptances as held (uid, version)
    values (auth.uid(), accepted_version)
    on conflict (uid) do update
        set version = excluded.version, accepted_at = now()
        where held.version < excluded.version;
    select held.version from public.terms_acceptances held where held.uid = auth.uid();
$$;

revoke all on function public.terms_accepted (), public.accept_terms (integer)
    from public, anon;
grant execute on function public.terms_accepted (), public.accept_terms (integer)
    to authenticated;
