# Aim

Status: agreed with the user on 2026-09-24. Scope: pan and tilt of moving
heads. This spec follows [clip-forms.md](./clip-forms.md): the same clip row,
input sources, presets and rules apply unless this spec says otherwise.

## Goal

A moving head's position is authored the same way as its color: one clip,
one form, a few inputs. The score says where each beam points in the room.
It never says pan or tilt. A solver turns directions into pan and tilt at the
very end, the way color becomes DMX at the end.

## Frame

- **Aim** is a direction in the room: U = stage right, V = downstage,
  Z = up. It is stored as a raw unit vector `[u, v, z]`. There are no named
  points or named directions.
- **Offsets** (fan, wobble) are degrees around an aim, in room terms:
  - *left/right* turns around Z;
  - *up/down* tilts toward or away from Z.

  At an aim `d`, `right = normalize(d × Z)` and `up = right × d`. When `d`
  is straight up or straight down, `right` is stage right `(1, 0, 0)`.
  A hung head and a floor head move the same way on stage for the same
  offset.
- **Points** are in venue coordinates, in metres, the same space as fixture
  positions.

## The form: `aim@1`

One form. Each clip has a **base** (where the heads rest) and a **motion**
(how they move around it). The order is always:
base → fan → motion → alpha.

### Inputs

Promotable key as in clip-forms: **T** time, **H** hit, **N** noise, **A**
audio, **—** plain only.

| Input | Type | Promotable | Meaning |
|---|---|---|---|
| base | `direction` / `point` | — | What the heads rest on |
| direction | vector | T N | The aim, when base is `direction` |
| point | U, V, Z (m) | T | The spot every head points at, when base is `point` |
| fan | degrees, −90 to 90 | T H N A | How far the heads spread apart; 0 = all alike |
| axis | axis | — | How the heads are laid out, for fan and spread |
| motion | `none` / `shape` / `noise` | — | How the heads move around the base |
| shape | `swing_left_right` / `swing_up_down` / `circle` / `figure_8` | — | The wobble, when motion is `shape` |
| size | degrees, 0 to 90 | T A | How big the wobble is |
| every | beats | T | One wobble cycle, and the time between hits |
| spread | degrees, −1440 to 1440 | T | Phase of the wobble across the axis; 360° = one full cycle; 0 = all together |
| speed | beats | T | How slowly noise wanders, when motion is `noise` |
| alpha | proportion | T H N A | Blend toward the aim under this clip |

- Rows that do not apply to the current base or motion are hidden in the
  sheet. Every input is still stored, as for every form.
- `axis` is the same axis as Chase and Gradient: `order`, `x`, `y`, `z`,
  `radial`, `angle`, `vector` or `random`, with spans (`selection`,
  `fixture`, `group`), a mirror (only `x`, `y`, `z` and `vector`) and, for
  radial and angle, a plane. One axis serves fan, spread and the mirror. See
  [clip-forms.md](./clip-forms.md#colorchase1) for the axis rules and
  [Mirror](#mirror) below for what a mirror does to an aim.
- `every` exists even when motion is `none`, because fan per hit uses it.
  It must be more than 0 when motion is `shape` or fan is per hit.
- Direction is edited as **Turn** (0° = downstage, + toward stage right)
  and **Tilt** (0° = level, −90° = straight down), with the stored vector
  shown underneath.

### Base

- `direction`: every head starts at the same aim.
- `point`: head *i* starts at `normalize(point − head_position_i)`. The
  beams meet at the point.

### Fan

Fan leans each head away from the base aim, by an amount set by its place on
the axis. Fan 0 changes nothing. A negative fan leans the other way. Fan
applies within each span, so a fixture span fans every bar on its own.

| Axis | Lean direction for a head | Amount |
|---|---|---|
| `order`, `x`, `y`, `z`, `vector`, `random` | the axis direction (`order` and `random`: from the first head of the span to the last) | `fan × (c − 0.5)`, with `c` the head's coordinate 0–1. The two end heads are `fan` degrees apart. `random` gives each head a shuffled share of the fan |
| `radial` | away from the span's center, in the plane | `fan × r`, with `r` the radial coordinate 0–1. The middle does not move; the outermost heads lean `fan` degrees. This is bloom, or tunnel with a negative fan |
| `angle` | along the circle around the center, in the plane (the tangent) | `fan × r`. Every head turns the same way around the center. This is pinwheel |

A lean rotates the aim `d` toward the lean direction `L` around the axis
`d × L`. If `L` is parallel to `d`, the head does not lean.

With a mirror, see [Mirror](#mirror).

### Motion

The motion is an offset (left/right `yaw`, up/down `pitch`, in degrees)
applied at the fanned aim with the frame above.

- `none`: no offset.
- `shape`: phase `φ = cycles − (spread / 360) × c`, where `cycles` counts
  `every` like the Chase odometer (seek-safe when `every` is a time curve)
  and `c` is the head's coordinate on the axis. Spread is in degrees, as the
  phase of a console effect: 360° puts one full cycle across the axis, so the
  two end heads move alike. Spread is not capped at one cycle: 1440° on 8
  heads puts neighbours in opposite phase. A negative spread runs the wave
  the other way. A stored `proportion` spread (from before degrees) is
  refused with a message that gives the value in degrees.

  | Shape | yaw | pitch |
  |---|---|---|
  | `swing_left_right` | `size · sin 2πφ` | 0 |
  | `swing_up_down` | 0 | `size · sin 2πφ` |
  | `circle` | `size · cos 2πφ` | `size · sin 2πφ` |
  | `figure_8` | `size · sin 2πφ` | `size/2 · sin 4πφ` |

  Spread 0: all heads move together (sweep, circle). Spread above 0: the
  motion travels across the heads along the axis (wave). With a mirror, the
  wave starts at the plane and runs out to both ends. With a random axis,
  each head takes one of the evenly spaced phases, shuffled.
- `noise`: yaw and pitch each wander smoothly between −size and +size, one
  new value about every `speed` beats. Each head has its own noise, from the
  clip's seed and the head, so no two heads move alike.

### Mirror

A mirror on the axis makes the two halves of each span mirror images: the
timing and the movement. Let `n` be the plane's unit normal and
`R(v) = v − 2 (v · n) n` the reflection across the plane: the component of
`v` along `n` flips.

- **Timing.** `c` is the folded coordinate, as for every form: the heads on
  the low side are reflected onto the high side before the axis reads them.
  So `c` is 0 for the heads nearest the plane and 1 for the heads farthest
  from it, on both sides.
- **Movement.** A head on the low side of the plane (signed distance below
  0; for Left–right, the stage-left half) takes the mirror image of its fan
  and its motion:
  - its fan leans it toward `R(L)` instead of the lean direction `L`;
  - its motion offset is applied in the mirror: the aim is
    `R(offset(R(d), yaw, pitch))`, with `d` the fanned aim.

  A head on the plane or on the high side is unchanged. The base aim is not
  flipped: with fan 0 and no motion, a mirror changes nothing.
- With a level normal (Left–right, Front–back, or any custom normal with no
  Z part), the mirrored offset has the opposite yaw and the same pitch. With
  Up–down, it has the same yaw and the opposite pitch.
- So on a truss along the plane's normal, with a base that is symmetric
  about the plane (for Left–right, a direction with no U part): a mirrored
  circle turns the other way on each half; a mirrored left–right swing
  moves both halves toward the middle together and away from it together;
  a mirrored fan fans each half out from the plane, and the halves are
  mirror images. The two end heads lean out by `fan / 2`, and the heads
  next to the plane lean in by `fan / 2`.
- Radial, angle, order and random take no mirror.

### Alpha and layers

- Aim clips use `replace` only. The sheet does not offer other blend modes
  for aim.
- Layers are applied bottom to top per head. A clip's aim is blended with
  the aim under it by alpha along the shortest arc:
  `aim = slerp(under, this, alpha)`.
- With no aim clip under it, `under` is the head's **home**: pan and tilt at
  the middle of their ranges, from the head's pose. This is fixed, so seeking
  gives the same frame as playing.
- A move from one position to the next is a new clip with
  `alpha = time[0 → 1]` over the move. There is no separate move time.
- A head with no aim clip at all has no aim. The solver keeps it at home.

## Solver (after the score)

The solver turns each head's aim into pan and tilt. It has no inputs; its
rules are code.

- It takes the head's pose (position and orientation) from the venue and
  its pan and tilt range from the fixture profile (`pan_max`, `tilt_max`).
- For each aim there are several pan/tilt pairs (pan ± 180° with the tilt
  mirrored, and pan ± 360° inside the range). The solver picks the
  reachable pair nearest the head's previous output, so a head never spins
  the long way round in the middle of a move. After a seek, "previous" is
  home.
- An aim the head cannot reach is clamped to the nearest reachable one.
- Timing comes from the clips. The fixture's pan/tilt speed channel is sent
  at its fastest, and the solver sends a new position every frame.

### Move in black

While a head is dark, it moves to where it will be needed next, so the
audience never sees a beam swing into place. This is automatic; there is no
setting and no clip.

- A head is dark when its composited light is exactly 0: no clip lights it,
  or every clip on it gives zero light. A fade that is still above 0 is not
  dark.
- During a dark interval, the head's aim is its aim at the first moment it is
  lit again. After the last lit moment, it keeps its last lit aim.
- The move starts as soon as the head goes dark. If the dark gap is too short
  for the motor, the preview shows the lag like any other move.
- The engine needs look-ahead per head: when the head is next lit. The score
  is a timeline, so this is known.

### Preview motor lag

The score and the DMX output are never slowed down. The stage preview shows
what a real head can do:

- Each previewed head turns toward the solver's target at most at its motor
  speed: 180° per second by default, or the profile's value when it has one.
- When the previewed beam is more than 3° from the target, the stage also
  draws a faint line where the score wants it.

## Presets

A preset exists only if it is more than two setting changes away from every
other preset. Every preset sets every input. Unlisted inputs take these
values: base `direction`, direction 40° down toward downstage
`(0, 0.766, −0.643)`, point `(0, 0, 0)`, fan 0, axis `order`, motion
`none`, shape `swing_left_right`, size 0, every 4, spread 0, speed 4,
alpha 1.

| Preset | Settings | Look |
|---|---|---|
| Position | (defaults) | All beams parallel, 40° down |
| Fan | fan 40° | The beams spread left to right across the rig |
| Converge | base `point`, point `(0, 0, 0)` | Every beam on center stage |
| Bloom | direction straight down, fan `time[0 → 40]`, axis `radial` (plane auto) | The beams start together and open out from the middle |
| Sweep | motion `shape`, shape `swing_left_right`, size 45°, every 8 | All beams sweep left and right together |
| Wave | motion `shape`, shape `swing_up_down`, size 25°, every 4, spread 216° | A nod that travels along the rig |
| Circle | direction 50° down, motion `shape`, shape `circle`, size 18°, every 4 | All beams draw the same circle |
| Figure-8 | motion `shape`, shape `figure_8`, size 25°, every 4 | All beams draw a figure-8 |
| Ballyhoo | direction 35° down, motion `noise`, size 40°, speed 4 | Each beam wanders at random |

Left out on purpose, because each is one or two settings away: Parallel,
Straight down, Audience, Tunnel (negative radial fan), Pinwheel (angle fan),
Look left–right, Spread circle (Circle with spread), Slow drift (Ballyhoo
with a slow speed), Bloom on the beat (Bloom with fan per hit).

## Examples

| Look | How |
|---|---|
| Fan while moving | Sweep with fan 40° |
| Slowly go to a fan | Fan clip with `alpha = time[0 → 1]` over the move |
| A fan that opens | Position with fan `time[0 → 40]` (this is Bloom on an `order` axis) |
| Circle around the singer | Circle with base `point` at the singer's mark |
| Wave across a fan | Wave with fan 30° |
| Bars that each fan on their own | Fan with axis span `fixture` |

## Stored form

```json
{
  "graph": "aim@1",
  "inputs": {
    "base": {"type": "choice", "value": "direction"},
    "direction": {"type": "vector", "value": [0, 0.766, -0.643]},
    "point": {"type": "vector", "value": [0, 0, 0]},
    "fan": {"type": "time", "value": {"points": [[0, 0], [1, 40]], "segments": ["linear"]}},
    "axis": {"type": "mapping", "value": {"source": {"kind": "radial"}, "plane": {"kind": "auto"}, "per_group": false, "reverse": false}},
    "motion": {"type": "choice", "value": "none"},
    "shape": {"type": "choice", "value": "swing_left_right"},
    "size": {"type": "number", "value": 0},
    "every": {"type": "beats", "value": 4},
    "spread": {"type": "number", "value": 0},
    "speed": {"type": "beats", "value": 4},
    "alpha": {"type": "proportion", "value": 1}
  }
}
```

The value types follow what the engine already has; the implementation may
rename a type tag to match, but not the input names.

## Delivery

1. Engine: `aim@1`, its presets, aim compositing between clips.
2. Solver, move in black, preview motor lag.
3. Sheet: Base and Motion rows, presets in the browser and picker.

## Later

- Move in black for other mechanical parts (color wheel, gobo, zoom).
- A real motor speed per fixture profile.
- The in-app agent's skills for aim.
