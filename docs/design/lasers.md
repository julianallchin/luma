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
  zone, and a venue with none draws ahead into the house, 5° to 25° up.
- **Limit** (`laser_limits`): optional, per projector, a polygon in the
  projector's own view, galvo coordinates −1 to 1. Light outside it is
  blanked, never bent or clamped.

## Clips

- **Laser** output node, the geometry: `beams` (a whole number fixed for
  the clip), `shape` (`fan`, `circle`, `grid`), `size`, `x` and `y`,
  `rotation`, `join` (0 beams, 1 lines), `alpha`, and the `zone` setting.
  Motion is a Time and a Curve into any number, as for every node. Size and
  offset are enough to anchor a growing fan on one side. A laser clip's
  heads are its beams, `projector:0` to `projector:{beams − 1}`, spread a
  little across each projector's scan so Space and Mirror read them; a
  mirror reflects the canvas.
- **Color** is the existing Color output. A projector gives a Color clip 32
  heads, the colour along a laser clip's order; a beam at place `s` (0 to 1
  within its clip) takes the colour at `s`, so a gradient never depends on
  how many beams exist. One Color clip can cover lights and lasers. With no
  colour on a projector, its beams are white at full.
- Layers (`luma_patterns::laser`): Add draws both. Replace morphs: on each
  projector and zone the top draws, the beams under it and its own meet by
  `s`, place and `join` included, by the top's alpha.

## Output

```
UniverseState → laser solver (zone, scan plan, limit) → LaserFrame
                                                         ├→ visualizer
                                                         └→ DAC driver
```

The solver runs before the output hub, so the visualizer reads the same
frames a DAC would and keeps working with physical output off. The hub only
sends.

The solver (`backend/src/laser`) maps canvas → zone → galvo angles, plans
the scan (blanked jumps that settle, lit lines in short steps, and what is
left of a 30-scans-a-second budget shared out as dwell on the beams), blanks
outside the limit, and keeps lit points an overshoot margin inside it.
Brightness is power times time: sixteen beams each get a sixteenth of the
frame, and nothing hides that.

## Preview

- Timeline: canvas U over time, in beam colours.
- Viewer: buffers the points emitted during each display frame, runs them
  through a galvo model, and draws each pair of consecutive lit points as a
  thin triangle from the projector whose brightness is time spent over angle
  swept. A dwell collapses to a bright line, a sweep spreads to a dim sheet.
  The result is added into the HDR target before glare.
- A scan shorter than the persistence window counts as one whole scan, so it
  does not flicker as the window slides; a longer one's window fades at its
  ends. Beams widen with distance (1.5 mrad) and fade across their width.
  Beams and sheets scatter by the haze's own density field, so they show its
  clouds and wind.
- What the beams scatter lights the room and the haze round them, through
  the same haze glow as the fixtures (`stage-rendering-quality.md`, "Haze
  glow").
