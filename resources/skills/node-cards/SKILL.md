---
name: node-cards
description: One card per clip form. Every form id, what it does, its inputs with units and the sources each accepts, and its named presets with their values. Read this before placing a clip; use luma.track.definition(form) only for exact types and defaults.
---

# Form clips and sources

Read the live form definitions and presets before authoring a score. A clip
holds a value for every input. Use the normal score edit/check/apply flow.
Colors are linear Rec. 2020; convert sRGB with `luma.track.color`.

## Forms

- `color@1`: `color`, `brightness`, `fade`. Sparkle and Noise are presets of
  Color, not separate forms.
- `aim@1`: `base` (`direction` or `point`), `direction`, `point`, `lean`,
  `horizontal`, `vertical`, `axis`, `fade`. Position is a direction or a
  target in stage metres. Lean is a vector in stage coordinates; Space on
  lean makes a fan. Horizontal and vertical are degrees around the aim.
  `replace` establishes an aim; `offset` adds turns to the aim underneath.
  Use overlapping position clips with fades for transitions. An Offset
  circle can run above both. Circle and Figure-8 are paired Time curves.
- `strobe.constant@1`: `rate`, `fade`; writes the shutter only.

Clip `fade` is fixed or a Time curve over the whole clip. It scales Color
and Strobe, blends toward an absolute Aim, or scales relative aim offsets.
Put rhythmic or audio modulation in the effect's sources, not clip fade.

## Sources

Every numeric source input accepts another source. Settings are fixed.

- Time: `points`, or `gradient` and `curve`; optional `events`, `phase`
  (turns), and `gain`. Curves use `[x,value,ease]`; the final point has no
  ease. Alongside linear/hold/Bézier easing, sine-in, sine-out and
  sine-in-out express exact oscillations.
- Space: `axis`, `curve` or `gradient`, optional `offset`, `width`,
  `width_relative`, `boundary` (clip/wrap), `gain`, `grain`. Offset from a
  repeating Time source makes one stroke per event. Relative width is a
  fraction of the gap between strokes.
- Random: `events`, `coverage`, `level`, `grain`. Selection stays stable
  within an event. Animate coverage for Build/Dissolve, level for Shimmer.
- Noise: `speed` in beats, `range` of two numeric inputs, `contrast`,
  optional spatial `scale`, `grain`. Omit scale for uniform wandering;
  `independent: true` gives separate wandering per unit. `key` preserves a
  source's stable random identity when restructuring its inputs.
- Audio: `from_hz`, `to_hz`, `floor`, `threshold`, optional numeric `gain`.
  Reads normalized full-mix energy and requires track analysis.

Grain: `head`, `fixture`, `clump2`, `clump4`, `clump8`.

## Events

`events: {every: <numeric input>, life: <numeric input>}`. Both durations
are in beats; omitted life follows every. Zero every means one event over
the clip. Life longer than every overlaps events.

Omitted events inherit the enclosing source's events. At the root they
mean over clip. Use explicit every 0 to keep a nested curve over the whole
clip. A Space source exposes its offset's clock to width and gain.
`events: {same_as: "brightness"}` follows another input's events. References
must be acyclic and resolve to one clock.

Brightness keeps the strongest live event per head. Color following those
events comes from that winning event; ties choose the newest. RGB components
are separate from event identity. Independent color events use the newest
live color. Numeric nested inputs with their own independent events use
the newest live value. Seek and batched evaluation give the same frame.

Presets are saved inputs. Read their exact values from the live catalogue;
do not recreate them from prose or add a special form for an effect.
