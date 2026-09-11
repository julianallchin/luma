# Disposable PowerSync stack

`run.py` brings up the three services a Luma client talks to, on loopback
ports, in containers named `luma-rows-*`:

| container | image | port | role |
|---|---|---|---|
| `luma-rows-postgres` | `postgres:16` (`wal_level=logical`) | 55450 | the database |
| `luma-rows-postgrest` | `postgrest/postgrest` | 55451 | uploads, as Supabase PostgREST |
| `luma-rows-powersync` | `journeyapps/powersync-service` | 55452 | downloads |

Postgres gets a minimal stand-in for the Supabase auth schema — the `anon`,
`authenticated` and `authenticator` roles, `auth.users` and an `auth.uid()`
that reads `request.jwt.claims` — and then
`supabase/migrations/20260912000000_row_model.sql`, and nothing else. The
older migrations assume the deployed baseline that the row model replaces;
the row model migration drops that baseline itself, so it is the only one a
disposable database needs.

PowerSync reads `service.yaml` here and `deploy/sync-rules.yaml` from the repo,
so the sync rules under test are the ones that ship.

```
python3 experiments/powersync/run.py                    # start, check, tear down
python3 experiments/powersync/run.py --keep             # leave the stack up
python3 experiments/powersync/run.py --sql              # psql against it
python3 experiments/powersync/run.py --test <name> ...  # also run Rust tests
```

`--test` runs `cargo +1.97.1 test --manifest-path backend/Cargo.toml -p luma
--lib <name> -- --ignored --nocapture` against the live stack, with

    LUMA_TEST_POWERSYNC_URL  LUMA_TEST_POSTGREST_URL
    LUMA_TEST_JWT_SECRET     LUMA_TEST_PG_URI

in the environment. `mint_jwt(user_id)` mints the matching HS256 token; the
same secret is in `service.yaml` as an inline JWKS key, so one token is
accepted by both PostgREST and PowerSync.

## What the built-in checks prove

Three users are minted. The owner creates a venue with a share code; the
joiner calls `join_venue(code)` and gets in; the stranger gets nothing.

- the owner's venue, track and score are private until shared
- the joiner writes a clip into the owner's score, and the owner reads it back
- the joiner reads the track behind the shared score; the stranger cannot
- the stranger cannot write into the shared score, and a wrong code does not join
- a verified pattern is readable by everyone and writable only by its author
- every venue-child table takes a row and replicates without a sync-rule error
- the joiner's sync stream delivers the whole shared venue and withholds the
  owner's threads, drafts and change log

The runner refuses to start if a `luma-rows-*` container already exists; it
never removes a container it did not create.
