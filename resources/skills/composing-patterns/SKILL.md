---
name: composing-patterns
description: How to build clip graphs and place them as clips, in the fewest calls. The working order, a complete timed example, how to measure the result as numbers, how to read the checker's errors, and the API rules that cost the most retries. Read before placing clips.
---
# Composing patterns

A clip has one graph on one selection for one time range. You build the
graph in Python with bare builder functions. Layers combine clips by blend
mode. Build, add, measure, apply.

## The working order

1. **Read the rig.** `luma.venue.describe()` for the shape, `luma.venue.groups()`
   for the exact group names. Use those names as written. Never make a name
   from a fixture label.
2. **Find the effect.** Load the `effect-catalog` skill. It gives each effect
   as Python. Load `node-cards` for what each node and input does.
3. **Build the graph.** Start from a catalog row or `preset("Chase")`. Change
   the numbers that the music asks for.
4. **Add** with `edit.add_clip(graph, name="Kick chase", at=("9", "13"), selection="name")`.
   A position is `bar.beat.16th`, all three from 1; the range excludes its end.
   The checker runs at once. A `ClipError` tells you the node, the input and
   an example fix.
5. **Measure** with `edit.window(...)`. Read the numbers before you look at a
   picture.
6. **Look once** with `luma.venue.render(edit=edit, only=clip, t=...)`.
7. **Apply** with `edit.apply()`, then tell the user what the room will feel like.

## A complete example: a floor, a chase and a hit on every snare

A dim blue floor, a chase that crosses the bars once every two beats, and a
short white hit placed on each snare the track really has. Nothing here
follows the audio level: the chase runs on the clock and the hits sit where
the snares were measured.

```python
edit = luma.track.edit()
drop = ("33", "49")

edit.add_clip(color(color="#1030ff", brightness=0.15),
              name="Blue floor", at=drop, selection="all")

t = time(every=2)
place = space(shift=curve(t, "Ramp up", low=-0.2, high=1), scale=0.2)
pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]])
edit.add_clip(color(brightness=0.6 * pill), name="Chase", at=drop,
              selection="led_bars_vertical", blend="screen")

def hit(every=None):
    return color(color="#ffffff", brightness=0.8,
                 alpha=curve(time(every=every), "Ramp down"))

snare = luma.music.onsets["snare"]
start, stop = (luma.track.seconds_at(p) for p in drop)
for s in snare.time_s:
    if start <= s < stop:
        edit.add_clip(hit(), name="Snare", seconds=(s, s + 0.2),
                      selection="strobes", blend="screen")
edit.check()
```

The look is one function, so every hit shares one graph. When every hit in
the range sits on the grid, one clip with `hit(every=4)` does the same job;
when any hit is off the grid or a bar is skipped, it is one clip per hit.

`clip.graph.source()` prints Python that builds the same graph. Use it to
read a clip that is already in the score, change a line, and pass the new
graph to `edit.update_clip(clip, graph=...)`.

## Rules that cost the most retries

- **A coordinate goes through a curve.** `brightness=time()` is an error.
  Write `brightness=curve(time(), "Ramp up")`.
- **A clip needs a name.** `add_clip(graph, name="...")`. A preset graph
  keeps its preset name.
- **One output per clip.** Color and strobe together are two clips.
- **Events come from `time(every=...)`.** For once over the clip, give
  `time()` no every. Every, duration and delay are beats; phase is degrees (360 is one event).
- **Reuse a variable to share a node.** `t = time(every=2)` used twice is one
  time node. Two `random(...)` calls are two streams.
- **Two ways to move.** Slide the place: a curve over time or audio on
  `space(shift=...)` moves a shape along the heads with its own ease
  (chase, bounce, meter); on `space(scale=...)` it grows (bloom). Or give
  each head its own clock: a curve over space on `time(...)`, on `phase` for
  a loop (wave, spin), on `delay` for a one-shot (wipe, dissolve).
- **Variable names are node names.** `pill = curve(...)` is node `pill` on
  its card and in errors. Name the nodes that matter.
- **A region is a jump.** Two points at the same x make a step:
  `[[0, 1], [0.5, 1], [0.5, 0], [1, 0]]`. "Step up" and "Step down" are
  presets.
- **Multiply with `*`.** `brightness=cut * bloom * fade` is one math node.
  `+`, `-`, `max(a, b)` and `min(a, b)` work too, with numbers on either
  side (`1 - fade`, `0.5 * pill`). A list such as `[a, b]` is an error.
- **Brightness is the pattern; alpha is opacity.** Put the chase, pulse or
  cut on `brightness` (or strobe `rate`). `alpha` mixes the whole clip with
  the light below: use it for a fade in or out of the clip.
- **Settings are not wired.** `kind`, `wrap`, `base` and `by` are plain words.
- **`edit.check()` and `edit.apply()` take no arguments.** `edit.diff()` prints
  a short plan; `edit.diff(full=True)` gives the raw clips.
- **`luma.track.document` is a property.** Do not call it.
- **Python calls need `purpose`.** The tool refuses a cell without it.
- **A selection is `"all"`, one group name, or several joined with `|`.**
  `|` is their union; there is no other operator. Names are lower case with
  underscores, as `luma.venue.groups()` prints them. For part of a group or
  an overlap of two, make a group of those lights with
  `luma.venue.group(name, fixtures)` and select it.
- **A separate axis per fixture or group.** Put `split()` or
  `split(by="group")` into `heads`.
- **Values keep units.** Shares are 0–1. Degrees for yaw and pitch. Colors
  are linear Rec. 2020 triples 0–1, or `"#RRGGBB"` (sRGB, converted for
  you). Rec. 2020 holds colors sRGB cannot: `(0, 1, 0)` is a deeper green
  than any hex code.
- **Keep the seed** when you update a clip. `update_clip` keeps it unless you
  give one.

## Read an error

A `ClipError` has this form:

```
clip Kick chase (3f24…): color1.brightness: expected a number 0–1 (share) or a number curve; got a coordinate wire from time1. Example: brightness=curve(time1, "Ramp up")
```

The node id (`color1`, `time1`) is the same as the Python variable name in
`clip.graph.source()`. Do the example and add the clip again.

## Measure before you look

Renders are pictures. They are slow to judge and small heads are hard to see.
The composited output is available as numbers:

```python
view = edit.window(seconds=(55.0, 65.0))
out = view.output
vals = out.values             # numpy, [primitive, time, rgb], linear Rec. 2020 0..1
ids = out.primitive_ids       # "fixture_id:head_index", same order as axis 0
t = out.times_s               # seconds, same order as axis 1
bright = vals.max(axis=2)     # [primitive, time]
lit_fraction = (bright > 0.05).mean()
aim = out.aim.values          # [primitive, time, 3] unit vectors in U, V, Z
weight = out.aim.weight       # [primitive, time]
shutter = out.strobe.values   # [primitive, time]
```

Use this to check three things fast:

- Which heads are on during a hit and between hits, and how bright the
  floor is against the peak.
- The order of heads along an axis. Sort by the fixture's position from
  `luma.venue.nodes(...)` and make sure brightness changes in that order.
- Whether a color band reaches the top only at peaks.

Then render once at the busiest second to confirm the look.

To see how much one clip adds, take the window twice, with the clip and
with it removed from a second edit, and compare `values`. A layer that moves
the output by a few percent will not be noticed in the room.

## Calibrate a level, when the user asks for a reactive look

Time a show; do not make it follow the level. `audio()` is for the user who
asks for a light that reacts. Then find a loud second and a quiet second from `luma.audio.mix` RMS. Change the
shape, `low` and `high` of the audio curve, and measure the lit fraction at
both. A floor is a shape that starts above 0, such as `[[0, 0.2], [1, 1]]`.
A threshold is a held start, such as `[[0, 0, "hold"], [0.3, 0.2], [1, 1]]`.
Pick the values where the quiet section shows a little and the loud section
reaches the top some of the time, not all of the time. Set it with
`edit.update_clip(clip, graph=...)`.

## Batches

Build several clips in one edit and apply once. `edit.check()` checks all of
them. Do not apply after every clip.
