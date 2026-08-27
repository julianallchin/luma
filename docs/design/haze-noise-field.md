# The haze density field — replacing `haze_noise`

Status: steps 1-2 implemented (see §5, §7). Scope: `gpui/crates/render/` — `beam_transport.wgsl`'s
density term and the resource that feeds it.

Prerequisite reading: [`beampass-phase2.md`](beampass-phase2.md) §0.5, which is the
measurement that produced this document. Reference hardware: Apple M3 Max dev / M2 target,
wgpu 30.0.1 / Metal.

---

## 1. Thesis

**The volumetric march's cost is one function, and it is not the transport.**

`haze_noise` — two octaves of gradient noise, sixteen `hash3` evaluations, 48 `sin` — is
87–95 % of a volumetric sample. Everything else in `beam_scatter`'s inner loop (the angular
profile, the taper, the MIS weight, the phase function, the transmittance, the gobo's early
return) costs, together, about an eighth of it.

Baking that field into a 3D texture and reading it with hardware trilinear filtering makes
the density term **cost less than nothing measurable**: a sample with the texture costs the
same as a sample with the density term deleted outright. The transport is then the cost, as
it should have been all along.

This is not a quality lever and it is not a cull. Nothing is clamped, nothing is skipped, no
timer is read. The same two-octave field, at the same amplitudes, drifting at the same rate,
is read from memory instead of recomputed per sample. The vetoes (beam-length clamp,
intensity-range cull, frame-time governor) are untouched and stay untouched.

**What it does change is the image**, in two specific and bounded ways, and §4 is about
exactly those two. This document cannot gate on byte-exact goldens and does not pretend to.

---

## 2. Measurements this design rests on

All from `scratchpad/beamprobe/`, M3 Max, best of ten runs after two warm-ups. Reproduce
before trusting; the probes are out of tree and take seconds.

### 2.1 The density term's share of a sample

`src/bin/marchbench.rs` — a 1280×720 fragment grid, 8 lights per pixel, 8 samples per light
along that pixel's own ray, with the full `beam_scatter` sample body (angular profile, taper,
MIS weight, HG phase, transmittance; gobo taking its early return, as the profiled rigs do).
This is the marcher's real access pattern: threads in a workgroup are adjacent pixels whose
rays nearly coincide, so spatial reuse across a warp is high, while one thread's successive
lights land in unrelated parts of the volume, so reuse along a thread is low.

| kernel | ns/sample | vs current |
|---|---|---|
| full sample, **no density term at all** | 0.0177 | — |
| full sample, **current `haze_noise`** | 0.1342 | 1.00× |
| full sample, texture 64³ r8snorm (256 KB) | 0.0155 | 8.7× |
| full sample, texture 128³ r8snorm (2 MB) | 0.0198 | 6.8× |
| full sample, texture 256³ r8snorm (16 MB) | 0.0206 | 6.5× |
| full sample, texture 128³ r16f (4 MB) | 0.0160 | 8.4× |
| full sample, texture 256³ r16f (32 MB) | 0.0153 | 8.8× |

Two things to read off this table, both load-bearing:

1. **Every texture variant lands on the no-density-term cost (0.0177) to within measurement
   scatter.** The fetch latency hides completely under the sample body's arithmetic. The
   spread between variants (0.0153–0.0206) is not ordered by size or format — 256³ r16f, the
   largest, is the fastest — so it is scatter, not a size effect.
2. **Texture size does not matter.** 256 KB and 32 MB perform identically, which says the
   path is latency-hidden rather than capacity-bound. Size is therefore free to be chosen on
   *image* grounds (§3), which is the opposite of the usual trade and worth not
   second-guessing.

### 2.2 Why the coherent microbenchmark was not enough

`src/bin/noisebench.rs`, same substitutes under three access patterns, density term only:

| access pattern | current | prefiltered texture |
|---|---|---|
| coherent (every thread walks one path) | 0.120 | 0.034 |
| **realistic** (§2.1) | 0.134 → sample | ≈ free |
| fully scattered (no thread shares a line) | 0.126 | 0.223 |

The fully-scattered column is the honest scare: with zero cache reuse the texture is **1.8×
slower than the ALU it replaces**. That case is not the marcher — a warp of adjacent pixels
sampling the same light shares nearly every texel — but it is the failure mode, and it is
what a fragment shader with `beam_scatter`'s register footprint would drift toward if
occupancy collapses. §5's falsification threshold exists for this reason and no other.

### 2.3 Ruled out by measurement, so nobody re-proposes it

- **A cheaper hash.** Replacing all 48 `sin` with a PCG-style integer hash measures 0.122
  against 0.120 — **no win**. The cost is the sixteen gradient evaluations and the register
  pressure they carry, not the transcendental. This is the obvious micro-optimisation and it
  does nothing.
- **Dropping to one octave.** 0.059 vs 0.120 — a 2× win for a visibly different field. The
  texture gets 7× and keeps two octaves. Strictly dominated.
- **Gradient table in a texture, existing interpolation kept** (8 point loads, structurally
  exact Perlin). 0.076 coherent, 0.382 scattered. Half the win of the prefiltered field and
  five times its worst case. Kept as §6's fallback because it is a strictly smaller image
  change, not because it is faster.

---

## 3. The field

### 3.1 What is baked

`haze_noise`'s structure is preserved exactly:

```
p     = (p_world.x, p_world.z, -p_world.y)        // the three.js Y-up basis, unchanged
drift = elapsed * (0.4, 0.25, 0.15)
q     = p * 2 + drift
n     = F(q) * 0.6 + F(q * 3 + drift + 3.7) * 0.4
     -> max(1 + 1.1 * n, 0.05)
```

Only `F` changes: from `noise3d` (evaluate 8 gradients, smoothstep-interpolate) to one
`textureSampleLevel` of a wrapping 3D texture. The two octaves keep their own coordinates,
so they still drift at different rates relative to each other and the composite field is
still not a rigid translation — which is what makes the smoke read as smoke rather than as a
sliding photograph. **One texture, two fetches**, not a baked sum.

### 3.2 Periodicity is the whole design question

The current field is aperiodic; a texture wraps. A texture of `size` texels at `K` texels per
lattice cell holds `size/K` cells, and octave 1 rides `q = p*2` so one cell is half a metre:

```
octave-1 period = (size / K) / 2  metres
octave-2 period = octave-1 / 3    metres     <- the binding constraint
```

`K` buys reconstruction fidelity and spends repeat distance. Measured (`src/bin/fieldfit.rs`,
200 k samples over a 40 × 40 × 12 m volume; error is trilinear reconstruction against the
*same* gradient field the texture was baked from, so it isolates filtering from the period
change; reported against the field's own spread, sd = 0.145):

| K | reconstruction rms | max | 256³ periods (o-1 / o-2) |
|---|---|---|---|
| 1 | 100 % | 444 % | 64 m / 21.3 m |
| 2 | 34 % | 149 % | 32 m / 10.7 m |
| **4** | **11 %** | **53 %** | **32 m / 10.7 m** |
| 8 | 2.9 % | 15 % | 16 m / 5.3 m |

Second-order convergence, 4× per doubling, as linear reconstruction should. K = 1 is not a
reconstruction at all (100 % error — it is a different field); K = 2 visibly smooths the
high frequencies; K = 8 buys fidelity nobody can see at the cost of halving the repeat.

**K = 4.** The statistics are preserved regardless — every periodic variant measures
mean 1.000, sd 0.1446 against the current field's 0.9996 / 0.1449, i.e. −0.2 %. The field is
the same *process*; it is a different *realization*, which is not something a golden diff can
be asked to bless. §4.

### 3.3 The recommendation, and why size is free

**256³ R16Float, K = 4 — 32 MB, periods 32 m / 10.7 m.**

Size costs nothing in time (§2.1), so it is spent entirely on repeat distance. 256³ is the
last size whose memory is unremarkable for a desktop app; 512³ would be 256 MB for one more
doubling and is not worth it.

R16F is the *starting* format, not the final one. R8Snorm would halve the memory for a
quantization error of 0.0052 in `haze_noise` units against the K = 4 reconstruction error of
0.0159 — a third of the error already accepted, in quadrature — but the risk it carries is
not magnitude, it is *kind*: quantization is uniform, so it can contour where a random error
would not. Blue-noise jitter plus subframe accumulation should dither that away, but "should"
is not a measurement. Start on the safe format and sweep *down* to R8Snorm as an
optimisation (§7 step 3), so the decision put in front of Julian is "can we save 16 MB"
rather than "is this artefact acceptable".

Make `size`, `format` and `K` a single named constant group so that sweep costs nothing.

### 3.4 Where it comes from

Baked once at renderer construction by a compute dispatch, joining the existing
construction-time GPU work (`warmup.rs`, the environment filter, the BRDF LUT).

**Measured, drafting this:** GPU bake plus the copy into the texture is **11.2 ms** at 256³.
A CPU bake of the same field is **1014 ms** (128³: 127 ms, 64³: 16 ms) — a second of
construction time for something a compute pass does in eleven milliseconds, so the CPU path
is out, and with it any temptation to make the field a build-time asset. The bake shader is
then the field's *single* definition: nothing on the CPU evaluates it in the shipping path,
and the sampling side is one `textureSampleLevel`.

Two mechanical constraints the draft hit, both real:

- **`r16float` is not a WebGPU storage-capable format.** The bake writes packed f16 pairs
  into a storage buffer, which is then `copy_buffer_to_texture`'d into the R16Float texture
  and dropped. `bytes_per_row` is `size * 2` against a 256-byte
  `COPY_BYTES_PER_ROW_ALIGNMENT`, which holds from 128³ up and **fails validation at 64³** —
  measured, not assumed. A field smaller than 128³ needs padded rows, not a smaller constant.
- **Workgroup counts cap at 65535 per dimension.** 256³ is 8.4 M f16 pairs, so a 1-D
  dispatch would need 131072 workgroups. Dispatch is 2-D: x over a slice's pairs, y over
  slices.

### 3.5 The bake hashes with integers, and that is a correctness requirement

The current `hash3` is `sin`-based. `sin` of a large argument depends on the device's range
reduction, so **a `sin`-hashed field bakes differently on different GPUs** — and once the
field is a baked resource rather than a per-sample computation, that difference is a
different texture, i.e. different golden images on different machines. The existing shader
has the same property, but it has never mattered because goldens are produced and compared on
one machine; making the field a resource is exactly the change that would turn it into a
portability bug.

The bake therefore uses a PCG-style integer hash. Integer arithmetic is exact and identical
everywhere, so the field is bit-reproducible across devices *and on the CPU* — which is what
makes it testable at all (§7 step 1). §2.3 measured the integer hash as no faster per sample,
which was the reason to reject it there; here the bake runs once and its cost is irrelevant,
while its determinism is the whole point. Same arithmetic, opposite verdict, for a different
reason — worth stating so the two conclusions do not look contradictory.

This was found by drafting: the out-of-tree validation could not reproduce the GPU's field on
the CPU at all (max error 0.60, i.e. unrelated gradients) until the hash became integer, at
which point agreement fell to 8.9e-4 — the sampler's 8-bit fixed-point filter weights against
an f32 CPU lerp, which is the floor for this comparison and not a defect.

### 3.6 Binding

Group 0 bindings **4 and 5 are already reserved and free** — they held the per-pass tile list
before the unified light index, and `beam_transport.wgsl` documents them as deliberately
vacant. The noise texture and its sampler take them. No renumbering, no layout churn, and
both consumers of the density term (`beam_scatter` and `haze.wgsl`'s ambient bed) inherit it
from the shared prelude, which is the point of that prelude existing.

---

## 4. The image, and who rules on it

This changes the integrand. Goldens cannot gate byte-exact, and **no re-baselined golden is
installed as accepted under this work.** Two distinct deltas, which must be reported
separately because they have different standing:

1. **A different realization.** The periodic field is not the aperiodic field. Same process,
   same statistics (§3.2), different smoke. No metric declares this acceptable; only Julian
   does.
2. **Reconstruction and quantization error** within that realization — 11 % of field spread
   rms at K = 4, plus R8's 0.0052. These are bounded and measurable and §3 already prices
   them.

Gate shape:

- **SSIM** against the current goldens for every haze-carrying scene, reported per scene, as
  a *number to look at* rather than a threshold to pass. A different noise realization moves
  SSIM a long way while looking identical in kind; a low SSIM here is a prompt to look, not
  a failure.
- **Side-by-side PNGs** — current and proposed, same scene, same frame, same camera — staged
  to the scratchpad for eyeballing. Representative set: a lit-and-hazy venue, a single beam
  (`one-beam`), overlapping beams, a gobo scene, and one wide-field wash where a 10.7 m
  repeat would show worst.
- **A drift check.** The repeat is spatial, so a moving camera is where it would betray
  itself. One orbit sequence, current vs proposed, saved as frames.
- Everything with `haze.enabled: false` and `haze_density < 0.001` must stay **byte-exact** —
  the bed's early return means the density term is never reached. Verify, don't assume.

Julian asked whether this campaign degrades quality. The answer this document is allowed to
give is: the transport is untouched, the field's statistics are preserved to 0.2 %, and the
two ways it differs are named and bounded above. Whether the smoke still looks right is his
call, and the deliverable for that call is the screenshots, not the SSIM.

---

## 5. Pre-registered result, and what falsifies it

Predicted, from §2.1 and honest per-pass timestamps as the baseline:

> The volumetric pass's per-sample cost drops **6–8×**, taking the pass to roughly what it
> costs with the density term deleted. On `stall-probe static30 + haze` at 2558×1357, the
> haze pass's share of the ~13 ms lit frame falls to near the unlit floor's neighbourhood.
> `profile-volumetrics` improves on every haze-carrying case and regresses on none.

**Falsification threshold: a measured in-situ sample-cost improvement below 2.5×.**

> **Measured 2026-08-25, in situ, against the honest per-pass baseline.** Machine quiet
> (load average 2.0, no builds running); medians of three `stall_probe` runs.
>
> | `stall_probe`, 2558×1357, 30 cones | before | after |
> |---|---|---|
> | `static+haze` `gpu_mean` | 11.78 ms | **5.46 ms** |
> | `static-haze` `gpu_mean` (no haze — control) | ~4.5 ms | 4.52 ms |
> | haze's own share, by difference | ~7.3 ms | **0.94 ms** |
>
> **~7.8× on the volumetric pass**, inside the pre-registered 6–8× band, and 2.16× on the
> whole lit frame. The control confirms the attribution: the no-haze frame does not move,
> which it must not, because the density term is unreachable there.
>
> The occupancy hypothesis (§2.2's scattered column, the named way this could have failed)
> did not materialise: the fragment shader hides the fetch as well as the compute
> microbenchmark did.

The named mechanism for coming in under it is occupancy: §2.1 is a compute dispatch, and
`beam_scatter` in a fragment shader carries a much larger live register set, which leaves
less arithmetic to hide the fetch latency under and drifts toward §2.2's scattered column. If
the measurement lands below 2.5×, that hypothesis is confirmed, and the honest report is that
the fragment shader cannot hide the fetch — not a quiet re-baseline of the claim. The
fallback in §6 is then the thing to price, because it trades a smaller win for a smaller
worst case.

Report `B̄_hit` alongside, from `beampass-phase2.md` §0.5's probe, so the pass time and the
sample count it came from are never separated again. That separation is how the 8489× lying
timestamps survived as long as they did.

---

## 6. Fallback, if §5 falsifies

**Gradient table in a texture, existing interpolation kept.** `noise3d`'s eight `hash3` calls
become eight `textureLoad`s of a wrapping gradient table; the smoothstep interpolation, the
octave structure and the amplitudes are untouched. The field is then *structurally exact*
Perlin — the only change is a periodic gradient table instead of an aperiodic hash, which is
what classic Perlin does anyway, so §4's delta 2 disappears entirely and only delta 1
remains.

Measured 0.076 vs 0.120 coherent — about half the prefiltered field's win — and 0.382
scattered, five times its worst case. It is the fallback rather than the plan because it is
worse on both the average and the tail; it is *a* fallback because it is a strictly smaller
image change and shares every piece of §3.4's and §3.5's plumbing. Choosing it later costs
one WGSL function and a different bake, not a redesign. Build the seam once.

---

## 7. Migration

Each step gated before the next. No commits; `git diff` patches to the scratchpad as
`checkpoint-15-hazenoise-<step>.patch` (probes are out of tree and are not in the diff).

### Step 1 — Bake the field, bind it, don't read it.

The compute bake, the texture, the sampler, bindings 4 and 5, the constant group from §3.3.
Nothing samples it yet.

Drafted and validated out of tree already (`scratchpad/haze_noise_bake.wgsl`,
`scratchpad/beamprobe/src/bin/bakecheck.rs`), so this step is close to a paste-in. The
validation harness becomes an in-tree unit test in this step, and it is worth keeping rather
than discarding: it checks the three things that are silent when wrong — the texel-centre
convention, the wrap, and the f16 pack — against an independent CPU reconstruction, plus a
direct seam check (points one period apart must agree), because a bad wrap is a plane of
discontinuity in the haze that no aggregate metric would flag. Current results: field 8.9e-4,
`haze_noise` 6.9e-4, seam 6.7e-4, all against a 2e-3 tolerance set by the sampler's
fixed-point filter weights.

**Gate: every golden byte-exact.** An unread binding cannot move a pixel; if one moves, the
bind-group change disturbed something and that is a bug to find, not to re-baseline.

### Step 2 — Swap `haze_noise`'s body. Both consumers, one function.

`beam_transport.wgsl` only — `beam_scatter` and the ambient bed both call `haze_noise`, so
there is one edit and no possibility of the two disagreeing.

**Gates:**
- `haze.enabled: false` and `haze_density < 0.001` goldens byte-exact (§4).
- The §4 SSIM table and the side-by-side set staged to the scratchpad. **Flagged, not
  installed.**
- The §5 measurement, against the honest per-pass baseline, with `B̄_hit` reported beside it.

> **Executed 2026-08-25.** Byte-exactness held exactly where §4 predicted and nowhere else,
> which is the result that says the change is confined to the density term:
>
> - **Byte-identical:** all 7 haze-disabled contract goldens (`metal-roughness-sweep`,
>   `textured-pbr`, `sun-off`, `sun-direction-{left,right}`, `sun-shadow-{hard,soft}`), all
>   3 `venue-no-haze-*`, all 3 `stage-builder-*`.
> - **Moved:** the 8 hazy contract goldens and 15 hazy `scenes-wgpu` images. Per-image
>   figures: 22–46 % of samples differ, **mean |Δ| 0.24–0.78 of 255**, max per-channel Δ
>   4–31, **SSIM 0.987–0.996**.
>
> The shape of that delta is the signature of a different realization rather than a
> different picture: a large *count* of touched samples at a very small *mean*, with the
> beam envelope, falloff and turbulence scale visually unchanged. No tiling artefact was
> visible in `dense-venue`, the widest scene and the one where a 10.7 m octave-2 repeat
> would show worst.
>
> Staged for Julian, before/after pairs, in `scratchpad/step2-review/`: `one-beam`,
> `overlapping-beams`, `gobo-seam-positive`, `fixture-shadowed-beam`,
> `volumetric-performance-smooth`, `dense-venue`, `led-bar`, `mover-fan`, `single-mover`.
> **Nothing is installed as accepted.**
>
> One thing the run exposed that is not about this change: `goldens/volumetric-stress-*.png`
> came back byte-identical, and they should not have — they are hazy. They are produced by
> `profile-volumetrics --capture`, not by the two golden binaries, so they were simply not
> regenerated. A golden set that a `render-goldens` run silently leaves stale is a trap;
> flagged, and regenerated as part of step 4.

### Step 3 — Sweep the constants, once, with the pictures in hand.

`size` / `format` / `K` from §3.3, on the same scene set. Cheap because §2.1 says time is
size-independent, so this is purely an image sweep: pick the largest period whose memory is
acceptable and whose quantization does not contour.

**Gate:** one recommendation, with the sweep's screenshots, for Julian to rule on.

> **Executed 2026-08-25 — and it exposed a cost rather than adding one.** Ten of eleven cases
> pass, every one far under budget, with `gpu_volumetric` p50 down hard: `transport-512`
> 177 → 45.9 ms, `beams-at-camera-128` 220 → 43.9 ms, `fixture-shadows-120` 34.6 → 9.5 ms.
>
> `fixture-shadows-120` fails on **`cpu_encode_submit_p95`: 3.75 ms against a 3.0 ms budget**,
> and the attribution is worth writing down because the obvious reading is wrong.
>
> | configuration | `cpu_encode` p95 | `gpu_volumetric` p50 |
> |---|---|---|
> | no bindings, ALU noise | 0.62 ms | 34.6 ms |
> | bindings present, ALU noise (step 1) | 0.62 ms | 34.3 ms |
> | bindings present, ALU noise **+ texture sampled at 1e-30** | 0.58 ms | 34.7 ms |
> | texture noise (step 2) | 3.75 ms | 9.5 ms |
>
> The third row is the control that settles it: the texture is genuinely sampled, and
> `cpu_encode` does not move. Sampling a 3D texture costs nothing on the CPU. Field size is
> not the cause either — 128³ (4 MB) measures 3.57 ms, the same as 256³ (32 MB).
>
> **What moves `cpu_encode_submit` is the GPU getting faster.** The span runs from frame
> entry through `queue.submit`, so it absorbs back-pressure: while the volumetric pass took
> 34 ms the CPU had slack and submit returned immediately; at 9 ms the CPU is the bottleneck
> for a 366-draw scene and the cost surfaces. The 3.0 ms budget was calibrated when GPU slack
> hid it.
>
> So this budget did not measure a cost this work removed, and it must not be quietly raised
> on that argument. It measured a cost this work *revealed*, and 3.75 ms of CPU per frame is
> 22 % of a 60 Hz budget — the next bottleneck for draw-heavy scenes, and now the honest
> headline number for them. `dense-geometry-noshadow-120` is worse at 6.02 ms and passes only
> because its budget is looser. Raising the budget is a decision about what to do next, not a
> bookkeeping step, so it is left failing and flagged.

> **Executed 2026-08-25.** Five configurations rendered through `render-goldens`, plus the
> pre-change aperiodic field as the reference the periodic ones have to be judged against.
>
> **Format — `R8Snorm` is measured safe, and is not being taken.** Rather than write a second
> pack path to find out, the bake rounded each texel to the grid `R8Snorm` lands on and kept
> storing f16, which is image-identical to the real thing. Against 256³ R16F: **meanAbs
> 0.004/255, max 1 LSB**, with plateau-run length and distinct-level count identical (80 / 163
> on `single-mover`). Quantisation is invisible, so the 16 MB is available whenever it is
> wanted — but taking it needs a second pack path (four snorm8 per `u32` rather than two f16)
> for a saving on an allocation nobody has complained about. Recorded, not built. The sweep
> knob was removed with it; this paragraph is the artefact.
>
> **Period — no visible tiling at any size tested, including a control chosen to fail.**
> Configurations were 256³ K=4 (32 m / 10.7 m, proposed), 128³ K=4 (16 m / 5.3 m), 128³ K=8
> (8 m / 2.7 m, a deliberately short positive control) and 256³ K=8 (16 m / 5.3 m). In the
> stacked comparisons — `step2-review/PERIOD-*.png` — all four read as the same beam with
> different smoke, the 2.7 m control included. A period three times shorter than the proposal
> does not read as a repeat at these scene scales.
>
> **The autocorrelation instrument was inconclusive and should not be cited as evidence.** It
> was intended to detect a repeat spike; on `dense-venue` it is dominated by the rig's own
> periodic fixture spacing, and on `single-mover` its short-lag values do not order by period
> or by K. The 2.7 m control and the aperiodic reference land on nearly the same numbers by
> different mechanisms — repetition raising correlation in one, genuine fine grain raising it
> in the other — which is exactly the confound that makes the statistic unusable here. The
> pictures are the evidence; this is written down so nobody re-derives the dead end.
>
> **Kept: 256³ R16F, K = 4, 32 MB.** Note this is margin, not measured need. 128³ (4 MB)
> showed no artefact either, and the honest reason to decline the 8× saving is that its
> octave-1 period of 16 m sits *inside* venue scale, where the proposal's 32 m does not — a
> risk argument, not an observation.

### Step 4 — Profiler and budgets.

Full `profile-volumetrics` green. Budgets that measured the cost this work removed get
adjusted **with a comment saying what removed them**, never deleted silently.

### Step 5 — App suites, machine quiet.

`CARGO_TARGET_DIR=…/target-pixel cargo test -p gpui-agent --features pixel --release --test
app_pixel`. Wall-clock bound; a loaded machine fails these for reasons unrelated to the
change.

### Step 6 — Record.

Append the measured result to this document's §5 in the house style, including a negative
result if that is what it is. Cross-link from `volumetrics-v2.md`, which currently has no
section that knows the march's cost was ever this.

---

## 8. Open questions

1. ~~**Does 10.7 m of octave-2 repeat show?**~~ **Closed, no.** Not at 10.7 m, and not at 2.7 m
   either — §7 step 3 rendered a deliberately-short control and it still reads as smoke.
2. ~~**Does R8Snorm contour?**~~ **Closed, no** — max 1 LSB, §7 step 3. Declined anyway, for
   the cost of a second pack path rather than for the image.
3. ~~**Fragment-shader occupancy.**~~ **Closed** — §5's measurement came in at ~7.8×, inside
   the pre-registered band, so the fragment shader hides the fetch as well as the compute
   microbenchmark did.
4. **What the *stills* could not answer, and now has:** temporal behaviour. Frame-to-frame
   delta over 60 frames at 1/60 s — long enough to advect ~1.6 texels and cross texel
   boundaries — measures 0.0142/255 against the old field's 0.0152 on `single-mover` and
   0.0211 against 0.0230 on `dense-venue`, with the per-step band flat to ±1 %. The new field
   moves *less* than the old one and does not pulse. `bin/haze-temporal-probe` is the
   instrument; keep it, because every future change to this field has the same blind spot.
5. **The ambient bed's eight taps** (`haze.wgsl`) become nearly free under this change, which
   makes the bed's current 8-tap budget an arbitrary number rather than a considered one.
   Worth revisiting *after* this lands, not during — it is a quality knob and this is not a
   quality change.
