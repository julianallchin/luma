---
name: dubstep
description: Scoring halftime bass music (dubstep, riddim, tearout, deep dubstep). Use when the drop is carried by bass articulation rather than a steady kick. Covers wub/growl tracking in Python, per-phrase build shaping, and fan-out across drops.
---
# Dubstep

## The thesis

In four-on-the-floor genres the grid carries the show: put motion on the pulse and it
reads as musical. Dubstep does not work that way. The kick and snare are sparse
scaffolding; the *event* is the bass — its rhythm, its timbre, and the speed at which it
moves. Two bars with identical drums and identical intensity can be a slow neck-snapping
growl and a triplet-rate screech, and lighting them the same way is the single most
common failure.

So this genre is meticulous, per-phrase, sound-design-tracking work. You are scoring
what the bass *does*, one phrase at a time, not what the grid says. Budget for that:
expect to look at every drop phrase individually, and expect to derive the analysis you
need rather than read it off a feature.

## Feel

- **Halftime.** The tempo is usually notated 140 but felt at 70. The backbeat lands on
  beat 3 of each bar, not on 2 and 4. Verify with `features.drum_onsets["snare"]` against
  `features.beats` — if snares cluster on every third-of-four beat, you are in halftime.
- **Do not put your main pulse on the snare.** At 70 felt BPM a strobe or chase on every
  snare is glacial. The snare gets a *hit*: one accent, short, high contrast.
- **Rides and hats do the subdivision.** Light, low-amplitude motion (small brightness
  wobble, hat-rate shimmer on a secondary group) can follow
  `features.drum_onsets["hat"]` or the kick pattern. Keep it dim; it is texture, not
  statement.
- **Space is the genre.** Deep dubstep leaves whole beats empty. Do not fill them.

## Drops are bass articulation, and you must measure it

There is no wub detector in the feature set. `features.bars.predictions` tells you a bar
is `halftime` and intense; it does not tell you the bass is doing a 3 Hz triplet growl
that switches to a held sub in bar 5. Derive it:

```python
import numpy as np
from scipy.signal import hilbert, butter, sosfiltfilt, find_peaks

sig = luma.audio.stems["bass"]
x = sig.values.mean(axis=1)              # [frames, channels] -> mono
t = sig.times_s                          # absolute track seconds
sr = 1.0 / float(np.median(np.diff(t)))

# 1. Amplitude envelope of the bass stem, decimated to a manageable rate.
env = np.abs(hilbert(x[::8]))            # decimate first: hilbert on 48 kHz is wasteful
fs = sr / 8
env_t = t[::8][: len(env)]

# 2. Band-limit to LFO range. Wubs live at ~0.5-8 Hz; below that is section
#    dynamics, above that is pitch/timbre, and neither is the modulation.
sos = butter(4, [0.5, 8.0], btype="band", fs=fs, output="sos")
lfo = sosfiltfilt(sos, env - env.mean())
```

Then, **per phrase** (per 4 or 8 bars, using `features.bars.starts_s`):

- **Rate.** Take the dominant frequency of `lfo` inside the phrase — an FFT peak, or
  mean inter-peak spacing from `find_peaks`. Express it in Hz *and* as a musical
  subdivision (rate × 60 / bpm, in beats). A 2.33 Hz wub at 140 BPM is triplet-eighths;
  that is a real, nameable rhythm you should light.
- **Onsets.** `find_peaks(lfo, ...)` inside the phrase gives you the actual accent times.
  These are what you align to. They are frequently *not* on the grid — a triplet growl,
  a swung wub, or a bass that drags behind the beat will all produce off-grid onsets, and
  snapping them to eighth notes is exactly the error this skill exists to prevent.
- **Character.** Compare envelope depth (peak-to-trough ratio) between phrases. A deep,
  fully-gated wub wants hard on/off lighting; a shallow undulating one wants a
  brightness/color breathe with no blackout.

Plot the phrase envelope with the peaks marked and look at it before scoring. The figure
comes back to you as an image; use it.

Cross-check against the mix: run the same envelope on `luma.audio.mix` for a bar you are
unsure of, since stem separation smears aggressive tearout bass into `other`.

## Per-bar hooks you do get

`features.bars.predictions` is `[bar, tag]` sigmoid probabilities over
`features.bars.tags`; threshold each tag with `features.bars.thresholds`, never a flat
0.5. Relevant tags: `halftime`, `breakbeat`, `build`, `riser`, `impact`, `fill`, plus
`kick` / `snare` / `hats` / `perc` for the drum bed and `sustain` / `pad` / `lead` /
`vocal_chop` for the top. `features.bars.intensity` is a continuous 0-5 regression, not a
probability — use it for terracing section brightness, not for detecting events.

Note the confidence gradient: `kick`, `snare`, `halftime`, `four_four` and `vocal_lead`
are reliable; `impact` (F1 0.20) and `fill` (F1 0.38) are weak. Treat a positive `impact`
as a hint to go *look at the audio* around that bar, not as a cue to place a hit.

## Technique

**Intro.** Establish palette and a single slow foundation. Almost no motion.

**Build.** Find the run of `build` / `riser` bars before each drop. Ride the riser: put a
pulse on a mid group and *tighten its rate* across the build — quarter, eighth,
sixteenth — while brightness terraces upward. Match the last acceleration to where the
riser actually peaks in the audio, not to the bar line.

**The bar before the drop.** Go near-black. One or two bars of near-darkness (or a single
held dim wash) is the strongest tool in this genre; it makes the drop free. Do not
decorate this bar. Snare rolls and vocal one-shots in it get nothing.

**Drop.** Full-rig accents on the **bass onsets you measured**, not on eighth notes.
Every hit is short and hard; between hits, either hold a dim floor or nothing. Give
sustained sub notes a held state, and reserve the fastest strobing for the phrase whose
measured LFO rate is actually fastest. Keep the color palette tight — contrast comes from
brightness and rhythm, not from cycling hues.

**Second drop.** It must not be a copy. Change one axis and only one: new color, inverted
group assignment (what was front is now back), or a rhythmic inversion (accent the gaps).
If the second drop's measured wub rate or character differs, that difference *is* your
variation — follow it.

**Breakdown / outro.** Long, slow, minimal. Let the room breathe before the next build.

## Delegation

This genre earns fan-out; the analysis above is expensive and it is per-drop.

1. First, alone: get the section map — beats, downbeats, bar tags, and the drop
   boundaries — and decide the palette and the group roles for the whole track. Say the
   direction out loud in your plan so the children are consistent.
2. Then spawn **one subagent per drop section** with the `Agent` tool and
   `run_in_background: true`, all in a single response so they run concurrently. Each
   prompt must be self-contained: the bar range, the BPM and beat grid, the palette and
   group roles you decided, and an instruction to run the wub-tracking analysis above for
   its own range and score only its own bars. Collect them with `get_subagent_result`.
3. Meanwhile score the intro, breakdowns, builds and outro yourself.
4. Stitch: after the children merge, open one edit over the whole track, check the
   transitions at each section boundary, and verify with `view.output.heatmap()` that the
   drops actually read as louder than everything around them.

Do not fan out per-bar; the phrase is the unit. Do not fan out at all on a short track
with a single drop.
