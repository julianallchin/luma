---
name: lasers
description: How laser projectors work in a clip graph and when a show should use them. What a figure, an element and the venue's canvas are, the mistakes that make lasers glitch or clump, and worked looks. Read before building any laser clip.
---
# Lasers

A laser is the loudest instrument in the room and the least forgiving. A
light that is slightly wrong looks dull; a laser that is slightly wrong
looks broken. Understand the mechanism first, then decide whether the music
wants one at all.

## How a laser clip works

- **A projector draws on a canvas.** The venue has one laser window
  (`luma.venue.laser_window()`), a range of directions in the room, and
  the canvas runs from -1 to 1 across it and from -1 to 1 up it. Every
  `x`, `y` and `size` is in canvas units, not metres and not degrees, and
  the same place on the canvas is the same direction from every projector.
  Until the venue sets its window (`window.set` is false) it is fitted to
  the projectors: ahead into the house, just above level. Check it before
  the first clip, and ask the user where the beams should go if it is not
  set.
- **A figure is elements of one kind.** `figure(count=8)` is eight beams;
  `figure("line")` is a sheet; rings, polygons and grids are the others. A
  clip is `laser([figure, ...])`, at most 64 elements per projector.
- **Every projector draws the whole figure.** Four projectors and
  `count=6` is twenty-four beams, six from each. To make the projectors
  differ, read them as heads: `space(split(), kind="order")` gives each
  projector its place, `mirror(direction=(1, 0, 0))` makes the two sides of
  the rig draw mirror images, and `shuffle` picks among them.
- **An element is a head of its figure.** `space`, `random`, `shuffle` and
  `mirror` read a figure's elements the way they read a bar's cells, so the
  ways you already move light across heads work on beams.
- **`along()` is the place within one element**, from a line's start to its
  end. It bends a line (`y=curve(along(), "Sine", ...)`) or runs a color
  down it.
- **Color is the figure's.** A `color` clip never reaches a projector, and
  black is the laser off. Fade a figure by fading its color or the clip's
  `alpha`.
- **Replace clips morph; colors cut.** Where two Replace laser clips
  overlap, elements slide from one clip's places to the other's, matched in
  order, but the color switches at once. Add draws both.

## What makes lasers look broken

Each of these came from a real show.

- **A snap.** A value that jumps (a fan that opens instantly on every kick,
  a wave that flips sign, a loop that wraps to a different place) makes the
  beams teleport. On lights a jump is a hit; on lasers it is a glitch. Give
  every move an ease, and let a one-shot end where the next begins.
- **A hard handoff.** Two clips that meet at a bar line cut color and place
  together. Carry one look through with a single longer clip, or overlap
  them so the places morph.
- **A clump.** Independent random positions bunch up. For beams that feel
  random but stay spread, give each element its own slot across the canvas
  and add a small random offset to it: `x = slot + jitter`. For beams that
  swap places, `shuffle` the order they take the slots in.
- **Randomness on the clock.** A random value belongs on a place or a
  color, redrawn per event by `random(time=...)` or `shuffle(time=...)`. Do
  not write a jittery curve over time to imitate it.
- **A shape too small to see.** A wave whose height follows a quiet signal
  is a flat line. Give motion a fixed, visible range.
- **Too much at once.** Sheets, beams and rings together, each moving, read
  as noise. One figure with one motion is a look.

You cannot judge a laser from one still. Measure the figure's places over
the clip, render two or three moments from the audience view, and say so
plainly when a render is too faint to confirm what the numbers show.

## When the music wants lasers

Lasers are thin, hard and precise, so they answer sounds with those
qualities: a screech, a stab, a sharp snare, a filtered sweep that opens and
closes, a sustained tone that wants a horizon. They answer warmth, groove
and vocals poorly. Decide from what you hear:

- **Scarcity is most of their power.** A show whose lasers appear only when
  the sound turns sharp or the drop lands has an event. Lasers running
  through the intro and verses have spent it. When in doubt, hold them for
  the peak sections and leave them dark elsewhere.
- **One sound, one job.** Pick the sound the lasers answer in a section and
  give them nothing else. If the bars already carry the kick, the lasers do
  not need it too.
- **Flat reads cleanest.** Beams in one horizontal row, sheets parallel to
  the ground and travel from side to side read as architecture over a
  crowd. Vertical motion and tall shapes scatter and usually read as chaos.
  Go tall only when the sound itself rises or falls.
- **Wide, and symmetric.** Use the width of the canvas; a narrow cluster in
  the middle looks like a fault. Mirror the two sides of the rig unless the
  music is throwing something from one side to the other.
- **Speed follows the sound's edge.** A long filtered tone gets a sheet that
  drifts. A stab gets beams that are there and gone, with a short fade and
  never a hard cut to black.
- **Some songs want none.** A warm, emotional or acoustic track is often
  better with the projectors dark all night. Say so; the user can overrule.

## Worked looks

Change the numbers to what you measured. `Ramp up` and the other shape
names are curve presets.

A beam from each side on a snare, bright and then fading, mirrored:

```python
t = time()
snare = laser([figure(heads=mirror(direction=(1, 0, 0)), count=1, x=0.0)],
              alpha=curve(t, [[0, 1, "hold"], [0.3, 1, "ease-out"], [1, 0]]))
```

A sheet that opens as it crosses and closes as the sound ends, low and
flat, drawn by one projector chosen at random for each clip. One clip per
sound:

```python
t = time()
pick = space(shuffle(), kind="order")
only_one = curve(pick, [[0, 1], [0.25, 1], [0.25, 0], [1, 0]])   # a quarter of the projectors
sweep = curve(t, "Ramp up", gradient=[(0, "#200a60"), (0.5, "#3080ff"), (1, "#e0f0ff")])
sheet = figure("line", y=-0.85,
               x=curve(t, "Ramp up", low=-0.6, high=0.6),
               size=curve(t, [[0, 0, "sine-out"], [0.45, 0.55, "sine-in-out"], [1, 0]]),
               color=sweep * only_one)
wah = laser([sheet], alpha=curve(t, [[0, 0, "sine-in"], [0.25, 0.8, "sine-out"], [1, 0]]))
```

A row of beams that jump a little on every beat and never clump:

```python
k = time(every=1)
slot = curve(space(split(), kind="order"), "Ramp up", low=-0.9, high=0.9)
jitter = curve(random(time=k), "Ramp up", low=-0.08, high=0.08)
row = laser([figure(count=8, x=slot + jitter)])
```

Two beams per projector that fade with a bass note:

```python
t = time()
place = curve(space(split(), kind="order"), "Ramp up", low=-0.6, high=0.6)
fade = curve(t, [[0, 1, "sine-out"], [1, 0]], gradient=[(0, "#000000"), (1, "#ff2060")])
bass = laser([figure(count=2, x=place, color=fade)])
```

A horizon for a held tone: one soft-ended sheet that drifts over sixteen
beats and never flips:

```python
soft = curve(along(), gradient=[(0, (0, 0, 0)), (0.25, (0, 1, 0.6)), (0.75, (0, 1, 0.6)), (1, (0, 0, 0))])
slow = time(every=16)
horizon = laser([figure("line", size=1, y=curve(slow, "Sine", low=-0.9, high=-0.7), color=soft)])
```

The `effect-catalog` laser table has more shapes; `node-cards` has every
input of `figure`, `laser` and `along`.
