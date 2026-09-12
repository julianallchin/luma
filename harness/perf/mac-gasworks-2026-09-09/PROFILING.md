# Full-score profiling

`backend/src/bin/profile_score_timeline.rs` evaluates the saved score through
the production backend, assembles production frames, and profiles the renderer's
compositor destination without PNG readback. It records every frame and per-bar
p50/p95/p99/max distributions. This is a serial GPU/CPU capacity measurement;
it does not measure presented FPS or GPUI scheduling.

Build from the repository root:

```sh
cargo +1.97.1 build --release --manifest-path backend/Cargo.toml --bin profile_score_timeline
```

Use a SQLite backup of the library for reproducible runs, an explicit score ID,
and a new output path. The current Gasworks Get Lucky example score is
`7de2624c-6095-8ae1-a7bf-c9496baae2d8`; the older similarly named score has a
different workload. The default camera is the 2227x1391 close stress camera in
`native-suite.json`. `--camera`, `--camera-case`, `--width`, and `--height`
allow explicit alternatives. Do not compare different cameras or sizes as an
optimization.

```sh
LUMA_PROFILE_DETAIL=1 python3 harness/perf/mac-gasworks-2026-09-09/run_isolated.py \
  --metadata /absolute/new-run-metadata.json --timeout 600 -- \
  backend/target/release/profile_score_timeline \
  --score 7de2624c-6095-8ae1-a7bf-c9496baae2d8 \
  --db /absolute/library-backup.db --rate 75 --warmup 60 \
  --output /absolute/new-timeline.json
```

When a known Luma process competes for the GPU, add `--pause-pid PID` to the
runner. It resumes that process on exit, error, timeout, or interruption. This
does not establish machine-wide idleness: avoid concurrent builds and GPU
jobs for timing acceptance. The runner records the executable hash separately
from runtime source hashes; source files can change after a binary was built.

The default range includes the pickup and ends after bar 41. For this score,
that is the half-open interval [0, 85.0599975586) seconds, or 6,380 samples at
75 Hz. `--start 52 --end 52.32` is a short dense-section probe. Short probes
warm the cache at their starting state and cannot replace continuous playback.

## Captures with playback history

To inspect a late transition after all preceding frames have populated the
renderer caches, keep `--start 0` and add an explicit capture window:

```sh
--end 58.1 --capture-dir /absolute/new-history-captures \
--capture-start 57.22666666666667 --capture-end 58.08
```

All three capture options are required together. The window is half-open and
must lie within the profiled range. Every warmup and sampled frame uses the
same RGBA readback destination; only frames in the selected window become
PNGs. There is no extra render or destination switch at the start of capture.
The capture manifest records exact frame indices, score-time float bits,
image hashes, input provenance, and the matching profile-output hash.

**Every timing from a capture run is ineligible for performance acceptance.**
Use a separate run without capture options for timing. Compare captures with
matching prefix, warmup, cadence, camera, database, and score inputs; list any
intentional renderer-environment differences. Keep the isolation metadata with
both runs. The output and capture directory must be new paths.

## Baseline and acceptance

The immutable full-run artifacts are under ignored directory
`run-20260911-full-score/`: `library.db`, `control-full.json`,
`control-full-meta.json`, and the preserved control executable. Database SHA-256:
`e5664072546c04a3b9b329851a48a13891646b91011703239ee61e47a79f8420`.

On the Apple M3 Max, the original continuous run has GPU p50 18.16 ms,
p95 181.74 ms, and p99 217.55 ms; 3,051 of 6,380 frames exceed 20 ms.
Bars 25–28 contain 470 cones. A brief CPU build overlapped the opening of the
run, so early CPU timing is not a pristine baseline. Paired short probes at
52 seconds measure about 85 ms, demonstrating why short probes and full-run
history must be reported separately. No sustained-FPS success is claimed.

Check both aggregate and per-bar tails against 20 ms (50 Hz) and 13.333 ms
(75 Hz). GPU pass brackets overlap and can include dependency waits; their
durations must not be added or interpreted as isolated costs. The final gate
also requires actual app presentation through bar 41.

Use [QUALITY.md](QUALITY.md) for immutable image references and visual gates.
Every optimization must exercise its intended path: inspect `compact.active`,
overflow, suspension, cache modes, and rebuild counts before accepting a
capture or timing. Fallback-only images do not validate a fast path. Preserve
rejected results alongside accepted witnesses so the diagnosis stays reviewable.
