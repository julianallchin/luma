---
name: finding-things-in-audio
description: How to hear a track before lighting it — pick a phrase, find its most distinctive sound, see how it moves across the beats with luma.music, then give each event one motion. Genre skills say WHAT to light; this is HOW to listen. Load before designing any section.
---
# Finding things in audio

Listen like the designer: focus on one phrase, narrow in on its most
distinctive sound, watch how the beats and sounds change across the beats,
then design motion that highlights the subtle parts.

Positions everywhere are `bar.beat.16th`: UI bar from 1, UI beat from 0, felt
16th inside that beat. Check `luma.music.feel` first: grids are often stored
at half the felt tempo (70 for a 140 track), so a UI beat can hold eight felt
16ths.

## Procedure

1. **Pick a phrase.** `luma.music.sections()` gives boundaries, drops and
   repeats (`41-64 ≈ 9-32`). Light one phrase fully, then reuse it on its
   repeats. Watch for 2-bar inserts that shift the phrase grid.
2. **Look at it.** `luma.music.listen("9-12")`: the mix without vocals in four
   bands (sub, low, mid, high), n2n kick/snare/hat, and vocals, per felt 16th.
3. **Find the most distinctive sound.** The ear goes to rate and tone, not
   loudness. `luma.music.modulation("9-16")` lists wobble and sweep peaks per
   felt bar and flags every rate change (`! mid 1/8 wobble (was kick-gap
   sweeps)`). A 3 dB change of rate is louder to the ear than a 10 dB change
   of level. Name the sound in the room's words: growl, womp, scoop, wipe,
   pulse.
4. **Find what breaks the loop.** `luma.music.deviations("9-32")` compares
   each bar with its same-parity neighbours: a missing kick, hats cut for a
   beat, a hole in the bass. Its first line is the 2-bar cycle itself (odd
   bars against even bars), where scoops and 2-bar stabs live.
5. **Find where else it happens.** `luma.music.similar("13")` ranks places
   that share what is special about a beat, bar or phrase. Use
   `mode="rhythm"` for the same rhythm in a different sound, `mode="sound"`
   for the reverse.
6. **Design.** Light the most distinctive sound in the most detail, then the
   loop around it.

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

## Don't

- Don't trust stem labels. One sound spreads across demucs stems; only
  `vocals` is exposed, and everything else is `rest`, split by frequency.
- Don't transcribe pitch only. A "held" note can carry a filter wobble;
  `modulation` sees it and a pitch track doesn't.
- Don't average whole bars. Rate switches and one-16th notches vanish in bar
  means. Stay at 16th resolution.
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
