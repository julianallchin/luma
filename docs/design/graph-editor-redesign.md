# Graph editor redesign

This is the implementation ledger for the September 9, 2026 product agreement.
It supersedes earlier graph-reset choices about hiding graph inputs and limiting
travel to the repeat interval. This is a chronological ledger: intermediate
remaining-work notes below are superseded by the final checkpoint and
`graph-editor-completion-audit.md`. The full redesign scope is complete.

## Agreed behavior

- Numerical wires carry fixture × time × channel signals with broadcasting.
  Preserve units, channel meaning and fixture identity; structured values such
  as dropdown choices, mappings, envelopes and gradients remain structured.
  Keep canonical score-owned definitions and unify execution rather than
  retaining another permanent parallel engine.
- One Chase implementation accepts reusable triggers. Its unwired default is
  the existing periodic rhythm. Beat/drum trigger sources can drive Chase,
  Pulse and other effects; named combinations can be ordinary preset graphs.
- Every event launches an independent journey. Travel time and curve are
  independent of event spacing. Equal duration/spacing means back-to-back
  journeys; longer travel means overlap; shorter travel means darkness after
  completion. Later events never truncate existing journeys. Seeking must
  reconstruct the same result as uninterrupted playback.
- Execute this as tensor algebra: broadcast sample times against event timestamps,
  derive normalized ages and travel positions, broadcast against fixture coordinates,
  and reduce by Max over the event axis. No mutable journey state or per-event graph
  replay. The event axis is distinct from the requested output time axis.
- Spatial wrapping still governs edges. Preserve axis, order, major-axis and
  circle mapping presets; add custom UVZ direction vectors. Vector magnitude
  does not alter the normalized mapping or travel duration.
- Chase mirroring folds spatial positions before mapping. Plane orientation
  and position are independent of travel direction. Provide Off, left/right,
  front/back, up/down and custom normal controls, centered on the selection by
  default, with an adjustable plane position.
- Input nodes are a name and one output socket. The destination supplies type,
  default and editor metadata (including dropdown options). Shared destinations
  use one value. Renaming changes the exposed label without invalidating clip
  overrides or references. Input nodes/wires replace the competing exposure UI.
- Other node cards contain names/ports, not duplicate parameter widgets or
  serialized JSON. Inspector controls own parameter editing. Numerical signals
  feed explicit Output capability inputs; bundled fixture output is internal.
- Drag to connect with a visible wire, compatible target feedback, cancellation
  and useful errors. Right-click/Space opens focused search at the cursor and
  inserts there. Edges can be selected and deleted. Undo/redo covers changes.
- Use the existing visualizer for play/pause, looping and scrubbing. Remove the
  static output strip. Preview the current graph in the actual clip's audio,
  timing, group and override context. Separate graph defaults, clip overrides
  and temporary preview settings; show invalid drafts explicitly.
- Migrate built-ins, authored graph documents and clip bindings to the new core.
  Preserve authored intent with before/after numerical and render comparisons.
  Use backed-up working copies for persisted data and append-only migrations
  when persisted schema changes are necessary.

## Overlap composition

Confirmed by the user: simultaneous journeys combine with Max at each fixture.
Preserve their independent progress; never truncate an earlier journey when a
new event arrives. No product question remains pending.

## Work and verification

- [x] Canonical numerical execution in one tensor runtime
- [ ] Signal interface migration and legacy execution consolidation
- [x] Reusable triggers and overlapping Chase journeys in the core and built-in recipes
- [x] Custom mapping vectors, existing presets and independent mirror planes
- [x] Shared native mapping controls and lossless structured values
- [x] Simple Input nodes, inference, names and shared bindings
- [ ] Compact cards and explicit Output
- [x] Drag wiring, cursor search, edge deletion and ordinary undo/redo
- [x] Preserve individual draft gestures when an incomplete graph becomes playable
- [x] Clip-context visualizer preview and transport for canonical graphs
- [ ] Pattern/document/override migration and numerical/render comparisons
- [ ] Targeted backend/core tests and native GPUI harness verification

Historical evidence: cc89792e introduced the typed pattern engine September 5;
26691c19 connected canonical score graphs to native editing/playback September 6;
33ca2b97 added signal recipes within that engine. The older tensor evaluator
still resides in backend/src/eval. Preserve useful canonical document/authoring
contracts while unifying their execution.

## September 9 implementation checkpoint

Implemented custom UVZ vectors and independent spatial mirror planes in the
mapping resolver, preserving the existing preset sources and circle topology.
Circle and major-axis fits are computed from the original rig; folded positions
are evaluated against that unchanged reference. A physical plane with the
nonspatial Selection order source is rejected explicitly.

Graph and clip inspectors now share a structured MappingEditor. The authoring
projection keeps full Mapping values rather than dropping circle origins,
orientation vectors, reverse/per-group flags, and new mirror/vector settings.
Typed graph cards no longer duplicate parameters as inline text/JSON.

Verification so far: all 68 luma-patterns tests pass, the targeted backend
mapping codec test passes, and all 8 Python score API tests pass with the bundled
Python runtime (system Python lacks NumPy). Core Clippy and workspace/all-targets
cargo check pass. Native harness verification passes for custom vectors, mirror
presets and custom normals, offset/reverse, graph undo/redo, and persisted clip
overrides remaining separate from graph defaults.

Build prerequisite: initialized the existing consonance-ACE submodule at the
repository's pinned commit, because its requirements file is embedded at compile
time. No live library data has been migrated or altered in this checkpoint.

Adjacent issue: the host's per-group domain currently labels a selection union
with its full expression, not each constituent group. Existing per_group values
are preserved, but a new nonfunctional toggle was not exposed in the UI.

## Event tensor checkpoint

The user clarified that Chase must consume events through tensor algebra, with
no per-event graph replay or mutable journey state. The implementation follows
that rule: sample times × event timestamps produce normalized ages; broadcasting
against fixture coordinates produces fixture × time × event coverage; Max over
events returns fixture × time × channel output. Pulse shares the same age tensor.
There is no ForEachEvent graph wrapper or nested graph replay in the source.

Beat and drum trigger nodes feed the same Chase/Pulse kernels. An unwired trigger
uses existing repeat/grid/delay controls. Travel is independent of repeat for
these kernels. Drum analysis is an immutable, validated timestamp tensor shared
across frames. Periodic sources retain the existing continuous grid convention,
including events before the clip boundary when their tails reach the clip.

Added an ndarray-backed Signal with units, channel meaning, fixture identities
and strict singleton broadcasting. This is a foundation, not completed execution
consolidation: the current typed graph seam still converts kernel results to Mask
values, and the older tensor evaluator and scalar clock remain. Their replacement,
the scalar clock's old travel/repeat constraint, and migration of authored graph
copies are still required. Built-in Chase/Pulse recipes already use the new core.

All 78 core tests pass, including 10 new tensor tests covering independent bands,
Max brightness, short/equal/long lifetimes, out-of-order time batches, empty event
sets, drum-trigger wiring, strict signal broadcasting and previous pill edge/curve/
wrap baselines. Core Clippy, workspace/all-targets check, and the native mapping/
undo/persistence test pass with the final immutable event-source implementation.
No live library data has been changed or migrated.

## Native interaction and Input checkpoint

Drag wires in either direction. Socket hit regions include their full painted
area; valid targets are highlighted and incompatible targets explain the refusal.
Compatibility and cycle checks use the core edit operation. Failed drops preserve
existing bindings. Edges have stable selection identities and Delete disconnects
them. Node insertion, wiring, movement and deletion keep the viewport in place.
Right-click and Space open a floating search at the pointer; Enter inserts at its
original graph position. Empty search, Escape, arrows and text-field key ownership
are covered by native pointer/keyboard tests.

Multi-node deletion and add-plus-placement are single history operations. An
unfinished draft keeps its redo branch attached to the published state it started
from, including after stepping backward through earlier valid history. Stale
preview jobs are retired when an edit or history step makes the graph incomplete.
A remaining history limitation: completing a draft still publishes the whole draft
as one timeline transaction; preserving every intermediate gesture across that
publication requires further work.

Input nodes are compact name-and-socket cards backed by existing formal graph
parameters, not additional execution kernels. A new unwired Input has no invented
type. Its first wire inherits the destination's type, editor metadata and effective
default (including an existing local or exposed default). Further consumers share
that value and must be compatible. Detached Inputs retain their value; explicitly
deleting an Input removes its wires/interface and reconciles callers and overrides.
Names commit on Enter/blur and cancel on Escape. Renaming changes the display name,
while stable argument keys preserve clip overrides and graph references. Defaults
remain editable in the inspector; named Input badges point to their source.
The former expose/make-local buttons and core Expose operation are removed. Python
add_input/Input.rename/Input.move and the existing expose convenience builder use
the same Input operations. Existing formal parameters acquire explicit layout
metadata when edited; old documents without that optional field remain readable.

Verification: 80 core tests and core Clippy pass; all 8 Python score tests pass with
the workspace Python runtime. Native tests cover dropdown inference, shared consumers,
name edits, clip overrides, Input movement/deletion and undo, graph search, wiring,
edge deletion, and mapping/vector/mirror edits through the Input inspector. Five
existing legacy graph interaction tests also passed. The GPUI workspace/all-targets
check passed. Strict app Clippy still reports pre-existing issues outside these
changes (including scene lifetimes, float type complexity and app library/visualizer
lints); core Clippy is clean.

Visual limitation: the interaction sequence also completed using the platform's
real text metrics, but screenshot capture failed because this pinned GPUI only
provides a headless renderer on macOS. The pixel test is retained for a supported
host; no screenshot or GPU visual-verification pass is claimed on Linux. Interaction
fixtures do not have a canonical venue graph root, so their preview error does not
provide evidence about fixture rendering. No live library data has been changed.

## Visualizer preview checkpoint

Auto-layout is now frozen from the measured canvas before the first edit. Adding
an Input or connecting a node cannot reindex existing Inputs or move nodes between
dependency columns. Both native interaction/Input tests pass with initially
unpositioned fixtures that exercise this case.

Canonical score graphs now preview in the existing visualizer. Their static
output strip is removed. A typed, authorized dispatch entry prepares an isolated
clip Scene without installing it on the output engine. The selected clip supplies
its real beat-grid span, group selection, seed and input overrides. Sampling uses
the same compositor and is pure in absolute track time; scratch allocations are
reused. The visualizer shows clip context plus Play/Pause, looping and scrubbing.
These controls do not write the score or its overrides. Legacy graph preview is
still awaiting the document migration.

Audio preview owns a bounded interval of the loaded track. The audio host uses
the same end sample for looping and stopping; its clock also stops there. Interval
validation caps reads at the available samples. Preview request identities outlive
tabs, so closing/reopening a graph cannot reuse a former request identity. Leaving,
hiding or closing the graph releases playback; incomplete/erroring previews stop
and clear stale output. Changing which clip a shared graph previews resets its
temporary cursor. The host's general track loader still lacks cancellation of an
in-flight decode/install across competing track opens; the graph checks ownership
before seeking/configuring/playing, but general load arbitration remains adjacent
work.

Isolated clip preparation passes a backend test covering
variable tempo, group identity, clip overrides, single-clip isolation, out-of-order
seeks and clip boundaries. The native preview test passed scrubbing, stopping,
looping, leaving and close/reopen while confirming byte-identical persisted score
data. Both bounded-audio tests pass. All six lighting-pattern native tests pass
after updating their search/preview and Chase-inspection expectations to the new
model. The composition test also caught two canvas issues: clearing a draft error
on wire press shifted the target sockets, and new cards could sit behind existing
cards at the insertion point. Error chrome now stays in place during the drag;
selected cards rise to the front with link indices remapped to keep endpoints.

## Compiled tensor execution checkpoint

Canonical graph execution now keeps every numerical wire as a shared immutable
Signal. Literals become tensors during preparation; arithmetic, noise, reductions,
curves, feature samples, color and capability output retain their time axis.
Structured controls keep their document/editor forms, including envelopes whose
parameters vary over time. Fixture tensors align by identity without changing
selection order or mapping geometry.

Removed the separate recursive graph evaluator and the scalar-map primitive
kernels. Library inspection and prepared playback use the same flattened tensor
program. The single-sample Value API remains at the authoring/inspection boundary;
it is no longer the representation between execution steps. The host renderer
evaluates a whole requested time batch once and packs its capability tensor into
the existing compositor, rather than rerunning the graph for each sample.

Chase and Pulse keep their event axis inside the tensor operation. Each time row
gathers only timestamps that can still contribute, then broadcasts ages/spatial
coverage and reduces Max. Periodic sources form those timestamps algebraically.
This bounds work by simultaneous journeys rather than total track events, even
for widely separated or out-of-order requested times. No mutable journey state,
event replay, or frame history is used. Animated Chase width, endpoints and curves
are sampled on the output time axis.

This does not complete the port/document migration. Authored scalar/field type
spellings, explicit conversion nodes, existing Lighting recipe interfaces and
the old cyclic motion primitive still need migration into the agreed signal and
Output model. The legacy-document evaluator in backend/src/eval also remains
until those documents can be lowered through the canonical program. The overall
goal and the unchecked migration/history work above remain active.

Verification: all 84 core tests and strict core Clippy pass. New batch tests cover
every built-in with complete defaults, animated Chase width/envelopes, one event
source read per batch, and a 40,000-event track sampled across a wide time span.
All four backend lighting tests pass, including host batch packing against direct
core output. Isolated clip-preview validation and the native preview/transport
test pass. No live library data was changed. The previously documented Linux
pixel-renderer limitation still applies.

Adjacent limitation: the host kernel interface still logs runtime errors and
returns black output. A failed batch currently fails as a whole; propagating that
diagnostic to the graph/visualizer belongs in the remaining Output integration.
Pixel capture remains unavailable on this Linux GPUI host.

Final checkpoint checks: the five-test `graph_` native group passes with the
completed canvas and transport changes; all six `lighting_patterns` tests pass;
the workspace/all-targets check and `git diff --check` pass. The three new backend
tests (isolated clip scene and bounded audio) pass. Core tensor/output consolidation,
completed-draft gesture history, legacy migration and render comparisons remain
unfinished; this checkpoint does not complete the goal.

## Signal interface and terminal Output checkpoint

Generic arithmetic now carries inferred units and channel meaning through nested
graphs. Scalars broadcast across fixtures, time and RGB channels inside arithmetic;
socket compatibility checks meaning rather than a separate scalar/field tag.
The same inference serves core validation and native wire gestures. Runtime checks
also enforce unconstrained generic graph inputs. A Chase intensity can multiply
RGB and feed Output directly, without explicit broadcast/color adapter nodes.

Output is a terminal with optional color, dimmer, pan, tilt, strobe and movement
speed inputs. Adding it selects the destination automatically; its internal
fixture bundle has no outgoing canvas socket. Unwired capabilities remain
unwritten, explicit zero remains a real write, and Clear removes a local write.
Color carries RGB intensity; a connected dimmer scales that intensity. Angles have
degree-valued literals and normal numeric controls. Required numerical sockets
can seed editable scalar Input defaults. Shared graph/clip argument projection
keeps inferred units, names, defaults and clip overrides consistent, including
degree-valued Inputs. The Python API accepts numerical/color literals for generic
signal sockets without requiring tensor-shaped JSON.

This is not the completed document migration. Old Lighting-producing recipe
interfaces, Chase/Chase Mask naming, explicit conversion recipes, and the cyclic
motion constraint still remain. Existing authored documents and legacy patterns
must be migrated and compared before those interfaces can be removed. No live
library data has been changed.

## Evaluation failure checkpoint

Canonical failures now propagate through the host evaluator, preview taps and
clip thumbnail generation. The graph visualizer displays the failing node and
reason, stops its audio preview, and recovers on a valid seek. Errors do not poison
later evaluations. The realtime output boundary retains a logged empty-frame
fallback; authoring paths use the fallible result instead of interpreting it as
valid black output. This resolves the preview diagnostic limitation above.

The compositor evaluates each annotation once over only its active sample times.
An undefined expression outside a clip can no longer fail the active part of a
batch. It still evaluates all active samples together, with no per-time graph
execution or playback history.

Verification: 89 core tests, strict core Clippy, all 104 backend evaluator tests
and all nine Python score API tests pass. The evaluator suite includes the
committed gradient golden (its Git LFS object was fetched), generic composition,
unit/channel rejection, optional Output semantics, dynamic failures and subsequent
out-of-order seek recovery. Native Output tests pass for wiring, local capability
edits/Clear/undo, Input inference and separate clip overrides. Native error tests
pass for visible failures, stopping audio and recovery. All seven native graph
tests, the workspace/all-targets check and `git diff --check` pass.

The remaining goal includes individual draft-gesture history across publication,
pattern/document migration, removal of obsolete interfaces/execution paths and
reference renders on a host with a working GPUI pixel renderer. The existing
per-group domain-labeling and general audio-load arbitration limitations remain
as documented above.

## Unified draft history checkpoint

The timeline now owns both playable score snapshots and per-graph draft snapshots
in its existing undo history. Removed the graph editor's separate draft history
and origin bookkeeping. Each node insertion, Input insertion and connection stays
a distinct undo/redo step when the final wire makes a draft playable. Undo can
return to an incomplete graph without persisting an invalid score. Closing and
reopening a graph tab restores its pending draft and redo branch while the owning
timeline remains open. Draft bases remain available for three-way merging after
other score edits, and pending drafts survive a score reload.

The native test also exposed a shared history defect: recording and then
abandoning a no-op click discarded redo and could evict the oldest checkpoint.
History now retains the displaced entries until the gesture is committed or
abandoned. All six standalone history tests and all eight native graph tests
pass, including completion, individual undos/redos and closing/reopening an
undone draft. Pattern migration and removal of old interfaces remain unfinished.

## Versioned migration candidate checkpoint

Captured the original v2 catalog from commit
`7cfc6e8faf6ee583e7b50830d1878cbde12a8270` as immutable migration data. A temporary
isolated build of that commit generated independent numerical samples for all
18 complete effects, each with defaults and another set of color/grid/delay/angle
overrides. The generator and fixture are retained for reproducibility; no second
runtime is linked into Luma.

`migration::upgrade_v2` now converts a copy into a v3 candidate. It expands old
capability bundles into named numerical ports, specializes shared helper calls
to the capabilities actually present, and inserts explicit terminal Output nodes.
Clip identities, timing, seeds, selections, blend modes and argument keys/values
survive; authored labels, helper sharing and existing graph/Input positions are
retained where their nodes survive. Broadcast-only adapters disappear. Chase and
Pulse use the event kernels, including after increasing migrated travel beyond
repeat. Unknown or invalid source documents fail without altering the source.

Added a channel-axis Maximum operation used by color decomposition. An explicit
Output dimmer now remains independent of its supplied RGB color; color-only output
still extracts brightness automatically. Emitted RGB remains color × dimmer, and
preserving the independent values also preserves opacity in later score blends.
This supersedes the earlier implementation's automatic extraction even when a
dimmer was explicitly connected.

The 36 captured cases match the old engine across nine fixtures and nine times.
Additional tests cover differently shaped calls of the same authored helper,
unwritten capabilities, independent color/dimmer composition, overlap after
migration, idempotence and rejection of injected primitives without a panic.
All 97 core tests and strict core Clippy pass; the workspace/all-targets check
passes. No live score or database has been changed.

This is still a migration candidate API, not automatic persisted migration.
Default score creation remains v2 until the new catalog and authored-history seam
are switched together. Historical source bytes must retain their original hash;
do not silently upgrade within deserialization. Saved-history integration,
legacy-pattern lowering, picker/interface cleanup, remaining unit conversions and
cyclic motion, old evaluator removal and final render/native verification remain.
Overall progress reported to the user: approximately 65%, with migration and
execution cleanup constituting most of the remaining work.

The wider migration check now covers every one of the 97 frozen node types as
an unused authored helper. It caught the old beats-to-field adapter's implicit
unit erasure; migration expresses that conversion as division by one beat.
Scalar arithmetic and conversions lower to the shared numerical operators too.
The drum-pulse recipe now uses complete event timestamps, with a rising-envelope
test proving that a later event does not erase an earlier tail. Capability ports
are named color/dimmer/pan/etc.; the old generic Lighting prefix is not exposed.
Anonymous wrappers preserve their inherited names when Output is added.

An isolated SQLite/authored-history test applies v2-to-v3 as one ordinary source
edit, verifies rollback when history recording fails, retries idempotently, and
restores both versions with their original source bytes and revisions. This
proves the existing history seam supports the conversion; automatic adoption on
opening a score still awaits the catalog switch.

The native migrated-score test caught an independent save-path defect: constructing
every graph candidate from Score::default reset its document version. Candidates
now retain the base version. The test verifies clip overrides, graph defaults,
inherited labels, explicit color wires, edge deletion/undo, and saved v3 data.
All nine native `graph_` tests pass with this fix. No live library data was touched.

## Canonical catalog and Python checkpoint

The public catalog now contains v3 signal recipes and the live primitive set.
Chase and Pulse expose dimmer signals; effect placement adds a visible terminal
Output. Color stays independently optional on Output. Removed the old duplicate
Rust recipe builders and public Lighting writer/combine primitives. The canonical
recipe data no longer depends on migration data at startup. Incompatible wire
messages describe signal units/channels instead of dumping Rust type structures.

Owned v2 score documents upgrade through the authored-history seam when opened;
read-only documents get an in-memory v3 projection. Historical deserialization
and restoration still preserve exact source bytes and hashes. Immutable v2
recipe transformations resolve newly reserved ID collisions without replacing
authored helpers. Standalone typed-pattern playback now decodes against the frozen
v2 vocabulary and runs the converted canonical graph. Numerical previews evaluate
a whole time batch once.

Python edits upgrade their private v2 candidate, retaining the original source
revision as the apply CAS. New Python graphs use the same Rust instance constructor
as native placement, including explicit Output wiring. Real Python integration
covers old-document conversion, graph authoring, save/apply, isolated clip renders,
saved-vs-private pixel equivalence, and detached workspaces.

Verification: 100 core tests, strict core Clippy, 104 backend evaluator tests,
13 authored graph-score tests, 10 Python unit tests, nine native graph tests,
six native pattern tests (five in the group run and the corrected sixth separately),
and the real Python/render/workspace integration test pass. The workspace with
all targets checks successfully. Backend scene rendering is available on Linux;
the native GPUI pixel renderer remains unavailable. The render integration's
images are stored under /tmp/luma-graph-redesign-renders for inspection.

Remaining: migrate row-based scores and untyped standalone graphs, remove the old
category-based evaluator and obsolete primitive execution branches, and complete
visual/reference review. No live library database was modified.

## Typed row migration and retired-kernel checkpoint

Row-based scores whose selected implementations use the historical typed
vocabulary now convert on open. The conversion reads one authorized snapshot,
copies each shared pattern once into score-owned definitions, namespaces its
helpers without collisions, and creates named Input cards. It preserves defaults,
shared/disconnected controls, labels, node positions, per-clip overrides, layering
and selections. Seconds map through the track's actual tempo grid. Historical
random selection used a different seed from effect randomness; `selection_seed`
is optional on Clip and preserves that distinction without changing old source
bytes when absent. No library pattern is rewritten.

The standalone typed playback adapter uses the same structural conversion. Its
former decoder duplication is removed. Unimplemented library placeholders and
untyped scores stay explicitly legacy; conversion never publishes only part of
a score. Their oldest node vocabulary still needs conversion.

Removed execution of retired scalar conversions, broadcasts, bundle writers,
Add Lighting and Travel Clock from the core runtime, along with their capability
packing/addition helpers. Historical validation now checks frozen structure and
values without preparing/running historical kernels. The remaining serialized
enum names and signatures are migration metadata; live execution rejects them.
Current score validation still prepares canonical graphs and checks fixed control
relationships.

Automatic publication now belongs to `AuthoredDocuments::upgrade_score_for_scope`.
Its operation identity uses the current history head under the document guard.
Restoring the same old source and reopening it therefore performs a fresh
conversion; two concurrent openers return the same current graph document, and a
delayed opener cannot replace or return stale data over a newer graph edit. The
regression test converts, retries concurrently, restores exact old rows, reopens,
edits, and checks a delayed open. The unchanged library graph is asserted too.

Verified: 100 core tests and strict core Clippy after retired-kernel removal;
104 evaluator tests before removal and all five canonical playback adapter tests
afterwards; 14 authored graph-score tests; all ten native graph tests, with both
native migration tests repeated after the final automatic-publication fix.
The workspace/all-targets check passed after core deletion; a final check follows
the publication fix. Real render images preview-0 and preview-2 were inspected:
the dark initial chase frame and two purple beams agree with the scene assertions.
Native GPUI pixels remain unavailable on this host.

An adjacent harness race surfaced in the older timeline checks: two tests used
the same temporary library directory concurrently. They now name separate fixture
directories. Those timeline checks and the final workspace check are running.
No live database has been modified, and nothing is staged or committed.

Final checks at this checkpoint: both legacy timeline tests pass with isolated
fixtures, both native migration tests pass, the workspace/all-targets check
passes, and `git diff --check` is clean. Goal remains active: oldest untyped
vocabulary conversion, removal of that host evaluator, and final visual review
are not complete.

### Checkpoint: original numerical graph migration

Added conversion for the original scalar/color/math vocabulary into ordinary
score-owned graphs composed from the same live tensor operators. It covers
scalar/color, gradient/palette sampling, rainbow, ramps/sine, arithmetic,
rounding, threshold/remap/modulo, view taps, and color/dimmer/strobe/speed sinks.
Historical Time Delay was an identity in the old compiler; conversion reconnects
its input directly. Helpers preserve the former node layout, named shared Inputs,
disconnected arguments and clip override keys. Numeric overrides accept the older
`{value: ...}` encoding as well as plain numbers. Converted scores never contain
an old numerical execution primitive.

The core now has channel extraction; clamps, comparisons and choices preserve
RGB channel dimensions. Inferred Input metadata stays numerical and concrete,
with the bound value supplying any missing channel/unit meaning. Comparison units
are checked in the executing kernel as well as when wiring. Removed a stale
preparation comment that still described the deleted travel <= repeat rule.

62 independent original-engine cases are frozen in
`backend/src/node_graph/migration/fixtures/numerical-v1.json`, with a reproducible
capture source and provenance alongside. The active comparison no longer invokes
the old engine. It checks raw arithmetic before clipping, fixture output, and
individual seeks versus batch evaluation. Coverage includes RGB/scalar operands,
negative values, zero divisors, reversed/degenerate remaps, channel extraction,
old color encodings and empty/single-color palettes.

Limits of those references are explicit: comparison uses a constant tempo and
full master intensity. The canonical Output clamps capabilities earlier than the
old compositor, so previously overdriven values may respond differently below
full master intensity. Musical generators now follow the beat grid instead of
multiplying seconds by a single BPM. These are behavior differences, not evidence
of exact parity at every tempo/master setting. The older movement, spatial,
event, reduction and audio vocabulary still needs conversion before the old host
category evaluator can be removed.

A saved-score regression exposed that stored graphs were validated against the
current add-menu vocabulary before migration could read them. Stored graph reads
now use structural validation; publication still validates the candidate against
the current catalog. This allows migration of previously supported nodes such as
Apply Dimmer while retaining corruption, scope and write validation. Original
color/gradient codecs now live at the migration boundary and are shared by the
old compiler during its remaining lifetime.

The new saved-score test checks shared Input wiring, both numeric override
encodings, preserved card positions, rename without lost overrides, deterministic
output and unchanged source library bytes. A native test opens a real old
numerical pattern, deletes/restores a wire, and verifies the saved version and
unchanged library source. The lit fixture now has distinct node positions rather
than stacking all nodes at (0,0), so pointer interaction is meaningful.

Verification is in progress at this checkpoint. No live database writes, staging
or commits; the overall goal remains unfinished.

Verified at the end of this step: all 102 core tests, strict core Clippy, all
104 evaluator tests, all ten graph document tests, all 15 authored graph-score
tests, and all three migration-module tests (including the 62 frozen cases and
individual-seek comparisons). The ten existing native graph tests passed; the
new numerical native test passed after explicitly selecting the clip before
opening it, allowing the inspector's layout change to settle. The workspace
check across all targets and `git diff --check` passed. The moved hex decoder
also refuses non-ASCII hex before byte slicing, avoiding the old Unicode panic.

Remaining follow-through is unchanged: migrate the rest of the original
vocabulary, remove its category evaluator, settle earlier-clamping differences
for overdriven old patterns, and finish the visual/product review. The numerical
conversion is not a claim of complete old-vocabulary coverage.

### Checkpoint: movement migration and numerical vector controls

The numerical core now supports arbitrary nonzero channel counts with optional
RGB/pan-tilt meaning. Join Channels broadcasts fixture/time axes and concatenates
the channel axis; inference and runtime agree on the resulting width and units.
Generic vectors of matching width can acquire a destination's named channels,
while incompatible widths remain invalid. No special movement execution engine
was introduced.

Circle, Figure 8, Sweep and Apply Movement now migrate into standard sine,
arithmetic, channel extraction and join graphs. Original unequal-vector math
repeated the last component; migration represents this padding explicitly.
37 independently captured movement cases pass alongside the previous 62 numerical
cases, comparing raw taps, pan/tilt and other capabilities, and reordered/repeated
seeks. The original Apply Movement comments described a movement pyramid, but its
executed path passed UV directly as angles. Migration preserves actual playback.
That discrepancy is recorded with the capture provenance rather than presented
as a new movement feature.

A shared numerical editor now handles constant multi-channel graph defaults and
clip overrides. It uses the color picker for bounded RGB, named Pan/Tilt fields
for angles and numbered fields for generic vectors; larger vectors page eight
components at a time. Fixture/time-dependent values remain dimension summaries.
The first native test passed: a new Input inferred 12-channel angle metadata from
a bound destination, shared two consumers, retained values across paging, kept
clip overrides independent, and persisted graph edits through undo/redo. Color
picker and existing Input regressions are running after the final UI adjustment.

Core tests passed (105) before that final UI adjustment. Remaining work is still
the original spatial/event/audio/reduction vocabulary, retiring its evaluator,
overdriven output/master behavior, and final visual/product review. No live
library writes, staging or commits; the goal remains active.

The RGB regression passed after explicitly dismissing the picker before switching
tabs (the shared popover intentionally consumes an outside press). The inspector
also rebuilds its widget when a generic socket's resolved value changes shape;
a single-fixture tensor never becomes a broadcast scalar editor. Numeric vector
fields retain all finite values, including values outside the usual 0–1 range.

The earlier-clamping issue is now resolved for dimmer headroom. Output keeps
positive intensity above one, and the host adapter passes it through. The
compositor applies master/group intensity before bounding brightness. Color-only
Output extracts brightness without clipping individual channels first, preserving
their ratio. An explicit color/dimmer pair retains independent capability meaning.
All 99 frozen numerical/movement cases now also pass through the host adapter at
master levels 0, .25, .7 and 1, alone and over a lower layer. Dimmer is checked in
every case; layer color/opacity is checked for valid original RGB. Negative old
chromaticity is still clamped at the new capability boundary, an explicit parity
limit distinct from the now-fixed master-intensity issue.

Verified: 106 core tests, strict core Clippy, all 104 evaluator tests, four
migration-module tests including all 99 references/master comparisons, and all
13 native graph tests. The three Input tests passed again after the final finite
numeric-range adjustment. A direct generic-three-channel-to-RGB Output regression
also passes. Workspace/all-targets checking is running; `git diff --check` is clean.
No live database writes, staging or commits. Remaining work: original spatial,
event/audio/reduction/noise vocabulary (including mixed graphs), retiring that
host category evaluator, and final visual/product review.

Final workspace/all-targets check passed; only the pre-existing vendored GPUI
unused-import warning remains. `git diff --check` passed. This checkpoint is
progress, not completion of the full goal.

### Checkpoint: original spatial graph migration

Get Attribute and Mirror now convert to score-owned graphs built from shared
numerical geometry, arithmetic, channel and reduction operations. This covers
all 16 original attributes and six aliases, including raw/relative world axes,
selection index/count, dominant-axis mappings, fitted angles and angular ranks.
Mirrors preserve their center snap and side tolerance; folded positions remain
a three-channel value that subsequent geometry operations can consume. New Chase
mirroring remains built into its mapping controls.

The core exposes fixture geometry with world coordinates and original selection
index, radial coordinates, a 2D principal direction, circle fitting, and ranking
nearby points. These are stateless tensor operations, baked when their inputs are
constant. Fitting precision and sample order are explicit: the old f32 solver's
basis can change on symmetric or nearly isotropic clouds. Conversion supplies
the original selection index as order. Geometric fields retain their own fixture
domain; a domain-free broadcast value can also be ranked over the host domain.
Field reductions now preserve channel/time axes and meaningful units, and support
per-channel distinct counts. Shared point conversion validates coordinates without
mistaking a large sorting key for a geometric distance.

546 independent spatial reference cases are frozen across six layouts, with
reverse-lexical fixture IDs to detect row reassignment. Every attribute and alias
is tested with each mirror axis, alongside raw fold/side outputs. All comparisons
passed before a final helper refactor. Comparisons map raw tensor rows by fixture
identity and include the existing seek/batch and four-master-level checks. These
expanded cases caught an extra Apply Dimmer migration clamp that the previous
numerical/movement reference graphs had not exercised; that helper now also
retains headroom through the terminal boundary.

All 110 core tests and strict core Clippy pass after the geometry helper refactor.
The full numerical/movement/spatial migration comparison is rerunning, followed
by storage/native checks. Remaining original vocabulary includes event/audio,
noise/wander, whole-clip reductions and Soft Voronoi, plus mixed graphs; the old
host category evaluator cannot be removed until those paths are converted.
No live database writes, staging or commits. The full goal remains active.

The final spatial checkpoint passed all 645 numerical/movement/spatial reference
cases, 16 saved-score/Python integration tests, all 13 native graph tests, and the
workspace/all-targets check. The only workspace warning was the existing vendored
GPUI unused import.

### Checkpoint: harmony, pitch palettes and Falloff

Original Harmony Analysis now lowers to a twelve-channel numerical pitch signal
using the prepared track's harmony data. Pitch Palette lowers to palette matrix
mixing, explicit first-fixture selection, peak normalization and clamping. The
shared `mix_palette` operation projects any channel-weight vector through evenly
sampled colors, preserving fixture/time axes and headroom. Channel count is an
ordinary numerical query, so reconnecting a formerly empty/short pitch input to
a twelve-channel source works without rebuilding migration-specific helper code.
Pitch channels are constructed in a balanced tree to keep composed graphs below
the existing dependency-depth bound. The original palette's first-selected-row
behavior is explicit and independent of canonical fixture storage order.

Falloff uses clamping, width multiplication and a broadcast power operation;
its positive/negative curve and near-zero linear branch remain ordinary graph
math. Power rejects physical units, non-real results and overflow. First-fixture
reduction preserves all channels, supports per-sample ordering, and handles both
empty domains and domain-free values. It does not append a temporary channel,
so the full supported vector width remains usable.

110 independent original-evaluator cases cover these nodes, every pitch class,
no-chord overlaps/gaps, negative/small/mixed weights, per-fixture weights, fallback
palettes and scalar/RGB/spatial/twelve-channel Falloff inputs. All 755 frozen
numerical/movement/spatial/harmony cases pass, including raw taps, seek/batch
agreement and master-compositor comparisons. Shared named palettes, reconnection
after an unconnected input, and one-color palette/gradient overrides also pass.
One-color authored controls migrate to an equivalent two-stop flat gradient.
The original unconnected palette panicked; the new graph stays black until wired.

All 114 core tests and strict core Clippy passed. The eight migration-module tests
pass after the final singleton-control change. Backend evaluator/native/workspace
checks are next. No live data writes, staging or commits.

Remaining work includes original beat/drum envelopes and random selection,
frequency/stem analysis, noise/wander, whole-clip reductions, Soft Voronoi, mixed
typed/untyped graphs, old evaluator retirement and final product/visual review.
An additional palette migration edge remains: an empty exposed palette is still
rejected by the canonical gradient decoder, while an inline empty pitch palette
correctly uses its rainbow fallback. Shared empty palettes consumed by both a
plain gradient sampler and a pitch palette need to preserve their different
historical fallback semantics. This checkpoint does not claim full completion.

Final checks passed: all 104 backend evaluator tests, all 13 native graph tests,
the workspace/all-targets check, and `git diff --check`. Only the pre-existing
vendored GPUI unused-import warning remains. No test/build process is left running.

### Checkpoint: Noise, Wander and exact seed controls

Original Noise and Wander now convert to score-owned graphs of coordinate math,
shared one/three-dimensional lattice-noise operations and explicit seed streams.
They have no playback history. The original f32 interpolation, octave accumulation
and node-identity seed are preserved. Noise's implicit missing-X fixture index and
its X/Y-only output domain are now explicit graph operations; a time-only fixture
field is collapsed in original selection order. Unknown fixture identities are
rejected rather than silently reassigned.

Seeds are fixed, nonnumerical u64 controls, serialized as decimal strings so JSON
consumers cannot round them. A simple Input infers this type like other controls;
the inspector and clip sheet use the shared drafted-number editor with exact
integer parsing. Defaults, clip overrides, renaming and history keep every bit.
Local definitions remain graphs; no per-seed primitive definitions or exceptions
to score validation were added.

306 independent original-evaluator Noise/Wander cases pass, including empty and
noncanonical-order fixture selections, input-domain combinations, raw values,
seek/batch agreement and master compositing. All 117 core tests and strict core
Clippy pass. Workspace/all-targets checking passes. The new native test verifies
type inference, exact large seed edits, invalid-draft reversion, undo/redo, renamed
clip overrides and persisted values. The full migration/native suites are running
next. No live database writes, staging or commits.

Remaining original vocabulary includes beat/drum envelopes and random selection,
frequency/stem analysis, whole-clip reductions, Soft Voronoi and mixed graphs.
Empty exposed palette fallbacks, retirement of the old host category evaluator,
and final visual/product review also remain. The full goal stays active.

Final checks at this checkpoint passed: all nine migration-module tests covering
1,061 frozen reference cases, all 14 native graph interaction tests, three shared
number-field unit tests, and 11 Python score tests. Python seed literals now
serialize to exact decimal strings too, including explicitly typed values;
fractional, negative and overflowing values are rejected. `git diff --check`
passed. All test/build processes have completed.

### Checkpoint: frequency and stem graphs

Historical Frequency Amplitude and Stem Splitter now convert into score-owned
graphs. Stem outputs are named audio-source controls. An audio spectrum exposes
normalized FFT bins as signal channels and their spacing in Hz. Frequency ranges
become editable channel-vector controls; their weighting uses shared channel
indices, arithmetic and channel sums. Disjoint ranges and duplicate weight in
overlapping ranges preserve the original calculations. Channel sums retain
fixture/time axes and units. A source's spectrum width and spacing cannot silently
change within a batch, and invalid or negative magnitudes are rejected.

The causal FFT moved to the audio module and is shared with the remaining old
callers; it caches only the immutable plan. Migrated graphs explicitly hold the
audio boundary like the original. New spectrum nodes are silent outside the
audio. Requested stems use the existing strict preparation path: missing stem
data is an error rather than substitution of the full mix. The historical
infallible FFT callers still produce zero if FFT execution fails; that old error
contract will disappear with the remaining category evaluator.

160 independent audio reference cases cover five sources, empty/nonempty fixture
selections, three sample rates, ranges, boundaries and seeking. The first 140
passed immediately. A precision probe then found a neighboring-bin difference
at 44,101 Hz. Twenty added captures reproduce it; all original 140 captures stayed
identical after moving the FFT implementation. Migration now rounds range values
and bin quotients explicitly to f32 before floor/ceil, using a shared numerical
precision operation. Numerical operations otherwise retain f64 precision.

Before that final rounding correction, all 120 core tests, strict core Clippy and
105 backend evaluator tests passed, including the missing-stem and boundary-mode
checks. The expanded reference suite and final checks are running next. No live
database writes, staging or commits. Remaining work includes historical event
envelopes/random selection, whole-clip reductions, Soft Voronoi, mixed graphs,
empty exposed palettes, old evaluator retirement and final product/visual review.

The rounding correction passes all 1,221 frozen references in ten migration tests.
All 121 core tests and strict core Clippy pass, as do all 14 native graph tests
and workspace/all-targets checking. Final cleanup also moved the existing
single-band feature sampling/reduction into the audio module; canonical audio
no longer calls the old evaluator and propagates FFT errors. Historical callers
share the same band reduction while their existing error contract remains until
retirement. Backend checks are rerunning after this final helper move.

Final helper-move verification passed: all 105 backend evaluator tests, the ten
migration tests with all 1,221 references, workspace/all-targets checking and
`git diff --check`. Only the existing vendored GPUI unused import remains. No
test/build process is left running. This is progress on the full active goal.

### Checkpoint: recorded events and envelope migration

Beat Pulses, Drum Events, Beat Envelope and ADSR now migrate into canonical,
score-owned graphs. Beat Pulses' numeric output and event timestamps have
separate connections. Recorded grid sampling and greedy 25 ms drum coalescing
preserve the old timing; ADSR keeps shortest-gap fitting, its 120 BPM fixed-length
fallback, optional anticipation and latest-two-event overlap. Chase still uses
its full active-event axis with Max and no playback history.

Seconds are a numerical unit distinct from beats. The host supplies immutable
track timing alongside requested audio/drum analysis. Binding analysis prepares
constant event calculations and their dependent arithmetic once. Rebinding uses
fresh data; frame evaluation shares the prepared values. A recent-event query
produces timestamp/presence tensors, and reusable curve graphs use normal signal
math. No old envelope kernel is called by the converted graph.

Rate inference now follows actual dependencies and memoizes shared wires. Fixed
calculations can feed event configuration without accepting animated inputs.
Editable graph outputs allow later animation; capturing their current constant
rate in the interface had prevented reconnection. The shared finite dependency
limit now permits composed envelope arithmetic and still rejects excessive
expansion/nesting. No per-node kernel/interface exceptions were added.

960 independent event/envelope captures pass, bringing the frozen total to 2,181.
All twelve migration tests pass, including a named Input feeding fixed arithmetic
with a clip override. Core and backend tests pass; native checking is finishing.
The old hand-authored Boolean downbeat flag was inconsistently ignored by event
tracing; migration now honors it consistently. Numeric checkbox storage, used
by the old UI and frozen reference cases, retains its evaluated behavior.

Remaining: random selection, whole-clip reductions, Soft Voronoi, mixed graphs,
empty exposed palettes, old evaluator retirement and final product/visual review.
No live data writes, staging or commits.

Final event checks pass: 127 core tests, strict core Clippy, 105 backend evaluator
tests, all 19 native graph tests when run serially, eleven Python score tests,
and workspace/all-targets checking. The concurrent native run hit the documented
headless state leak in marquee selection; that test also passed in isolation.
The final timestamp-range guard and Boolean-checkbox correction pass targeted
regressions. Random-selection migration is next.

### Checkpoint: effect foundations and one editable envelope

The implementation strategy now starts with five small acceptance compositions:
event-driven Chase lifetimes; Chase values remapped directly to mover positions;
random per-head shimmer envelopes; phase-offset circular motion; and a wrapped
palette ribbon with independent intensity. These are executable composition
tests. A sixth test uses the same vector arithmetic for sampled path geometry;
it proves mathematical reuse, not a scan-point domain or laser output adapter.

Immutable events can carry explicit head weights or deterministic random subsets.
Selection uses event identity, seed and head identity; it is independent of seek
order and fixture ordering. Earlier selected cohorts finish their own envelopes,
and overlapping journeys combine with Max. Only queried event columns are
materialized. The Shimmer recipe has three nodes and Circle has four. The unused
selection-table/sample-index approach has been removed.

Envelope is the single editable curve model. ADSR and Beat Envelope have been
removed from the node catalog, and the old ADSR evaluator has been deleted.
Saved stage settings are converted once into ordinary Envelope anchors and
Bézier handles; their trigger connections feed the shared numerical sampler.

The proposed connected-stage adapter was a mistake: the original stage controls
were parameters, not connectable ports. The generated-points operation, its
128-node construction and the tests for those nonexistent connections are removed.
No replacement curve-constructor operation is being added.

Historical mixed graphs also use the same Envelope sampler through the normal
compiled-graph path. The remaining event-time composition preserves saved timing,
fit-to-gap, anticipation, amplitude and overlap behavior. Zero-duration envelopes
are silent; the original 100-microsecond sustain flash is intentionally removed.
Reference captures remain unchanged.

Remaining goal work includes historical random selection, whole-clip reductions,
Soft Voronoi, mixed graph migration, empty exposed palettes, full old-evaluator
retirement and final native/persistence/render review. No live database writes,
staging or commits have been performed.

Validation after deleting ADSR: all 141 core tests, 102 evaluator tests, 15
migration tests (including the 2,181 frozen numerical cases), and 14 native graph
tests pass. Workspace/all-target checking, strict core Clippy and diff whitespace
checks pass. The manifest check regenerated the IPC documentation for the existing
preview-range command and then passed.

The broader backend run reported eight other failures, left for the remaining
goal work: the assistant-turn preparation assertion; three auth-wipe tests for
unclassified `track_beat_validations`; two Python venue-chain direction tests;
the obsolete Python pattern test passing `color` directly to Chase; and the score
DSL test expecting a dangling old argument binding to be rejected. The new
Envelope playback failure from that run was fixed by binding owned timing data
through `PreparedGraph::with_features`; its regression test now passes.

### Checkpoint: clip-wide ranges and historical timing

The shared Clip range operation estimates a signal's minimum and maximum over
the clip during preparation. It samples only the upstream dependency cone, in
bounded batches, and prepares nested ranges in dependency order. Playback reuses
those results without resampling; rebinding track analysis rebuilds them.
Normalize and Invert are small compositions of this range and ordinary arithmetic.
They now migrate into canonical graphs without a separate reduction kernel.

160 frozen cases captured from the original category evaluator cover empty and
nonempty selections, constant and variable tempo, several source signals, and
nested reductions. Single-reduction output differences stay below 0.002; the
largest measured difference is 0.00161, from changing the range sampling grid.
The original nested-range bug returned zero because upstream statistics were
unfinished. Nested normalization and inversion now use the completed upstream
range; reference captures remain unchanged and the correction is tested explicitly.

Those captures also exposed incorrect timing in earlier migration work. Old
oscillators use elapsed seconds and average BPM; mapping them directly to detected
beats retimed effects on variable-tempo tracks. Migrated graphs now express the
original clock explicitly using shared Track time outputs. Clip start, duration
and BPM are fixed metadata; elapsed seconds remain frame-varying. New effects
retain musical timing.

ADSR remains deleted from the catalog and evaluator. Only saved-document
recognition, one-time conversion to Envelope curves, and reference tests retain
its name. The generated stage-control adapter and curve-constructor primitive
remain removed.

Remaining work: historical random selection, Soft Voronoi, mixed graph migration,
empty exposed palettes, old evaluator retirement, and final persistence, rendering
and product review. The old reduction kernels remain only in the historical
category evaluator until that remaining graph migration is complete. No live
database writes, staging or commits.

Verification passed: 146 core tests, 16 migration tests covering 2,341 frozen
cases, 14 serial native graph tests, strict core Clippy, workspace/all-targets
checking and diff whitespace checks. All test/build processes have completed.

### Checkpoint: held random selections

Random Select Mask now migrates into shared Random subset and Recent events
operations. Recent events returns a weight for each head and queried event;
requesting one event holds the current selection until the next trigger. Empty
recorded streams stay black. Global events broadcast; targeted streams retain
their head identity and channel metadata. Query memory depends only on requested
event identities, not elapsed playback.

Old Avoid Repeat becomes the Shuffled cycle option. A seeded head ordering and
event-index offset select each group directly. For a stable selection this gives
the requested count and the minimum possible consecutive overlap, including
when more than half the heads are selected. Independent random rerolls remain
the default for new effects and Shimmer. Exact historical head sequences change;
the cyclic no-repeat sequence also repeats sooner than independent draws. This
tradeoff is explicit in the reference documentation. Neither mode remembers or
replays previous selections, and fixture reordering leaves draws unchanged.

All 672 original selection cases retain counts, timing and black intervals.
Additional checks cover held states, no-repeat overlap, shared Count inputs,
renaming, serialized clip overrides and out-of-order seeks. The existing 2,341
frozen reference cases still pass.

Remaining: Soft Voronoi, mixed graph migration, empty exposed palettes, full
category-evaluator removal, and final rendering/product review. The historical
random kernel remains only in that category evaluator pending its removal.

The native graph checks and 15 authored-score persistence tests pass. Adjacent
validation cleanup now rejects a stored wire from a deleted exposed input during
structural validation, even for a retired node vocabulary. This addresses the
previously logged score DSL workflow failure; its regression and all ten graph
document tests now pass.

Final checks pass: 148 core tests, strict core Clippy, 18 migration tests covering
3,013 frozen cases, 14 serial native graph tests, 15 authored-score persistence
tests, the graph-document and score DSL regressions, workspace/all-targets
checking and diff whitespace checks. All test/build handles are complete. No
live database writes, staging or commits; the full goal remains active.

### Checkpoint: Soft Voronoi and palette opacity

Soft Voronoi now composes bounded moving XYZ sites, normalized proximity weights
and palette mixing. The geometry operations accept explicit numerical positions
and time; they do not read fixture state or contain a whole lighting effect.
All 380 frozen original cases pass at the existing 3e-5 tolerance, including
empty selections, flat bounds, custom parameters and transparent palettes.
The actual original bounds had zero-width axes; its contrary comment was wrong.

Gradient stops retain opacity through saved data, clip overrides and native
editing. Mix Palette exposes opacity separately. Optional perceptual color
mixing uses the original OKLab/vibrance behavior; ordinary mixing retains its
numerical headroom. Graph defaults and clip overrides now use the same gradient
editor, with stop opacity, RGB edits that preserve alpha, and undo/redo coverage.
The separate clip-gradient implementation has been removed.

### Checkpoint: mixed graph migration

Historical numerical nodes can now connect to typed pattern calls and receive
their numerical outputs. The existing v2 conversion supplies each typed call's
explicit capability ports; the final graph has one Output and no intermediate
lighting bundles. Shared inputs, labels, positions and clip bindings survive.
The old compiler rejected mixed graphs; analytic composition tests establish the
new behavior rather than claiming an original mixed rendering existed.

Unknown wires are no longer silently discarded. Conversion rejects missing nodes,
duplicate connections and, for previously unsupported mixed graphs, competing
terminal writers for the same capability. Pure historical numerical graphs retain
their established writer order. The old stem-source wire is validated before its
splitter becomes explicit track stem controls.

Database/history coverage now runs the same migration/restore/retry proof for a
mixed Chase graph as for a typed graph. Candidate creation remains read-only;
the library's original graph bytes are unchanged. No live database writes,
staging or commits have been performed.

Remaining goal work: historical low/high-pass audio filters, empty exposed
palettes, complete category-evaluator retirement, and final rendering/product
review. Native pixel rendering remains unavailable in this Linux GPUI build;
headless interaction tests do not establish visual appearance.

Verification passed: 151 core tests, strict core Clippy, 23 migration tests
(3,393 frozen cases plus mixed/override regressions), 16 authored-score persistence
tests, 15 serial native graph tests, 102 evaluator tests, ten graph-document tests,
workspace/all-targets checking and diff whitespace checks. Single-color imported
gradients also keep explicit stop opacity when expanded to a flat two-stop curve.
All build/test handles have completed; the full goal remains active.

### Checkpoint: audio filter chains

Repository history revealed that low/high-pass nodes originally transformed PCM
before analysis. The later category compiler had never connected them; its unused
windowed RMS variants were a different operation. Those variants are deleted.

Audio lowpass/highpass now compose fixed, immutable source descriptors. Feature
preparation loads the exact mix/stem and runs each requested chain once through
the existing Butterworth implementation. The tensor evaluator receives the
resulting spectra or band values. Missing filtered sources never substitute the
mix or unfiltered PCM. Existing bare-source JSON stays byte-compatible.

Saved `audio_in`/`audio_out` chains migrate with their cutoffs, order and exposed
source controls. Source-origin preprocessing deliberately replaces per-clip
filter restarts, so overlapping clips and out-of-order seeks agree. Tests cover
source selection, original DSP output, frequency rejection, immutable shared
preparation, validation, serialization and negative-cutoff migration.

A catalog-to-migration audit also identified three remaining entries: Spectral
Shift, View Events and Mel Spectrogram. Spectral Shift and Mel Spectrogram were
likewise absent from the category compiler; their older behavior needs review.
They join empty exposed palettes, complete evaluator retirement, and final
rendering/product review as remaining goal work. No live data writes or commits.

Verification passed: 153 core tests, strict core Clippy, 24 migration tests
(3,393 frozen cases plus migration regressions), 101 evaluator tests, the prepared
filter DSP test, 16 authored-score persistence tests, 15 serial native graph tests,
workspace/all-targets checking and diff whitespace checks. All test/build runs
have completed. ADSR remains absent from the catalog and both runtime paths;
its remaining references only recognize and convert saved documents or verify
that migration. The full redesign goal remains active.

### Checkpoint: hue composition and event inspection data

Spectral Shift now converts to channel argmax, division and hue rotation, with
explicit first-selected-head reduction and RGB extraction. These are ordinary
shared operations: argmax preserves fixture/time axes and returns the first
maximum's index; hue rotation broadcasts and preserves RGB extrema/headroom.
The saved helper has twelve calls, including the old domain and channel choices.
There is no separate musical color kernel. All 153 frozen cases from the earlier
HSL implementation pass at 3e-5, alongside end-to-end harmonic-color playback
and RGBA/first-head regressions. The historically ignored Strength setting stays
ineffective during conversion.

View Events now preserves its connection as a structured Events graph output,
matching the existing diagnostic-output treatment of View Signal and View UV.
The event data survives serialization and arbitrary seeks without rasterizing
timestamps into a fixed preview grid. Invalid beat output names and numerical
connections to event viewers are rejected. A saved audio wire into Harmony
Analysis is validated before conversion to the track's stored chord analysis.

Remaining: Mel Spectrogram migration, empty exposed palettes, complete old
evaluator retirement, and final rendering/product review. The latter includes
surfacing migrated diagnostic outputs in the native inspector; preserved root
outputs alone do not establish that UI. The migrated four-output Drum Events
helper also requests all four drum analyses even when only one output is used.
That unnecessary preparation is an adjacent cleanup item, not a new trigger
requirement. Native pixel rendering is still unavailable in this Linux build.

Verification passed: 156 core tests, strict core Clippy, 28 migration tests
(3,546 historical reference cases plus focused regressions), 101 evaluator tests,
16 authored-score persistence tests, 15 serial native graph tests, workspace
checking across all targets, and diff whitespace checks. All build/test handles
have completed. A fresh catalog audit leaves only `mel_spec_viewer` without a
converter; the other limitations above still require work. No live database
writes, staging or commits. The full goal remains active.

### Checkpoint: empty and single-color palettes

Gradients now retain 0–64 stops. An empty value samples as opaque black; a single
stop is constant and retains its authored position/opacity. Decoding no longer
invents black stops or duplicates single colors. The shared `palette_fallback`
operation explicitly selects another palette when the first has no colors;
fixed choices fold during preparation. Migrated harmonic palettes use it to
preserve their original context-sensitive fallback while sharing one Input with
ordinary samplers and Soft Voronoi. An explicit black color does not trigger it.

The native gradient editor can remove the last color, show an empty state and
add a color. It guards stale selections and uses shared action buttons. Clip
editing preserves an explicit empty override instead of displaying the graph's
fallback default. Single colors no longer acquire an accidental black stop.

Ten new frozen original-renderer cases verify shared empty/single/transparent
palettes across harmonic and ordinary consumers. Other tests cover nonempty
defaults with empty clip overrides, shared bindings, renaming, serialization,
native add/remove/undo/redo, and persisted separation of defaults from overrides.
The capture generator is retained as reference source, not linked into tests.

Verification passed: 157 core tests, strict core Clippy, six gradient model tests,
30 migration tests (3,556 historical cases plus focused regressions), 16 authored
score persistence tests, 16 serial native graph tests, workspace/all-targets
checking and diff whitespace checks. All build/test handles have completed.

Remaining: diagnostic/spectrogram migration and native inspection, complete old
evaluator retirement, unnecessary multi-output drum preparation, and final
rendering/product review. Product review must also address the existing mismatch
where Sample Gradient exposes editable stop opacity but only returns RGB; Mix
Palette already returns opacity separately. Native pixel rendering is still
unavailable in this Linux build. No live database writes, staging or commits;
the full redesign goal remains active.

### Checkpoint: connected analysis and native graph inspection

Preparation now removes unused output dependencies after flattening nested
definitions and before collecting track-analysis requests or preparing fixed
reductions. Connecting only Snare from a four-output drum source needs only
snare analysis. Regression coverage includes nested wrappers, an unused audio
clip-range branch, constant-only consumers, and errors when an unavailable
branch really is connected. The migrated event-viewer test now supplies snare
analysis alone rather than fabricating empty results for the other drums.

The clip visualizer offers an optional Inspect signals panel for additional
numerical and event root outputs, including migrated View Signal, View UV and
View Events connections. It samples the existing compiled clip once when the
preview is prepared, preserving the actual selection, overrides and beat clock.
It keeps fixture identities, named channels, channel paging, plotted value ranges
and an aligned transport cursor. Event ticks use actual timestamps and the
tempo map; targeted events currently show their schedule and target-head count.
An inspection error is separate from transport errors. Closing/paging/selecting
the inspector does not change the score. Spectrogram inspection is still pending.

Verification: all core tests and strict core Clippy pass; 30 migration tests,
16 authored-score persistence tests, 17 serial native graph tests and two plot
geometry/timing tests pass. The native case verifies output selection, head
paging, clip overrides, scrubbing, closing/reopening and unchanged stored score
content. The backend case verifies tempo changes and exact override values.
Workspace/all-targets and whitespace checks pass. Small preexisting Clippy
findings in tensor metadata copies and reference-test loops were corrected.

The architecture reference now describes canonical score execution instead of
nonexistent executor/state files and a fixed simulation rate. Remaining work:
Mel Spectrogram migration/inspection, complete category-evaluator retirement,
Sample Gradient opacity output, and final rendering/product review. Pixel
appearance is not proven by headless interaction checks. No live database writes,
staging or commits; the full goal remains active.

### Checkpoint: canonical playback boundary and audio diagnostics

The category compiler, operation enums, slot interpreter, frozen-stat pass and
all six old kernel files have been removed. A host Plan now contains one
canonical PreparedGraph, its fixture IDs and a boolean capability-write mask.
Scene compositing still observes unwritten capabilities. Saved standalone
patterns cross the same graph migration boundary before compilation; the full
argument declarations now travel through that entry point so palette, selection
and numerical overrides retain their meaning. The ADSR-only compiler bridge is
also gone. ADSR exists only as a saved-document spelling converted to Envelope.

The frozen numerical comparison suite now checks the public saved-pattern
compiler against score playback as well as the original reference frames.
An unfinished historical graph receives an explicit unwritten Output instead of
failing solely because it has no output capability. Diagnostic channels come
from canonical signal metadata and retain fixture identity when publishing rows.
The backend benchmark and reference-capture tools use the canonical playback
entry point; old slot-stat introspection was removed from the capture tool.

Mel Spectrogram viewers migrate into audio diagnostic outputs. Inspection uses
the connected AudioInput descriptor, including filters and clip overrides, and
the shared mel DSP with actual sample-clock cropping. The former graph-run
filter-tracing bypass has been removed. Native inspection caches the spectrogram
image, provides a temporary beat overlay, and keeps missing-source errors local
to the diagnostic. Audio inspection works with an empty fixture selection.
Default beat overlays and actually applying connected filters are intentional
corrections to the previous viewer behavior. Venue-binding rig coordinates and
their geometry tests moved out of the deleted evaluator into node_graph/geometry.

Verification: 48 graph/migration/geometry tests pass, including public
import-versus-score comparisons across the frozen reference cases. All 22 host
evaluator tests pass, including a standalone audio-diagnostic regression that
checks the connected filter, overridden cutoff and exact cropped spectrum.
All 18 native graph interaction tests pass, including the spectrogram test with
zero fixtures. The cached image's frequency orientation/BGRA test passes, and
GPUI workspace/all-targets and backend/all-targets checking pass.

The full backend run finished with 928 passed, 8 failed and 13 ignored. One failure
regenerated the IPC manifest's source locations; its targeted rerun passes. Six
remaining failures repeat the prior assistant-turn preparation, unclassified
track_beat_validations auth-wipe policy, and Python venue-chain direction issues.
The seventh is the old Python PatternDraft authoring path, which still assumes
Chase owns color. That API exports the old lighting projection while advertising
the current catalog, so it needs consolidation with the canonical score builder;
simply changing the test's color argument would not complete that migration.
The canonical Python score editing, graph-run publication and graph-score
persistence/history tests pass in the full run.

Remaining goal work includes gradient-sampling opacity, the obsolete Python
PatternDraft authoring path, final product/reference render review, and a final
requirement-by-requirement completion audit. Native
headless pixel rendering remains unavailable on this pinned Linux GPUI platform;
interaction and bitmap tests are not a substitute for final visual review.
No live database writes, staging or commits were performed.

### Checkpoint: canonical Python authoring, opacity and physical UI review

The obsolete Python PatternDraft builder and its host commands have been
removed. Canonical graph authoring now covers source roundtrips, previews,
score-owned storage and idempotent publication. Gradient alpha survives Python
defaults and clip overrides. Sample Gradient exposes opacity independently of
RGB, and automatic clip placement retains auxiliary outputs for inspection and
further composition.

Physical native-window review is available on this Linux host by launching the
ordinary app on an isolated Xvfb display with a backed-up harness database. This
does not change the pinned platform's missing headless pixel renderer. It found a
search focus bug concealed by the harness's click-before-type helper: the field
now receives focus after mounting. Direct typing after right-click and Space
passes both the updated harness test and the running native app. Native captures
also show the compact Input, preview scrubbing and fixture output. The app and
virtual display were stopped after inspection.

All 160 core tests, strict core Clippy, 18 serial native graph tests, 16 authored
score persistence tests, the Python authoring and scene-render integrations,
12 Python score unit tests and 27 Python track unit tests pass. Backend/all-targets
and GPUI workspace/all-targets checking pass.

The full goal is still incomplete. The audit identified the separate
`open_pattern` / `Source::Legacy` library-editor route, which still presents old
cards, exposure controls and the static preview strip. Row-score migration did
not retire that entry point. The next work is to consolidate this route and then
finish the product review, including vocabulary duplication and narrow-canvas
framing. See [the completion audit](graph-editor-completion-audit.md) for explicit
requirement evidence, current limitations and capture/log paths. No live library
writes, staging or commits were performed.

### Checkpoint: library templates enter the canonical score editor

The library browser and new-tab Pattern choice now lead to the ordinary pattern
insertion preview. Canonical scores list saved library patterns alongside built-in
nodes and score-local graphs. Confirming a library result reads the venue-visible
implementation, converts it at the backend migration seam, and inserts an
independent clip/graph copy through the score history and CAS writer. The library
source is not rewritten. New copies retain selection and controls, use the
selected timeline span, and receive a new clip seed.

`Score::import_clip` shares reachable-definition copying with `make_independent`.
It preserves clip data, keeps built-in dependencies shared, namespaces copied
local dependencies, and leaves both documents unchanged on invalid input or an
identity collision. No separate template graph model or writer was introduced.

A native regression exposed a real same-name bug: programmatic search setup
reset the chosen library row to a same-named score graph. The query subscription
now resets selection only when the query actually changes. The test imports a
library pattern already used in the score, undoes/redoes insertion, opens the
imported clip, deletes/restores a wire, then verifies distinct graph roots and
exactly unchanged saved library JSON.

Validation: 161 core tests, strict core Clippy, four native migration tests,
six picker/editor regressions, eight tab-chrome unit tests, backend all-targets
check and regenerated IPC manifest. Final logs use `/tmp/luma-library-import-*`;
the strongest import/persistence check is `native-final.log`. No live library DB
was used. The completion audit records the remaining work: delete the old editor
fallback used only by unconverted row scores (including obsolete save/preview
paths and tests), then finish vocabulary and physical product review.

### Final checkpoint: one editor, portable score data, native product review

The obsolete native editor and its detached document/history, inline parameter
widgets, static image preview and save/reload entry points are deleted. Every
admitted graph opens the canonical score editor; missing implementations retain
saved rows and show an explicit error. Closing or switching the score closes its
graph views. Reopening keeps saved edits. A real double-click layout bug is fixed.

Library import, custom UVZ vectors and independent mirror planes were exercised
in a real desktop window on an isolated fixture copy. The new visible Expand /
Show chat and zoom/100%/Fit controls were exercised too. Source library rows remain
unchanged; imported clips own independent graph roots. Two old sampler spellings
remain readable for saved documents but are absent from node search; all built-in
graphs use the shared Envelope/gradient names. ADSR remains deleted.

The final data-boundary audit extended the existing rejection of resolved cell
snapshots to fixture-targeted event literals. Authored graphs retain selectors;
head IDs are resolved at runtime. 162 core tests, strict core Clippy, 23 native
graph tests, the strengthened score lifecycle test, 31 backend migration tests,
16 persistence/history tests, real Python/render integration and workspace checks
pass. The completion audit records exact logs, visual artifacts and adjacent
limitations. No live library data was written and no commits were made.
