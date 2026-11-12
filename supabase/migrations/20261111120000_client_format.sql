-- A build too old for the data is refused, and can tell.
--
-- A client keeps running for weeks after a newer release ships. When a
-- migration changes what synced rows mean, a build from before it would
-- read the new rows wrongly and write rows in the old form over them. Until
-- now the only protection was to never make such a change in one release.
--
-- The data has a format: a number, `public.data_format()`. A migration that
-- an older build cannot follow replaces the function with the next number,
-- in the same file as the change. A client says the format it reads in the
-- `x-luma-format` header of every PostgREST request, and asks for the
-- server's before it opens its sync stream.
--
-- A write from a client behind the server's format is refused with HTTP 426
-- (Upgrade Required), whatever table it touches and whether it comes as a
-- row or through a function. A request with no header is from a build
-- before the header, all of which read format 1. A statement outside a
-- request (a migration, the SQL editor, replication) is nobody's client and
-- passes.
--
-- The format is 1 now, so this refuses nothing yet.

create or replace function public.data_format() returns integer
language sql
stable
set search_path = ''
as $$ select 1 $$;

grant execute on function public.data_format() to authenticated;

create or replace function public.require_current_client() returns trigger
language plpgsql
set search_path = ''
as $$
declare
    headers text := nullif(current_setting('request.headers', true), '');
    client integer;
begin
    if headers is null then
        return null;
    end if;
    client := coalesce(nullif(headers::json ->> 'x-luma-format', '')::integer, 1);
    if client < public.data_format() then
        -- PostgREST answers a `PTxyz` state with the HTTP status xyz.
        raise sqlstate 'PT426' using
            message = 'update required',
            detail = format(
                'this build reads data format %s and the server holds format %s',
                client, public.data_format()),
            hint = 'Update Luma.';
    end if;
    return null;
end;
$$;

-- Every synced table: what the `powersync` publication replicates. A table
-- added to the publication later gets the same trigger in its own migration.
do $$
declare
    name text;
begin
    for name in
        select tablename from pg_publication_tables
        where pubname = 'powersync' and schemaname = 'public'
    loop
        execute format('drop trigger if exists %I on public.%I', name || '_client_format', name);
        execute format(
            'create trigger %I before insert or update or delete on public.%I
             for each statement execute function public.require_current_client()',
            name || '_client_format', name);
    end loop;
end
$$;

notify pgrst, 'reload schema';
