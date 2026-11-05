# Aim

> The `aim` node itself (its settings and inputs) is specified in [clip-graphs.md](./clip-graphs.md). This spec keeps what happens around it: the room frame, the solver that turns aims into pan and tilt, moving in black and the preview's motor lag.

Scope: pan and tilt of moving heads.

## Goal

A moving head's position is authored the same way as its color: one clip,
one graph. The score says where each beam points in the room.
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

Each previewed head turns toward the solver's target at most at its motor
speed: 180° per second. Fixture profiles carry no motor speed yet
(`backend/src/motors.rs`).
