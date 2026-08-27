# Movement System Design

## Core Concept

Movement is defined as **perturbation within a per-fixture pyramid of operation**. Each fixture in a group shares a base direction and angular extents, but the pyramid originates from each fixture's own physical position. Patterns output normalized (u, v) coordinates in [-1, 1] that map to angular offsets within this pyramid.

This separates concerns cleanly:
- **Venue designer** defines the pyramid per group (base direction + extents)
- **Pattern author** writes perturbation in normalized UV space
- **Render engine** maps UV to per-fixture pan/tilt automatically

Patterns are fully portable across venues. A pattern that says "circle at 50% radius" always traces 50% of whatever the venue designer configured.

## The Pyramid Model

```
        fixture head (tip)
           /    \
          /      \
         /  base  \
        /  dir |   \
       /       v    \
      +---------------+
      |   UV plane    |  <-- perturbation happens here
      |  (u,v in deg) |
      +---------------+
```

Each fixture in a group has its own pyramid:
- **Tip**: the fixture's physical head position (from venue patch data)
- **Base direction**: a unit vector shared by all fixtures in the group, set in the venue
- **UV plane**: perpendicular to the base direction, centered on the base direction
  - **U axis**: the "primary" axis — by convention, the most visually prominent sweep direction
  - **V axis**: the "secondary" axis — perpendicular to U on the plane
- **Extents**: half-widths in degrees along U and V (e.g., extent_u=30 means ±30 degrees)

The base direction determines the fixture's default aim. All effects are angular offsets from this direction within the UV plane.

## UV Semantics

U and V are abstract axes, not tied to any physical direction (not "left-right" or "up-down"). Their physical meaning depends on how the venue designer orients the pyramid.

The **convention** is:
- **U = primary axis** — the bold, visually prominent sweep direction
- **V = secondary axis** — the complementary direction

A pattern author trusts that "sweep on U" produces the natural primary sweep in any venue, because every venue designer follows this convention. The result "kinda looks the same" across venues — not geometrically identical, but perceptually similar.

U and V are in **degrees** (not meters). This means the angular motion is consistent per-fixture regardless of throw distance. A ±20 degree sweep is the same motor movement whether the fixture is 2m or 10m from the floor.

## Group Data Model

Added to `FixtureGroup` (venue-level, per-group):

```rust
struct MovementConfig {
    // Base direction (unit vector, Z-up coordinate system)
    base_dir_x: f64,
    base_dir_y: f64,
    base_dir_z: f64,

    // Angular extents (degrees, half-width)
    extent_u: f64,  // primary axis, e.g., 30.0 = +/-30 degrees
    extent_v: f64,  // secondary axis

    // Rotation of the UV plane around the base direction (degrees)
    // 0 = U aligned to the most horizontal direction on the plane
    uv_rotation: f64,
}
```

Only applies to groups containing moving fixtures (MovingHead, Scanner). Groups without movers ignore this config.

The UV axes are derived:
1. Start with the base direction vector
2. Compute the default U as the most horizontal direction perpendicular to the base direction: `U_default = normalize(cross(base_dir, world_up))` (with fallback if base_dir is vertical)
3. V_default = cross(base_dir, U_default)
4. Apply `uv_rotation` to rotate U and V around the base direction

This means the venue designer only needs to set:
- A direction (where the fixtures point by default)
- Two extent values (how far they can swing on each axis)
- An optional rotation (to reorient U if the default isn't right)

## Pattern-Side: Perturbation Nodes

All perturbation nodes share the same output contract:
- Output: Signal with C=2 (channel 0 = u, channel 1 = v)
- Values in [-1, 1] normalized range
- Beat-synced (speed parameter in cycles/beat)
- Phase input port (Signal) for fan/spread effects

### Perturbation Generators

| Node | Description | Key Params |
|------|-------------|------------|
| `circle` | Circular motion: `(cos(t)*r, sin(t)*r)` | `radius` (0-1), `speed` (cycles/beat) |
| `figure_8` | Lissajous 2:1: `(cos(t)*w, sin(2t)*h)` | `width` (0-1), `height` (0-1), `speed` |
| `sweep` | Oscillation on one axis or at an angle | `angle` (deg, 0=U axis, 90=V axis), `range` (0-1), `speed` |
| `wander` | Noise-based organic drift | `radius` (0-1), `speed`, `smoothness` |

All have:
- `phase` input port (Signal) — for per-fixture phase offset via any spatial attribute
- `speed` param — cycles per beat (0.25 = one cycle every 4 beats)

### Phase Spread

Phase spread works via existing spatial attributes and broadcasting:

```
get_attribute("normalized_index")  [N=fixtures, T=1]
  -> math(multiply, spread_amount)
  -> feeds into perturbation node's phase input
```

Any attribute works: `normalized_index`, `rel_x`, `rel_y`, `angular_position`, `circle_radius`. The N dimension expands automatically via broadcasting. The perturbation node doesn't know or care what drives the spread.

**Mirroring** naturally works through the existing `mirror` node applied to the spatial attribute before it feeds into phase. Mirrored `normalized_index` (0,1,2,3,4 -> 0,1,2,1,0) produces mirrored phase spread, which produces mirrored movement. No special movement mirroring needed.

## Apply Node

### `apply_movement`

Replaces `apply_position` and `look_at_position`.

**Inputs:**
- `selection` (Selection) — which fixtures to affect
- `uv` (Signal, C=2) — normalized perturbation, u and v channels

**Behavior:**
1. For each fixture in the selection, look up the group's `MovementConfig`
2. Map normalized (u, v) to angular offsets: `(u * extent_u, v * extent_v)` degrees
3. Apply the angular offset to the base direction using the group's UV axis orientation
4. Convert the resulting aim direction to absolute pan/tilt using the fixture's physical position and rotation
5. Write to `PrimitiveTimeSeries.position`

The fixture-specific math (direction → pan/tilt) uses the same inverse kinematics already implemented in the current `look_at_position` node, but the input is now a per-group angular offset rather than a world-space target point.

## Nodes to Remove

| Node | Reason |
|------|--------|
| `apply_position` | Raw pan/tilt degrees — not portable across fixtures or venues |
| `look_at_position` | World-space target point — venue-coupled, verbose to wire |
| `orbit` | Replaced by `circle` perturbation + `apply_movement` |
| `random_position` | Replaced by `wander` perturbation + `apply_movement` |
| `smooth_movement` | Slew limiter in degree space — doesn't fit the new model |

## Render Pipeline

```
Perturbation node (circle, sweep, etc.)
  outputs Signal [N, T, C=2] normalized (u, v)
        |
        v
  apply_movement + selection
        |
        | per-fixture:
        |   1. Look up group's MovementConfig
        |   2. (u, v) * (extent_u, extent_v) -> angular offset in degrees
        |   3. Rotate offset by UV axes on the plane perpendicular to base_dir
        |   4. Compute absolute direction = base_dir rotated by angular offset
        |   5. Direction -> pan/tilt via fixture rotation inverse kinematics
        |
        v
  PrimitiveTimeSeries { position: Series(dim=2, [pan, tilt]) }
        |
        v
  Compositor (60Hz resampling, layer compositing)
        |
        v
  Render loop (interpolation, NOT nearest-neighbor — fix needed)
        |
        v
  DMX output (16-bit pan/tilt, ArtNet 60Hz)
```

## Render Pipeline Fixes (Pre-existing Issues)

These were identified during research and should be addressed alongside the movement system:

1. **Interpolation in render_frame()**: Currently uses nearest-neighbor sampling. Should use linear interpolation (binary search + lerp). Causes micro-stutter.

2. **Output-stage slew limiting**: No slew limiting between patterns or at pattern boundaries. Causes visible snap/jerk when a new pattern starts at a different position while the fixture is lit. Add per-fixture velocity clamping in the render loop (user-configurable or sensible defaults, since actual motor speed is unknown).

3. **Crossfade position blending**: Currently weighted average — `pan=170` blended with `pan=-170` gives `pan=0` (wild sweep through center). Should use shortest-path angular interpolation or winner-takes-all.

## Universe Designer UX

When a group containing movers is selected, the grouped fixture tree sidebar shows a "Movement" section:

### Setting the Base Direction
- **Interactive**: Select group, enter "aim" mode, click a direction in the 3D visualizer (click a point on the stage and the system computes the direction from the group's centroid toward that point)
- **Manual**: Input direction vector or pick from presets (Down, Forward, etc.)
- **Feedback**: Fixture beams in the visualizer all point along the base direction

### Setting Extents
- **Sliders** for extent_u and extent_v (in degrees, e.g., 0-90)
- **Visualizer overlay**: Show the pyramid's UV plane as a translucent shape at a representative distance, with U and V edges color-coded
- **Preview buttons**: Run a test pattern (circle, sweep) to see the extents in action

### Setting UV Rotation
- **Dial/slider** for rotating the UV plane around the base direction (0-360 degrees)
- The default (0) aligns U to the most horizontal direction, which is correct for most setups
- Only needs adjustment for unusual mounting angles

### Smart Defaults
When a group is created with movers:
- Base direction defaults to (0, 0, -1) — straight down (most common for overhead truss)
- Extents default to 30 degrees each
- UV rotation defaults to 0

## DSL Integration

The bar-by-bar DSL can express movement concisely:

```
bars 1-4:  hit > circle(speed=0.25, radius=0.8)
bars 5-8:  hit > sweep(speed=0.5, range=1.0, angle=0)
bars 9-12: hit > figure_8(speed=0.25, width=0.8, height=0.5)
bars 1-12: hit > circle(speed=0.25, radius=0.6, spread=1.0)
```

Where `spread` is syntactic sugar for "apply phase spread via `normalized_index` at this amount."

## Portability Contract

The system maintains portability through separation:

| Concern | Who owns it | What they set |
|---------|-------------|---------------|
| Movement shape | Pattern author | Perturbation type, speed, radius, phase spread |
| Movement range | Venue designer | Base direction, U/V extents, UV rotation |
| Physical mapping | Render engine | Angular offset → pan/tilt via fixture IK |

A pattern never contains degrees, world coordinates, or fixture-specific data. A venue config never contains pattern shapes or timing. The render engine bridges the two using fixture physical data (position, rotation) that's already in the patch.
