# Lasers

Status: agreed with the user on 2026-10-24. Scope: beam shows from ILDA
projectors over Ethernet. DMX party lasers stay ordinary fixtures. Graphics
(logos, text, surface zones) and audience scanning are out of scope.

## Model

A beam and a line are the same point stream. A beam is the scanner dwelling
on one direction with the light off while it jumps; a line is the scanner
moving with the light on. The score never says points or galvo angles: it
says where light goes and what colour it is, and a solver at the end turns
that into a point stream, the way aim becomes pan and tilt.

## Venue

- **Projector**: a fixture whose QXF `<Type>` is `Laser Projector`
  (`Generic/Generic-ILDA-Laser-Projector.qxf` ships). Its scan field is the
  definition's focus pan and tilt range; its point rate is 30k points a
  second. It scans around its mount normal with its mirrors taken to sit
  level (`backend/src/laser/mod.rs`, `Basis`).
- **Zone** (`laser_zones`): a named window of directions in the room, e.g.
  5° to 20° up and ±60° across, shared by every projector. Clips draw on a
  canvas from −1 to 1 that maps onto the window, so (0, 0) is the same
  direction from every projector and six projectors give six parallel fans.
  Clips reference a zone by id; an empty or unknown id is the venue's first
  zone. A venue with none draws ahead into the house: two thirds of the
  narrowest projector's scan across, and from 5° up through two thirds of
  its half-field above that, so a full-size fan stays inside every scan.
- **Limit** (`laser_limits`): optional, per projector, a polygon in the
  projector's own view, galvo coordinates −1 to 1. Light outside it is
  blanked, never bent or clamped.

## Clips

- **Laser** output node: `kind` (`beams`, `line`, `ring`, `polygon` with
  `sides`, `grid`) and `count` (whole numbers fixed for the clip), the
  per-element inputs `x`, `y`, `size`, `rotation` and `color`, `alpha`,
  and the `zone` setting. A clip's heads are its elements,
  `projector:0` to `projector:{count − 1}`, spread a little across each
  projector's scan so Space and Mirror read them; a mirror reflects each
  element. An empty `x` or `y` is the element's home place: beams in a row
  and a grid in rows, `size` wide and turned by `rotation`; one line, ring
  or polygon in the middle.
- **Along** (`along()`) is 0 to 1 within an element. Only the laser's
  per-element inputs read it, through curves and math; the lowering
  evaluates those inputs at 32 places along each line, ring or polygon
  (`luma_patterns::laser::ALONG_SAMPLES`) and the solver interpolates
  between them. Black along an element is the light off: dashes.
- Each element is a place, a frame (2×2: size and rotation, a mirror's
  reflection), a figure and its samples (`luma_patterns::laser::Element`).
- Layers (`luma_patterns::laser`): Add draws both. Replace morphs: on each
  projector and zone the elements under the top and its own meet by their
  place along their order, place and frame by the top's alpha. Colours
  never blend: a morphing element takes the top's colour.

## Output

```
UniverseState → laser solver (zone, scan plan, limit) → LaserFrame
                                                         ├→ visualizer
                                                         └→ DAC driver
```

The solver runs before the output hub, so the visualizer reads the same
frames a DAC would and keeps working with physical output off. The hub only
sends.

The solver (`backend/src/laser`) maps canvas → zone → galvo angles, turns
each element into strokes (a beam a dot; a line, ring or polygon lit runs
between its corners and dark stretches, a closed figure drawn once round),
plans the scan (blanked jumps that settle, lit runs in short steps, and
what is left of a 30-scans-a-second budget shared out as dwell on the
beams), blanks outside the limit, and keeps lit points an overshoot margin
inside it. Brightness is power times time: sixteen beams each get a
sixteenth of the frame, and nothing hides that.

## Preview

- Timeline: canvas U over time, in beam colours.
- Viewer: buffers the points emitted during each display frame, runs them
  through a galvo model, and draws each pair of consecutive lit points as a
  thin triangle from the projector whose brightness is time spent over angle
  swept. A dwell collapses to a bright line, a sweep spreads to a dim sheet.
  The result is added into the HDR target before glare.
- The galvo model is a damped spring per mirror, exact over each point
  period (`laser.rs`, `Galvo`): 4 kHz natural frequency, damping 0.7. A
  30 kpps scanner settles a 0.1° step in 100–130 µs with a small mirror
  (Cambridge Technology 6210H/6215H datasheets); a show scanner's larger
  mirror at ILDA's 8° test angle is slower, a rise of about three points.
  So a fast shape's corners round, the mirrors slow through them and the
  light dwells there, brighter than the edges, and a jump overshoots about
  5% before it settles. It is linear: a real scanner's large steps are also
  slew-limited, which this does not model.
- A scan shorter than the persistence window counts as one whole scan, so it
  does not flicker as the window slides; a longer one's window fades at its
  ends. Beams widen with distance (1.5 mrad) and fade across their width.
  Beams and sheets scatter by the haze's own density field, so they show its
  clouds and wind.
- Lasers scatter by their own phase, a mix of two Henyey-Greenstein lobes:
  half at g 0.9, half at g 0.3 (`laser.wgsl`, `phase`). Depence uses g ≈ 0.9
  with a floor. Down a beam it blazes, about 75 times the isotropic light a
  few degrees off the axis where one g 0.6 lobe gave 10; from the side it
  shows as before (0.44 against 0.40). Each lobe is normalised, so the mix
  scatters only what the haze takes out of the beam. Fixture beams keep
  `Transport::PHASE_G`.
- A beam a few millimetres wide samples one line through the haze's eddies,
  which a metre-wide cone averages away, so a laser shows them even where
  the clouds are even: one more tap of the haze noise at 15 cm, turned off
  the axes so it never repeats along a beam, drifting faster than the
  clouds, swaying the density up to 25% either way at full Turbulence (0 is calm haze) and fading
  out where a pixel spans an eddy. It is a symmetric contrast about 1, so a
  beam's light in all is unchanged (`laser.wgsl`, `eddies`). Cones and the
  glow do not take it.
- What the beams scatter lights the room and the haze round them, through
  the same haze glow as the fixtures (`stage-rendering-quality.md`, "Haze
  glow"). In the glow a beam stops at the first surface on its line, and
  walls and decks shade the glow from their far side.

## v2 (agreed 2026-10-25)

The first round's laser node fixed a beam's place by its `shape`, faked
sheets with many joined beams, and coloured beams through 32 colour samples
per projector that Color clips over the projector filled and beams blended
between. v2 follows how Beyond, ILDA and FB4 work: the drawing owns its
points and its colours.

- **Elements are the laser node's heads.** `kind` is `beams`, `line`,
  `ring`, `polygon` or `grid`, with `count`. A 16-beam fan is 16 elements,
  four rings are 4, a sheet is 1. Space, Random, Shuffle and Mirror work
  over them as over a mover's heads, and the clip owns them, so their
  identity is stable.
- **`along`** is a source node like Time and Space: 0 to 1 within an
  element, from a line's start to its end or once round a ring. An input
  that reads it varies within the element (a gradient, a bend, dashes),
  and the solver samples it as finely as it draws.
- **Per-element inputs** on the laser node: `x`, `y`, `size`, `rotation`,
  `color`. Wired only from Space, Random, Time and the like they give one
  value per element; wired from `along` they vary along it.
- **Laser clips combine by Add (draw both) or Replace (morph). There are no
  clips on top:** Color clips no longer reach projectors, and
  `COLOR_SAMPLES` and the blending between them go. A beam is never a blend
  of two colours.
- The old fan, circle and grid looks become presets.
- **Random** replaces Noise: `every`, `glide` (0 jumps on the turn and
  holds, 1 drifts the whole turn) and `scale`. Saved graphs and presets
  move by a one-shot migration; nothing converts at runtime. Shuffle stays
  for orderings and composes with Mirror. Random gives a value, Shuffle
  gives an order.
- A venue with no zone draws into a sensible window ahead of the house.

Rendering: fine haze detail along beams (small eddies a thin beam samples
and a wide cone averages away), a second-order galvo model, stronger
forward scatter, and occlusion for the haze glow.
