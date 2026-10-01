# Gasworks, all lights on, haze off: measured surface experiments

## Outcome

The fixture surface-lighting path is the main measured cost in this test.
Removing it reduces median GPU time from roughly 7–10 ms to 2.4–2.7 ms at
960×640. Removing face lights or surface shadows alone does not produce a
similarly clear, consistent reduction across views and repeated runs.

No production renderer change was retained. Three output-preserving prototypes
were tested and rejected for insufficient or negative performance benefit:
earlier rejection of noncontributing lights, shadow-hierarchy early exits, and
4-pixel light tiles. The repeatable orbit profiler and results are retained.

This corrects the strongest hypothesis in the
[prior-art note](many-light-surface-shading-prior-art-2026-09-12.md): the saved
Gasworks rig has **310 fixture cones but only 10 active face point lights**.
Most of its heads are procedural emitters, which do not create the mesh-fixture
face lights. The unindexed face-light loop remains an adjacent scaling smell,
but this scene does not have hundreds of face lights.

## Inputs and method

- Apple M3 Max, Metal; current dev profile (`luma-render` optimized at level 2).
- Saved Gasworks export: 92 fixtures, 95 venue pieces, 907 renderer draws.
- All 310 exported head states forced to full white, no strobe, zero pan/tilt,
  no gobo; haze explicitly zero. The library database was not changed.
- Three pinned views: wide, close, side. Each measured sequence uses a repeating
  ±0.2-radian orbit about the target. Score time and all lights/casters stay fixed.
- 960×640 physical pixels, 120 warm-up frames then 240 measured frames per view
  per run. Each variant ran twice. Controls bracketed the first omissions;
  subsequent prototypes used an A/B/B/A order. Runs were sequential, never
  concurrent GPU benchmarks. No builds ran during the timed sequences.
- 14,400 measured orbit frames overall, plus warm-up frames and image captures.
  All measured orbit samples report **zero redrawn fixture shadow maps**.
- Timing uses the renderer's production compositor-surface path, with blocking
  completion and GPU timestamp readback. Frame construction is outside timing.
  These are **GPU times, not delivered app FPS or input-to-present latency**.

This is a controlled reconstruction from an existing export, not a capture of
the user's precise current camera, viewport dimensions, or library state. The
desktop and Luma stayed open. There was substantial run-to-run timing variation;
the tables deliberately show both run medians rather than picking the fastest
control or claiming small improvements. Short initial captures were insufficient
for attribution and are not used for the decision.

## Shader omissions

Each cell is the range of the two run medians, in milliseconds. An omission
changes the image and is a diagnostic, not an acceptable optimization. Deltas
are not additive: removing work can change scheduling and shader occupancy.

| Variant | Wide | Close | Side |
|---|---:|---:|---:|
| Baseline | 9.90–10.50 | 7.40–10.05 | 9.27–10.21 |
| Omit face lights | 8.12–8.72 | 6.66–9.73 | 6.19–10.19 |
| Omit surface shadow sampling | 9.47–9.99 | 7.48–7.54 | 9.22–9.28 |
| Omit fixture surface lighting | 2.53–2.60 | 2.63–2.71 | 2.41–2.43 |

The last row preserves face lighting, geometry, sky, and compositing, but also
removes the surface depth-refinement pass associated with the fixture cone
loop. It isolates that whole path, not BRDF arithmetic alone. The results do
not establish whether candidate traversal, ALU, register pressure, or texture
latency dominates within the loop. In particular, the shadow-filter-only
hypothesis is not established by these measurements.

## Rejected prototypes

### Earlier back-face and fully shadowed rejection

Moved the existing normal/light-facing test ahead of angular-profile and gobo
evaluation; skipped material lighting when visibility was zero. No light
threshold, sample count, or authored setting changed.

| Variant | Wide | Close | Side |
|---|---:|---:|---:|
| Prototype | 8.06–9.55 | 4.44–7.14 | 9.47–9.87 |
| Nearby controls | 7.75–8.20 | 4.65–6.52 | 10.52–10.76 |

The side view improved, but other views did not show a repeatable win. The
settled close/side images were byte-identical to baseline. The wide image
differed in one green channel by 1/255 in one of 614,400 pixels, consistent
with floating-point code-generation differences. Rejected as a default change.

### Existing min/max shadow hierarchy before PCF

Used a conservative depth range covering the full clamped 4×4 filter footprint,
bounded the receiver plane with an outward margin, and returned fully lit or
shadowed only when every comparison was provable. Otherwise used the existing
four-gather, sixteen-comparison filter.

| Variant | Wide | Close | Side |
|---|---:|---:|---:|
| Prototype | 10.69–10.99 | 7.35–7.61 | 10.95–11.24 |
| Nearby controls | 9.32–9.56 | 6.40–6.67 | 6.04–10.02 |

All three settled images matched baseline bytes, but GPU time regressed.
The extra lookup, bounds arithmetic and branching did not pay for themselves
in this implementation. No proof-hit counter was added, so the results do not
distinguish low proof coverage from overhead on successful proofs.

An implementation constraint also surfaced: the scene pipeline already binds
15 sampled textures under a 16-texture device limit. Binding both hierarchy
banks failed pipeline validation. The timed prototype bound only the first
bank and retained the original filter for slots ≥256; it does not evaluate a
full-bank implementation. Both the binding and shader changes were removed.

### Smaller light-culling tiles

Changed light-index tiles from 8×8 to 4×4, including the CPU/GPU bounds,
consumer addressing, and surface-depth sampling. Preserved the two depth
buckets and exhaustive contributing-light evaluation.

| Variant | Wide | Close | Side |
|---|---:|---:|---:|
| Prototype | 9.61–10.93 | 7.38–7.61 | 6.04–10.39 |
| Nearby controls | 8.80–8.97 | 6.43–7.29 | 10.40–11.69 |

All three settled images were byte-identical to baseline. The side-view result
was variable; wide and close views regressed. No default tile-size change was
retained. Smaller candidate regions also require more index work and memory;
the total cost is the relevant acceptance criterion.

## Reproduction and retained artifacts

The new [profile-surfaces binary](../../gpui/crates/render/src/bin/profile-surfaces.rs)
accepts the same catalogue/camera suite shape as `haze-lab` and defaults to 600
measured frames per view. It always disables haze and holds lighting state
constant during the camera orbit. Output files must be new.

```sh
cargo +1.97.1 build --manifest-path gpui/Cargo.toml -p luma-render --bin profile-surfaces
gpui/target/debug/profile-surfaces experiments/surface-gasworks-20260914/suite.json /tmp/gasworks-control.json 600
LUMA_PROFILE_OMIT=face-lights gpui/target/debug/profile-surfaces experiments/surface-gasworks-20260914/suite.json /tmp/gasworks-no-face.json 600
LUMA_PROFILE_OMIT=surface-shadows gpui/target/debug/profile-surfaces experiments/surface-gasworks-20260914/suite.json /tmp/gasworks-no-shadows.json 600
LUMA_PROFILE_OMIT=surface-lighting gpui/target/debug/profile-surfaces experiments/surface-gasworks-20260914/suite.json /tmp/gasworks-no-lighting.json 600
```

Use a clean renderer environment apart from the named omission, the same power
conditions, and no concurrent build or GPU workload. Do not compare results
from different viewport sizes or light states.

The local, git-ignored directory
`experiments/surface-gasworks-20260914/` contains the pinned catalogue and suite,
every orbit sample, per-run p50/p95 summaries, baseline/prototype images,
prototype patches, and source/binary provenance. It is intentionally not part
of the tracked research document. The three prototype patches are alternative
experiments against the recorded baseline, not cumulative changes.

Validation: workspace `cargo +1.97.1 check --workspace --all-targets` passed,
the final profiler build passed, and a 60-sample smoke run produced valid GPU
timings with zero shadow redraws. `git diff --check` passed. Production renderer
files match their task-entry contents; the existing shared-checkout edits were
preserved.

## Next investigation

Capture the original shader with Apple GPU counters to distinguish fixture-loop
ALU/register pressure from memory stalls and obtain the actual refined light
visits and contributing lights per fragment. The existing fragment counter
counts the broad index rather than the final surface buckets, so it cannot
answer that question. That attribution is needed before another exact culling
or shading change. A deferred rewrite, stochastic light sampling, or reduced
shadow quality is not justified by these results alone.
