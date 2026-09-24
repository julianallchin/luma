# Agent Python workspaces and score authoring

Status: built. The Python sandbox exists only on macOS (§17.4–§17.7). Text
marked **Not built** or **Not met** has no code yet.

Scope:

- the track copilot, the pattern-graph agent and the venue rig agent;
- the durable agent threads they share;
- the binding and artifact data plane that exposes Luma state to Python;
- the narrow host capabilities behind score and venue authoring;
- the local Python runtime, artifacts, interruption and sandbox.

Code: `backend/src/agent_execution/`, `backend/python/luma_exec/`,
`backend/src/agent/tools/python.rs`. Code cites this document by section
number, so keep the numbering.

---

## 1. Decision in one paragraph

Every durable agent thread owns one persistent Python workspace. The model gets
one notebook-like `python` tool. Before each cell, Luma refreshes one reserved
root named `luma`; variables created by the agent stay in the kernel namespace
across calls. All track, audio, musical-feature, venue, pattern and graph-run
data reaches Python through one versioned binding manifest and one artifact
store. Numerical data keeps its semantic axes, units, identities and
provenance. In a thread with a score, `luma.track` is the complete saved score
and the entry point to one staged edit: the agent builds graphs and clips in a
private candidate, checks it, previews it and applies it through the host.
Python receives no database or general application authority. A score is
ordinary rows that sync through PowerSync (`docs/design/sync.md`); a subagent
edits a draft instead of the live rows. On macOS, Python runs with no network,
only its workspace inputs readable, and only its scratch space writable.

The important shape is:

```text
durable agent thread
    owns one Python workspace
        owns zero or one live Python kernel
        owns cell history, scratch files, and generated artifacts

current Luma state
    -> one binding assembler
    -> one immutable binding revision
    -> reserved Python object: luma
```

---

## 2. Why this exists

The north star is:

> Give agents a real code-execution environment over the audio and the lighting
> output, so one piece of code can measure what the track is doing, measure what
> the pattern emits, and correlate the two.

The desired capability is deliberately open-ended:

- use the already-computed beats, drum onsets, bars, chords, spectral features,
  and venue geometry;
- access the source audio mix and stems when the question is about audio rather
  than an already-derived feature;
- derive custom thresholds and new features with NumPy, SciPy, and librosa;
- inspect graph outputs as semantic spatiotemporal tensors;
- correlate musical events against lighting events;
- create diagnostic figures such as spectrograms and overlays;
- preserve helper functions and intermediate variables across an agent thread.

This is not merely a larger menu of fixed analysis tools. The model must be able
to write the analysis that the task requires.

---

## 3. Goals and non-goals

### 3.1 Goals

1. One Python execution mechanism shared by every agent.
2. One persistent workspace per durable agent thread.
3. One generic path for all Luma data entering Python.
4. Semantic numerical data: axes, coordinates, labels, units, and identities
   remain attached to values.
5. Exact, composable analysis over audio, features, venue geometry, the score,
   graph definitions, and graph output.
6. Notebook-native output: last expression, stdout, stderr, tracebacks, and
   figures.
7. Immutable Luma snapshots in Python, plus one explicit staged score edit.
8. A single coherent authoring surface instead of one model tool per clip
   operation.
9. Local execution with interruption, bounded output, and production
   sandboxing.
10. Components that are independently unit- and integration-testable.
11. Host-derived authorization and authoritative validation for every score
    mutation.
12. Authored state is ordinary synced rows. There is no second authored-state
    store.

### 3.2 Non-goals

- Python is not a second application backend or a direct database API.
- Python does not receive SQLite handles, credentials, or arbitrary host paths.
  Its host-call protocol exposes only named, scope-bound capabilities installed
  by the trusted command layer.
- Python does not mutate application state through arbitrary APIs. The
  exceptions are the explicit `luma.track.edit()` candidate and the
  `luma.venue` verbs.
- The executor does not invent a separate transport for every domain value.
- The executor does not promise exact serialization of arbitrary CPython heap
  state across app restarts.
- A Python program does not become the persisted representation of a score.
- Subagent scheduling is not part of the Python executor.

---

## 4. Terminology

Use these terms consistently. Avoid the unqualified word `context`, which is
ambiguous between model tokens and runtime variables.

| Term | Meaning |
|---|---|
| **Agent thread** | Durable conversation identity and structured message history. |
| **Model context** | The messages and tool history sent to the language model. |
| **Python workspace** | Thread-owned cells, artifacts, scratch files, and live-kernel association. |
| **Python kernel** | The live subprocess and its mutable Python namespace. |
| **Kernel namespace** | Variables, functions, and imports created by executed cells. |
| **Analysis scope** | IDs and time window identifying the current track, venue, score, pattern, and graph run. |
| **Luma bindings** | Host snapshots exposed under the reserved `luma` object. Bound values are immutable; selected objects may expose explicit host capabilities. |
| **Binding revision** | One immutable, internally versioned set of Luma bindings used by a cell. |
| **Edit** | Python-local candidate that holds the complete score, created by `luma.track.edit()`. |
| **Draft** | A subagent's private copy of a score: one `drafts` row. |
| **Semantic tensor** | Numerical values plus named axes, coordinates/labels, units, and provenance. |
| **Artifact** | An immutable large input or generated output referenced by opaque ID. |
| **Cell** | One invocation of the model-facing `python` tool. |

---

## 5. Durable agent threads and authored state

### 5.1 Thread contract

A thread is an `agent_threads` row (`backend/src/models/agent_threads.rs`). It
holds the id, the owner `uid`, `agent_kind`, the subject kind and id,
`venue_id`, `score_id`, a title, the fork source
(`forked_from_thread_id`, `forked_at_message_id`), and for a subagent
`parent_thread_id` and `parent_call_id`.

Messages are `agent_thread_messages` rows. Each holds the complete structured
parts: text, reasoning, tool calls with their inputs, and tool results.
`agent_thread_transcript_heads` names each thread's last message.

`AgentKind` is `TrackCopilot` or `VenueRig`. `ThreadScope`
(`backend/src/agent/mod.rs`) is the kind, the subject, the venue and the
score. The subject is metadata, not identity: several threads may
share one track.

The Python workspace belongs to exactly one thread id. The host takes the owner
from the signed-in session; the client never supplies it. Changing the
account, venue or score resolves a different thread. It never retargets an
existing kernel.

### 5.2 Rows and sync

Threads, messages and transcript heads sync to their owner as rows
(`docs/design/sync.md`). A retried write is an upsert.

`agent_thread_runs` is local only. `backend/src/agent/engine/claim.rs` records
which device runs a thread and clears the row when the turn ends. Nothing
refuses a second device yet.

### 5.3 Conversation lifecycle

A new conversation creates a new thread and so a new Python workspace. It never
deletes or reuses the previous thread. Navigation, closing an editor, score
edits and binding changes do not reset a workspace.

### 5.4 Authored state is rows

A score is `scores` and `clips` rows.
`luma_patterns::Score` is the in-memory type. There is no revision history, no
document head, no compare-and-swap and no server RPC.

- Saving compares the candidate with the current rows and writes only the rows
  that changed.
- There is no revision token. A stale candidate overwrites the rows it touches.
- Concurrent edits to one row on two devices merge per column. The last write
  wins.
- Postgres row-level security decides who may write. The server does not run
  domain validation. The app validates before it writes.

### 5.5 History

A Postgres trigger records every authored row change on the server, with the
row before and after and the actor: the person, or the model that made it.
History does not sync to devices. Undo in the editors stays in memory. See
[sync.md](sync.md).

### 5.6 Agent edits

- In a root thread, `track.score_apply` writes the live score rows
  (`agent_execution/track_host/score.rs`).
- In a subagent thread, `track.score_apply` writes the draft's `state_json`.
  When the child succeeds, `services/drafts.rs` merges the draft onto the live
  rows per clip and per definition. See `docs/design/subagents.md`.

---

## 6. Core invariants

These are architectural requirements.

1. One agent thread owns exactly one logical Python workspace.
2. Workspaces never share a Python namespace.
3. A live kernel is created lazily on the thread's first Python call.
4. Only one cell runs at a time within a workspace.
5. Different workspaces may execute concurrently.
6. Before every cell, the host atomically reinstalls the reserved `luma`
   binding from one immutable binding revision.
7. Agent-created variables remain intact when `luma` is refreshed.
8. Reassigning or deleting `luma` inside a cell affects only that cell; the host
   reinstalls it before the next one.
9. All Luma data enters Python through one binding-manifest contract.
10. All large inputs and outputs use one artifact store.
11. The model never supplies host file paths or binding operations.
12. Every multidimensional numerical value carries semantic axes.
13. Times used for cross-domain comparison have explicit units and time origin.
14. Spatial tensor rows have explicit primitive identities.
15. Missing or failed data is distinguishable from genuinely empty data.
16. Bound application values are immutable. Mutation is possible only through
    an explicit, scope-bound host capability.
17. Every score mutation from Python passes through the scoped track host;
    Python never writes rows directly.
18. Worker death or forced termination never masquerades as preserved state.
19. Production execution hard-stops if the sandbox cannot be established.
    This holds on macOS only (§17.7).

---

## 7. Agent-facing Python experience

### 7.1 The tool

Every agent receives the same model-facing tool:

```ts
python({
  purpose: string,
  code: string
})
```

`purpose` is a short noun phrase that labels the running cell in the UI (for
example, `"section energy analysis"`). It does not select scope, authority,
execution policy, or a different operation. `code` is ordinary cell-shaped
Python source. The tool description is
`backend/src/agent/prompts/python-tool.md`.

Python is the one analysis and authoring surface. The agent registry also holds
`skill` and, in a thread with a score, `subagent`
(`backend/src/agent/tools/mod.rs`).

The model does not choose:

- workspace or thread IDs;
- binding revisions;
- track, venue, score, pattern, or graph-run IDs;
- input paths;
- artifact paths;
- timeout or sandbox policy.

The host resolves all of those from the durable thread.

The code does not require a wrapper function or an explicit `return`.

### 7.2 Notebook semantics

The kernel preloads or makes available:

```python
import numpy as np
import scipy
import scipy.signal
import librosa
import matplotlib.pyplot as plt
```

Definitions persist:

```python
def nearest_error(reference, candidates):
    return np.array([
        np.min(np.abs(candidates - t))
        for t in reference
    ])
```

A later cell may use `nearest_error` without redefining it.

The last expression is displayed automatically:

```python
np.quantile(luma.features.bars.intensity.values, [0.25, 0.5, 0.9])
```

The worker also exposes ordinary `print()` and notebook-style figure display.

### 7.3 The `luma` namespace

The stable top-level shape is:

```python
luma.meta
luma.window
luma.track
luma.audio
luma.features
luma.venue
luma.patterns
luma.graph
```

In a thread with a score, `luma.track` is a domain object backed by the binding
tree (`GraphTrack` in `luma_exec/score.py`). It exposes track metadata plus:

```python
luma.track.document          # read-only score mapping; clips by ID
luma.track.clips             # every saved Clip, ordered by start beat, z, then ID
luma.track.editable          # descriptive; the host rechecks authority
luma.track.nodes(search)     # node IDs and names
luma.track.definition(id)    # one built-in definition, such as a form
luma.track.edit()            # start a private edit of the complete score
luma.track.window(beats=...) # an immutable view of the saved score
```

A clip holds its ID, graph, start and duration in beats, selection, seed, `z`,
blend mode and input overrides. `z`, not list order, defines stacking.

Branches not applicable to a given agent or unavailable for the current scope
remain discoverable but report why they are unavailable.

`luma.catalog()` returns a compact description of:

- available paths;
- tensor shapes and dtypes;
- axis names, units, and labels;
- provenance and processor version;
- unavailable paths and reasons.

It is an explicit escape hatch, not a mandatory first call and never forced
into model context. Normal discovery should be incremental: inspect a branch,
use `dir(...)`, read a small repr or slice, then go deeper only where the task
requires it. Large catalogs and arrays must not consume the first turn merely
to prove that data exists.

`luma.meta.revision` and scope information are available for debugging, but
routine tool output does not dump revision bookkeeping at the model.

### 7.4 Audio and musical features are distinct

This distinction is fundamental:

- `luma.audio` contains audio signals: the mix and stems.
- `luma.features` contains information derived from audio: beats, downbeats,
  drum onsets, bar classifications, chords, mel data, waveform bands, and MERT
  features when exposed.

Neither is a fallback for the other. The agent uses the namespace matching the
question.

Use already-derived drum onsets:

```python
kicks = luma.features.drum_onsets["kick"].values
snares = luma.features.drum_onsets["snare"].values

{
    "kick_count": len(kicks),
    "snare_count": len(snares),
    "median_kick_gap_s": float(np.median(np.diff(kicks))),
}
```

Operate on audio:

```python
drums = luma.audio.stems["drums"]

envelope = librosa.onset.onset_strength(
    y=drums.values,
    sr=drums.sample_rate_hz,
)

threshold = np.quantile(envelope, 0.88)
peaks, props = scipy.signal.find_peaks(
    envelope,
    height=threshold,
    prominence=np.std(envelope) * 0.6,
)
```

The first example asks questions about Luma's precomputed onset feature. The
second asks a new question of the audio signal itself.

### 7.5 Correlating audio features and lighting output

```python
view = luma.graph.run.views["view_signal_1"]
dimmer_channel = view.channels.index("dimmer")
dimmer = view.values[:, :, dimmer_channel].mean(axis=0)

light_peaks, _ = scipy.signal.find_peaks(dimmer, prominence=0.2)
light_times = view.times_s[light_peaks]
kick_times = luma.features.drum_onsets["kick"].values

errors = np.array([
    np.min(np.abs(light_times - kick))
    for kick in kick_times
])

{
    "median_lag_ms": float(np.median(errors) * 1000),
    "p90_lag_ms": float(np.quantile(errors, 0.9) * 1000),
}
```

This is the core loop the system must make easy.

### 7.6 Plotting

```python
fig, ax = plt.subplots(figsize=(12, 4))
ax.plot(view.times_s, dimmer, label="mean dimmer")
ax.vlines(
    kick_times,
    ymin=0,
    ymax=1,
    alpha=0.25,
    label="detected kicks",
)
ax.set_xlabel("absolute track time (s)")
ax.legend()
fig
```

The model receives the figure as an image part in the tool result. The host also
registers the PNG under the workspace's artifact store, but the current
transcript representation does **not** retain that artifact reference: it keeps
base64 strings up to 2,000,000 characters and stores only a placeholder for a
larger figure. Persisting only an artifact ID and resolving it for transcript
replay is remaining work. Until that exists, figures are a deliberate bounded
exception to artifact-only transcript persistence; large numerical tensors are
not.

### 7.7 Mutation boundary

Python sees immutable snapshots of the track, the score, audio, analysis
products, the venue and graph output. It receives no database handle and no
generic mutation callback.

Two host capabilities can change state:

- `luma.track.edit()` returns a Python-local `Edit` that holds the complete
  score candidate. `add_clip`, `update_clip` and `remove_clip` change only
  that candidate. `edit.diff()` is local.
  `edit.check()`, preview through `luma.venue.render(edit=edit)`, and
  `edit.apply()` cross the sandbox through named host calls
  (`track.score_check`, `track.score_render`, `track.score_apply`).
- `luma.venue` verbs place, attach, aim and remove venue pieces through
  `venue.*` host calls (`agent_execution/venue_host.rs`).

The trusted host owns scope, authorization, compilation, compositing,
validation, ID assignment and the database write. The host-call protocol is an
internal detail, not another model tool.

After a successful apply, the next cell receives a refreshed `luma` binding
while agent-created variables remain. `apply()` closes the edit; further
changes start from a new edit.

The score loop is:

```text
inspect and compute with Python
    -> stage one complete candidate
    -> check it and preview it through the compositor
    -> apply
    -> inspect the refreshed track with Python
```

---

## 8. Semantic tensors: the core data model

Luma's evaluator is already fundamentally tensor-shaped: values flow between
nodes and produce spatial-temporal lighting signals. The agent data plane must
preserve that model rather than reducing it to anonymous arrays.

### 8.1 Python representation

Numerical bindings are exposed through a lightweight `LumaTensor` envelope:

```python
tensor.values       # lazy, read-only numpy.ndarray
tensor.axes         # ordered semantic axes
tensor.unit         # optional unit for values
tensor.provenance   # source, processor version, and relevant metadata
tensor.shape
tensor.dtype
```

`LumaTensor` supports `np.asarray(tensor)` and can provide convenience
properties derived from axes:

```python
tensor.times_s
tensor.primitive_ids
tensor.channels
tensor.frequencies_hz
```

It is not a new numerical computing library. NumPy remains the computation
engine; `LumaTensor` preserves meaning at the boundary.

### 8.2 Axis model

Every tensor axis has:

```rust
enum AxisSpec {
    Linear {
        name: String,
        start: f64,
        step: f64,
        count: usize,
        unit: Option<String>,
    },
    Coordinates {
        name: String,
        tensor: TensorRef,
        unit: Option<String>,
    },
    Labels {
        name: String,
        labels: Vec<String>,
    },
    Index {
        name: String,
        count: usize,
    },
}
```

Examples:

- evenly sampled audio: linear `time` axis;
- graph view: exact or linear absolute `time` axis;
- onsets: `event` index with timestamp values in seconds;
- venue positions: labeled `primitive` and `coordinate=["x","y","z"]`;
- graph signal: labeled `primitive`, absolute `time`, labeled `channel`;
- mel spectrogram: frequency and time coordinates;
- MERT: frame/time and feature axes;
- bar predictions: bar coordinates and tag labels.

### 8.3 Canonical numerical shapes

| Binding | Tensor shape | Required semantics |
|---|---|---|
| Audio mix | `[time]` or `[time, channel]` | absolute time, sample rate, channel labels |
| Audio stem | `[time]` | absolute time, sample rate, stem name |
| Beats/downbeats | `[event]` | values are absolute seconds |
| Drum onsets | `[event]` per class | values are absolute seconds, class provenance |
| Bar intensity | `[bar]` | bar index plus start/end seconds |
| Bar predictions | `[bar, tag]` | tag labels and processor version |
| Chord roots | `[section]` | section start/end, pitch class or missing |
| Waveform bands | `[band, time]` | band labels and bucket times |
| Mel spectrogram | `[frequency, time]` | frequency/time coordinates |
| MERT | `[frame, feature]` | frame times and model/layer provenance |
| Venue positions | `[primitive, coordinate]` | primitive IDs and xyz labels |
| Fixture attributes | `[primitive, attribute]` | primitive IDs and attribute labels |
| Graph view | `[primitive, time, channel]` | labeled primitive IDs when the tap is actually primitive-indexed; an explicit index axis for broadcast/mismatched taps; absolute time and channel meaning |
| Track candidate output | `[light, time, RGB]` | stable light IDs, exact absolute time, `r/g/b`; color already multiplied by dimmer |

### 8.4 Time semantics

All cross-domain times are absolute track seconds unless a binding explicitly
declares another origin. Graph-agent audio may be span-sliced for efficiency,
but its time axis remains absolute.

The analysis window is separately available at:

```python
luma.window.start_s
luma.window.end_s
```

No agent code should have to reconstruct graph times with `linspace(span)`.

### 8.5 Spatial identity

Any tensor genuinely indexed by spatial `primitive` or `light` identity must
attach the exact ordered IDs used by its evaluator/compositor. Positions,
attributes, graph-view rows, and candidate-output rows must share compatible
identities where they are combined or fail binding validation.

A graph tap may instead be broadcast (`n == 1`) or otherwise disagree with the
run's primitive count. The current provider publishes that dimension as an
unlabeled `Index("primitive")` axis rather than inventing fixture identity. Such
a tensor is valid for inspecting the signal, but it must not be joined to venue
positions or treated as one row per fixture until it has been explicitly
broadcast against a labeled primitive axis.

Shape equality without identity equality is insufficient.

---

## 9. One binding and artifact data plane

### 9.1 Manifest

All host data is described by one recursive manifest:

```rust
struct BindingManifest {
    schema_version: u32,
    revision: BindingRevision,
    scope: AnalysisScope,
    root: BindingValue,
    artifacts: BTreeMap<ArtifactId, ArtifactDescriptor>,
}

enum BindingValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    List(Vec<BindingValue>),
    Record(BTreeMap<String, BindingValue>),
    Tensor(TensorRef),
    Unavailable {
        reason: String,
        provenance: Option<Provenance>,
    },
}

struct TensorRef {
    artifact_id: ArtifactId,
    encoding: TensorEncoding,
    dtype: DType,
    shape: Vec<usize>,
    byte_offset: u64,
    axes: Vec<AxisSpec>,
    unit: Option<String>,
    read_only: bool,
    provenance: Provenance,
}
```

The manifest is an internal host/worker contract. It is not dumped into model
context.

### 9.2 Binding providers

Domain modules contribute through one builder API:

```rust
builder.inline("track", track_metadata)?;
builder.tensor("features.beats", beat_tensor)?;
builder.tensor("venue.positions", positions_tensor)?;
builder.record("track.clips", authored_clips)?;
builder.unavailable("audio.stems", "stem preprocessing has not completed")?;
```

Expected providers:

```text
agent_bindings/
  track
  audio
  features
  venue
  patterns
  graph_definition
  graph_run
```

Providers understand their domain sources but know nothing about Python.
The Python loader understands the manifest but knows nothing about SQLite,
graph compilation, or venue databases.

This is one system even though several domain providers contribute to it.

### 9.3 Artifact store

Large values live in one artifact store and are referenced by opaque IDs.

An artifact descriptor contains host-internal resolution information:

```rust
struct ArtifactDescriptor {
    id: ArtifactId,
    kind: ArtifactKind,
    encoding: String,
    byte_len: u64,
    content_hash: Option<String>,
    ownership: ArtifactOwnership,
}
```

The model never receives or chooses the host path.

Supported initial codecs:

- `raw_le`: headerless contiguous little-endian numerical data;
- `npy`: existing or newly written NumPy arrays;
- `pcm_f32`: existing Luma PCM cache with header/offset metadata;
- `png`: figures;
- bounded UTF-8 or JSON artifacts for explicitly exported proposals.

One mechanism does **not** mean one physical encoding. It means:

- one manifest;
- one artifact identity system;
- one loader;
- one permission policy;
- one lifecycle;
- one agent-facing namespace.

### 9.4 Reusing existing files

Existing caches should be imported without needless conversion:

- `.pcm` audio uses its existing 18-byte header:
  `version u32 LE | sample_rate u32 | channels u16 | len u64`, then `f32 LE`;
- MERT's `.fullmix.npy` and `.drum.npy` remain `.npy`;
- fresh Rust `Vec<f32>` values such as graph views can be materialized as
  headerless raw files plus complete tensor metadata;
- small values remain inline.

The Python materializer returns a uniform `LumaTensor` regardless of codec.

### 9.5 Workspace-visible inputs

Each workspace has an app-owned directory:

```text
agent-workspaces/<thread-id>/
  inputs/       read-only inside the sandbox
  scratch/      writable inside the sandbox
  outputs/      host-registered generated artifacts
```

The artifact store makes the current revision's inputs available under
`inputs/`, using hard links, reflinks, or copies as appropriate. The worker does
not receive arbitrary original music-library paths.

This allows a stable sandbox policy: read the workspace's input root and write
only its scratch/output roots.

### 9.6 Immutability and revisions

Binding revisions are immutable. Each cell pins exactly one revision for its
duration.

When app state changes, including after a successful track transaction:

1. providers assemble a new revision;
2. the next cell receives the new `luma`;
3. agent-created variables remain;
4. old artifacts remain leased while a cell or retained artifact references
   them;
5. unreferenced transient revisions can be collected.

The host tracks revisions for correctness. Routine model output does not include
revision IDs.

### 9.7 Unavailable versus empty

These must remain different:

```python
luma.features.drum_onsets["kick"]  # available tensor with length 0
```

versus:

```text
Unavailable("drum-onset preprocessing failed: …")
```

Providers must preserve source errors instead of converting every failure to
`None`, `{}`, or `[]`.

---

## 10. Concrete `luma` binding schema

### 10.1 Shared

```text
luma.meta
  schema_version
  revision                  debugging only
  agent_kind
  availability/catalog

luma.window
  start_s
  end_s

luma.track
  id
  title
  artist
  album
  duration_s
  bpm
  key
  beat_origin_s             absolute seconds of musical beat 0
  editable                  the owner may edit, check and apply
  document                  the complete saved score: clips by ID

luma.audio
  mix                       lazy AudioTensor
  stems
    drums                   lazy AudioTensor
    bass
    vocals
    other

luma.features
  beats                     event-time tensor
  downbeats                 event-time tensor
  bpm
  beats_per_bar
  drum_onsets
    kick                    event-time tensor
    snare
    hat
    cymbal
  bars
    indices
    starts_s
    ends_s
    intensity
    predictions             [bar, tag]
    tags
  chords
    starts_s
    ends_s
    root_pitch_class
  waveform_bands            [band, time]
  mel                       [frequency, time]
  mert                      optional [frame, feature] products

luma.venue
  id
  name
  fixtures
  pieces                    set design; flattened world pose per piece
  groups
  positions                 [primitive, xyz]
  uv                        [primitive, uv] rig-intrinsic pattern space
  views                     camera names render(view=...) accepts
```

### 10.2 Track agent

```text
luma.patterns
  summaries
  argument_schemas
```

The score does not live in a parallel namespace branch. It lives on
`luma.track`, because it is the thing being understood and edited. The Python
loader materializes the bound track record as the `GraphTrack` facade in
`luma_exec/score.py`. It does not create a second data source.

The graph branch is unavailable unless a graph run is placed in the thread's
scope.

### 10.3 Graph agent

```text
luma.graph
  definition
    nodes
    edges
    args
    arg_values
  run
    views                    name -> semantic tensor
    mel_views
    primitive_ids
    positions
    span
    fingerprint/provenance
```

The graph agent does not receive the whole authored timeline by default. Its
subject is one pattern and one preview instance.

### 10.4 Laziness

All artifact-backed tensors may materialize lazily. From the agent's
perspective, access remains ordinary:

```python
luma.audio.mix.values
luma.graph.run.views["view_signal_1"].values
```

The Python loader caches an opened artifact by immutable artifact ID. It uses
read-only mappings where beneficial and ordinary reads where mapping is slower.
The model does not choose the loading strategy.

---

## 11. Graph-run integration

### 11.1 What a run publishes

`crate::eval::graph_run::GraphEvaluation` is the complete result of a graph
run. The editor draws only part of it.

### 11.2 The run store

`agent_execution/graph_runs.rs` keeps the latest evaluation for each execution.
The next cell's binding assembly publishes it under `luma.graph.run` through the
normal binding and artifact system.

### 11.3 Compatibility

A graph-run contribution is usable only when these match the current scope:

- track ID;
- venue ID;
- span;
- graph hash;
- argument values;
- preview selection/instance seed when they affect output.

Otherwise `luma.graph.run` reports that the graph has changed since its latest
run. It must never silently pair a new audio/track scope with an old graph
tensor.

---

## 12. Source-data integration

### 12.1 Do not teach the executor cache paths

The executor must not reconstruct domain file paths. Domain services resolve
beats, stems, audio, roots, MERT, waveforms, venue data and scores, and the
binding providers call those services.

### 12.2 Share loaders with the evaluator, not its loading conditions

The evaluator's context loads only the data the current graph consumes. The
agent bindings must expose all available data, even when no graph references
it. Both must use shared domain-loading helpers. The executor must not depend
on evaluator-only loading conditions.

---

## 13. Workspace and kernel lifecycle

### 13.1 Minimal ownership model

The required registry is intentionally small:

```rust
struct PythonWorkspaceService {
    workspaces: HashMap<AgentThreadId, WorkspaceHandle>,
}
```

A workspace is created lazily. It is not tied to an open editor.

### 13.2 Live persistence

While the kernel remains alive, variables survive:

- multiple Python calls;
- multiple model turns;
- navigation away from the editor;
- returning to the same thread;
- changes to the current `luma` binding revision.

### 13.3 Durable workspace data

The current implementation persists:

- complete executed cell source through structured tool history;
- bounded cell outcomes in structured tool history;
- the workspace directory, including explicit scratch files and generated
  input/output bytes.

The in-memory artifact registry, leases, and kernel namespace do **not** restore
after an app restart. Reopening an existing workspace currently creates an
empty artifact registry even though old files may remain on disk, so those bytes
are not discoverable by their old artifact IDs. Durable artifact metadata,
transcript artifact references, and safe reconciliation/collection of files
already on disk are remaining hardening work.

Do not duplicate cell source into an unrelated second history system when the
structured thread already contains it.

### 13.4 App restart

An arbitrary live CPython namespace cannot be reliably serialized. Modules,
native objects, open files, iterators, closures, memmaps, background threads,
and C-extension state make generic pickle/dill persistence incomplete and
fragile.

The current semantics are:

- structured cells and workspace files remain beside the durable thread;
- a live kernel and its arbitrary namespace exist only while that process is
  alive;
- after app restart, a fresh kernel starts with no restored variables, imports,
  functions, open files, or artifact registry;
- a worker death detected by the still-running host produces a concise
  state-loss notice, but a full app restart is not yet detected as prior-state
  loss and therefore does not currently emit that notice.

Persist enough workspace metadata to distinguish a thread's first-ever kernel
from a post-restart kernel, then surface the same concise state-loss notice on
the first cell after restart. That is required hardening, not an implemented
guarantee today.

Best-effort cell replay may be added later, but it is not exact restoration and
must not be silently represented as such. True exact restoration would require
process/VM checkpointing and is outside this design.

### 13.5 New conversation and deletion

- New conversation: create a new thread and Python workspace. Never clear or
  reuse the old identity.
- Explicit Python reset: replace the process, not merely `globals().clear()`.
- Thread deletion: terminate the kernel and remove thread-owned scratch and
  unreferenced artifacts. Deletes are hard deletes and sync to other devices.
- Thread archive/navigation: may stop the live kernel later if a trustworthy
  restoration policy exists; no idle eviction is required initially.

Replacing the process on an explicit Python reset is important because code can
mutate module globals, matplotlib configuration, native-library state, and
background threads outside the user globals dict.

---

## 14. Worker architecture

### 14.1 Subprocess, not embedded Python

Run CPython as a subprocess.

Reasons:

- crash isolation from native extensions;
- an OS sandbox can wrap the whole process;
- process-group interruption and forced termination are available;
- no Python ABI/PyO3 packaging coupling;
- existing bundled Python and managed venv are reusable;
- subinterpreters are unsuitable for NumPy-heavy execution.

The existing one-shot Python workers prove interpreter provisioning and
dependency installation, not this persistent runtime. The new executor still
requires a worker protocol and lifecycle layer.

### 14.2 Python worker responsibilities

`luma_exec/worker.py` owns:

- persistent user namespace;
- installation of the current `luma` binding;
- manifest and artifact materialization;
- preloaded analysis libraries;
- notebook last-expression evaluation;
- stdout/stderr capture;
- traceback formatting;
- matplotlib figure capture;
- bounded representations;
- the generic synchronous host-call transport;
- one request loop.

It knows nothing about tracks, patterns, scores, venues, or SQLite.

### 14.3 Host process responsibilities

Rust owns:

- mapping thread IDs to workers;
- constructing binding revisions;
- artifact resolution and leases;
- sandboxed worker launch;
- request correlation;
- timeouts and cancellation;
- process-group signals;
- output and artifact quotas;
- crash detection and state-loss notification;
- installing an optional scoped, allowlisted host-call handler for a cell.

### 14.4 Internal wire protocol

Use newline-delimited JSON over pipes. This is an internal worker protocol, not
the model-facing tool result.

Conceptual request:

```json
{
  "id": "cell-17",
  "op": "exec",
  "code": "kicks = luma.features.drum_onsets['kick'].values\nlen(kicks)",
  "manifest_rel": "inputs/manifest-r-x.json"
}
```

The host owns timeout policy; it is not a worker- or model-selected request
field.

Conceptual frames:

```json
{"id":"cell-17","type":"started"}
{"id":"cell-17","type":"stream","stream":"stdout","text":"..."}
{"id":"cell-17","type":"stream","stream":"stderr","text":"..."}
{"id":"cell-17","type":"result","status":"ok","repr":"128","artifacts":[]}
```

`started` is a synchronization boundary, not progress decoration. The worker
emits it only after installing the requested binding revision and parsing the
cell, once its execution guard is active and immediately before entering user
code. Rust correlates it to the execution ID and withholds a cancellation
`SIGINT` until that acknowledgement. Without the handshake, a signal sent after
the request was written but before Python entered the guarded region could be
correctly ignored as a between-cell signal, after which the cancelled cell
would run indefinitely.

During an execution, a bound domain facade may make a synchronous internal host
call:

```json
{"id":"cell-17","type":"host_call","call_id":"h-1","method":"track.score_check","payload":{"candidate":{…}}}
{"id":"cell-17","op":"host_response","call_id":"h-1","ok":true,"value":{…}}
```

Calls are correlated to the active cell, bounded in count, and allowed only from
the cell's main thread. Rust answers each call through the handler installed for
that cell. Missing handlers and unknown methods return structured errors. The
transport is deliberately domain-free: it carries JSON method/payload/result
frames but owns no track policy.

Exactly one terminal result finishes a request. `started` is non-terminal and
must occur at most once for an execution. Interrupt is out-of-band via an OS
signal, never queued as an NDJSON command behind running code.

NDJSON was measured at tens of microseconds per trivial round trip. Jupyter,
ZeroMQ, Arrow RPC, or a socket protocol do not solve a problem present here.

### 14.5 Protocol stdout isolation

Agent `print()` and native libraries can write directly to file descriptor 1.
If protocol frames also use fd 1, those bytes can corrupt framing.

At worker startup:

```python
proto_fd = os.dup(1)
capture_r, capture_w = os.pipe()
os.dup2(capture_w, 1)
proto = os.fdopen(proto_fd, "w", buffering=1)
```

The private duplicate carries protocol frames. The redirected fd captures both
Python and native stdout. `contextlib.redirect_stdout` alone is insufficient.

Stderr needs equivalent deliberate treatment or a separate host-consumed pipe.

### 14.6 Displayhook

Parse the cell with `ast`. If its final statement is an expression:

1. execute the preceding statements;
2. evaluate the final expression;
3. render a bounded notebook-style representation.

Do not require `return`; models naturally write notebook-shaped Python.

Catch `BaseException`, not only `Exception`, so `KeyboardInterrupt` becomes a
result rather than killing the worker read loop.

### 14.7 Figures

Set:

```python
matplotlib.use("Agg")
```

before importing pyplot.

After a cell:

1. discover newly created figures;
2. save them into the thread output area;
3. close them;
4. register them with the artifact store;
5. return workspace-relative figure references to the host;
6. read bounded PNG bytes into the model-facing image parts.

The stored transcript keeps bounded base64 PNGs, not the registered artifact ID
(§7.6). **Not built:** storing artifact references and creating base64 only
when a provider request needs an image block.

### 14.8 Output limits

Bound:

- last-expression representation;
- stdout and stderr;
- traceback length;
- figure count and dimensions;
- total generated artifact bytes;
- scratch-directory size.

Truncate in the worker before constructing an enormous Python `repr` when
possible. Include an explicit truncation marker.

Never JSON-encode large numerical arrays for the model or transport. If the
agent evaluates a huge tensor as its last expression, show a dtype/shape and a
bounded NumPy-style summary.

---

## 15. Model-facing output

The model-facing result is notebook-native, not a diagnostic JSON envelope.

Depending on the cell, the model receives:

1. captured stdout;
2. captured stderr;
3. the last-expression representation;
4. a traceback when execution failed;
5. generated figures as image parts.

Example presentation:

```text
128
```

or:

```text
stdout:
selected threshold 0.418

{'median_lag_ms': 31.4, 'p90_lag_ms': 58.7}
```

or a traceback plus the output emitted before failure.

The host may retain internal metadata such as:

- execution ID;
- duration;
- binding revision;
- kernel generation;
- namespace delta;
- artifact IDs;
- whether a signal interrupt preserved state.

That bookkeeping is for orchestration, UI, diagnostics, and tests. It is not
dumped into the model context on every call.

Only exceptional state information becomes model-visible:

- “The Python kernel was restarted; variables from earlier cells were lost.”
- “The selected track/window changed; `luma` now refers to the new selection.”
- a specific sandbox denial the agent can recover from.

Even those should be concise prose, not a large status object.

Namespace discovery is available on demand through normal Python helpers such
as `dir()`, `globals()`, and `luma.catalog()`.

---

## 16. Cancellation and resource control

### 16.1 Cancellation contract

The model-turn abort signal propagates to the active Python cell and to any
host call currently serving that cell. A host handler inherits the cell's
deadline and cancellation token. Read-only calls and track validation remain
cancellable; dropping one of those futures drops its open transaction, so
uncommitted work rolls back.

`track.score_apply` has one explicit commit barrier. After the cancellable compile
and validation pass, the host atomically chooses whether cancellation or the
write begins first. If cancellation wins, no write starts. If the write wins,
Rust awaits the transaction through commit and flushes its correlated,
authoritative host response before releasing the barrier. A Stop received in
that interval remains pending and interrupts Python immediately afterward; it
does not shield the rest of the cell. The UI reloads the authoritative score
after every execution outcome, so a committed edit is observable even if the
pending interrupt lands before Python consumes the response.

The command claims its cancellation token before resolving scope or assembling
bindings, so stop covers the entire cell request rather than only time spent in
Python. If cancellation is already set when execution reaches the workspace,
the workspace returns `interrupted` without launching or touching Python. It
checks again after a cold worker has started, preserving that fresh namespace
for the next cell instead of killing it. A final check before writing the
`exec` request closes the remaining pre-dispatch gap.

Once an `exec` request has been written, cancellation remains pending until the
worker emits the matching `started` acknowledgement. Only then may Rust send
the cancellation `SIGINT`. Each execution has an ID, and both the start frame
and later result frames are correlated to it, so a late cancel or frame cannot
interrupt the following cell.

### 16.2 Escalation ladder

Spawn the worker in a new process group/session. Pre-execution cancellation
during binding assembly or worker startup follows the short-circuit path above:
no user code runs, no signal is needed, and the namespace survives.

For a cancellation after the matching `started` frame, or an execution timeout:

1. send `SIGINT` to the worker process group;
2. allow roughly two seconds for a `KeyboardInterrupt` result;
3. if received, preserve the kernel namespace;
4. otherwise kill the process group;
5. mark the namespace lost;
6. surface a concise loss notice on the next agent-visible interaction if the
   current turn is already gone.

Measurements on this machine showed that `SIGINT` interrupts Python loops,
sleep, and many NumPy operations while preserving state. A single long native C
call may delay signal handling until it returns, which is why forced kill
remains necessary.

### 16.3 Limits

The host currently enforces:

- wall-clock cell timeout;
- a shorter per-host-call ceiling inherited from the cell deadline;
- host-call count and serialized-payload caps;
- a maximum complete-candidate clip count and byte size, checked before taking
  SQLite's write reservation;
- output byte caps;
- figure/artifact caps;

Production additionally requires:

- a hard aggregate scratch/workspace byte quota;
- a child-process count limit;
- aggregate memory and CPU controls for the worker process tree;
- GPU restrictions if libraries expose uncontrolled GPU allocation.

Those process-tree and storage controls are not implemented in v0. The macOS
filesystem/network sandbox and wall-clock cancellation materially constrain
authority, but they do not prevent a fork bomb, memory exhaustion, or filling
the allowed workspace. Until the remaining limits are enforced and tested, the
executor is a developer/v0 facility and must not be represented as
production-safe.

The initial wall-clock ceiling may use 90 seconds, matching comparable code
execution tools, and can be tuned from real agent traces.

---

## 17. Sandbox and host-capability security

### 17.1 Code execution is an authority boundary

Arbitrary Python execution is intentionally remote code execution by the model.
The question is what authority that code receives.

Immutable bindings are not sufficient. Unsandboxed Python can still:

- read or delete arbitrary user files;
- read Luma databases and credentials;
- inspect inherited environment secrets;
- open sockets;
- launch subprocesses;
- persist modifications outside Luma;
- exhaust resources.

Network denial alone is not enough. Code could read `~/.ssh/id_rsa`, print it,
and the tool result would be sent back through the remote model request. The
model/tool channel is itself an egress path.

### 17.2 Production capability policy

Allow:

- read/execute the bundled interpreter and managed venv;
- read the current workspace's explicit `inputs/`;
- write only the current workspace's `scratch/` and controlled output area;
- return bounded text and registered artifacts to the host.
- invoke only the narrow named host methods installed for the current trusted
  thread scope. An exact score/track/venue scope permits `track.score_render`;
  `track.score_check` and `track.score_apply` additionally require
  authenticated owner authority.

Deny:

- network;
- home-directory reads;
- unrelated track inputs;
- app databases, app config, auth material, and credentials;
- writes to the interpreter, venv, inputs, app, PATH entries, or settings;
- arbitrary subprocess execution where analysis libraries do not require it;
- environment inheritance beyond a small allowlist.

The sandbox must resolve symlinks and must not let a writable path widen the
policy.

### 17.3 Application mutation boundary

Python receives copies or read-only mappings, never live mutable Rust
objects. Even if agent code mutates its local Python object, the host reinstalls
the canonical `luma` binding on the next cell. Ordinary Python assignments do
not change application state.

The track facade is a capability object, not a live model object. Its candidate
is local Python data. Only `check`, `render`, and `apply` send a complete
candidate through the worker's synchronous host-call seam. The host handler is
constructed from durable-thread scope and authenticated user state; the model
cannot select a different track, venue, score, user, or workspace, install a
handler, or widen its method allowlist. A validated exact track scope installs
read-only rendering even when the current user is not the score owner. The
separate edit capability exists only for the authenticated owner, and both
`check` and `apply` reject calls without it. The transaction service
independently rechecks scope and ownership, so the descriptive
`luma.track.editable` bit is never the security boundary.

The leading underscore on `_luma_host_call` is API hygiene, not protection:
arbitrary executed code can call it directly. Therefore every handler treats
its method and payload as hostile input, validates the complete candidate, and
derives all authority server-side.

Graph proposals and any future capabilities require their own explicit,
independently testable host policy. The worker protocol remains domain-neutral;
it routes a named request to an installed handler and grants nothing when no
handler is present.

### 17.4 macOS

`agent_execution/sandbox/macos.rs` runs the worker under a Seatbelt profile via
`sandbox-exec`:

- no network;
- explicit read/execute roots;
- per-workspace read/write roots;
- environment scrub;
- process restrictions.

`sandbox-exec` is deprecated but has no published removal timeline or supported
replacement for this desktop use case. Profile generation stays behind the
swappable `WorkerLauncher`.

Packaging considerations:

- the bundled Python executable and native extensions must work under hardened
  runtime/notarization;
- library-validation entitlements apply to the child executable, not merely the
  parent app;
- do not convert the whole app to App Sandbox as a shortcut;
- use the non-GUI Matplotlib backend.

### 17.5 Linux

**Not built.** The design is Landlock for filesystem restrictions and seccomp
for syscall and network restrictions, launched directly from Rust, with no
bubblewrap, Docker, root setup or AppArmor configuration.

Today `sandbox::default_launcher` returns the passthrough launcher on Linux.
Agent Python runs with the app's full authority.

### 17.6 Windows

**Not built.** The design is an AppContainer/job-object sandbox that meets the
same policy without an elevated developer setup. WSL is not acceptable. Today
Windows also gets the passthrough launcher.

### 17.7 Failure behavior

On macOS, sandbox initialization failure is a hard stop for the Python tool. It
never warns and continues unsandboxed. The passthrough runs on macOS only in a
debug build with `LUMA_UNSANDBOXED_PYTHON=1`.

On Linux and Windows this rule does not hold yet (§17.5, §17.6).

---

## 18. Score authoring in Python

The agent does not read or edit a score file. The complete saved score is part
of the `luma.track` binding, and the only authoring surface is a staged Python
candidate (`backend/python/luma_exec/score.py`):

```python
edit = luma.track.edit()
form = "color.chase@1"
inputs = {key: spec["default"] for key, spec in luma.track.definition(form)["inputs"].items()}
inputs["width"] = 0.4
clip = edit.add_clip(form, bars=(49, 57), selection="front_wash", inputs=inputs)
edit.update_clip(clip, bars=(49, 65))

edit.diff()
edit.check()
luma.venue.render(t=120.0, edit=edit)
edit.apply()
```

### 18.1 Complete snapshot

`luma.track.document` holds every clip, keyed by stable ID. Timestamps, ownership and sync columns are not part of it.

### 18.2 Staged candidate

`luma.track.edit()` works only when `luma.track.editable` is true: the host
resolved owner authority for the exact score, track and venue. The edit copies
the complete saved score. All changes stay local until `apply()`.

- Exactly one of `beats=(start, end)`, `bars=(start, end)` or
  `seconds=(start, end)` gives a range. Ranges are half-open. Beats start at 0.
  Bars start at 1 and follow the detected downbeats.
- `add_clip(form, ...)` takes a form ID, plus `selection`, `z`, `blend`,
  `seed` and `inputs`. `inputs` holds a value for every input of the form.
- `update_clip` changes only the fields it is given. `remove_clip` removes one
  clip.

### 18.3 Views and preview

`edit.window(...)` and `luma.track.window(...)` take the same explicit range and
return an immutable view. `luma.venue.render(edit=edit)` renders the
uncommitted candidate through the real compositor.

Candidate output is sampled at 16 samples per beat, capped at 2,048 samples
(`agent_execution/track_host.rs`).

### 18.4 Check and apply

`edit.check()` sends the complete candidate to the host (`track.score_check`).
The host re-resolves scope from the durable thread, validates the score and
prepares its clips. It changes nothing.

`edit.apply()` (`track.score_apply`) repeats the checks and writes the
candidate. A root thread writes the live score rows; a subagent writes its
draft (§5.6). Only changed rows are written. There is no base revision, so an
apply overwrites concurrent changes to the rows it touches. `apply()` closes the
edit. The next cell sees the refreshed `luma.track` and keeps the agent's
variables.

### 18.5 The score document

`luma.track.source()` returns the saved score as canonical JSON text. It is a
view of the rows, not a stored file.

---

## 19. Subagents

A subagent is a turn on a child thread that edits a draft of the score. See
`docs/design/subagents.md`. Children never share a Python namespace, because
each child thread owns its own workspace.

---

## 20. Component boundaries

```text
backend/src/agent_execution/
  mod.rs
  workspace.rs             thread -> workspace registry
  worker_process.rs        protocol, process, interrupt
  worker_launcher.rs       launcher trait and passthrough launcher
  cell_host.rs             host-call routing for one cell
  track_host.rs            scoped score check, render and apply
  track_host/score.rs
  venue_host.rs            scoped venue queries and edits
  graph_runs.rs            latest graph evaluation per execution
  headless_env.rs          worker environment for headless hosts
  bindings/                manifest, assembler, domain providers
  artifacts/               store and codecs
  sandbox/                 platform launcher selection; macos.rs

backend/python/luma_exec/
  worker.py                persistent cell loop and generic host-call bridge
  bindings.py              manifest -> Python namespace
  track.py                 track metadata, clips and output views
  score.py                 score facade: GraphTrack, Edit, graphs
  venue.py                 venue facade
  figures.py, display.py   figure capture and notebook display

backend/src/agent/tools/python.rs   the model-facing tool
backend/src/services/drafts.rs      subagent drafts
```

The dependency direction is:

```text
domain providers -> binding manifest -> worker
artifact store -----------------------^

workspace registry -> worker process -> launcher/sandbox

score / venue facades -> generic host call -> scoped track / venue host
                                                   |
                                                   v
                                     score rows or draft, compositor

agent tool -> workspace service
```

No UI dependency is needed inside:

- manifest validation;
- artifact codecs;
- Python protocol implementation;
- worker process state machine;
- sandbox profile generation.

The Python facades are testable with ordinary binding values and a fake
synchronous host callback. The generic worker protocol knows nothing about
tracks, and the track host knows nothing about model messages. Dispatch
handlers and the agent tool are thin adapters.

### 20.1 Headless hosts

Two binaries serve this data plane to a process that is not the desktop app.
Both are thin adapters over `luma_lib::dispatch`. Both boot through
`luma_lib::headless_host` with the same flags (`--config-dir`,
`--fixtures-root`, `--cache-dir`, `--fixture-principal`) and the same
migrations. Events go to stderr, because both put their protocol on stdout.

| | |
|---|---|
| `backend/src/bin/agent_harness.rs` | one JSON dispatch request per line |
| `backend/src/bin/luma-mcp.rs` | MCP over stdio, so an out-of-process coding agent gets the `python` tool itself |

`luma-mcp` tools:

```text
find   {track?, venue?}                   matching tracks and venues
open   {track_query|track_id, venue_id?}  the bound namespace's catalog
open   {venue_id}                         the same, for a room with no track
python {code}                             stdout/repr/traceback + figures
reset  {}                                 a fresh workspace and kernel
cancel {}                                 interrupt the cell in flight
skill  {name}                             one lighting-craft playbook
```

An MCP client has no editor to read a track from, so `open` pins one durable
agent thread to the resolved scope, and every later call addresses that thread.
`python` takes only code.

`python`'s description is `PYTHON_TOOL_DESCRIPTION`, and its result uses the
same projection as the in-app tool, clamping and figure budget included.

The loop is concurrent, one task per request, because `cancel` must reach a
`python` that is still in flight. Framing, `initialize`, `ping`, `tools/list`
and `prompts/*` live in `backend/crates/mcp-stdio`.

The sandbox is not a flag on these hosts. They resolve the worker environment
through `agent_execution::headless_env`, so `sandbox::default_launcher` decides
(§17.7).

Register the server in a local `.mcp.json`:

```json
{ "mcpServers": { "luma": { "command": "./backend/target/debug/luma-mcp", "args": [] } } }
```

Add `["--config-dir", "/path/to/scratch"]` to work against a disposable copy of
the library. `scripts/headless/README.md` lists the scripts that drive it.

---

## 21. Testing requirements

### 21.1 Durable threads

- two threads for one track have different IDs and workspaces;
- full tool history survives round-trip persistence;
- New Conversation keeps the old transcript and allocates a distinct thread
  and Python workspace;
- the first cell after a kernel death reports that the prior namespace was
  lost;
- thread deletion cleans up its workspace.

### 21.2 Authored state

- `track.score_check` changes no row;
- `track.score_apply` writes only changed rows;
- a failed apply leaves every row unchanged;
- a subagent's apply writes its draft, not the live rows.

Sync behaviour belongs to `docs/design/sync.md`.

### 21.3 Binding manifest

- schema-version checks;
- deterministic provider merge;
- duplicate-path rejection;
- shape and byte-length validation;
- axis count/shape validation;
- unit and time-origin validation;
- unavailable versus empty values;
- provenance preservation.

### 21.4 Semantic alignment

- graph view times exactly match evaluator times;
- primitive-indexed graph views exactly match evaluator ID ordering;
- broadcast or mismatched graph taps expose an unlabeled index axis and cannot
  be mistaken for venue-aligned rows;
- positions and labeled views reject mismatched primitive identities;
- audio and graph time axes correlate in absolute seconds;
- mel and band axes cover their real data dimensions.

### 21.5 Artifact codecs

- PCM header/offset/sample-rate/channel parsing;
- NPY loading;
- raw little-endian dtype/shape loading;
- read-only NumPy mappings;
- unaligned PCM offset correctness;
- input path ownership;
- artifact leases and cleanup;
- current bounded-base64 figure persistence and oversized-figure placeholders;
- durable artifact registry/reference restoration once artifact-only transcript
  persistence is implemented;
- path traversal and symlink rejection.

### 21.6 Kernel semantics

- variable persists across cells;
- `luma` changes across binding revisions while user variables persist;
- reassigning `luma` is repaired before the next cell;
- each cell that reaches user-code entry emits exactly one correlated `started`
  frame after binding installation and syntax parsing, before any user
  statement;
- last-expression display;
- stdout and stderr capture;
- native fd-level stdout cannot corrupt protocol frames;
- exceptions preserve prior stdout;
- figures are captured and closed;
- huge representations are bounded.

### 21.7 Interruption

- cancellation during async binding assembly short-circuits before Python and
  preserves the existing namespace;
- cancellation during cold worker startup prevents the cell from running and
  leaves the freshly started kernel reusable;
- for an executable cell, cancellation after an `exec` write but before
  `started` is held until the matching acknowledgement, then interrupts the
  intended cell;
- Python loop interrupted with namespace preserved;
- sleep interrupted;
- representative NumPy operation interrupted where CPython permits;
- forced timeout kills the process group;
- forced kill produces a state-loss notice;
- late cancellation cannot hit the next execution;
- cancellation before the track commit barrier prevents the write;
- cancellation during a track commit waits for the authoritative host response
  and then interrupts any remaining cell code without losing the kernel;
- model-turn stop propagates to the cell.

### 21.8 Sandbox

Platform acceptance tests must prove:

- current input artifacts are readable;
- scratch is writable;
- app databases are unreadable;
- home credentials are unreadable;
- input artifacts are not writable;
- network access fails;
- environment secrets are absent;
- disallowed subprocesses fail;
- denial errors identify the rejected capability.

### 21.9 Score candidate and apply

- the bound score round-trips exact clip IDs, graphs, beat ranges, `z`, blend
  mode, seeds and input values;
- `add_clip`, `update_clip` and `remove_clip` change only the local candidate;
- beat, bar and second ranges are half-open and require exactly one form;
- a window stays immutable after later candidate changes;
- `check()` changes nothing and reports preparation failures;
- a read-only scope can render but cannot create an edit or apply;
- caller-supplied scope cannot retarget the durable thread;
- a real worker-to-host-to-SQLite test applies a candidate, and the next cell
  sees the refreshed track while prior Python variables remain.

---

## 22. Acceptance criteria

1. Every agent operates on durable thread IDs and structured tool history.
2. Every agent exposes the same `python` tool contract.
3. A variable defined in one cell is usable in a later turn of the same thread.
4. A different thread cannot access that variable.
5. `luma` refreshes after score, graph, selection or analysis changes without
   clearing agent variables.
6. The agent can compute directly over precomputed drum onsets.
7. The agent can compute over the audio mix or any stem.
8. Graph tensors include exact time and channel axes. Primitive-indexed views
   carry exact ordered IDs; broadcast or mismatched taps are explicitly
   unlabeled.
9. Venue positions align only with labeled primitive identity, not merely row
   count.
10. The agent can produce and see a Matplotlib figure.
11. The model-facing result is notebook-native rather than a bookkeeping JSON
    object.
12. No large numerical array crosses through JSON lists or permanent base64.
13. Graph, audio and feature inputs use one binding/artifact mechanism.
14. `luma.track` holds the complete saved score and the editability bit. There
    is no parallel timeline branch.
15. Check is non-mutating and uses the authoritative current scope.
16. Score mutation scope and ownership come from the durable thread and the
    trusted host, never from model-selected IDs.
17. Python has no generic application mutation, database or filesystem
    authority beyond explicitly installed host capabilities.
18. A new conversation cannot inherit the prior thread's Python namespace.
19. Cancellation covers binding assembly, cold startup, dispatch, host calls
    and running user code. Pre-execution cancellation runs no user code and
    preserves the namespace. `SIGINT` is sent only after the matching `started`
    acknowledgement. Forced process death reports state loss.
20. Execution cannot read home or app secrets, write outside scratch, or reach
    the network. **Met on macOS only** (§17.5, §17.6).
21. Sandbox failure disables the tool rather than running with broader access.
    **Met on macOS only** (§17.7).
22. **Not met:** figure transcripts keep durable artifact references instead of
    stored base64, and those references replay after app restart (§7.6,
    §14.7).
23. **Not met:** artifact metadata is restored after app restart, and the first
    new kernel after a restart reports loss of the prior namespace (§13.3,
    §13.4).

---

## 23. Current assets and measured constraints

These findings informed the design and should prevent repeated research.

### 23.1 Existing Python environment

Luma already ships:

- bundled CPython 3.12;
- a managed venv under the app cache;
- interpreter validation and environment creation;
- requirements hashing and cached installation;
- machine-aware PyTorch installation;
- one-shot workers for beats, roots, stems, MERT, drum onsets, and bar
  classification;
- NumPy, SciPy transitively, librosa, matplotlib, soundfile, Demucs, and the
  other preprocessing dependencies.

This removes interpreter/dependency bootstrap work. It does not remove the need
for the new persistent worker, data plane, or sandbox.

### 23.2 Data already available

- `track_beats`: beats, downbeats, BPM, downbeat offset, beats per bar;
- `track_drum_onsets`: kick, snare, hat, and cymbal times;
- `track_bar_classifications`: per-bar intensity and tag probabilities;
- `track_roots`: chord-section start/end/root;
- `track_stems`: drums, bass, vocals, other;
- `track_waveforms`: low/mid/high band envelopes;
- track mel spectrogram generation;
- MERT full-mix and drum arrays;
- graph run views, mel views, universe state;
- evaluator positions and primitive ordering;
- current graph definition and pattern arguments;
- timeline annotations, pattern summaries, and argument schemas;
- venue fixtures, stage pieces, and group data.

### 23.3 Import timings measured on this machine

Warm page cache, bundled app venv:

| Operation | Approximate wall time |
|---|---:|
| Bare interpreter | 0.01 s |
| `import numpy` | 0.05 s |
| `import scipy.signal` | 0.51 s |
| `import matplotlib.pyplot` | 0.28 s |
| `import librosa` | 0.02 s |
| first `librosa` hot attribute use | about 0.90 s |
| `import torch` | about 0.84 s |

Librosa uses lazy loading. Prewarming must touch the attributes the agent is
likely to use; importing the top-level package alone merely moves the delay.

### 23.4 Do not fork-prewarm

Forking after importing pyplot crashed deterministically on macOS due to ObjC
runtime initialization. Native numerical libraries may also initialize thread
pools that are unsafe after fork.

Spawn a fresh persistent process and pay the roughly one- to two-second warmup
once. Do not use a preloaded fork server.

### 23.5 Array transport measurements

For a 20 MB float32 array:

| Transport | Measured cost/size |
|---|---|
| JSON list | about 1.87 s, 103 MB |
| base64 raw bytes | about 22 ms, 26.7 MB |
| `np.save` | about 3.3 ms, 20 MB |
| `np.load` plus sum | about 4.2 ms |
| mmap open | about 8.7 ms |

Never use JSON lists for large arrays. Mmap is not automatically faster for
small/medium arrays; the loader may choose full read versus mapping.

### 23.6 Interrupt measurements

`SIGINT` delivered to a worker process group:

- interrupted a Python infinite loop at about 0.5 seconds;
- interrupted `sleep`;
- interrupted repeated matrix multiplication while preserving prior state;
- was delayed by about two seconds during one long native matrix multiply.

This supports the SIGINT-then-kill policy.

---

## 24. Deferred product capabilities

The following are compatible with this foundation but are not part of the core
executor:

- real-renderer stage filmstrips;
- deterministic rig-safety linting;
- agent access to evaluator goldens;
- reference-video-driven authoring;
- persistent pattern memory;
- live StageLinQ/ProDJ Link operation;
- multi-agent section authoring and judging.

They should consume the same durable threads, binding revisions, artifact
store, and scoped host-capability boundary rather than creating parallel
systems.

---

## 25. Reference sources from the research

Primary/comparable systems:

- Anthropic code execution:
  <https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool>
- Claude Code sandboxing:
  <https://code.claude.com/docs/en/sandboxing>
- Anthropic sandboxing engineering:
  <https://www.anthropic.com/engineering/claude-code-sandboxing>
- Anthropic sandbox runtime:
  <https://github.com/anthropic-experimental/sandbox-runtime>
- Cursor agent sandboxing:
  <https://cursor.com/blog/agent-sandboxing>
- E2B persistence:
  <https://e2b.dev/docs/sandbox/persistence>
- Open Interpreter safety:
  <https://docs.openinterpreter.com/safety/introduction>

Threat model and platform references:

- The lethal trifecta:
  <https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/>
- Landlock:
  <https://docs.kernel.org/userspace-api/landlock.html>
- AppContainer:
  <https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-for-legacy-applications->
- Apple containerization sandbox question:
  <https://github.com/apple/containerization/issues/737>

Python/runtime references:

- Jupyter messaging:
  <https://jupyter-client.readthedocs.io/en/latest/messaging.html>
- Python shared memory:
  <https://docs.python.org/3/library/multiprocessing.shared_memory.html>
- PEP 684:
  <https://peps.python.org/pep-0684/>
- IPython display behavior:
  <https://ipython.readthedocs.io/en/stable/api/generated/IPython.core.interactiveshell.html>
- RestrictedPython's own scope warning:
  <https://restrictedpython.readthedocs.io/>
