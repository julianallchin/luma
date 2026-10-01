---
name: composing-patterns
description: How to build clip graphs and place them as clips, in the fewest calls. The working order, a complete audio-reactive example, how to measure the result as numbers, how to read the checker's errors, and the API rules that cost the most retries. Read before placing clips.
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
4. **Add** with `edit.add_clip(graph, name="Kick chase", beats=(32, 48), selection="name")`.
   The checker runs at once. A `ClipError` tells you the node, the input and
   an example fix.
5. **Measure** with `edit.window(...)`. Read the numbers before you look at a
   picture.
6. **Look once** with `luma.venue.render(edit=edit, only=clip, t=...)`.
7. **Apply** with `edit.apply()`, then tell the user what the room will feel like.

## A complete example: a kick-driven chase over a wash

A dim blue wash, with a white chase above it. The kick makes the chase
brighter.

```python
edit = luma.track.edit()
end = luma.track.duration_s

edit.add_clip(color(color="#1030ff", brightness=0.3),
              name="Blue wash", seconds=(0.0, end), selection="all")

k = clock(every=1, duration=2)
move = curve(time(k), "Ramp up", low=-0.2, high=1)
pill = curve(space(shift=move, length=0.2), [[0, 0], [0, 1], [1, 1], [1, 0]])
kick = curve(audio("Kick"), "Ramp up", low=0.3, high=1)
chase = color(brightness=[pill, kick])
clip = edit.add_clip(chase, name="Kick chase", seconds=(0.0, end),
                     selection="led_bars_vertical", blend="screen")
edit.check()
```

`clip.graph.source()` prints Python that builds the same graph. Use it to
read a clip that is already in the score, change a line, and pass the new
graph to `edit.update_clip(clip, graph=...)`.

## Rules that cost the most retries

- **A coordinate goes through a curve.** `brightness=time()` is an error.
  Write `brightness=curve(time(), "Ramp up")`.
- **A clip needs a name.** `add_clip(graph, name="...")`. A preset graph
  keeps its preset name.
- **One output per clip.** Color and strobe together are two clips.
- **A clock needs `every`.** For once over the clip, give `time()` no clock.
- **Reuse a variable to share a node.** `t = time(k)` used twice is one
  time node. Two `noise(...)` calls are two streams.
- **Two ways to move.** Slide the place: a curve over time or audio on
  `space(shift=...)` moves a shape along the heads with its own ease
  (chase, bounce, meter). Or give each head its own clock: a curve over
  space on `time(...)`, on `phase` for a loop (wave, spin), on `delay` for a
  one-shot (wipe, dissolve), on `length` for a speed per head.
- **Variable names are node names.** `pill = curve(...)` is node `pill` on
  its card and in errors. Name the nodes that matter.
- **A region is a jump.** Two points at the same x make a step:
  `[[0, 1], [0.5, 1], [0.5, 0], [1, 0]]`. "Step up" and "Step down" are
  presets.
- **A list multiplies.** On brightness, alpha, strobe rate and noise
  contrast, `[a, b, 0.5]` is a × b × 0.5. On other inputs a list is an error.
- **Settings are not wired.** `kind`, `wrap`, `base` and `by` are plain words.
- **`edit.check()` takes no arguments.** So do `edit.diff()` and `edit.apply()`.
- **`luma.track.document` is a property.** Do not call it.
- **Python calls need `purpose`.** The tool refuses a cell without it.
- **Selection is a group expression.** Operators: `&` and, `|` or, `^` xor,
  `~` not, `>` fallback, parentheses. `"all"` is the whole venue. Names are
  lower case with underscores, as `luma.venue.groups()` prints them.
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
vals = out.values             # numpy, [light, time, rgb], linear Rec. 2020 0..1
ids = out.light_ids           # "fixture_id:head_index", same order as axis 0
t = out.times_s               # seconds, same order as axis 1
bright = vals.max(axis=2)     # [light, time]
lit_fraction = (bright > 0.05).mean()
aim = out.aim.values          # [light, time, 3] unit vectors in U, V, Z
weight = out.aim.weight       # [light, time]
shutter = out.strobe.values   # [light, time]
```

Use this to check three things fast:

- Which heads are on at a loud moment and at a quiet moment.
- The order of heads along an axis. Sort by the fixture's position from
  `luma.venue.nodes(...)` and make sure brightness changes in that order.
- Whether a color band reaches the top only at peaks.

Then render once at the loudest second to confirm the look.

## Calibrate a level

Find a loud second and a quiet second from `luma.audio.mix` RMS. Change the
shape, `low` and `high` of the audio curve, and measure the lit fraction at
both. A floor is a shape that starts above 0, such as `[[0, 0.2], [1, 1]]`.
A threshold is a held start, such as `[[0, 0, "hold"], [0.3, 0.2], [1, 1]]`.
Pick the values where the quiet section shows a little and the loud section
reaches the top some of the time, not all of the time. Set it with
`edit.update_clip(clip, graph=...)`.

## Batches

Build several clips in one edit and apply once. `edit.check()` checks all of
them. Do not apply after every clip.
