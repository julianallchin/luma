# Sources and Aim — implementation, September 28

Implemented and launched on `patterns-principles`. Julian approved activation
on September 28. The live local database and Supabase are migrated. All 5,494
queued updates finished without rejections.
The original plan is [the handoff](2026-09-28-sources-handoff.md).

## What changed

Color is one form with color, brightness and clip fade. Random and Noise
produce the old Sparkle and Noise looks. Time replaces Time/Hit; events live
on sources. Every numeric source parameter accepts another source. Audio's
existing frequency/floor/threshold controls stay as before, with a sourceable
amount. Space, Random and Noise share head/fixture/clump grain.

Aim has a direction or point, a spatial offset, horizontal and vertical
offsets, an offset frame, and clip fade. Fan, Sweep, Wave, Circle, Figure-8 and
Ballyhoo are saved source inputs. Overlapping Replace position clips make a
transition; an Offset motion clip can run above both. The old separate
fan/motion/shape/size/spread controls are gone. New positions are edited directly; transition them with clips. Existing animated direction
and point sources remain readable and editable to preserve saved movements.

Clip `fade` runs once over the clip. Rhythmic or audio modulation belongs in
brightness or motion. Color still scales light before its blend mode; this
change does not turn its fade into another compositor opacity model.

## Source contract

See [node-cards](../../resources/skills/node-cards/SKILL.md) for the JSON fields.
Time and Random can own `events: {every, life}`, inherit enclosing events, or
follow another input with `events: {same_as: "brightness"}`. Omitted life
follows every. Zero every means one event over the clip. Space exposes its
Time offset's events to width and amount. Explicit zero every on a nested
Time source keeps it over the whole clip instead of inheriting stroke life.

Brightness chooses the strongest live event at each head. Color following
brightness uses that event's color; ties choose the newest. Previously the
validator rejected color following moving strokes, so there was no valid old
RGB-overlap behavior to preserve. Independently clocked color and numeric
nested sources use their newest live event.

Forms lower into the existing prepared tensor graph. Source names and nesting
are resolved during compilation. Kernels process fixture × time × event
tensors; three component output wires keep RGB/vectors separate from events.
Geometry, integrated period tables and audio normalization prepare once.
Static forms fold completely. Random ranks all groups once per distinct event
in the batch. The initial recursive evaluator has been deleted.

## Migration and verification

`backend/scripts/migrate_sources.py` converts stored clips, drafts and presets.
It is a one-time data conversion, not a load-time compatibility layer. Dynamic
old alpha sources move into brightness/rate amount; whole-clip alpha becomes
fade. Noise keys preserve old random streams, and exact sine easing preserves
Aim motion. There is no database schema change.

`backend/scripts/check_sources_migration.py` compares old and new evaluators,
including saved drafts and optionally the original presets. Build the same
`patterns/examples/source_parity.rs` against each version. It evaluates real
clip playback, including the exclusive end and the immediately preceding
representable beat. Audio clips are schema-validated and listed separately;
output comparison requires their track analysis.

The September 28 snapshot contains 5,492 clips and two drafts. The comparison
checked 6,016 non-audio instances (including draft snapshots and all 30 shipped
presets), and validated 366 audio instances. No validation errors. There are
two output differences, both at the representable beat immediately before a
clip ends (1.8e-15 and 3.5e-18 beats before the end). The old zero-period clock
rounds to a new event there. The new once-per-clip event does not restart.
The report retains these as differences and exits unsuccessfully; its tolerance
has not been relaxed. All other sampled frames match within 1e-6.

The backup is under `~/.config/com.luma.luma/backups/pre-sources-20260928/`.
The report and proposed SQL are in `/tmp/luma-sources-review/`. The SQL was
applied to an offline copy there: all clips and drafts convert idempotently,
and SQLite's integrity check passes. Optimistic predicates and a transaction
guard reject a stale migration instead of silently leaving old rows behind.
Activation followed approval on September 28. The live rows still matched the
verified snapshot. A fresh SQLite backup of both databases is in
`~/.config/com.luma.luma/backups/pre-sources-activation-20260928-130835/`.
Supabase backups are `backup.clips_pre_sources_20260928` and
`backup.drafts_pre_sources_20260928` (5,492 clips and two drafts).

`curve_points_apply` applied the guarded transaction through the app database
connection, generating 5,494 upload entries. All local clips have the new
fade input and SQLite integrity passes. The native Wayland app is running
with the hardware renderer. Cloud sync completed: Supabase has all 5,492 clips in the new format, the local
upload queue is empty, and there are no rejected updates.

## Checks

- Pattern tests: 68 passed across Aim, Color, forms and event tensors.
- Backend evaluator/compositor tests: 21 passed.
- 17 native UI tests cover source edits, nested inputs, shapes, gradient editing,
  Aim Replace/Offset and mirrors, plus fade dragging, resizing, overlaps and undo.
- Pixel captures cover Chase, Position and Circle under the real renderer.
- Changed Python source tests pass. The complete existing `test_score.py` also
  exposes three unrelated stale revision/manifest expectations; these were left
  unchanged.
- Workspace all-target checking and native application builds pass. Clippy
  completes with warnings: three source-editor builders exceed its argument
  count guideline, alongside unrelated existing app warnings. Those builder
  signatures are a remaining cleanup item.
- Generated node-reference pages and embedded agent instructions use the new
  source schema.

## Tensor graph correction

The first build bypassed graph lowering and evaluated sources recursively per
head. In particular, Random sorted all groups again for each head, causing
quadratic work. This was a regression; it was not an inherent lighting cost.
Color, Aim and Strobe now all use the shared graph execution path.

The `source_throughput` example measures warm clip evaluation, including the
lighting output tensor. It excludes preparation, the score compositor, UI and
GPU work. Both versions used optimized pattern code on the same machine,
300 frames per case. Results for 480 heads:

| Preset | Recursive evaluator | Tensor graph |
|---|---:|---:|
| Chase | 0.427 ms | 0.237 ms |
| Shimmer | 17.904 ms | 0.359 ms |
| Drift | 0.378 ms | 0.226 ms |
| Circle | 0.570 ms | 0.162 ms |

At 960 heads, Shimmer is 0.720 ms through the graph versus 76.098 ms in the
recursive evaluator. Fixed Wash is folded before playback. These numbers
are evaluator throughput, not displayed frame rate. The existing graph
kernels still allocate temporary tensors, and the backend still converts
lighting tensors to per-head maps for score composition; those remaining
costs are separate from the removed recursive evaluator.

Focused checks: 79 pattern tests cover sources, Aim, overlap, batching, range
preparation and clock caching. The batch coverage test now checks that every
form is exercised instead of requiring an arbitrary historical graph count.
The stored/preset comparison checks 6,016 non-audio cases against the deployed
recursive evaluator, plus 366 audio schema checks. The final pass has no
mismatches (maximum error 1.5e-14). Audio-dependent output also has targeted
feature-source tests; the stored audio cases still need real track analysis.

The corrected native build was relaunched on Wayland. The workspace all-target
check, 21 backend evaluation/compositor tests, and pattern Clippy with warnings
denied also pass. Playback instrumentation is disabled in the launched app.


## Source editor cleanup

The shared editor no longer adds inactive Phase and Amount controls to Color
or nested Time curves. Aim offset sources expose Size in degrees and Phase;
animating Size does not add another size multiplier beneath its curve.
Spatial color gradients also omit their inactive multiplier. Existing
non-default phase and intensity modulation remain editable, with intensity
named Level, so opening an existing clip does not change its output. Audio
and Random retain a Level control for their output. This is an editor change;
there is no schema migration or rewrite of saved clips.
