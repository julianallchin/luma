---
name: finding-things-in-audio
description: How to hear a track before lighting it — an investigation loop over luma.music (hypothesis, test, zoom, compare the repeat, revise), a skeptic pass, and parallel listeners for detail-heavy styles. Genre skills say how deep; this says how. Load before describing or designing any section.
---
# Finding things in audio

The first grid you read is a hypothesis. Keep testing it until the stopping
bar below is met.

Positions everywhere are `bar.beat.16th`, all three from 1, as in the
editor. A 16th is a quarter of a beat. A range such as `9.1.1-13.1.1`
includes its start and excludes its end. Check `luma.music.feel` first: it
reports the felt tempo and the drum feel. When the grid is stored at half the
felt tempo (70 for a 140 track) a felt 16th is half a 16th, so a felt 16th
can print as `13.1.3.5`. Quote positions exactly as the tools print them:
`13.1.4`, not "top of 13".

## Tools

- `sections()`: boundaries, drops and repeats (`41.1.1-65.1.1 ≈ 9.1.1-33.1.1`). Watch for
  2-bar inserts that shift the phrase grid.
- `listen("9-13")`: the mix without vocals in four bands (sub, low, mid,
  high), n2n kick/snare/hat, and vocals, per felt 16th.
- `modulation("9-17")`: wobble and sweep peaks per bar; `!` flags a rate
  change (`! mid 1/8 wobble (was kick-gap sweeps)`). A change of rate is
  louder to the ear than a 10 dB change of level.
- `deviations("9-33")`: each bar against its same-parity neighbours. The first
  line is the 2-bar cycle (odd bars against even), where scoops and 2-bar
  stabs live.
- `similar("13")`: places that share what is special about a bar (`"13"`),
  a beat (`"13.2"`) or a range (`"9-17"`). `mode="rhythm"`: same rhythm, other sound; `mode="sound"`: the
  reverse.
- The recipes below: 5 ms envelopes, pitch, kick templates.

Evidence strength: `rest` band levels and `modulation` are strong. n2n hats
over- and under-trigger: treat any hat pattern as low-confidence and confirm
it in the high-band envelope before you build on it. Stem labels are weak: one
sound spreads across demucs stems; only `vocals` is separated, everything else
is `rest`, split by frequency.

## The loop

1. **Style.** Name the style from `features.genres` and what `listen` shows.
   Load its genre skill. Its "Listening" section sets the depth.
2. **Phrase.** Pick one phrase from `sections()`. Understand it fully, then
   reuse the result on its repeats.
3. **Hypothesis.** Read `listen` over the phrase. Write one claim: the most
   distinctive sound, where it sits, what it does. Name it in the room's
   words: growl, womp, scoop, wipe, pulse.
4. **Test.** Run the tool that could prove the claim wrong: `modulation` for
   rate and filter motion, `deviations` for loop breaks, the envelope or pitch
   recipe for motion inside a held note.
5. **Zoom.** Where anything changes, go to 16th resolution, then to the 5 ms
   envelope. Never average whole bars; rate switches and one-16th notches
   vanish in bar means.
6. **Compare with a repeat.** Put the same-parity bar, the phrase repeat, and
   the `similar()` matches next to it. List what differs.
7. **Revise.** Rewrite the claim to fit every result so far. Go to 4.

Ask these of every phrase:

- What moves inside a held note: pitch, filter, or rate?
- What changes on the repeat?
- What is missing compared with the loop?
- What would a listener notice first?
- What would I notice only on the fourth listen?

**Stopping bar.** Stop when every phrase in scope has a named most-distinctive
sound with evidence at 16th positions, every `deviations` line and every `!`
in `modulation` is explained or marked `unexplained`, and each standing
question has an answer or `unknown`.

## Evidence rows

Record findings as rows, not prose:

```
13.1.4-13.2.1 | low  | -9 dB, kick present       | deviations | high | scoop cut?
10.1.1-11.1.1 | mid  | 1/8 wobble, was 1/4 in 9  | modulation | high | LFO rate change
11.3.3        | hat  | missing                   | listen     | low  | n2n hat
```

Position, band or source, observation, tool, confidence, interpretation.
Every sentence you tell the user maps to at least one row.

## Skeptic pass

After drafting a description, attack each claim: what tool output would make
it false? Run that. For "the scoop goes down", show the pitch; for "the drums
cut", show the kick onsets and the low band at that 16th. Answer each
objection with a row, or revise the claim.

When the budget calls for it, spawn a skeptic subagent instead. Brief: the
bars, `luma.music.feel`, your claims as rows, and "Try to disprove each claim
with `luma.music` and `luma.audio`. Return each claim as upheld, refuted or
untested, with the output that decides it. Do not edit the score. Do not
spawn subagents." Every refuted or untested claim gets a new test or leaves
the description.

## Parallel listeners

When the genre skill calls for detail, fan out up to four subagents over the
same bars, one per aspect:

- bass: growl, womp, scoop; `modulation`, low/mid envelopes, pitch;
- drums and cuts: onsets, `deviations`, holes in the mix;
- vocals: the `vocals` envelope, chops, phrase ends;
- transitions: the bars around each boundary, risers, fills, fake drops.

Brief each with the bars, the feel, its aspect, the row format and "Return
evidence rows only, no prose. Do not edit the score. Do not spawn
subagents." Merge the rows by position. Where two listeners explain one
position differently (drums: missing kick at `13.1.4`; bass: bass cut at
`13.1.4`), that position is probably the most interesting thing in the
phrase: test it yourself, or ask the user.

## Motion rules

- One motion per event: three womps are three chase bounces, not one long
  chase.
- Clip length = sound length. Read it off the peaks and gaps, not the grid.
- Direction is choreography, not pan. Drops are usually mono; choose a
  direction and keep it meaningful (a sweep up can travel up the rig).
- Dark by default. Light what you heard; leave the rest dark.
- The subtle part is the point: a rate switch, a missing kick, a scoop. A
  change the loop doesn't make deserves a change the lights don't make
  anywhere else.
- Don't explain every event as drums. Some events exist only in the mix: a
  missing kick exposes the bass duck and makes a hole.

## Recipes

The text tools are short functions over arrays you can use directly.

Cosine similarity of one bar against every bar (MERT, bar-pooled):

```python
import numpy as np
bars = luma.music.mert.bars                 # rows: bar, fullmix[768], drum[768]
v = bars.fullmix - bars.fullmix.mean(0)
v /= np.linalg.norm(v, axis=1, keepdims=True)
score = v @ v[list(bars.bar).index(13)]
[(int(b), round(float(s), 2)) for b, s in sorted(zip(bars.bar, score), key=lambda x: -x[1])[:8]]
```

Felt 16ths with a kick in at least 90% of drop bars, but not in this one:

```python
kick = luma.music.onsets["kick"]            # rows: time_s, bar, beat, sixteenth, slot, level_db
drop = range(9, 33)
per_bar = 4 * luma.features.beats_per_bar * luma.music.feel.ratio
grid = np.zeros((len(drop), per_bar), bool)
for bar, slot in zip(kick.bar, kick.slot % per_bar):
    if bar in drop:
        grid[bar - drop.start, slot] = True
usual = grid.mean(0) >= 0.9
[luma.music.label((13 - 1) * per_bar + s) for s in np.flatnonzero(usual & ~grid[13 - drop.start])]
```

The mid-band envelope of one bar at 5 ms, to see a wobble for yourself:

```python
rest = luma.music.envelopes.rest            # rows: time_s, sub, low, mid, high (dB)
t0, t1 = luma.music.beats.time_s[luma.music.beats.bar == 10][[0, -1]]
window = (rest.time_s >= t0) & (rest.time_s < t1)
rest.mid[window]
```

Pitch and brightness inside a held note. A pitch scoop moves `f0`; a filter
sweep moves the centroid and leaves `f0` flat:

```python
import librosa
audio = luma.audio.rest
sr = int(audio.sample_rate_hz)
y = audio.values.mean(1) if audio.values.ndim == 2 else audio.values
t0, t1 = luma.music.beats.time_s[luma.music.beats.bar == 13][[0, -1]]
y = np.asarray(y[int(t0 * sr):int(t1 * sr)], np.float32)
f0, voiced, _ = librosa.pyin(y, fmin=30, fmax=500, sr=sr, frame_length=4096, hop_length=sr // 200)
centroid = librosa.feature.spectral_centroid(y=y, sr=sr, hop_length=sr // 200)[0]
```
