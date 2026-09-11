# Deploying sync

Luma syncs rows. PowerSync moves them between the local SQLite database and
Supabase Postgres; there is no write service to deploy. See
[docs/design/sync.md](../docs/design/sync.md).

Two things live here:

- `sync-rules.yaml` — the PowerSync sync rules, pasted into the instance.
- `../supabase/migrations/20260912000000_row_model.sql` — the whole remote
  schema: every synced table, its row-level security and the `powersync`
  publication.

## Supabase

Apply the migration (`supabase db push`, or paste it into the SQL editor). It
is self-sufficient: it drops the old sync schema first, so it also runs on a
project that has been reset.

It creates `powersync_role` with a placeholder password. Set a real one and
keep it for the next step:

```sql
alter role powersync_role with password '<generated>';
```

The role already has `replication`, `bypassrls` and `select`, and the
`powersync` publication already lists every synced table
(https://docs.powersync.com/installation/database-setup).

## PowerSync Cloud

1. Create an instance in the same region as the Supabase project.
2. Connect it to the Supabase Postgres with `powersync_role`, its password and
   the `powersync` publication.
3. Client Auth: enable **Use Supabase Auth**.
4. Paste `sync-rules.yaml` into the instance's sync rules and deploy.
5. Put the instance URL in `backend/src/config.rs` as `POWERSYNC_URL`.

Uploads go straight to Supabase PostgREST with the user's access token, so the
policies in the migration are the only thing deciding who may write what.

## Checking a change

`experiments/powersync/run.py` brings up disposable Postgres, PostgREST and
PowerSync containers, applies the migration to an empty database and asserts
the access rules and the sync rules against three minted users. Run it after
touching either file.
