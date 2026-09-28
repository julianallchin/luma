---
name: composing-patterns
description: How to choose forms and place them as clips, in the fewest calls. The working order, a complete audio-reactive example, how to measure the result as numbers, and the API rules that cost the most retries. Read before placing clips.
---
# Composing patterns

A clip plays one form on one selection for one time range. Layers combine
clips by blend mode. Pick the form, set its inputs, check, place, measure,
apply. There are no custom graphs.

## The working order

1. **Read the rig.** `luma.venue.describe()` for the shape, `luma.venue.groups()`
   for the exact group names. Use those names as written. Never build a name
   from a fixture label.
2. **Read the form cards.** Load the `node-cards` skill. It lists every form
   with its inputs, sources and presets.
3. **Set the inputs.** Start from the form's defaults and change what the look
   needs. A clip needs every input of its form.
4. **Place** with `edit.add_clip(form, seconds=(0, duration), selection="name", inputs=inputs)`.
5. **Check** with `edit.check()`. It takes no arguments.
6. **Measure** with `edit.window(...)`. Read the numbers before you look at a
   picture.
7. **Look once** with `luma.venue.render(edit=edit, only=clip, t=...)`.
8. **Apply** with `edit.apply()`, then tell the user what the room will feel like.

## A complete example: a kick-driven chase over a wash

A dim blue wash, with a white chase above it whose brightness follows the kick.

```python
edit = luma.track.edit()

def defaults(form):
    return {key: spec["default"] for key, spec in luma.track.definition(form)["inputs"].items()}

wash = defaults("color@1")
wash.update(color="#1030ff", alpha=.3)
edit.add_clip("color@1", seconds=(0.0, luma.track.duration_s),
              selection="all", inputs=wash)

# A chase is a moving space source on brightness: one stroke per hit of
# `every`, `travel` beats to cross the axis.
chase = defaults("color@1")
chase.update(
    every=1,
    brightness={"type": "space", "value": {
        "axis": {"source": {"kind": "z"}, "per_group": False, "reverse": False},
        "curve": {"points": [[0, 1], [1, 1]]},  # brightness across the stroke
        "move": {"path": {"points": [[0, 0], [1, 1]]},
                 "travel": {"type": "beats", "value": 2},
                 "width": {"type": "number", "value": .3},
                 "width_relative": True, "boundary": "clip"}}},
    alpha={"type": "audio", "value": {"from_hz": 40, "to_hz": 100, "floor": 0.2}})
clip = edit.add_clip("color@1", seconds=(0.0, luma.track.duration_s),
                     selection="led_bars_vertical", inputs=chase, blend="screen")
edit.check()
```

## Rules that cost the most retries

- **Every input, every time.** `add_clip` needs a value for every input of the
  form. Start from `luma.track.definition(form)["inputs"]` defaults.
- **`edit.check()` takes no arguments.** So do `edit.diff()` and `edit.apply()`.
- **`luma.track.document` is a property.** Do not call it.
- **Python calls need `purpose`.** The tool rejects a cell without it.
- **Selection is a group expression.** Operators: `&` and, `|` or, `^` xor,
  `~` not, `>` fallback, parentheses. `"all"` is the whole venue. Names are
  lower case with underscores, exactly as `luma.venue.groups()` prints them.
- **Axis spans.** An axis normalizes over the clip's whole selection. Set
  `"span": "fixture"` or `"span": "group"` in the axis value to give each
  fixture or group its own axis in one clip.
- **Axis shorthand.** `axis="z"` is the same as
  `{"source": {"kind": "z"}, "per_group": False, "reverse": False}`. Radial
  and angle also need `"plane"`; the shorthand gives the Auto plane.
- **Inputs keep units.** Proportions are 0..1. Colors are linear Rec. 2020
  triples in 0..1, or `#RRGGBB` (sRGB, converted for you). Convert an sRGB
  color for a keyframe with `luma.track.color("#ff8000")`. Rec. 2020 holds
  colors sRGB cannot: `[0, 1, 0]` is a deeper green than any hex code. Curves are points `[x, value]` or `[x, value, ease]`,
  x from 0 to 1, for example
  `{"points": [[0, 0, "ease-in"], [0.5, 1, "hold"], [0.8, 1], [1, 0]]}`. An
  envelope's values are 0..1.
- **Preserve the seed** when you update a clip.

## Measure before you look

Renders are pictures. They are slow to judge and small heads are hard to see.
The composited output is available as numbers:

```python
view = edit.window(seconds=(55.0, 65.0))
out = view.output
vals = out.values          # numpy, [light, time, rgb], linear Rec. 2020 0..1
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
`floor` and `threshold` of an audio source and measure the lit fraction at
both. Pick the values where the quiet section shows a little and the loud section reaches the top some of the
time, not all of the time. Set it with `edit.update_clip(clip, inputs={"alpha": source})`.

## Batches

Build several clips in one edit and apply once. `edit.check()` validates all
of them. Do not apply after every clip.
