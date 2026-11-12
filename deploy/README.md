# Deploying sync

See [docs/design/sync.md](../docs/design/sync.md).

## Releases deploy the server

The release workflow's `migrate` job (`.github/workflows/release.yml`) runs
before anything is built, on every nightly from `dev` and every `v*` tag:

1. `supabase db push --include-all` against project `smuuycypmsutwrkpctws`
   applies each file in `../supabase/migrations/` that
   `supabase_migrations.schema_migrations` does not list, in filename order,
   one transaction per file. A failure stops the release, so no build ships
   ahead of its schema; rerunning applies only what is still pending. Before
   pushing, the job lists the pending files and checks them against the
   contract rule below.
2. If `powersync/sync-config.yaml` differs from the deployed sync rules, it
   runs `powersync deploy sync-config --directory=deploy/powersync`.

A manual run from another branch only checks: it fails if production lacks a
migration or sync rule that branch has. Secrets are listed in
[RELEASING.md](../RELEASING.md).

Nobody pastes migrations into the SQL editor any more: a file run by hand is
not recorded, and the next push runs it again. To apply the pending files
before a release, run what the job runs from the repository root:
`supabase db push --dry-run --include-all --db-url "$SUPABASE_DB_URL"`,
`scripts/release/check_migrations.py` on the files it lists, then the push
without `--dry-run`.

### Expand, then contract

A client keeps running for weeks after a newer release ships, so a migration
may only add to what an older client uses. One that takes something away
(drops a table or column, deletes or truncates rows, renames, changes a type,
adds `NOT NULL`) is a contract, and says which release stopped using the thing:

```sql
-- contract: after 0.6.3
```

It applies only in a release whose version (`backend/Cargo.toml`) is newer
than 0.6.3, so it lands one release after the clients that stop using the
thing. `scripts/release/check_migrations.py` enforces it: CI runs it on the
migrations a pull request adds or changes, and the release on the pending
ones. A destructive file without the marker, or with a version the release
is not past, stops both. The marker is also the go-ahead: nothing that
deletes data runs until someone has written one.

Nightlies build from `dev` and migrate the same production database, and a
nightly counts as its crate version's release. While `dev`'s version equals
the latest stable's, that is safe: a nightly may run only what that stable's
own release could. But `dev` is bumped right after a tag, while the release
is still a draft, and its version alone would then let a nightly drop what
the published stable still uses. So the release also passes the latest
*published* stable version (`--published`): a contract runs only once the
release it names is out.

The same check refuses a file not named `<14-digit version>_<name>.sql` (the
CLI would skip it) or sharing another's version, and a `pg_temp` function
created without `or replace`: the push runs every pending file over one
connection, so the next file may create the same helper.

Sync rules deploy after the migrations, so a rule may name a table the same
release creates. A rule that stops naming a table ships in the release before
the contract that drops it.

### Migrations

Every file in `../supabase/migrations/`, with what must happen before or
after it. A new migration adds its line here.

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
- `20261024000200_drop_venue_conversion.sql` — **not applied.** Deletes
  twelve venues and everything in them. Repeat the dry run in its header,
  then add its contract marker to let a release apply it
- `20261024120000_lasers.sql` — then redeploy `powersync/sync-config.yaml`, which
  names `laser_zones` and `laser_limits`
- `20261025120000_random_node.sql`
- `20261025130000_laser_elements.sql`
- `20261026120000_laser_figures.sql`
- `20261102120000_drop_midi_mappings.sql` — **not applied.** After
  redeploying `powersync/sync-config.yaml`, which no longer names
  `midi_modifiers` or `midi_bindings`, and after every client runs a build
  without them; its contract marker names that build
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
- `20261110120000_qlc_fixtures_to_library.sql` — with the first nightly that
  reads `lib:` fixtures. A 0.6.2 or older install cannot draw or drive the
  fixtures it moves until it updates
- `20261110130000_group_references.sql` — before shipping the client that
  reads `{"groups": …}` selections: it cannot read an expression, and a
  client from before it cannot read the rewritten selections. Server only;
  there is no local migration
- `20261111030434_track_art_read.sql` — any time; until it runs, venue
  members cannot download a shared track's cover art
- `20261111120000_client_format.sql` — any time; refuses nothing until a
  later migration raises `public.data_format()`. Before any migration that
  does: a server without it takes writes from a build of any format
- `20261111121000_terms_acceptance.sql` — before the release whose sign-in
  shows the terms: that client cannot record an acceptance without it, so
  nobody new gets past the terms page. Server only; there is no local
  migration
- `20261111130000_profiles_private.sql` — any time; until it runs, anyone
  holding the app's public key can list every account id and display name.
  Server only; there is no local migration

`row_model.sql` drops the old sync schema first and builds the row model, so
the folder starts there and runs on a project that has been reset. On an
empty project run the first push by hand: the release would refuse the
unmarked drops in the oldest files, which predate the contract rule.

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
4. Run `powersync deploy sync-config --directory=deploy/powersync` once;
   from then on every release deploys it.
5. Put the instance URL in `backend/src/config.rs` as `POWERSYNC_URL`.

`deploy/test-stack/run.py` checks a change to either file against
disposable containers.
