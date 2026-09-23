# Deploying sync

See [docs/design/sync.md](../docs/design/sync.md).

## Supabase

Run `supabase db push`. Or paste each file in `../supabase/migrations/` from
`20260912000000_row_model.sql` on into the SQL editor, in filename order:

- `20260912000000_row_model.sql`
- `20260916000000_stage_child_venue_id.sql`
- `20260918000000_cued_patterns.sql`
- `20260920000000_track_media_read_row_model.sql`
- `20260921000000_venue_haze.sql`
- `20260923100000_drop_cues_and_pattern_library.sql` — after the clip-forms
  change set is applied and after the sync rules below no longer name
  `patterns`, `implementations` or `cues`

`row_model.sql` drops the old sync schema first, so it also runs on a project
that has been reset.

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
