---
name: composing-patterns
description: How to build a new effect as a graph and place it as a clip, in the fewest calls. The working order, a complete audio-reactive example, how to measure the result as numbers, and the API rules that cost the most retries. Read before composing any graph that is not a single built-in node.
---
# Composing patterns

A pattern is a graph. A clip places one graph on one selection for one time
range. Build the graph once, check it, place it, measure it, apply. Most of
the time goes to discovery. Spend it on the three or four nodes you need, not
the whole library.

## The working order

1. **Read the rig.** `luma.venue.describe()` for the shape, `luma.venue.groups()`
   for the exact group names. Use those names as written. Never build a name
   from a fixture label.
2. **Read the node cards.** Load the `node-cards` skill. It lists every node
   with inputs, units and outputs. Use `luma.track.definition("band_mask")`
   only when a card is not enough.
3. **Build the graph** with `edit.graph()` and `graph.node(...)`. End with an
   `output` node and `graph.output(...)`. See the example below.
4. **Check** with `edit.check()`. It takes no arguments.
5. **Place** with `edit.add_clip(graph, seconds=(0, duration), selection="name")`.
6. **Measure** with `edit.window(...)`. Read the numbers before you look at a
   picture.
7. **Look once** with `luma.venue.render(edit=edit, only=clip, t=...)`.
8. **Apply** with `edit.apply()`, then tell the user what the room will feel like.

## A complete example: audio level as a height meter

This graph lights each head when the mix level is above that head's height.
Low heads are green, high heads are red.

```python
edit = luma.track.edit()
graph = edit.graph()

# 0..1 level from the mix. Gain scales the band energy before the shape.
level = graph.node("band_mask", source="mix", low_hz=30.0, high_hz=16000.0,
                   gain=12.0, shape=[[0.0, 0.0], [1.0, 1.0]])

# 0..1 position of each head along Z, lowest head 0, highest head 1.
pos = graph.node("mapped_position",
                 mapping={"source": {"kind": "z"}, "per_group": False, "reverse": False})

# 1 where level > position, else 0.
lit = graph.node("core/greater", a=level.output("mask"), b=pos.output("value"), tolerance=0.0)

# Color by height.
color = graph.node("spatial_gradient",
                   mapping={"source": {"kind": "z"}, "per_group": False, "reverse": False},
                   gradient={"stops": [{"t": 0.0, "color": [0.05, 0.85, 0.12]},
                                       {"t": 0.85, "color": [0.95, 0.78, 0.05]},
                                       {"t": 1.0, "color": [1.0, 0.04, 0.04]}]})

lit_color = graph.node("core/multiply", a=color.output("color"), b=lit.output("mask"))
final = graph.node("output", color=lit_color.output("value"))
graph.expose(level, "gain")
graph.output(final.output("lighting"))

edit.check()
clip = edit.add_clip(graph, seconds=(0.0, luma.track.duration_s),
                     selection="led_bars_vertical", inputs={"gain": 12.0})
```

## Rules that cost the most retries

- **A graph must end in `output`.** Wire the final color, dimmer or position into
  `graph.node("output", ...)` and declare it with
  `graph.output(final.output("lighting"))`. A graph without this fails check
  with "must produce fixture output".
- **`edit.check()` takes no arguments.** So do `edit.diff()` and `edit.apply()`.
- **`luma.track.document` is a property.** Do not call it.
- **Python calls need `purpose`.** The tool rejects a cell without it.
- **Selection is a group expression.** Operators: `&` and, `|` or, `^` xor,
  `~` not, `>` fallback, parentheses. `"all"` is the whole venue. Names are
  lower case with underscores, exactly as `luma.venue.groups()` prints them.
- **Position mappings normalize over the clip's whole selection.** `per_group`
  has no effect inside one clip, because a clip has one selection. To normalize
  height within each tower, place one clip per tower group. Loop over the
  group names and add each clip in the same edit.
- **Mapping shorthand.** `mapping="z"` is the same as
  `{"source": {"kind": "z"}, "per_group": False, "reverse": False}`. Kinds are
  `u`, `v`, `z`, `order`, `major_axis`, `circle`, `vector`.
- **Inputs keep units.** Masks and proportions are 0..1. Colors are RGB triples
  in 0..1 or `#RRGGBB`. Shapes are envelopes: a list of `[x, y]` knots in 0..1.
- **Preserve the seed** when you update a clip.

## Measure before you look

Renders are pictures. They are slow to judge and small heads are hard to see.
The composited output is available as numbers:

```python
view = edit.window(seconds=(55.0, 65.0))
out = view.output
vals = out.values          # numpy, shape [light, time, rgb], 0..1
ids = out.light_ids        # "fixture_id:head_index", same order as axis 0
t = out.times_s            # seconds, same order as axis 1
bright = vals.max(axis=2)  # [light, time]
lit_fraction = (bright > 0.05).mean()
```

Use this to check three things fast:

- Which heads are on at a loud moment and at a quiet moment.
- The order of heads along an axis. Sort by the fixture's position from
  `luma.venue.nodes(...)` and confirm brightness changes in that order.
- Whether a color band reaches the top only at peaks.

Then render once at the loudest second to confirm the look.

## Calibrating a level

Find a loud second and a quiet second from `luma.audio.mix` RMS. Sweep the
exposed gain and measure the lit fraction at both. Pick the gain where the
quiet section shows a little and the loud section reaches the top some of the
time, not all of the time. Set it with `edit.update_clip(clip, inputs={"gain": g})`.

## Batches

Build several clips in one edit and apply once. `edit.check()` validates all
of them. Do not apply after every clip.
