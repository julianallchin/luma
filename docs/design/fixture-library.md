# The Luma fixture library

Built (phase B, step 2). Luma loads, searches, patches, drives and draws
fixtures from the Luma fixture library (`lumashow/fixtures`) beside the QLC+
`.qxf` library it has always had. Existing venues are untouched: their rows
still name `.qxf` files and load them as before.

## The library in Luma

The library repository's `tools/release.py` builds a release; Luma ships it
unpacked at `resources/fixture-library/<version>/`:

```text
index.json                 one entry per shippable fixture: id, manufacturer, model,
                           category, cell count, modes with footprints, quality, aliases
fixtures/<mfr>/<model>.json  the minified spec, only its shippable modes
```

Each spec is a `luma-fixture/0` file (the library's `docs/FORMAT.md` and
`docs/CONTROL.md`): geometry, which `backend/crates/luma-fixture` builds into
meshes, joints, cells and lenses, and a `control` section of modes, typed in
`luma_fixture::control`. To update it, rebuild the release from the library's
main with its `.venv` and replace the version directory (one version is
shipped; the newest directory wins).

`resources::fixture_library_root` finds it beside the fixtures root
(`<resources>/fixtures/<version>` → `<resources>/fixture-library/<newest>`), or
at `LUMA_FIXTURE_LIBRARY_ROOT`. There is no other fallback, so a test or the UI
harness with its own fixtures root sees no library unless it asks for one.
`scripts/release/stage-resources` copies the tree into a bundle.

## `lib:` references

A patch row names a library fixture as `fixture_path = "lib:<id>"`
(`lib:claypaky/sharpy`) with the library's mode name (`16ch`), and its
`num_channels` is that mode's footprint. The fixture's venue node carries the
same string in `catalog_ref`, because `distribute` writes `catalog_ref` from
the fixture path. Rows are unchanged in shape; only what the two text columns
can hold grew.

## The loading path

`fixtures::definition(root, path)` is still the one seam. For a `lib:` path it
reads `fixture-library/<v>/fixtures/<id>.json`, builds it and lowers it
(`fixtures::library::load`), under the same per-file memo as a `.qxf`. The
result is a `FixtureDefinition`, so DMX output, role derivation, patching, aim,
groups and the agent read a library fixture through the type they already
read. The lowering adds two things the QLC+ shape cannot carry:

- `FixtureDefinition::library` (`library::Source`): the `Built`, and the pan
  and tilt travel `[lo, hi]` from the rig's joints.
- `Channel::lowered` (`library::Lowered`): which byte of which resolved value
  (`luma_fixture::control::Param`) a slot is. `get_type` and `get_colour` read
  it rather than sniffing names, and the engine reads it for what QLC+ cannot
  say.

What a lowered mode looks like:

- One channel per DMX slot, named `<mode> <slot>: <attr>`, so a name never
  stands for two different bytes.
- One head per canonical cell (CONTROL.md §5) when the fixture has more than
  one, in every mode, so `fixture:<cell>` is the same primitive whatever mode
  is patched. A channel with `cell: k` sits in head `k`; one over `[a, b]` sits
  in head `a`. Each cell reads its own primitive, else the fixture's, else
  cell 0's.
- A `fixtures::light::ModeLight` per mode, on `library::Source::light`: which
  emitters reach each cell and what each param does to the light.
- The QLC+ `Type` comes from the library category (`moving_head_*` → Moving
  Head, a many-celled bar or panel → LED Bar (Pixels), …), because role
  derivation keys on it.
- `Physical.Dimensions` is the build's box, `Physical.Lens` the optics' zoom or
  beam angle.

### What the engine does with it

- **The light** (`fixtures::light`). `ModeLight::realize` turns each cell's
  request into the values of the mode's dimmer, emitter, CMY, shutter and
  colour wheel params, and `ModeLight::emit` turns those values back into each
  cell's light. The engine encodes what `realize` decides, at the param's full
  width (a 16-bit dimmer or colour sends both bytes).
  - A cell emits through a `colour/<emitter>` param only where the param
    covers it and the cell's geometry has a die the param lights
    (`emitter.colour`; a white param lights any white die). A cell's emitters
    split its colour among themselves (white, amber and lime take what they
    can make, red, green and blue the rest); a colour it has no die for is
    lost. A param shared by several cells carries the mean of what the cells
    that can emit through it ask for. A channel marked `emulated` (a colour
    the fixture mixes from its other dies in firmware) lights every cell it
    covers.
  - Brightness goes to the dimmers over a cell, widest first, each at the
    brightest of its cells; what they do not make scales the emitters, so a
    mode without a dimmer still dims. The grand master scales it all.
  - A shutter strobes when every cell it covers asks for the same rate,
    narrowest shutter first; any other strobing cell is gated on the strobe
    clock through its brightness.
  - A colour wheel or preset picks the nearest slot; behind CMY flags it
    stays on its whitest slot, and on a cell whose colour only it makes, a
    dark colour dims the lamp. CMY flags pass `1 − level` of a white lamp.
  - A cell no channel colours (a dimmer-only white, a UV bar, a hue or CCT
    control the engine does not drive yet) shines in its dies' colour under
    its dimmers; UV shows as the faint violet it gives off.
  - Pan and tilt read the first cell they cover, so each head of a
    multi-head fixture aims where its own cell asks.
  - Two params driving one emitter on a cell show the higher, not the sum.
- **Pan and tilt.** Mapped through the channel's own `phys` degree range at
  full width, rank-0 and rank-1 bytes both, so a 16-bit pan is 16-bit and an
  asymmetric travel (`[-270, 360]`) puts 0° where the fixture has it. Past the
  travel, the nearest end. The aim solver solves within `travel()`'s `[lo, hi]`.
- **Shutter bytes.** `open` is the middle of the `open` range; strobing maps the
  requested rate (`strobe · 20 Hz`) through the `strobe` range's Hz `phys`, or
  across the range when it has none. A shutter without ranges is a plain
  0–100 % channel.
- **Colour wheel slots.** A wheel's `slot` ranges carry the slot's `rgb` (open
  is white). A `colour/macro` engages only on a fixture with no colour
  emitters.
- **Everything else** is written at its home value (CONTROL.md §7, resolved in
  `Control::resolve`), never at 0 by default.

### Pan and tilt sense

Luma has one pan/tilt convention, `fixture-kinematics`', which the renderer
draws a bundled moving head with (`Ry(+pan)` on the arm, `Rx(-tilt)` on the
head, housing space). A build's joint axes are its builder's (pan reads `-Y`
once a spec is hung), not a measured motor direction. So the stage turns each
DMX-driven pan and tilt joint by the sign that matches Luma's sense
(`luma_scene::library::angles`), and DMX maps Luma's degrees straight through
`phys`, as for a `.qxf` mover. A library fixture and a `.qxf` one at the same
pan and tilt point the same way on the stage and send the same bytes; the
render test `library_fixture` pins the stage half.

## Geometry and drawing

`fixtures::layout::head_geometry` gives a library fixture its built cells:
every canonical cell's centre at rest, in the mount frame. The housing's origin
is the pivot, which is the mount origin, so there is no pivot offset, and the
cells sit on the emitters, not on a grid guessed from a QLC+ layout. Eval, aim,
groups and spatial selection all read cells through it.

`luma_scene::definition::Definition::built` carries the `Built` to the stage
(`From<&FixtureDefinition>` copies the `Arc`; the build happens once, in the
backend's memo). The frame builder draws a library fixture from it
(`luma-render`'s `library_housing`): every node's meshes posed by
`world_matrices`, interned once per `lib:` path, with the build's materials
(sRGB to linear). Each cell's emitter meshes glow with that cell's light as
`ModeLight::emit` gives it (`stage_render::StageLights`), so the stage shows
what the DMX makes rather than what was asked for. A
lens casts one cone from `heads[i]`, lit by the cell on the lens's node; a
build with no lens (a bar, a strip) casts one per cell from the cell's face.
A fixture shines along the normal of the surface it is mounted on: a
hand-set tilt (a bracket or yoke locked with a knob) sits at 0, beam straight
out of the clamp, whatever `rest` the spec gives it, and aiming it elsewhere is
the placed fixture's own `pan`/`tilt`, set from the stage inspector. A spec's
`rest` of 90 ("facing front", the way a maker photographs it) posed on a truss
face shone the light along the truss. Fixed joints are how the head is built
and sit at their `rest`. The clamp's tangent is the bar's long axis and a truss
face's tangent is its run, so a bar lies along the truss it is clamped to. The
clamp standoff is the build's top (`bounds().max.y`),
and the selection cage is its box. `.qxf` fixtures draw exactly as before.

## Search

`services::fixtures::catalogue` lists the library's fixtures and one `.qxf`:
the ILDA laser projector (`models::fixtures::LASER_PROJECTOR_PATH`), which the
library does not carry. No other `.qxf` is offered while a library is present;
a tree with no library beside it is searched as QLC+ alone. A library entry
carries its modes, footprints and quality, and the add dialog's preview shows
them under a render of the fixture. The `.qxf` files stay on disk for the
venues that use them.

## What lowering still loses (step 4: the engine upgrade)

The engine still walks QLC+-shaped channels. To read `Resolved` directly it
must add:

- Additive `colour/cyan`, `indigo`, `mint`, `orange` and `uv` in the mixer, and
  the spec's measured emitter colours (`control.emitters`, `native_cct_k`)
  instead of the fixed sRGB, amber and lime assumptions; `colour/cct`, `tint`,
  `hue` and `saturation` as controls.
- `shutter/fx`, `pulse` and `random` states; closing a shutter (`closed`) for
  blackout.
- Zoom, focus, iris, frost, prism, gobo wheels and rotations, spins, effect
  wheels and macros from their ranges and states, and pan/tilt speed from
  `phys` rather than a preset suffix.
- Snap and fade (CONTROL.md §7): crossfades on DMX that respect range
  boundaries.
- Per-head DMX joints of arrays: the stage turns them with the fixture's one
  pan and tilt.
- A colour preset on a fixture that also mixes colour.
- `FixtureDefinition` as an enum over the two sources rather than a lowering,
  once nothing reads `.qxf` shapes.

## Step 3: migrating rows

Not done here. Existing rows still name `.qxf` paths. The migration maps
`(qxf path, QLC+ mode name)` to `(lib:<id>, library mode)` through
`aliases.jsonl` (`qlc_path`, `qlc_modes`), checks `num_channels` against the
new footprint, and rewrites `fixtures.fixture_path`, `fixtures.mode_name` and
the node's `catalog_ref` together, in both migration folders (local and
Supabase). Rows that do not map keep their `.qxf`. The laser SQL predicates
compare `fixture_path` against `'Generic/Generic-ILDA-Laser-Projector.qxf'`;
the ILDA projector stays a QLC+ built-in, so they stay valid, but any new laser
predicate must keep naming that path.
