---
name: rig-craft
description: What different rigs can say. Match effects to the geometry and instruments actually hanging — dense axes speak granular motion, sparse axes speak states. Read before authoring motion on an unfamiliar venue.
---
# Rig craft

A rig is an instrument, and every rig plays some things beautifully and some
things not at all. Half of design is knowing which is which *before* you
author. Study `luma.venue` — positions, groups, fixture kinds — and build a
mental model of what this rig can articulate. Author to its strengths; the
same idea forced onto the wrong rig reads as a glitch.

## Geometry decides the vocabulary

Where the lights are dense, motion can be granular; where sparse, speak in
states. Concrete shapes:

- **Stacked verticals** (LED bars on towers, crates, columns): granular
  up/down is the superpower — rising sweeps, ceiling-to-floor slams, gravity
  play. If the stacks also spread horizontally, left/right works too but
  coarser — waves across stacks, not smooth chases. Author vertical fine,
  horizontal broad.
- **A horizontal line** (bars along one truss, strip on a shelf): chases and
  wipes are the native tongue. Center-out mirroring doubles the vocabulary
  (`mirror`), and `u` runs along the line no matter how the room is rotated.
- **A grid or ceiling array**: 2D territory — radial blooms, diagonal wipes,
  rain. The rare rig where spatial patterns are the show.
- **Sparse pars** (a handful of cans): no motion to speak of — alternation,
  color states, and group contrast ARE the vocabulary. Two groups of three
  pars alternating reads clean; a "chase" across five scattered cans reads as
  malfunction.
- **Fixtures with heads** (multi-head bars, pixel fixtures): sub-fixture
  detail exists — use it for texture (shimmer, scan) while whole fixtures
  carry the structure.

Use `u`/`v` (rig-intrinsic axes) rather than world x/y/z when authoring —
patterns written on `u` survive any venue's rotation and layout.

## Instruments have roles

Like an orchestra, not interchangeable:

- **Wash/pars** — atmosphere and color. The strings: foundation, always doing
  something quiet.
- **LED bars/pixels** — motion and texture. The rhythm section: chases,
  shimmer, gradients.
- **Strobes** — punctuation only. The cymbal crash: if it plays constantly,
  it's noise (see contrast-and-darkness on rarity budgets).
- **Movers/beams** — the soloists, when the venue has them: aerial moments,
  sweeps, the drop's exclamation. Don't make soloists play rhythm all night.
- **Blinders/audience-facing warm** — connection: the singalong chorus, the
  hands-up moment. Sparingly; the crowd blinded is the crowd removed.

A rig missing an instrument means substituting, not forcing: no strobes → the
punch comes from a full-rig white pop of one frame; no movers → the "aerial"
moment becomes the whole rig breathing in sync; no washes → bars at low
saturation carry the atmosphere.

## Fit check

Before authoring a section, ask: which axis carries this idea, and is that
axis dense enough here? What instrument plays the accent, and does this venue
have it? Preview on the heatmap: if a motion idea needs squinting to see in
the heatmap's spatial order, the room won't see it either — replace it with a
state change the rig can actually pronounce.
