# Gasworks / Get Lucky quality contract

This file is the durable acceptance contract for the renderer work at commit
`e7287e9acc8916d5f076c780f4bc2b3736ed888e`. The large `README.md` is the
experiment log; its 2026-09-11 approval describes the binary captured then and
does not approve a later build by itself.

## Current evidence (2026-09-12)

The latest full-score timing witnesses are the immutable P8/P4/P8 bracket in
`run-20260911-full-score/scalar-k-period8-v1-report.md`. Each covers 6,380 samples
through bar 41 with the same executable and 192 MiB arena. P8 GPU p95 is
38.125 / 35.470 ms versus the intervening P4's 52.644 ms; GPU medians are
11.839 / 12.039 ms versus 11.904 ms, so there is no stable median gain.
The repeated P8 run still has 1,851 frames above 20 ms. Strict schedule and
dispatch audits pass with no observed overflow. Unmodified GPU brackets and
algorithm-inactive frames also vary between runs; the measured tail reduction
does not establish that its entire magnitude is caused by P8. These are
headless GPU timings, not presented frame rates, and the sustained target
remains unmet.

P8 passes bounded close-view 50/75 Hz and re-entry image comparisons against
P4 (maximum same-frame difference one code, temporal difference two codes),
with crisp shadow boundaries and smooth clouds in native inspection. This
accepts a candidate for further work, not shipping quality: cumulative original
baseline comparisons with true playback history, additional views, and actual
app frame pacing remain required. The original quality baseline is unchanged.

The current binary's reconstructed period-1 reference now has a measured
bridge to the saved reference captures: close52 (24 frames), truss52 (64), and
re-entry74 (64). All differences are at most one code; maximum whole-image
RMSE is respectively 0.002803, 0.001968, and 0.002989, with at most 73, 36,
and 82 changed pixels. Native review retains the same silhouettes, shadow
edges, and cloud structure. Close/truss bridge directly to the source-era P1
binary; re-entry uses the earliest accepted later P1 transition capture.
These local snapshot comparisons support the reconstructed reference at those
views; they do not replace playback-history validation. Evidence is in
`run-20260911-full-score/cumulative-p1-bridge-*-audit.json`.

The first cumulative playback-history gate also passes: reconstructed P1 versus
P8 at the close camera, 50 Hz, replayed from time zero through 53.28 s. The
64 captured frames at indices 2600–2663 have maximum one-code image error,
RMSE 0.049449, gradient RMSE 0.069011, worst-tile RMSE 0.212523, and maximum
two-code temporal error. Native worst-tile review retains aligned shadow
details and smooth clouds. The strict auditor verifies the complete matching
prefix and constant byte destination; these captures make no timing claim.
This is bounded close-view acceptance. See
`run-20260911-full-score/cumulative-p1-to-p8-close52-50hz-audit.json`.

The truss camera's cumulative 75 Hz history gate also passes: 3964 prefix
frames, with indices 3900–3963 captured. Maximum image error is one code,
RMSE 0.081180, gradient RMSE 0.110282, worst-tile RMSE 0.351591, and temporal
RMSE 0.112192 with a two-code maximum. Native review preserves truss and beam
edges and smooth fog; the strict same-prefix audit excludes timing claims.
Re-entry history remains a separate gate. See
`run-20260911-full-score/cumulative-p1-to-p8-truss52-75hz-audit.json`.

The default-off fog visibility cache v2 also passes its bounded Metal lifecycle,
intentional-poison, and cross-bank slot-copy tests. Its four cold-transition
images are pixel-exact against cache-off. The measured converged late-window
gain is 1.663 ms whole GPU time, and snapshot filling takes 321–385 rendered
frames; neither establishes full-score performance or full-history quality.
See `run-20260911-full-score/FOG-CACHE-V2-P8-RESULT.md`.

The preceding full-score timing witness is
`run-20260911-full-score/per-resident-domain-v3-camera-k4-full.json`
(SHA-256 `cd0ae44ae39eb23c410d2771c9ad13ba940ddc76db6886f53b9f537f9b5e5861`).
It covers the same 6,380 samples through bar 41: GPU median 12.126 ms,
p95 44.204 ms, p99 49.083 ms, and 1,996 frames above 20 ms. Its 4,259 distinct
joined GPU submissions pass queue and dispatch conservation with no observed
overflow. These are headless GPU timings, not presented frame rates.

The cumulative image comparison directly from the accepted period-1 control to
v3's period-4 resident/venue-domain configuration is
`run-20260911-full-score/cumulative-v3-camera-k4-reentry74.json`, with the native
review recorded alongside it. Across this 64-frame transition, maximum image
RMSE is 0.34255, gradient RMSE 0.37203, and worst tile RMSE 1.17386; one pixel
reaches four codes and the 99th/99.9th percentile errors are one/two codes.
Native review accepts the low-amplitude cloud differences and preserved sharp
shadow boundaries for this window. This bounded acceptance does not replace
the original baseline or establish all-view, full-history, or live-motion
quality. P8's newer bounded gates do not extend this cumulative acceptance.

The paragraphs below retain the earlier evidence and its original scope.

**Verdict: scalar-K period 4 has broad current-score image acceptance, but the
50 FPS whole-score target is still unmet. The later scalar residual stride-1
change passes its bounded steady image gates; it does not yet have a whole-score
performance or transition-quality result.**

The authoritative broad image witness is
`run-20260911-full-score/scalar-k-period4-v2-quality-report.md`. One archived
binary (`haze-lab-scalar-k-period4-v2`, SHA-256
`091c4bf7ccb742d842784a766576b32b1650bca91790a772c524fe396510e053`)
was compared at period 1 versus period 4 across eight 64-frame pairs: 52 s and
56 s plateaus, close and truss cameras, 50 Hz age, and the cold/growth/fade/re-entry
transitions. Every same-frame RGB error is at most one code; compact-active frames
have no overflow; native review retains crisp truss/beam silhouettes and smooth
clouds. The 50 Hz suite observes 60.001 ms maximum K age. Transition inactive and
full-reset frames are reported separately rather than counted as phased evidence.

The matching 6,380-frame K4-v2 run preserves exact score times, 42 bins, camera,
database, light counts, and draw counts against control. GPU p95 improves from
99.611 to 62.264 ms and p99 from 106.399 to 77.032 ms, with no observed overflow
and a 20,177,251-entry maximum list below the 24 Mi-entry capacity. It still has
2,280 frames above 20 ms and therefore does not establish sustained 50–75+ FPS.

The later stride-1 archive (`haze-lab-scalar-residual-stride1`, SHA-256
`f9b7975981ec4c061d3fac30b4364b74526df6782143116151cd241ac7fc8fd4`)
reduces scalar residual storage from three words to one. Fresh period-1 close and
24 Mi-entry truss pairs are compact-active throughout with no overflow and retain
the accepted sparse one-code arithmetic floor. These are steady quality gates,
not a broad transition or sustained-performance approval.

The unconditional pinned outdoor-domain diagnostic is rejected: its zero-light
re-entry frame visibly changes global opacity and fixture contrast by moving the
sampled-cloud/analytic-tail boundary. Active-frame crops remain crisp and smooth,
so an active-only successor is under review, including required one-light/sparse
fixtures witnesses. Evidence is sealed in
`run-20260911-full-score/pinned-lighting-domain-diagnostic/REPORT.md`.

Outstanding final gates are a single candidate's broad transition/image sweep,
quiet full-score timing through bar 41 with no overflow/fallback, and matched-age
live presentation evidence. Replay track time does not by itself prove identical
live cloud phase because the app's medium clock uses elapsed haze age.

## Earlier image evidence (2026-09-11)

**Verdict: the current `release-replay-3s-210006` images pass the accepted
3-second stage-image envelope. Full release quality and sustained playback are
not yet proved.**

The 24-frame candidate has the same 2227x1391 source catalogue, suite, camera,
score state, settings, Metal adapter and environment as the accepted
`run-20260911-060051-final-audit/final-3s` replay. Nineteen frames are pixel
exact. The other five contain one changed pixel at one RGB code value each
(whole-image RMSE at most 0.000329, worst 64x64 tile RMSE 0.0091). Against the
original tile-8 `run-20260910-200424/3s-control`, it reproduces the approved
envelope:

| 24-frame replay metric (RGB codes 0-255) | Current vs original | Accepted maximum |
| --- | ---: | ---: |
| whole-image RMSE, median / maximum | 0.2012 / 0.2092 | 0.2012 / 0.2092 |
| peak channel / p99 / p99.9 | 4 / 1 / 1 | 4 / 1 / 1 |
| worst 64x64 tile RMSE | 0.6691 | 0.6691 |
| gradient RMSE | 0.2789 | 0.2789 |
| temporal residual RMSE / peak | 0.2774 / 3 | 0.2774 / 3 |

Frame 12, its worst truss tile, and the 16x absolute residual were inspected at
native pixels. Truss members and haze shafts remain resolved; the cloud field
is smooth. The residual is low-amplitude scene-following structure and
1-code speckle, with no new blotch, band, or missing shadow edge.

This is bounded proof. The candidate directory has no timestamp, git state,
command, or executable hash in `replay.json`, so its pixels cannot be tied to a
specific release executable. It has no current close/truss still suite, 4.9 s
moving replay, full-bar playback, or post-upscale output. Its 63.234 ms median
GPU time was recorded while another GPU workload was active; it is neither a
valid regression result nor evidence for the FPS goal.

Two current-score workload controls now also exist at 52 s and 56 s. Each has
24 successive 75 Hz frames at 2227x1391 and 470 emitter states in every frame,
using score `7de2624c-6095-8ae1-a7bf-c9496baae2d8` from the immutable full-score
database. They were rendered by the preserved control executable whose SHA-256
is `0cfec91e0c98643ada868010871a7c498380cf0277553004c23e51ce0b01df0e`.
Their PNG hash-list SHA-256 values are `093936a663413f80f85c3605fee267ba1ab23805d198568c9d1395b707568c4a`
at 52 s and `0be6b925e9032da64942ca8403d832014f90fb0e889c5207095e9545ba9b4268`
at 56 s. These are image controls only: concurrent CPU compilation makes their
timings inadmissible.

Four six-frame current-score transition controls additionally cover shadow
capacity growth at 17.04 s, a one-frame 0-to-470 cold relight at 49.9067 s,
the 470-to-254 partial fade at 58.1867 s, and a 0-to-204 reentry at 74.72 s.
They use consecutive 75 Hz states with at least one preceding frame. Their
large intended image changes are temporal-edge references: a candidate must
match each frame without smoothing, lag, pre-echo, or stale-light resurrection.
Commands, counters, hashes, and contact sheets are sealed in
`run-20260911-full-score/transition-control-baselines.md`. Their short-run
timings are not performance evidence.

The archived 512-slot candidate executable (`73b0430ec63344b9e22a1e780bbef3677a0a0fe456a8d34ef1c5b80da4e06788`)
passes the image envelope against these controls while running the fused
fallback: at 52 s, whole-image RMSE median/maximum is 0.01615/0.01669,
gradient maximum 0.02350, worst tile 0.06988, and temporal maximum/peak
0.02290/2; at 56 s these values are 0.01588/0.01631, 0.02299, 0.06751, and
0.02226/2. Every same-frame channel error is at most one code. Worst-tile 8x
crops, truss crops, signed residuals, and block maps show sparse arithmetic
noise without new blur, banding, cloud blotches, or moved shadow edges.
This does **not** approve the compact fast path: all 48 candidate frames report
`compact.active=false`, `overflow=true`, a 16,864,826-entry list against
8,388,608 capacity, and 22-45 suspended slots. A compact-active paired witness
was therefore run at 52 s with a measurement-only 20 Mi-entry, 40/12/48 split.
The first 64-lane attempt is invalid: its 6,870,502-entry single segment asked
for 107,352 indirect compute workgroups, above Metal's 65,535 limit, and left
residual RGB unwritten. Its fireflies and large coherent error do not diagnose
the 512-slot packing.

The dispatch-safe `LUMA_HAZE_RESID_LANES=128,128,128` repeat is compact-active
24/24 with no overflow or suspended slots and passes the image gates. Its
whole-image RMSE is 0.01826 median / 0.01878 maximum, peak error is one code,
gradient RMSE reaches 0.02647, worst-tile RMSE reaches 0.07273, and temporal
residual reaches 0.02586/peak 2. Control and candidate motion match within the
reported 0.0001 precision. Worst-tile and truss crops preserve the shadow
edges; signed residuals and block maps show sparse arithmetic noise without
bands, blotches, fireflies, blur, or halos. This validates the 512-slot packed
path at 52 s under the recorded diagnostic environment. It does not make that
high-memory configuration a shipping setting or supply final timing evidence.

The follow-up 2D-dispatch / corrected-GC candidate executable
(`33df6f15457fec3a2a2877c0b68f1843cbe040f4663cf642dff71d0e821a6abd`)
also passes at 52 s using the default 64/64/32 lanes, with the same 20 Mi-entry
diagnostic capacity and no lane override. All 24 frames are compact-active,
with no overflow or suspension; `dirty_slots=0`, `rebuilt=false`, and
`classify=false` throughout. Whole-image RMSE is 0.01825 median / 0.01879
maximum, peak error is one code, gradient RMSE reaches 0.02648, worst-tile
RMSE reaches 0.07273, and temporal residual reaches 0.02586/peak 2. Adjacent
motion drift is at most 0.000024 RMSE. Native truss and worst-tile crops,
signed/absolute residuals, and the block map show only sparse one-code noise;
shadow edges remain crisp and clouds remain smooth. In the quiet paired
offscreen run, GPU median/p95 was 85.14/88.39 ms for the preserved control and
78.37/85.20 ms for the candidate. This is evidence for the 2D dispatch and GC
fixes at one steady window, not sustained presented FPS or a shipping-capacity
result.

Using the same 2D/GC executable, the memory-heavy table-24 experiment also
passes at 52 s. Its only runtime changes are
`LUMA_INTERVAL_CACHE_TABLE_BITS=24` and a 40/50/10 residual split. The 24
frames are active with no overflow or suspension and report segment counts
`[6,870,502, 8,778,369, 1,215,955]`; dirty, rebuild, and classify stay false.
Whole-image RMSE is 0.01807 median / 0.01859 maximum, peak error is one code,
gradient RMSE reaches 0.02621, worst-tile RMSE reaches 0.07217, and temporal
residual reaches 0.02562/peak 2. Adjacent-motion drift is at most 0.000023
RMSE. Native truss/worst-tile crops and residual maps preserve crisp edges and
smooth clouds. Quiet offscreen GPU median/p95 is 50.18/51.40 ms versus the
same 85.14/88.39 ms control. This establishes leverage, but remains well above
the 20 ms minimum-FPS budget and uses a memory-heavy diagnostic table and
20 Mi-entry queue. The table-bits setting is present in adjacent isolation
metadata but omitted by `replay.json`'s environment recorder; the binary and
other captured inputs are identified.

The archived direct-arena v1 executable
(`3874f4561d542b7c3abd6c8dae873ee595d6b6ab1998e5d07c6eae08f8a5805d`)
passes the same current-score quality gates at 56 s and across the capacity,
cold-470, partial-fade, and reentry transition windows. At 56 s all 24 frames
are compact-active with no overflow; maximum whole-image/gradient/worst-tile
RMSE is 0.01834/0.02586/0.07160, temporal residual reaches 0.02512/peak 2,
and every same-frame error is at most one code. The first three transition
windows likewise peak at one code and retain their intended cuts, crisp truss
shadows, and smooth cloud field.

The original `control-transition-reentry-74s/frame-003.png` is a preserved
bug witness and is not the quality oracle for that frame. At 74.733 s the old
control enters compact mode before a later list-count readback reveals that
the list is too small; without a same-frame fallback it drops coherent light.
Against a same-v1 forced-fused oracle the old control has RMSE 2.81069, peak
68, p99 14, and worst-tile RMSE 18.71136. Direct-arena v1 instead matches that
oracle at RMSE 0.08911, peak one, gradient RMSE 0.12385, and worst-tile RMSE
0.375; the other five frames are exact or within one code. This exception is
limited to the proved bad reference frame and does not relax any structural or
numeric gate. The arena-off capture also misses first-frame work, then reports
a 7,248,527-entry traversal segment against 5,242,880 capacity and disables
compaction. It cannot serve as a substitute baseline.

In a quiet uncounted steady 52-second pair using the same v1 binary, control
GPU p50/p95 was 76.494/96.224 ms and arena-128 was 64.805/65.361 ms; median
improved 11.689 ms (15.3%). Both were compact-active for all 24 frames with no
overflow or classification. The sequential control developed late p95
excursions, so this is leverage evidence rather than a release-grade sustained
FPS claim. The arena also consumes 128 MiB and remains an opt-in prototype.
Full metrics, provenance, transition audits, the fused oracle, and sealed
hashes are in `run-20260911-full-score/direct-arena-prototype-v1-report.md` and
`run-20260911-full-score/direct-arena-v1-quality-evidence.sha256`.

The subsequent same-frame overflow-fallback v3 executable
(`53a24ffd09d68ed2a3fd6975d13c8217964115b7bb999123cf84b11412f2926d`)
corrects the default 8 Mi-entry saved-score reentry itself: all six 74 s PNGs,
including the formerly defective 74.733 s frame, are byte-identical to the
forced-fused oracle. The delayed following-frame counters still expose the
12,636,113-entry list and oversized traversal segment, while the first image
proves the GPU chose fused work in that same frame.

The same v3 binary's packed 128 MiB arena passes at both 52 s and 56 s. Against
same-binary arena-OFF controls, maximum whole-image RMSE is 0.00407/0.00390,
peak error one code, gradient RMSE 0.00576/0.00551, worst-tile RMSE
0.02552/0.02706, and temporal residual 0.00558/0.00529 with peak one/two.
Both sides are compact-active 24/24 with no overflow or classification; native
inspection retains smooth clouds and crisp shaft/truss edges. Quiet offscreen
GPU p50/p95 is 76.374/78.323 to 56.707/57.004 ms at 52 s and
76.035/76.901 to 56.903/57.690 ms at 56 s. These sequential runs establish
leverage, not sustained presented FPS or approval of a 128 MiB shipping cost.

The Sunstrip pair-merge diagnostic in that binary is rejected. It reduces the
52 s scene from 470 to 320 cones, but moving each equal pair to one midpoint
produces coherent source and shaft-edge residuals. The close view reaches RMSE
0.312, peak 23, and worst-tile RMSE 2.690. A derived current-score truss view
reaches RMSE 0.577, peak 28, gradient RMSE 0.455, and worst-tile RMSE 2.411;
signed residuals follow truss chords, braces, lit surfaces, and vertical shafts.
This is structured shadow-boundary error rather than permitted noise. The
broader pair sweep was stopped and the experiment must remain disabled.
`run-20260911-full-score/same-frame-overflow-fallback-v3-quality-report.md`
contains the full audit.

The scalar-K period-4 v2 diagnostic passes the broader current-score image
gate. Eight 64-frame pairs cover steady 52/56 s, the truss-near camera, 50 Hz,
and the cold-470, capacity-growth, fade/reorder, and re-entry transitions.
Every same-frame difference is at most one code. The steady views are
compact-active 64/64 with no overflow; 75 Hz observes a 40.001 ms maximum K
age and 50 Hz observes 60.001 ms. Transition reports distinguish inactive,
full-refresh, and phased frames instead of counting fallback as temporal
evidence. Native/crop inspection retains crisp truss and beam boundaries and
smooth clouds. The truss maximum crosses the numeric review margin (RMSE
0.05333, gradient 0.07417, tile 0.22299), but its one-code residual is sparse
and has no coherent structural pattern. Exact paths, coverage, hashes, and
limitations are in `run-20260911-full-score/scalar-k-period4-v2-quality-report.md`.

The matching uninterrupted 6,380-frame offscreen diagnostic validates the
frozen score, 42 timing bins, camera, database, light counts, and draw counts.
It reports no overflow, a maximum 20,177,251-entry list below the 24 Mi-entry
cap, 4,260 compact-active frames, 1,953 classification frames, and 2,872
full-refresh / 1,388 phased frames. GPU p95 improves from 99.611 to 62.264 ms
and p99 from 106.399 to 77.032 ms, but 2,280 frames remain above 20 ms. This is
large progress and still fails the sustained 50 FPS gate.

The interval-cache blackout-retention v1 experiment is correctness-accepted
and performance-unaccepted. Focused tests prove exact 300→3→0→300 retention,
global/key/pressure invalidation, frustum-cull safety, and shader sentinel
guards. Its authored 74 s replay remains within one code and matches K4-v2
coverage, but retained dense identities trigger repeated dirty-majority
compact-list rebuilds during the shrinking cue. The experiment was archived
and removed from the live source; it must not be stacked into a performance
candidate until that transition churn is fixed. Evidence is in
`run-20260911-full-score/interval-cache-blackout-retention-v1-report.md`.

## Immutable references

Do not regenerate or edit these inputs. `manifest sha256` below is SHA-256 of
the sorted concatenation `relative-path NUL file-sha256 NEWLINE` for the PNG and
capture/replay input files under the named directory.

| Artifact | Purpose | Files | Manifest sha256 |
| --- | --- | ---: | --- |
| `native-before-shadow/` | close-view first/pan/blackout/relight/still reference | 13 | `6fd43cedd8837301c49c486d4f82f75ee54dd8c0f76ceff366acb398a90eb688` |
| `baseline/` | five wide and truss views, 60 probes | 65 | `dc0b48d103f089c5d8bc15be86ab088929b8fe30c1b7742e951d24e0c6b8798a` |
| `run-20260910-200424/3s-control/` | original tile-8 moving reference | 27 | `a8261aa15a2873a7b186a6a690d8610b9c553e05e32ecd84cdb3c2943e8f16b0` |
| `run-20260910-200424/3s-candidate/` | accepted tile-16 morning comparator | 27 | `176f52dd35760ad81dabbb4f6796047a4f973c3ab9213d335863be6edc365d6d` |
| `run-20260911-060051-final-audit/final-3s/` | accepted end-to-end image stream | 27 | `011e89402bbce65827903efa927ac0c3cfd290cfa442032445026b69ce008a97` |
| `release-replay-3s-210006/` | current, incompletely identified candidate | 27 | `2826fb71f491e9b091bb2204abc765a2f210f8231f0b49137ff3fb85c0e5447a` |
| `run-20260911-full-score/control-52s/` | current-score heavy control, 24 PNGs plus sealed hash list | 28 | `093936a663413f80f85c3605fee267ba1ab23805d198568c9d1395b707568c4a` (hash list) |
| `run-20260911-full-score/control-56s/` | current-score heavy control, 24 PNGs plus sealed hash list | 28 | `0be6b925e9032da64942ca8403d832014f90fb0e889c5207095e9545ba9b4268` (hash list) |
| `run-20260911-full-score/candidate-512-52s/` | 512-slot candidate, fused fallback only | 28 | `4b0eefb48c0968ded3aaa22bc3f1c69e1e0a18fdb619938a3e2ca9bc02b640cd` (hash list) |
| `run-20260911-full-score/candidate-512-56s/` | 512-slot candidate, fused fallback only | 28 | `a9ed7d1a3f70540e2abc73b586f72707d97fe0ac3bd724f0e650e69e63309eb7` (hash list) |
| `run-20260911-full-score/control-diag20m-52s/` | compact diagnostic control; byte-identical to original control | 28 | `093936a663413f80f85c3605fee267ba1ab23805d198568c9d1395b707568c4a` (hash list) |
| `run-20260911-full-score/candidate-512-diag20m-52s/` | invalid: indirect dispatch exceeds device limit | 28 | `368ebfabd048fd8fbf4ab95b763fc2111d5f0794a060022436a9b5d46295df9e` (hash list) |
| `run-20260911-full-score/candidate-512-diag20m-lanes128-52s/` | compact-active diagnostic quality pass | 28 | `d92f7763633189c4a1bff7df531e76bbc379aee89ff3f7a58ffc386883757d31` (hash list) |
| `run-20260911-full-score/control-paired-2d-gc-52s/` | quiet paired 2D/GC control | 28 | `50b862860301d89834a999bcf1d10776478c9a5ba1a84074500f045e7ef2aa2a` (hash list) |
| `run-20260911-full-score/candidate-512-2d-gc-52s/` | default-lane 2D dispatch / corrected-GC quality pass | 28 | `a8dd4be465a08af86a14e716396c83dfd583f5ce51e45846dc18654e301f75e0` (hash list) |
| `run-20260911-full-score/candidate-512-2d-gc-table24-52s/` | memory-heavy table-24 leverage experiment; quality pass | 28 | `7b55c1e3145fd6d33cfa4ed2a4b80ff3fba70b84a1830b9b2fc8b1bbf7b449f2` (hash list) |
| `run-20260911-full-score/control-transition-capacity-17s/` | six-frame shadow-capacity growth control | 10 | `720d094c25f86531fddc01a386aab5e056b01a2a8bedd2634a5f55333cf2c7f6` (hash list) |
| `run-20260911-full-score/control-transition-cold470-49s/` | six-frame 0-to-470 cold-relight control | 10 | `895fdb4220a001b3ce2d0beb8609fd854584244802a9807d3892666e41c81713` (hash list) |
| `run-20260911-full-score/control-transition-partial-fade-58s/` | six-frame 470-cone partial-fade control | 10 | `4761100f29dcb66d7e204d795f40981e93b4ba613dd9a5249135466388a0ee7d` (hash list) |
| `run-20260911-full-score/control-transition-reentry-74s/` | post-blackout control; frame 003 is the preserved dropped-light bug witness | 10 | `b2a372ad4e94c2759e8a9f0424c175005827d429df19cd16323b5c8bdad6114c` (hash list) |
| `run-20260911-full-score/direct-arena-v1-quality-evidence.sha256` | v1 controls, candidates, fused oracle, reports, and inspection artifacts | 431 | `575038083840b6ae2e5a0ccb30c42e38abc1c5bfd33e42d61a4116629b95b34a` |
| `run-20260911-full-score/same-frame-overflow-fallback-v3-quality-evidence.sha256` | v3 overflow, packed-arena pass, and rejected pair experiment | 481 | `f91fa1514c513b0fab1d4838d214d7d35b820e09f8e81aa2a504f3fc4886e555` |

All four moving sets use source catalogue SHA-256
`dfa82d11bc77795a2a32ea5107036b69e64426e30d6b987dbcf52c0fa3302378`
and 3-second suite SHA-256
`3a3f6252864704a9078af02c692c86fd2f9e224fc6012561bce076bbc409b1c9`.
The accepted audit reports and inspection crops are in
`run-20260911-060051-final-audit/`.

## Acceptance gates

Every candidate capture must record commit, dirty-diff hash, executable
SHA-256, command, environment, OS/GPU, output and internal render sizes,
display refresh, UTC start time, and whether another GPU process was active.
Input JSON must be byte-identical to its reference. Environment differences
must be named explicitly; a measurement-only comparison cannot silently become
an approval.

Stage-image gates use native, unscaled pixels:

- Compare each probe against its original reference. The accepted 2026-09-11
  values are the baseline, not an exact-pixel ceiling. Review a candidate when
  whole-image, gradient, or temporal RMSE rises by more than 0.05, or worst
  64x64 tile RMSE rises by more than 0.10, over the matching accepted probe.
  These provisional review margins permit modest additional unstructured
  1-code noise; an increase needs image evidence, and crossing a margin calls
  for review rather than automatic rejection. The accepted global baselines
  are: still suite RMSE 0.280, gradient 0.347, worst tile 0.900; 3 s replay
  RMSE 0.210, peak 4, gradient 0.280, worst tile 0.670, temporal residual
  0.278/peak 3; 4.9 s replay RMSE 0.184, peak 2, gradient 0.253, worst tile
  0.394, temporal residual 0.255/peak 2. The one known cold truss rim pixel may
  peak at 10; other settled stills may peak at 5.
- Frozen seven-frame still sequences should be exact. If a fresh adjacent
  control reproduces arithmetic jitter, capture it twice and use its max range,
  varying-pixel count, and RMSE as the nondeterminism floor; the candidate must
  not exceed that paired-control floor. On moving replays, candidate
  adjacent-frame motion RMSE should track reference motion within 0.001 on
  every frame, or any larger drift must be inspected as possible pop or lag.
- Inspect the full frame at 1x, the worst 64x64 tile at 8x, a signed residual,
  a 16x absolute residual, and 8/16-pixel block-mean maps. Modest unstructured
  1-code noise is allowed. Reject coherent bands, cloud blotches, fixed tile or
  quad patterns, blur, halos, and stair steps regardless of aggregate metrics.
  The accepted close-view block-mean neighbour step is at most 0.28 code; an
  increase beyond 0.30 requires explicit inspection and justification.
- `gasworks-truss-near`, `gasworks-truss-shadow`, and replay frame 12 are the
  semantic shadow witnesses. At 1x and in the worst crops, all truss members,
  diagonal braces, rims, shaft occlusions, and shadow boundaries must remain
  as crisp as the originals. Pixel aggregates cannot waive this inspection.

Presented-performance gates use the shipping app and compositor, continuously
from bar 1 through the end of bar 41 after thermal warm-up. Use the 120 Hz
display and record both stage GPU time and actual presented timestamps. The
hard pass is a whole-run mean of at least 50 presented FPS, no rolling 5-second
window below 50 FPS, presented-frame p95 at most 20 ms, and no interval above
100 ms. The target tier is at least 75 FPS mean with stage GPU p50 about 11 ms
and p95 below 16 ms. Report resize, shader compilation, score evaluation,
waveform, and readback stalls rather than trimming them from the run.

## Captures still required

1. Re-run the 3 s replay from an identified release executable on an otherwise
   idle GPU, preserving all 24 PNGs and the identity metadata above. Audit it
   against both the tile-8 original and `final-3s`.
2. Capture the identified release executable with the complete 12-probe close
   suite and 60-probe wide/truss suite, including cold `first`, pan, blackout,
   relight, and seven stills. Run `audit_pixels.py` against
   `native-before-shadow/` and `baseline/`, and retain the required crops/maps.
3. Re-run both 24-frame moving suites (3 s and 4.9 s) after cooldown. The 4.9 s
   replay is required because the current candidate covers only the 3 s phase.
4. After choosing a shipping-safe residual-queue capacity, repeat the
   compact-active pair in that production configuration and extend it to 56 s.
   Require `compact.active=true`, no overflow or device-limit violation, and
   every image gate above. The 128-lane and 2D-dispatch 52 s diagnostics
   validate image quality but use a measurement-only 20 Mi-entry allocation.
5. Record one uninterrupted shipping-app trace from bar 1 through bar 41 at the
   target window/fullscreen size, with presented timestamps and
   `LUMA_STAGE_TRACE`. Derive bar boundaries from the captured beat grid rather
   than hard-coding seconds. Publish whole-run and rolling-window FPS plus GPU
   p50/p95/p99 and stall counts.
6. Capture the post-compositor output at bars 1, 9, 17, 25, 33, and 41 and at
   the close truss witness, recording internal and display sizes and upscaler.
   The 2227x1391 replay proves the stage image only; it does not prove the
   fullscreen MetalFX/bilinear result is crisp or free of cloud blotches.

Old approval becomes current release proof only after these captures satisfy
the gates with complete build provenance.
