# Deploying sync

See [docs/design/sync.md](../docs/design/sync.md).

## Supabase

Run `supabase db push`. Or paste each file in `../supabase/migrations/` from
`20260912000000_row_model.sql` on into the SQL editor, in filename order. The
list below is every one of them; a note says what must happen before or after.
A new migration adds its line here.

- `20260912000000_row_model.sql`
- `20260916000000_stage_child_venue_id.sql`
- `20260918000000_cued_patterns.sql`
- `20260920000000_track_media_read_row_model.sql`
- `20260921000000_venue_haze.sql`
- `20260923100000_drop_cues_and_pattern_library.sql` — after the clip-forms
  change set is applied and after the sync rules below no longer name
  `patterns`, `implementations` or `cues`
- `20260923200000_drop_score_definitions.sql`
- `20260924000000_server_history.sql` — after the sync rules below no longer
  name `changes`, and before a client that uploads `scores.authored_at`
- `20260924100000_drop_dead_midi_and_thread_columns.sql` — after every client
  runs a build that no longer uploads `agent_threads.implementation_id`,
  `midi_bindings.mode_json`, `midi_bindings.target_override_json` or
  `midi_modifiers.groups_json`
- `20260929000000_clip_graphs.sql`
- `20261001000000_folders.sql` — then redeploy `powersync/sync-config.yaml`, which
  names `folders` and `folder_tracks`
- `20261002000000_placement_in_venue_nodes.sql`
- `20261002100000_agent_figures.sql`
- `20261002100100_bar_classifications_compact.sql`
- `20261024000200_drop_venue_conversion.sql`
- `20261024120000_lasers.sql` — then redeploy `powersync/sync-config.yaml`, which
  names `laser_zones` and `laser_limits`
- `20261025120000_random_node.sql`
- `20261025130000_laser_elements.sql`
- `20261026120000_laser_figures.sql`
- `20261102120000_drop_midi_mappings.sql` — after redeploying
  `powersync/sync-config.yaml`, which no longer names `midi_modifiers` or
  `midi_bindings`, and after every client runs a build without them
- `20261104120000_perform.sql` — then redeploy
  `powersync/sync-config.yaml`, which names `perform_patterns` and
  `perform_layouts`
- `20261105120000_perform_pad_in_graph.sql` — after every client runs a
  build that no longer uploads `perform_patterns.pad_json`
- `20261107120000_perform_controllers.sql` — then redeploy
  `powersync/sync-config.yaml`, which names `perform_controllers` and
  `control_bindings`; before shipping the client that uploads them
- `20261107130000_random_time.sql` — before shipping the client that reads
  `random.time`: it cannot read a random with an `every`, and a client from
  before it cannot read the rewritten graphs
- `20261108120000_track_media_read_bucket_path.sql` — any time; until it
  runs, venue members cannot download a shared track's audio or stems
- `20261108130000_venue_member_edits.sql` — any time, before shipping the
  client that lets members edit a shared venue: until it runs, a member's
  change to the venue row (environment, haze) uploads to no rows and is
  undone by the next download
- `20261108140000_keep_authored_rows.sql` — any time; no client depends on
  it. From then on deleting an auth user who authored anything another
  account can read is refused instead of cascading through shared venues

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
4. Run `powersync deploy sync-config --directory=deploy/powersync`.
5. Put the instance URL in `backend/src/config.rs` as `POWERSYNC_URL`.

`deploy/test-stack/run.py` checks a change to either file against
disposable containers.
