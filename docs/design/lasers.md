# Lasers

Status: built end to end in software, and **not yet run against hardware**.
Projectors and their settings, the venue's window, limits, the laser clip
and its figures, the scan solver (`backend/src/laser/`), the stage, timeline
and show-export previews, and output to an Ether Dream DAC
(`backend/src/output/`), whose tests play an emulator written from the
protocol page. [laser-hardware.md](laser-hardware.md) is what to check with
a projector on the bench. Scope: beam shows from projectors a DAC drives.
DMX party lasers stay ordinary fixtures. Graphics (logos, text, surface
zones) and audience scanning are out of scope.

## Model

A beam and a line are the same point stream. A beam is the scanner dwelling
on one direction with the light off while it jumps; a line is the scanner
moving with the light on. The score never says points or galvo angles: it
says where light goes and what colour it is, and a solver at the end turns
that into a point stream, the way aim becomes pan and tilt.

## Venue

- **Projector**: the built-in fixture `builtin:laser-projector`
  (`models::fixtures::LASER_PROJECTOR_PATH`; its definition is
  `backend/src/fixtures/laser_projector.qxf`, compiled in). Every projector
  is a box with an aperture and a scanner, so there is no library of
  models: one definition, and what tells two projectors apart is their
  settings. It has no DMX channels, takes no address and is not in the
  patch; it is a fixture of the rig like any other, placed, grouped and
  selected. It scans around its mount normal with its mirrors taken to sit
  level (`backend/src/laser/mod.rs`, `Basis`).
- **Settings** (`laser::Settings`, one number per `laser::Key`): watts and
  wavelength of each diode, scan angle, scanner speed (kpps at ILDA's 8°),
  beam width and divergence, analog or TTL, a power cap, and how the box
  hangs and is wired (mirror, swap, size, offset, colour delay, each
  diode's floor). Stored as venue node parameters (`venue_node_params`,
  `laser_*` keys), so they sync with the venue. A preset (1, 3, 5, 10 W) is
  a place to start, not a kind.
- **One kind for the venue.** Most rigs hang several of one projector, so
  what a projector is (its diodes, scanner, beam, modulation, cap) is held
  once, on the venue's root node, and every projector follows it
  (`LaserRig::shared`). How a box's picture is mirrored, sized and offset is
  that box's own (`Key::is_own`) and is held on its node. A projector that
  differs holds a whole kind of its own on its node (`Projector::own`,
  `set_laser_own`) and stops following the venue's. A key nobody holds
  reads as the 3 W preset's.
- **Window** (`laser::Window`): the one range of directions in the room the
  venue's laser clips draw in, e.g. 5° to 20° up and ±40° across, shared by
  every projector. Clips draw on a canvas from −1 to 1 that maps onto it,
  so (0, 0) is the same direction from every projector and six projectors
  give six parallel fans. Stored as four parameters of the venue's root
  node (`laser_turn_min`, `laser_turn_max`, `laser_tilt_min`,
  `laser_tilt_max`). Until a venue sets it, it draws ahead into the house:
  two thirds of the narrowest projector's half scan either side, and from
  5° up through as much again, so a full-size fan stays inside every scan.
  A venue had several named zones and a clip named one; nobody used a
  second, and a second window is a second thing to aim. `laser_zones` is
  no longer read or synced; a later migration drops it.
- **Limit** (`laser_limits`): optional, per projector, a polygon in the
  projector's own view, galvo coordinates −1 to 1. Light outside it is
  blanked, never bent or clamped.
- **Aperture**: the scan leaves the middle of the box's front face, where
  its glass is, `Definition::aperture_m` ahead of where the box is hung:
  the same point the solver scans from and the stage draws the glass at.
  The body is built from the definition's dimensions
  (`luma-render`, `laser_housing.rs`): chassis, aperture and glass, warning
  labels, handle, vents, feet. It is drawn handle-up whichever way the mount
  turns it, since the mount does not say which way up a box hangs.
- **Geometry** (`Projector::galvo`, `direction`): the two mirrors sit one
  after the other. The first turns the beam across; the second tips that
  whole fan up. So the angle up is about the scan's level axis, the angle
  across is measured in the tipped plane, and a level line off the scan's
  middle bows toward its corners, as on the wall. Which mirror is first is
  an assumption to check on hardware.
- **Light** (`Settings::light`, `drive`; `luma_scene::spectrum`): a diode's
  watts become lumens and a colour by the CIE 1931 observer at its
  wavelength, which is why a watt of 520 nm green reads several times
  brighter than a watt of 638 nm red or 450 nm blue. A clip's white is the
  brightest white the three diodes mix, and every other colour is in
  proportion, under the power cap. The preview draws those lumens.

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
  and `alpha`. On each projector the clip's heads are
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
  projector the elements under the top and its own meet by their
  place along their order, place and frame by the top's alpha. Colours
  never blend: a morphing element takes the top's colour.

## Output

```
UniverseState → LaserRig::solve (window, scan plan, limit) → LaserFrame
                                                              ├→ visualizer
                                                              └→ output hub → WireFrame → DAC driver
```

The solve is the same wherever it runs, so the visualizer draws the frames
a DAC plays and keeps working with physical output off.

The solver (`backend/src/laser`) maps canvas → window → galvo angles, turns
each element into strokes (a beam a dot; a line, ring or polygon lit runs
between its corners and dark stretches, a closed figure drawn once round),
visits them nearest-next, plans the scan (blanked jumps that settle, lit
runs in short steps, and what is left of a 30-scans-a-second budget shared
out as dwell on the beams), blanks outside the limit, and keeps lit points
an overshoot margin inside it. The steps are in degrees, 0.6° lit and 2.4°
dark, and the stream runs at the scanner's rating: a scanner at its rating
follows the same angle a point whatever the rating is. Brightness is power
times time: sixteen beams each get a sixteenth of the frame, and nothing
hides that. A frame with more to draw than the budget runs long;
`LaserFrame::scans` says how slow, and below 25 a second it flickers.

A point of a `LaserFrame` is where the mirrors point and how hard each
diode is driven, white balance and the cap included. The hub
(`output/laser.rs`) solves once a tick and makes each frame a `WireFrame`
for its box: mirrored, swapped, sized and offset, the colour sent late,
each diode lifted to its floor, and scaled by the output master (Max
Brightness). A driver plays the newest frame round and round, takes up the
next where the scan in hand ends, and walks the mirrors there in the dark.

- **Laser output is off at every launch.** It is a switch in memory, never
  a setting: nothing reaches a DAC until someone turns it on in Rig →
  Lasers, and the status bar says so while it is on and turns it off when
  pressed. Off, a DAC is stopped and holds no points.
- **A fader dims a laser pattern**, and so does the Perform master: a
  layer's level scales its elements' intensity (`eval/scene.rs`).
- **A DAC is bound per projector** (`laser_routes`, this machine's wiring,
  never synced, like `dmx_routes`). A projector with none draws on screen
  only.
- **A driver** implements `output::laser::LaserDriver`: find devices, play
  the projectors routed to them from `laser::Shared`, report each link.
  `ether_dream.rs` is the first: found by its UDP broadcast, one TCP
  connection, the buffer kept about 40 ms ahead. That 40 ms is how late the
  lasers run behind the lights; nothing compensates for it yet.
- **Test pattern**: the window's edge and a cross at its middle, at a
  quarter of the capped power, on one projector at a time, in place of what
  it would play (`LaserRig::solve_testing`). It shows on the stage as it
  does on the DAC, and with no DAC at all.

The venue output plays is the one the app last read the rig of, as the DMX
patch is.

## Editing

Rig → Lasers (`gpui/crates/app/src/patch/lasers.rs`). The window is edited
on a graph of directions (`lasers/window.rs`): dragged to move, by its
handles to resize, or drawn afresh, over each projector's scan so a window
a projector cannot reach is seen to be. An edit shows on the stage as it is
made and is written when it is let go; settings and the window are node
parameters, so each is a step of the stage's undo. A limit is a row of its
own and is not.

## Preview

- Timeline: canvas U over time, in beam colours.
- Viewer: buffers the points emitted during each display frame, runs them
  through a galvo model, and draws each pair of consecutive lit points as a
  thin triangle from the projector whose brightness is time spent over angle
  swept. A dwell collapses to a bright line, a sweep spreads to a dim sheet.
  The result is added into the HDR target before glare.
- The galvo model is a damped spring per mirror, exact over each point
  period (`laser.rs`, `Galvo`): 4 kHz natural frequency at 30 kpps, scaled
  with the scanner's rating so the mirrors answer a point the same way at
  any rating, damping 0.7. A 30 kpps scanner settles a 0.1° step in 100–130 µs with a small mirror
  (Cambridge Technology 6210H/6215H datasheets); a show scanner's larger
  mirror at ILDA's 8° test angle is slower, a rise of about three points.
  So a fast shape's corners round, the mirrors slow through them and the
  light dwells there, brighter than the edges, and a jump overshoots about
  5% before it settles. It is linear: a real scanner's large steps are also
  slew-limited, which this does not model.
- A scan shorter than the persistence window counts as one whole scan, so it
  does not flicker as the window slides; a longer one's window fades at its
  ends. Beams start as wide as the projector's beam, widen by its
  divergence and fade across their width.
  Beams and sheets scatter by the haze's own density field, so they show its
  clouds and wind.
- Lasers scatter by the haze's own phase (`haze_phase.wgsl`), the one every
  light in the haze uses: a forward peak blended with a Draine lobe, fitted
  to the Mie scattering of haze fluid's droplets (`haze_phase.rs`; asymmetry
  about 0.75). Down a beam it blazes, about 20 times isotropic; from the
  side and from behind it is faint (0.18 and 0.14 of isotropic), so a beam
  seen from behind its projector is some thirty times dimmer than one seen
  30° off its path from the front, and the exposure opens for it.
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
