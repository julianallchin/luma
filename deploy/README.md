# Deploying sync

See [docs/design/sync.md](../docs/design/sync.md).

## Supabase

Apply `../supabase/migrations/20260912000000_row_model.sql` and
`../supabase/migrations/20260916000000_stage_child_venue_id.sql` (`supabase db
push`, or paste them into the SQL editor). The first drops the old sync schema
first, so it also runs on a project that has been reset.

It creates `powersync_role` with a placeholder password. Set a real one and
keep it for the next step:

```sql
alter role powersync_role with password '<generated>';
```

## PowerSync Cloud

1. Create an instance in the same region as the Supabase project.
2. Connect it to the Supabase Postgres with `powersync_role`, its password and
   the `powersync` publication.
3. Client Auth: enable **Use Supabase Auth**.
4. Paste `sync-rules.yaml` into the instance's sync rules and deploy.
5. Put the instance URL in `backend/src/config.rs` as `POWERSYNC_URL`.

`experiments/powersync/run.py` checks a change to either file against
disposable containers.
