# Recording a score as video

Status: **built** for the CLI slice — see §8 for what landed, what was measured,
and what the measurements changed, and §9 for the second sampling mode. §1–§7
are the original design; where a later section disagrees with an earlier one,
the later one is what the code does.

Original scope note: Scope: one new module in `src-tauri/src`, one small change to
`stage_render`, one change to `ffmpeg_env`, three thin callers.

`docs/specs/wgpu-renderer.md` §6 already specified this. **Half of it landed**: the
renderer side — deterministic K-subframe jitter accumulation with the temporal pass
bypassed — is exactly what `Renderer::render(frame, w, h, subframes)` does today, and
`DEFAULT_SUBFRAMES = 16` is that spec's `K`. What did not land is the orchestration:
the time axis, the ffmpeg pipe, the audio mux, and the callers. This doc is that half,
and it supersedes §6's `RenderTarget` / `FrameRequest` sketch, which the renderer
implemented under different names (`Destination`, `Channels`).

---

## 1. What already exists

```
track_scores ⋈ scores (filtered by venue_id)
  → compositor::build_scene_strict(...)             compile once; PLAN_CACHE + bake_prologue
  → eval::Scene
  → Scene::render(&times, Scope::Composite, &mut Arena) -> Vec<UniverseState>
  → stage_render::primitive_state(state, id, head)
  → luma_render::build_frame_with(scene_desc, definitions, source, t, &mut assets) -> Frame
  → Renderer::render(&frame, w, h, subframes) -> Vec<u8>   sRGB RGBA8, packed
```

Every link is already public and already used by `venue.render`. Three properties make
recording nearly free to build on top:

- **`Scene::render` takes an arbitrary `&[f32]`.** Its own doc says a dense grid is a
  bake. Evaluation is a pure function of absolute `t` — ADSR trigger lists, random
  seeds, filter state and audio windows are all baked at compile or recomputed from
  `t`, so no frame needs a predecessor. Frames may be evaluated in one batched call.
- **The renderer is already reusable.** `Renderer::targets` reallocates only when
  `(width, height, destination, haze_size)` changes; at a fixed recording resolution
  nothing is reallocated per frame. Device, pipelines, shaders live in the process-wide
  `Gpu::shared()`; parsed GLBs live in a caller-owned `assets::Library`.
  `stage_render`'s pinned `luma-stage-render` thread already owns both for the life of
  the process.
- **ffmpeg is bundled and alive.** `build.rs` downloads it, `tauri.conf.json` ships
  `ffmpeg-runtime/*`, `ffmpeg_env::ffmpeg_path()` finds it. The local binary has
  `libx264`, `h264_videotoolbox`, `aac` and `aac_at`.

**The old export path is not in this tree.** `src-tauri/src/commands/export.rs` lives
only on `feat/video-export` (`4d39a707`), which is not an ancestor of `main` and never
merged. Nothing on main references it. §5 says what to take from it.

---

## 2. Where the time actually goes

`venue.render` warm at 960×540 costs ~150 ms. Almost none of that is the GPU.

The renderer's cost model, from the code: `subframes` loops **only the haze pass**
(`gpu.rs:2971`); depth, scene, shadow, temporal and composite run once. So haze
≈ `subframes × haze_steps × (haze_scale² × pixels)` and everything else is paid once.
Offscreen uses `HAZE_RESOLUTION = 1.0` and `subframes = 16`; live uses `0.5` and `2`.
That is 4× the march invocations and 8× the passes — 32× the live haze cost.

Calibrating against `docs/design/volumetrics-v2.md` §1.2 (M3 Max, 1920×1080,
subframes=2, haze_resolution 0.5, haze_steps 8):

| cones | measured `gpu_volumetric` p50 | ⇒ per subframe @1.0 res, 1080p | ⇒ ×16 subframes |
|---|---|---|---|
| 32 | 0.22 ms | 0.44 ms | ~7 ms |
| 128 | 0.78 ms | 1.56 ms | ~25 ms |
| 512 | 3.05 ms | 6.10 ms | ~98 ms |

At 960×540 (a quarter of the pixels) divide by four: ~6 ms of haze at 128 cones. Add
the once-per-frame passes (~1–4 ms), `build_frame_with` CPU (~1–3 ms), and readback.
**Predicted GPU+CPU cost of one 960×540 frame is ~10 ms, not 150 ms.**

The other ~140 ms is scene assembly, paid *per call* by `venue_host::state_at` and
`VenueGeometry::load`:

- `VenueGeometry::load` re-reads every patched fixture and stage piece and re-parses
  every `.qxf` fixture definition from disk;
- `build_scene_strict` re-reads the score rows and rebuilds each annotation's resident
  context (`PLAN_CACHE` saves the compile, not the DB reads or the audio residency);
- `Job` clones the whole `scene_desc::Scene` + definitions map into the render thread;
- `render_png` deflates a 960×540 RGBA buffer (~10–30 ms) that a video pipe does not
  want at all.

**A recording session pays all of that once.** That is the entire performance argument
for the seam, and it is worth more than any renderer tuning.

One measured caveat: `PendingFrame::complete_blocking` deliberately polls with
`PollType::Poll` + `sleep(1ms)` rather than `Wait`, to avoid starving live workers on
the shared device. A 6 ms frame therefore pays up to ~1 ms of sleep granularity. Fine
at 1080p, ~15% overhead at 540p.

### Throughput (M3 Max, ~128 cones, predicted — see §6)

| | subframes | ms/frame | fps | 5-min track |
|---|---|---|---|---|
| 540p30 | 16 | ~10 | ~100 | ~1.5 min |
| 540p30 | 8 | ~7 | ~140 | ~1 min |
| 1080p30 | 16 | ~33 | ~30 | ~5 min |
| 1080p30 | 8 | ~20 | ~50 | ~3 min |
| 1080p30, 512 cones | 16 | ~105 | ~10 | ~15 min |

x264 `veryfast` at 1080p is ~2–4 ms/frame but runs in a separate process draining the
pipe, so it overlaps the GPU and does not add to the frame time until it is the
bottleneck. `h264_videotoolbox` removes even that.

**Do not build a pipelined recorder.** `viewport.rs`'s `PRESENTATION_SLOTS` comment
records the experiment: there is no async compute in this wgpu/Metal path, one queue,
serial dispatch — depth cannot raise throughput when the GPU is the bottleneck, only
latency. A recording is throughput-bound by definition. Render serially.

---

## 3. The seam

**One module: `src-tauri/src/recording.rs`**, sibling to `stage_render.rs`.

`stage_render` is "one venue, one moment, one PNG". Recording is a different
abstraction — a *time axis* and a *container format* — so it is a different module,
not a longer `Shot`. It sits in `src-tauri/src` and not in a crate because everything
it composes (`compositor`, `eval`, `database::local`, `ffmpeg_env`, `stage_render`)
already lives there; a crate would have to drag the database upward.

The module owns, and no caller sees:

1. resolving `(track_id, venue_id)` → score rows → one `eval::Scene`, once;
2. loading `VenueGeometry` and installing the `scene_desc::Scene` on the render thread, once;
3. the time axis: `fps`, span clamping to track duration, frame count, shutter;
4. batched `Scene::render(&times_block, Scope::Composite, &mut arena)`;
5. the frame loop and the RGBA→ffmpeg pipe;
6. spawning ffmpeg, muxing the track audio, and reporting its stderr on failure;
7. progress and cancellation.

**The loop lives inside the seam.** Every caller only starts a recording and polls it.
This is the one structural lesson from the old export: it put the loop in the frontend
and therefore needed five Tauri commands, a raw-body IPC hack, and a batch-of-600
amortisation, all of which are pure consequences of that choice.

### The one change to `stage_render`

Today `Job` carries `scene` and `definitions` **by value on every frame**. For a
9000-frame recording that is 9000 clones of the whole venue description. Add a resident
handle on the pinned thread:

```rust
/// A venue installed on the render thread, ready to be lit repeatedly.
pub struct Sequence { /* opaque; drop uninstalls */ }

impl Sequence {
    /// Install `scene` at a fixed size and view. The camera is fitted once —
    /// framing does not depend on t.
    pub fn install(
        scene: scene_desc::Scene,
        definitions: BTreeMap<String, scene_desc::Definition>,
        meshes_root: PathBuf,
        view: View, booth: Option<Vec3>, size: (u32, u32),
    ) -> Result<Self, String>;

    /// One frame, tightly packed RGBA8. Blocks.
    pub fn frame(&self, state: Option<&UniverseState>, t: f32, subframes: u32)
        -> Result<Vec<u8>, String>;
}
```

Keep the per-frame channel round-trip (it is microseconds against 10 ms, and it keeps a
`venue.render` from an agent able to interleave rather than being starved for minutes).
`render_rgba` becomes `Sequence::install(...).frame(...)` — one implementation, not two.
`subframes` becomes a parameter instead of the hardcoded `DEFAULT_SUBFRAMES`.

### The change to `ffmpeg_env`

`ffmpeg_env::init(app: &tauri::AppHandle)` cannot be called by a headless bin, so
`ffmpeg_path()` silently falls back to system PATH there — which on a clean machine
fails at spawn time with no diagnosis. Split it: `init_from(dirs: &[PathBuf])` holds
the search, `init(app)` and `headless_host::boot` each supply their dirs. Small, and
required for the CLI to work at all.

---

## 4. The API

### Rust

```rust
// src-tauri/src/recording.rs

/// What to record. Everything else is derived from the library.
pub struct Recording {
    pub track_id: String,
    pub venue_id: String,
    pub view: View,
    /// `None` records the whole track.
    pub span: Option<(f32, f32)>,
    pub size: (u32, u32),
    pub fps: u32,
    /// The one quality dial: jitter subframes and encoder crf move together.
    pub quality: Quality,      // Draft | Standard | Final
    pub audio: bool,
    pub output: PathBuf,
}

pub struct Progress { pub frame: u64, pub total: u64, pub elapsed: Duration }
pub struct Recorded { pub path: PathBuf, pub frames: u64, pub duration: f32 }

/// Render `spec` to an mp4 with the track's audio.
///
/// # Errors
/// A missing track or score, a venue with nothing in it, a missing audio file,
/// no GPU, or a non-zero ffmpeg exit (whose stderr is in the message).
pub async fn record(
    pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    spec: &Recording,
    cancel: &CancelFlag,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Recorded, RecordError>;
```

ffmpeg argv (fixed by the module, never by a caller):

```
ffmpeg -y
  -f rawvideo -pix_fmt rgba -s {W}x{H} -r {fps} -i pipe:0
  [-ss {span.start}] -i {audio_path}
  -map 0:v:0 -map 1:a:0
  -c:v h264_videotoolbox -b:v {rate}      # libx264 -crf {crf} -preset veryfast elsewhere
  -pix_fmt yuv420p -colorspace bt709
  -c:a aac -b:a 192k -shortest -movflags +faststart
  {output}
```

`t` is file-relative — `host_audio::render_time()` is the audio file's own position —
so no offset is needed; only a non-zero `span.start` needs `-ss` on the audio input.

### Python

`figures.register` accepts only `outputs/<name>` PNG figures, so a recording is not a
figure. It returns a plain handle.

```python
job = luma.venue.record(view="front", fps=30, width=1920, height=1080)
job.wait()                     # polls; prints frames/total
job.path                       # workspace-relative path under outputs/
```

`MAX_HOST_CALL_DURATION` is 30 s and `MAX_HOST_CALLS_PER_CELL` is 64, so a full-track
recording **cannot** be one blocking host call. `record` starts a `tokio::spawn`ed job
in `VenueHost` keyed by uuid and returns `{jobId, totalFrames}`; `venue.record_status`
and `venue.record_cancel` are the other two methods. `wait()` sleeps between polls, so
a five-minute recording costs a handful of calls, not thousands. `luma.venue` is the
right binding because it already carries both the venue and the track scope
(`lighting: TrackScope`) and is already where the cameras live.

### MCP

**No new tool.** `luma-mcp` exposes `find / open / python / reset / cancel / skill`;
anything reachable from the kernel is reachable from MCP already. Adding a `record`
tool would be a second way to do one thing.

### CLI

`src-tauri/src/bin/luma-record.rs`, bootstrapping exactly like `agent_harness.rs`:

```rust
let config = HostConfig::parse_args(shared_flags)?;
let services = boot(&config).await?;
```

```
luma-record --track <id|query> [--venue <id>] --out <file.mp4>
            [--view front] [--fps 30] [--size 1920x1080] [--quality final]
            [--from 30 --to 90] [--no-audio]
luma-record --all --out-dir ./renders [same options]
```

`--all` enumerates every score in the library and records each in turn — one GPU, so
serially. Progress to stderr, one line per file, because stdout stays clean for a
manifest of what was written. This is the "render every score `claude -p` authored"
path: the authoring agent writes scores, the CLI renders them all.

### App

A dispatch handler `dispatch/handlers/recording.rs` exposing `record_video` /
`record_status` / `record_cancel`, progress via the existing `EventSink`
(`recording-progress`). The gpui app gets a dialog that fills in `Recording` and a
progress bar. No new mechanism.

---

## 5. The old export path: what to take

**Take (as a pattern — the file is on an unmerged branch, so this is transcription,
not import):**

- the audio-mux argv shape: `-map 0:v:0 -map 1:a:0 -c:a aac -b:a 192k -shortest -movflags +faststart`;
- `kill_on_drop(true)`, explicit `stdin.shutdown()`, then `wait()` and check status;
- `get_track_path_and_hash` + `get_track_duration` for the audio input and the frame count;
- the *idea* behind `render_frame_max(layer, t_prev, t)` — a shutter over the frame
  interval, not a point sample. See §7.

**Change:**

- `-f h264 … -c:v copy` → `-f rawvideo -pix_fmt rgba …` plus a real encoder. There is
  no WebCodecs in a Rust process; we have RGBA8, not encoded H.264.
- `stderr(Stdio::null())` → capture it. A bare exit code is not a diagnosis.

**Do not port:**

- the whole session protocol — `export_start` / `export_sample_frame` /
  `export_sample_batch` / `export_push_chunk` / `export_finish` / `export_cancel`,
  `ExportSessionsState`, the `x-session-id` raw-body invoke, the batch-of-600
  amortisation. Every one of those exists because the loop was in the frontend.
- `export-video-dialog.tsx`, `run-export.ts`, `use-export-store.ts` — React, dead.
- `render_frame_max` itself — already gone, and a fold over `Vec<UniverseState>`
  belongs beside `composite_frame`, not as a resurrected function.

**Leave alone:** `build.rs`'s ffmpeg download, the `ffmpeg-runtime/*` resource entry,
and `ffmpeg_env` — all still on main and still used by `audio/decoder.rs`,
`sync/files.rs`, `stem_worker` and `genre_worker`. Only the `init` signature changes.

---

## 6. Design it twice

### The alternative: no seam — the caller loops `venue.render`

Python (or the CLI) calls `luma.venue.render(t=n/fps)` in a loop, collects the PNGs
from `outputs/`, and runs `ffmpeg -i frame%05d.png -i audio.mp3 out.mp4`. Zero new
Rust. Composes primitives that already exist. An agent can do it *today*.

It loses on four counts, and they compound:

1. **It rebuilds the whole world per frame.** `venue_host::state_at` calls
   `build_scene_strict`, and `render` calls `VenueGeometry::load`, on every call. §2
   shows that is ~140 of the 150 ms. Over 9000 frames it is ~21 minutes of pure
   re-reading the same rows and re-parsing the same `.qxf` files.
2. **It PNG-encodes and round-trips the filesystem.** ~9000 deflates and ~9000 files,
   several GB, in an agent workspace, for an artifact that is 50 MB.
3. **The host-call budget forbids it.** 64 calls per cell, 30 s each. 9000 frames is
   141 cells.
4. **It leaks the use case into every caller.** fps, span clamping, shutter, audio
   offset and the ffmpeg argv would each be re-derived by the app, the kernel and the
   CLI, and they would drift. The time axis has to have one owner. This is the
   textbook special-purpose-API leak.

It is, however, the right answer for "give me six stills" — which is exactly what
`venue.render` already is. Recording is not a loop over stills; it is a different
abstraction that happens to contain one.

### A third, noted not chosen

Bake the time axis to a `UniverseState` tensor artifact, then render from the tensor in
a second pass. `Scene::render(&times)` already returns exactly that array, and
`track_host::render` already ships an f32 light tensor. It would let a DMX export and a
video export share one bake. Deferred: it doubles the artifact model for no
user-visible gain in v1, and the natural time to build it is when the second consumer
of a bake actually exists.

---

## 7. Open questions

1. **Shutter.** A 25 Hz strobe under a 30 fps point sampler beats and looks broken.
   Fold `K` sub-samples per output frame — `max` on dimmer/strobe, mean on colour,
   winner-takes-all on position? What is `K`, and is `max` right for colour? This is a
   pure `eval`-level fold (a sibling of `composite_frame`), costs nothing on the GPU,
   and should be decided before anyone trusts a recording. The old `render_frame_max`
   is the evidence someone already hit this.
2. **Colour.** The renderer hands back sRGB-encoded RGBA8; H.264 `yuv420p` limited
   range will crush the white-hot beam cores the tonemapper is producing. Tag `bt709`
   and full range, or accept the clip? Worth one golden comparison.
3. **`h264_videotoolbox` on non-mac.** Present in the local `ffmpeg-static` macOS
   build; assume `libx264` elsewhere and pick at runtime from `-encoders`, or pin
   `libx264` everywhere for byte-identical output across platforms?
4. **Camera.** `View` is a closed enum, so v1 records one static named view. A recording
   plainly wants an orbit or a keyframed path eventually. Does that camera path belong
   to the score, or to the recording request? Answering it later is fine; putting the
   camera in the wrong place is not.
5. **One GPU, one recording.** The pinned render thread is bounded at 1. A recording
   holds it for minutes and an agent's `venue.render` will queue behind each frame
   (fair, but slow). Explicit backpressure ("a recording is in progress"), or a second
   device for recordings?
6. **Batch concurrency.** `--all` is serial for the same reason. Is that acceptable for
   a nightly render of every authored score, or does it want N processes?
7. **`stage_render`'s known duplication** with `gpui/crates/app/src/visualizer.rs`
   (its own module doc flags it: private copies of `scene`, `flatten_pieces`,
   `local_matrix`, `definition`, `meshes_root`). Recording makes that a third consumer.
   Collapsing it first is probably cheaper than paying the change amplification twice
   more.
8. **Verify §2 before committing to the dials.** The numbers in the throughput table
   are extrapolated from the volumetrics goldens, not measured. The instruments already
   exist: `compositor.rs::profile_a_real_score_across_a_window` for the eval side,
   `gpui/crates/render/tests/stall_probe.rs` for the GPU side. A `bench_record` bin that
   renders 300 frames of a real score at both resolutions and both subframe counts
   settles it in an afternoon, and should exist before the quality presets are named.

---

## 8. What landed, and what the measurements said

Built on `agent-code-execution`: `src-tauri/src/recording.rs`, the `Sequence`
handle in `stage_render.rs`, an `ffmpeg_env` split, and
`src-tauri/src/bin/luma-record.rs`. Scope is one score in, one file out — no
`--all`, no Python binding, no MCP tool, no app dialog. The `Recording` /
`Progress` / `Recorded` shapes are §4's, minus `quality` (see the shutter, below)
and `audio` (a recording without the track is not a thing anyone asked for).

```
luma-record <score-id> <out.mp4>
    [--view front] [--width 1280] [--height 720] [--fps 30] [--span start:end]
    [--config-dir DIR] [--fixtures-root DIR] [--cache-dir DIR]
```

### The seam was worth what §2 said it was

`Sequence::install(...)` parks the scene, definitions and fitted camera on the
pinned render thread; `Sequence::frame(state, t, subframes)` carries only the
clock and the lit state. `render_rgba` is now install-plus-one-frame, so there
is one implementation. Several sequences can be installed at once, so an agent's
`venue.render` interleaves with a recording rather than evicting it.

`venue.render` itself was **not** reworked onto the handle: it needs one frame
of one venue and already gets it through `render_rgba`, which is the handle. The
part of it that still costs ~140 ms per call is `VenueGeometry::load` +
`build_scene_strict`, which are `venue_host`'s to cache, not `stage_render`'s.
That is the follow-up, and it is a `VenueHost` change, not a renderer one.

### Measured, M3 Max, `perf` profile, 63-clip claude-authored score

Track `c59907aa` / venue `7fb94fd7`, score `47f4c5ef`, 60 s span, front view,
shutter on (K=4).

| | render ms/frame | encode ms/frame | realtime factor |
|---|---|---|---|
| **720p30** | 50 | 6.8 | **0.59×** |
| **1080p30** | 89 | 14.5 | **0.32×** |

The whole 238 s track at 720p30, end to end, is **0.45× realtime** — 7140 frames
in 530 s, 67 ms/frame — the busier sections costing more than the 60 s bench
window. A five-minute track is therefore ~11 min at 720p30 and ~26 min at
1080p30.

Verified: `ffprobe` reports 1280×720, 30/1, 7140 frames, 238.000 s video against
237.772 s aac; frames pulled at 25 s, 125 s and 210 s are lit and match the clip
timeline (cyan chases, the purple `circle_pill` section, the closing wash). The
muxed audio correlates at 0.9991 with the source segment decoded independently.

§2's extrapolation predicted ~10 ms/frame at 540p and ~33 ms at 1080p for the
GPU. The measured cost is roughly 3× that, and the difference is *not* scene
assembly — that is now paid once, and it shows: `venue.render`'s 150 ms per
still is 50 ms per frame here at more than twice the pixels. The remainder is
the per-render fixed passes (~4 ms) and `complete_blocking`'s 1 ms poll
granularity, both paid four times per output frame.

`encode` is measured as time blocked writing to the pipe, so it is the
*encoder's* throughput showing through as backpressure: 14.5 ms/frame at 1080p
is `h264_videotoolbox` becoming the second bottleneck. A writer thread would
overlap it and buy ~15% there. Not built — §2's "do not pipeline" is about GPU
depth, and this is a different axis, but it is also not the biggest win
available.

### Shutter: measured, and §7.1's premise was wrong

**§7.1 proposed a fold over `UniverseState` sub-samples, a sibling of
`composite_frame`. That cannot fix strobes.** `PrimitiveState.strobe` is a
*rate*; the on/off gate is `luma_render::frame::strobe_gate`, applied inside
`build_frame_with` against the frame's own clock. Folding evaluated states over
K sub-times folds a constant, and the rendered frame is still a point sample of
the gate. Reproducing the gate in `eval` to fold it there would be a second copy
of a renderer constant that already exists in two versions (10 Hz/unit and
20 Hz/unit) — the wrong direction.

What landed instead: **K = 4 sub-*renders* per output frame, stratified over a
180° shutter (`SHUTTER_ANGLE = 0.5`), averaged in linear light.** Every
time-varying thing integrates — eval, the strobe gate, haze drift — with no new
knowledge anywhere. The renderer's jitter budget is *divided* between the
sub-renders (`DEFAULT_SUBFRAMES / K` each), so total haze samples are unchanged
and only the fixed passes are paid four times.

Averaging is in linear light, not on the sRGB bytes the renderer returns: a
half-duty strobe averaged as bytes lands at code value 128 instead of 188, about
a stop and a half dark. `luma_render::coords` gained the `linear_to_srgb` that
sat there as only half a pair.

Measured on a real `bass_strobe` clip (score `f9effb89`, 61–63.2 s, 480×270),
per-frame mean luminance:

| | frame-to-frame mean \|Δ\| | peak-to-trough spread |
|---|---|---|
| point sample | 2.81 | 11.8 |
| K=4 @ 180° | 2.28 | 11.1 |

**The strobe still reads as a strobe** — the amplitude spread is within 6% of
the point sample, which was the risk a 180° shutter runs — while the aliasing
stutter drops ~19%. On smooth content (a `kick_intensity` section) the
difference is ~6%, i.e. nothing, as expected.

The price is measured too: point sampling at equal haze quality is 34 ms/frame
at 720p (0.82× realtime) against 50 ms with the shutter (0.59×). **+38% wall
time for the shutter.** Kept on, and not behind a flag: two recordings that
sample time differently are two answers to one question, and §4's `quality`
enum was dropped for the same reason.

### `-shortest` is a trap behind a slow pipe

Every recording came out with **exactly ~7.04 s of audio missing from the end**
— 10 s of video with 2.97 s of audio, 60 s with 52.95 s. `-shortest` finalises
the file the moment the video pipe hits EOF, and a pipe delivering one frame
every 50 ms leaves ffmpeg's audio decoder that far behind when it does. §5's
"take the audio-mux argv shape" is therefore taken *except* for `-shortest`:
the audio input is cut with `-t {frames/fps}` instead, which is exact and does
not depend on how fast the video arrives. Verified at 0.9991 correlation against
the source segment decoded independently.

### Answers to the rest of §7

- **§7.2 colour.** `-pix_fmt yuv420p -colorspace bt709`, tagged, limited range
  accepted. The extracted frames match the offscreen stills; no visible crush on
  beam cores at 0.15 bpp. Not chased further.
- **§7.3 videotoolbox elsewhere.** `cfg!(target_os = "macos")` picks
  `h264_videotoolbox` with a bitrate target, everything else gets `libx264 -crf
  18 -preset veryfast`. No runtime `-encoders` probe: one branch, both arms
  tested by `the_encoder_matches_the_platform`.
- **§7.4 camera.** Still one static named `View` per recording. Untouched.
- **§7.5 / §7.6 one GPU.** A recording holds the pinned thread for minutes and a
  concurrent `venue.render` queues one frame deep. Accepted; the `Sequence` map
  is keyed so the two do not evict each other.
- **§7.7 `stage_render` / `visualizer.rs` duplication.** Still there. Recording
  is now the third consumer, exactly as predicted.

### Flagged while passing

`compositor::scores_for_track` composites **every** score on a `(track, venue)`
— score identity is not part of what it blends. `luma-record <score-id>` renders
that score's clips alone (`get_clips_of_score`), which is what the request
means. The two disagree whenever a track carries two scores in one venue, and
the compositor is the half that should grow a score id.

---

## 9. The second way to pay for a clean haze

§8 measured one sampling strategy and called it the recording. It is not: it is
the *isolated-frame* strategy, and the renderer has always had a second one that
only the live viewport could reach. `luma-record --haze temporal|accumulate`
now picks between them.

```rust
// recording.rs
pub enum Haze { Accumulate, Temporal }
```

The names are the two questions, not two quality levels:

- **`Accumulate`** (the default, and §8's behaviour): every output frame is a
  function of its own `t`. K = 4 sub-renders over a 180° shutter, the export
  jitter budget divided between them, averaged in linear light. 16 haze marches
  per frame.
- **`Temporal`**: frames are consecutive, so the renderer's haze history does
  the integrating. `LIVE_SUBFRAMES` = 2 marches per frame, each blended into the
  history at 18%, warmed up over 24 discarded frames before the span opens.

### The renderer already had both entries; one of them was private

`Renderer::render` passes `temporal: false` — it bypasses the history *and*
pins the blue-noise seed, which is what makes a golden reproducible. The
temporal path is `render_live_into`, `pub(crate)`, called by `viewport.rs` and
nothing else. The whole renderer change is one public method beside `render`:

```rust
/// Render the *next* frame of a sequence.
pub fn render_next(&mut self, frame: &Frame, width: u32, height: u32, subframes: u32)
    -> anyhow::Result<Vec<u8>>;
```

Three lines of body over the existing `render_live_into`. `stage_render` gained
a `Continuity { Next, Cut }` on `Sequence::frame` that picks between the two;
`render_rgba` — one still — passes `Cut`. There is deliberately no
`Renderer::cut()` and no ordering rule: `render` resets the history as a side
effect of bypassing it, so "call A before B" never arises, and a caller that
passes the wrong `Continuity` loses image quality, not correctness — the
renderer re-checks size, camera, cone geometry and clock and drops a history
that could not be this frame's past.

**Warm-up.** The resolve keeps 82% of history and mixes in 18% of the new
march, so the weight of the unconverged first frame decays as `0.82^n`: 4.2% at
16 frames, 0.9% at 24. `WARMUP_FRAMES = 24` — 0.8 s of frames, ~0.2 s of wall —
rendered on the output grid up to the span start and discarded, so the history
is continuous across the join. Clamped at zero, so a recording that starts at
the top of the track warms on its own first moment.

**The shutter collapses to K = 1 under `Temporal`, and that is the trade.** The
history *is* a shutter — an exponential one with a ~11-frame tail, twenty times
longer than the 180° box. Stacking a box inside a tail that long buys nothing,
and it would multiply the cost the mode exists to remove. What it costs is the
motion blur: see below.

### Measured, M3 Max, `perf` profile, score `47f4c5ef`, `--span 60:120`, front

| mode | | render ms/frame | encode ms/frame | realtime |
|---|---|---|---|---|
| `accumulate` | 720p30 | 55 | 0.8 | **0.60×** |
| `accumulate` | 1080p30 | 108 | 1.7 | **0.30×** |
| `temporal` | 720p30 | 8 | 0.8 | **3.62×** |
| `temporal` | 1080p30 | 12 | 1.8 | **2.35×** |

**6.0× at 720p, 7.8× at 1080p.** A five-minute track goes from ~8 min to
~1.4 min at 720p30, and from ~17 min to ~2.1 min at 1080p30.

Two corrections to §8's table while passing:

- `Exposure::resolve` — the linear-light average, ~6 ms of CPU at 720p — was
  being timed as *encode*. It is frame assembly and now runs before the encode
  clock starts. That is the whole of 720p `accumulate` reading 55/0.8 here
  against §8's 50/6.8; wall time is unchanged. **`encode` really is the pipe
  now, and the pipe is not a bottleneck in any of the four rows** — §8's "a
  writer thread would buy ~15% at 1080p" was measuring a `powf` loop.
- Wall time on a shared machine varies more than these numbers suggest: two
  back-to-back runs of 720p `accumulate` came in at 100 s and 146 s. The
  `render ms/frame` column is the stable one; treat the realtime factors as
  best-case.

### Does the history survive a real show? Mostly.

The renderer drops history whenever the *cone geometry* changes — every cone's
position, direction, beam and field angle, wash and gobo are hashed into the
key — which on a rig with sixteen moving heads sounded fatal. Instrumented over
the recording (84 frames = 60 output + 24 warm-up):

| span | history valid |
|---|---|
| 100–102 s (`y_chase`, intensity only) | 83 / 84 |
| 60–62 s (`z_chase` + `y_chase`, heads moving) | 45 / 84 |

So it degrades gracefully rather than collapsing: a moving section runs at about
half strength, and the frames that lose their history still get their two
jittered marches. The cost model does not change either way — the temporal
resolve is one full-screen triangle.

### Quality, honestly

Three timestamps out of both 720p files — a steady moment (video 40.0 s), a
flash (50.62 s), and 0.14 s after the hard cut at track 94.76 s — cropped 440×280
into the beams and pixel-doubled:

- **Haze grain: a wash, if anything favouring `temporal`.** Two marches folded
  into an ~11-frame tail is more effective samples than sixteen marches of one
  moment, and the cones read slightly creamier. Neither is noisy.
- **Motion blur: `accumulate` has it, `temporal` does not.** This is the one
  visible difference and it is not subtle at the flash: `accumulate`'s cones are
  wider and softer because the 180° shutter integrates the head's sweep across
  the frame interval; `temporal`'s are crisp point samples. Per still,
  `temporal` looks *sharper*. In motion it will judder where the other one
  blurs.
- **No smear at a hard cut.** Mean luma across the 94.76 s cut steps 32.3 → 28.5
  in one frame in both modes — the cut changes the cone set, which is exactly
  what invalidates the history.

Per-frame mean luminance across a real `bass_strobe` clip (score `f9effb89`,
61–63.4 s, 480×270), the same instrument §8 used:

| | frame-to-frame mean \|Δ\| | peak-to-trough spread |
|---|---|---|
| `accumulate` (K=4 @ 180°) | 2.25 | 11.11 |
| `temporal` (K=1) | 2.80 | 11.13 |
| §8's point sample, for reference | 2.81 | 11.8 |

`temporal` sits on the point-sample line, and it has to: **the temporal resolve
stabilises the haze buffer only.** Surface lighting and the strobe gate are not
in it, so nothing in this mode integrates the time axis. The 24% more
frame-to-frame stutter is exactly the aliasing the shutter was added to remove.

### Recommendation

**Keep `accumulate` as the default.** It is the mode that answers "what did the
room look like during this frame"; `temporal` answers "what did the room look
like at this instant, and here is a clean haze for free". For a 63-clip
dance-music score whose whole vocabulary is strobes and sixteenth-note chases,
the first question is the right one, and the 6× is bought with the only visible
quality difference there is.

`temporal` earns its place at the other end: previews, `--all` batches, the
"render every score `claude -p` authored" loop, and anything where 2.4× realtime
at 1080p versus 0.3× is the difference between a nightly job and no job. It is
also the honest mode for *checking what the app shows*, since it is the app's
own path.

Two things would change the recommendation and neither is built:

1. **A shutter that is not four sub-renders.** The reason `accumulate` costs 6×
   is that it re-renders the whole frame K times to integrate `t`. K = 2 with the
   temporal history on would get most of the motion blur at ~2× — worth measuring
   before anyone flips the default.
2. **Putting the strobe gate somewhere it can be integrated.** §8 established
   that folding evaluated states cannot fix strobes because the gate lives in
   `build_frame_with`. That is still the root cause of why time integration costs
   a whole re-render, and it is a `luma_render::frame` change, not a recording
   one.
