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

- **Figure** node: `kind` (`beams`, `line`, `ring`, `polygon` with
  `sides`, `grid`) and `count` (whole numbers fixed for the clip), and the
  per-element inputs `x`, `y`, `size`, `rotation` and `color`, and
  `heads`. A figure's elements are its heads, apart from any other
  figure's: Space, Random, Shuffle and Mirror read them as their own span,
  so one Space wired into two figures spreads each across its own row. An
  empty `x` or `y` is the element's home place: beams in a row and a grid
  in rows, `size` wide and turned by `rotation`; one line, ring or polygon
  in the middle.
- **Laser** output node: `figures` (one or more figure wires, in order),
  `alpha` and the `zone` setting. On each projector the clip's heads are
  every figure's elements, figure after figure, `projector:0` to
  `projector:{n − 1}` with n at most 64, spread a little across its scan;
  a mirror reflects each element. A sheet with rails and four beams is one
  clip: `laser([sheet, rails, dots])`.
- **Along** (`along()`) is 0 to 1 within an element. Only a figure's
  per-element inputs read it, through curves and math; the lowering
  evaluates those inputs at 32 places along each line, ring or polygon
  (`luma_patterns::laser::ALONG_SAMPLES`) and the solver interpolates
  between them. Black along an element is the light off: dashes.
- Each element is a place, a frame (2×2: size and rotation, a mirror's
  reflection), an outline and its samples (`luma_patterns::laser::Element`).
  Each figure lowers to its own output, so a figure of beams carries one
  sample per element however finely a line beside it is sampled.
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
- The haze is stirred air, one field for every light (`medium.wgsl`). The
  cloud size is its largest eddy L, and Turbulence how hard it is stirred,
  ε = 0.1·turbulence³ m²/s³ (still air to a fan on the haze). By
  Kolmogorov's law an eddy r across moves at (ε·r)^⅓ and turns over in r
  over that, and the haze's contrast at r is the clouds' times (r/L)^⅓.
  Every size, speed and strength follows from those three settings, so no
  setting gives haze air can't make. Eddies turn over in place: each half
  turn the haze fades from one place in the baked field to the next, a
  whole number of texels on, the weights' squares summing to one so
  contrast holds. The noise texture holds each texel and the one a step on
  (Rg16Float, 64 MiB), so that is one read, and the hand-over shows
  nowhere. It comes back after 128 turns (38 minutes of default clouds);
  the old wobble looped every 27 s. Cloudy haze costs about 3% more than
  before.
- A beam a few millimetres wide samples one line through the haze, so it
  shows the field's fine end, 0.5 m wisps a metre-wide cone averages away
  (`medium_wisps`): the level lines of a fine field, drawn out by the
  largest eddies at their speed, in patches where those churn, as strong as
  the law says for their size. Even haze (cloudiness 0) has none. They fade
  out where a pixel spans a wisp and only move the haze about. Still drawn
  by eye: the strand shaping and the patches' edges. Cones and the glow do
  not take the wisps.
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

## Figures (agreed 2026-10-26)

v2's laser node had one `kind`, so beams and a sheet took two clips. The
laser node's drawing moved to `figure` nodes, and a laser draws a list of
them. A figure owns its kind, count, place, size, turn, colour and heads;
the laser keeps the zone and the clip's alpha. Saved graphs moved by a
one-shot migration (`20261026120000_laser_figures.sql`): each laser node
became a figure, `figure1`, and a laser that draws it. The shipped
presets gained Settle (four beams that wiggle and stop), Soft sheet (no
hard edges) and Sheet and beams (a sheet, its rails and four random
beams).
