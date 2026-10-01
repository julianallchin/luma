---
name: effect-catalog
description: Every lighting effect Luma knows, as the Python that builds its clip graph, the node chain, and the shipped clip preset name. Color, movement, aim and strobe effects, and the effects that are not one clip (several clips, a transition, or the agent's own analysis). Read this to find how to build a look.
---

# Effect catalog

Each row is one clip. The name is the shipped clip preset: `preset("Chase")`
gives the same graph with its name and blend mode, and
`preset("Chase").source()` prints the row's Python. The Python builds the
graph with the bare builders; pass it to `edit.add_clip(graph, name=..., ...)`.
Change the numbers to fit the music. Read `node-cards` for what each node
does.

`D` below is the default aim `(0, 0.766, -0.643)`. Shapes and gradients are
preset names (see `node-cards`). Times are beats: `every`, `duration` and
`delay`. `phase` is turns.

Three patterns make most effects:

- **A moving shape** (chase, bounce, sweep, meter): a curve over time (or
  audio) on `space.shift` slides the shape along the heads, with that
  curve's ease. `scale` sets how much of the axis the shape covers; a curve
  over time on `scale` grows the shape (a bloom).
- **A loop** (wave, spin): each head runs the same shape over time, shifted
  by its place. Put a curve over space on `time.phase`.
- **A one-shot** (wipe, dissolve, grow): each head starts later by its place.
  Put a curve over space on `time.delay` (beats), and use a step shape.

A pill of width `w` that enters and leaves: `shift` runs from `-w` to 1
(`curve(time(every=2), "Ramp up", low=-w, high=1)`), `scale=w`, and the pill
shape is `[[0, 0], [0, 1], [1, 1], [1, 0]]`. Combine patterns with `*`:
`brightness=cut * bloom * fade`. A node you assign to a variable shows that
name on its card: `pill = curve(...)`.

## Color

| Name | Python | Chain |
|---|---|---|
| Wash | `color(color=(1, 1, 1))` | color |
| Pulse | `t = time(every=1); color(brightness=curve(t, "Drop"))` | time(every) → curve → brightness |
| Breathe | `t = time(every=4); color(brightness=curve(t, "Swell"))` | time(every) → curve → brightness |
| Fade | `t = time(); color(alpha=curve(t, "Fade in"))` | time → curve → alpha: the whole clip fades in |
| Color fade | `t = time(); color(color=curve(t, "Ramp up", gradient=[(0, "#b0400a"), (1, "#2449eb")]))` | time → curve(color) → color |
| Rainbow | `t = time(every=4); color(color=curve(t, "Ramp up", gradient="Rainbow"))` | time(every) → curve(color) → color |
| Gradient | `place = space(); color(color=curve(place, "Ramp up", gradient="Sunset"))` | space → curve(color) |
| Stepped palette | `t = time(every=4); color(color=curve(t, "Steps 4", gradient="Rainbow"))` | time(every) → curve(color) |
| Two-color swap | `t = time(every=2); color(color=curve(t, "Square", gradient=[(0, "#ff2a00"), (1, "#0040ff")]))` | time(every) → curve(color) |
| Follows a band | `kick = audio(low_hz=40, high_hz=100); color(brightness=curve(kick, "Ramp up"))` | audio → curve → brightness |
| VU meter | `bars = split(); bass = audio(low_hz=20, high_hz=250); height = space(heads=bars, direction=(0, 0, 1), shift=curve(bass, "Ramp up")); color(brightness=curve(height, "Step down"))` | audio → curve → space.shift up each bar; Step down lights the heads below the level |
| Random heads | `k = time(every=1); order = shuffle(time=k); rank = space(heads=order, kind="order"); color(brightness=curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]))` | time(every) → shuffle → space(order) → curve with a jump |
| Random bars | `bars = group(); k = time(every=1); order = shuffle(heads=bars, time=k); rank = space(heads=order, kind="order"); color(brightness=curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]))` | time(every) → shuffle(group) → space → curve |
| Sparkle | `k = time(every=0.125, duration=0.5); order = shuffle(time=k); rank = space(heads=order, kind="order"); color(brightness=curve(rank, [[0, 1], [0.3, 1], [0.3, 0], [1, 0]]) * curve(k, "Spike"))` | one time node: a random 30% × a spike over time |
| Build | `clip = time(); order = shuffle(); rank = space(heads=order, shift=curve(clip, "Ramp up"), kind="order"); color(brightness=curve(rank, "Step down"))` | each head turns on when the clip's progress passes its random number: the shuffled rank shifted by progress over the clip, then a step. Stretches with the clip |
| Dissolve | `clip = time(); order = shuffle(); rank = space(heads=order, shift=curve(clip, "Ramp down"), kind="order"); color(brightness=curve(rank, "Step down"))` | as Build backwards: each head goes off when the clip's progress passes its random number |
| Clouds | `cloud = noise(speed=8, scale=0.5); color(color=curve(cloud, "Ramp up", gradient="Ocean"), brightness=curve(cloud, "Ramp up", low=0.2, high=1))` | one noise → two curves |
| Sparkle rain | `bars = group(); columns = split(); k = time(every=0.25, duration=1); order = shuffle(heads=bars, time=k); drop = space(heads=columns, direction=(0, 0, -1), shift=curve(k, "Ramp up", low=-0.3, high=1), scale=0.3); rank = space(heads=order, kind="order"); color(brightness=curve(drop, "Comet") * curve(rank, [[0, 0], [0, 1], [0.2, 1], [0.2, 0], [1, 0]]))` | needs vertical bars; each event's fall is a shift down each bar; × a random 20% per bar |

## Movement

| Name | Python | Chain |
|---|---|---|
| Chase | `t = time(every=2); place = space(shift=curve(t, "Ramp up", low=-0.2, high=1), scale=0.2); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))` | time → curve → space.shift, scale 0.2 → a pill that enters and leaves |
| Wave | `place = space(); t = time(every=2, phase=curve(place, "Ramp down", low=0.5, high=1)); color(brightness=curve(t, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]))` | Chase with a wide soft pulse |
| Bounce | `t = time(every=4); place = space(shift=curve(t, "Triangle", low=0, high=0.8), scale=0.2); color(brightness=curve(place, "Soft"))` | a there-and-back time curve on space.shift; a soft pill over the shifted space |
| Comet | `t = time(every=2); place = space(shift=curve(t, "Ramp up", low=-0.2, high=1), scale=0.2); color(brightness=curve(place, "Comet"))` | Chase with a Comet shape: a sharp head and a tail |
| Wipe | `place = space(); t = time(every=4, delay=curve(place, "Ramp up", low=0, high=4)); color(brightness=curve(t, "Step up"))` | space → curve → time.delay, 0–4 beats; each head turns on in turn and stays on |
| Stepped chase | `t = time(every=4); place = space(shift=curve(t, "Steps 4", low=0, high=0.75), scale=0.25); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))` | a Steps 4 curve on space.shift: the block jumps a quarter at a time |
| Many pills | `t = time(every=0.5, duration=2); place = space(shift=curve(t, "Ramp up", low=-0.2, high=1), scale=0.2); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))` | Chase with four events alive at once |
| Wrapping chase | `ring = space(wrap=True); t = time(every=2, phase=curve(ring, "Ramp down")); color(brightness=curve(t, [[0, 0], [0.8, 0], [0.8, 1], [1, 1]]))` | a ring: the pill leaves one end and enters the other |
| Colored pills | `t = time(every=0.5, duration=2); place = space(shift=curve(t, "Ramp up", low=-0.25, high=1), scale=0.25); color(color=curve(t, "Ramp up", gradient="Rainbow"), brightness=curve(place, "Soft"))` | each event has its own progress, so its own shift and its own color |
| Speed-up chase | `clip = time(); k = time(every=curve(clip, "Ramp down", low=0.25, high=2), duration=curve(clip, "Ramp down", low=0.5, high=2)); place = space(shift=curve(k, "Ramp up", low=-0.4, high=1), scale=curve(clip, "Ramp down", low=0.1, high=0.4)); color(brightness=curve(place, "Comet"))` | one time() feeds every, duration and the scale; the shift runs on each event |
| Alternating sides | `place = space(); t = time(every=2, phase=curve(place, [[0, 0.5], [0.5, 0.5], [0.5, 0], [1, 0]])); color(brightness=curve(t, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]))` | the two halves are half a turn apart |
| Diagonal slash | `t = time(every=2); place = space(direction=(1, 0, 1), shift=curve(t, "Ramp up", low=-0.15, high=1), scale=0.15); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))` | Chase along a diagonal direction |
| Slash | `t = time(every=2); diag = space(direction=(-0.82, 0, 0.57), shift=curve(t, [[0, 0], [0.2, 1], [1, 1]])); cut = curve(diag, [[0, 1], [0, 0]]); line = mirror(normal=(0.57, 0, 0.82), at=0.68); dist = space(heads=line, direction=(0.57, 0, 0.82), scale=curve(t, "Ramp up", low=0.04, high=0.74)); bloom = curve(dist, [[0, 1], [0.76, 1], [1, 0]]); fade = curve(t, [[0, 1, "hold"], [0.2, 1, "sine-out"], [1, 0]]); heat = curve(t, gradient=[(0, (1, 1, 1)), (0.2, (1, 1, 1)), (0.5, (1, 0, 0.01)), (1, (1, 0, 0.01))]); color(color=heat, brightness=cut * bloom * fade)` | not a shipped preset. cut × bloom × fade: a cut sweeps across one diagonal, a bloom grows out from a mirror line at 0.68 on the other diagonal, then all heads fade; white turns red |
| Speed-up blocks | `clip = time(); k = time(every=curve(clip, "Ramp up", low=1, high=0.5), duration=curve(clip, "Ramp up", low=2, high=4)); x = space(direction=(1, 0, 0), shift=curve(k, "Ramp up"), wrap=True); color(brightness=curve(x, [[0, 0], [0, 1], [0.25, 1], [0.25, 0], [1, 0]]))` | not a shipped preset. Events speed up from every 1 beat to every 0.5 and live longer (2 to 4 beats); each slides a quarter-wide block once around a ring |
| Ripple | `t = time(every=2); radius = space(shift=curve(t, "Ramp up", low=-0.4, high=1), scale=0.4, kind="radial"); color(brightness=curve(radius, "Soft"))` | Chase over radius: rings go out from the centre |
| Wrapping ripple | `t = time(every=2); radius = space(shift=curve(t, "Ramp up", low=-0.4, high=1), kind="radial", wrap=True); color(brightness=curve(radius, [[0, 0, [0.4, 0, 0.6, 1]], [0.2, 1, [0.4, 0, 0.6, 1]], [0.4, 0], [1, 0]]))` | Ripple on a wrapped radius: rings leave the edge and come in again at the centre. One ring per turn: its width 0.4 is in the curve points, as a wrapped scale would tile |
| Zoom out | `clip = time(); x = space(direction=(1, 0, 0), shift=curve(clip, [[0, 0, "ease-out"], [1, 1]]), scale=curve(clip, "Ramp up", low=0.5, high=0.125), wrap=True); color(brightness=curve(x, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]))` | not a shipped preset. A wrapped space tiles, so its scale running 0.5 → 0.125 zooms out from 2 soft pills to 8 while a slowing shift drifts them; no head jumps |
| Spin | `turn = space(kind="angle"); t = time(every=2, phase=curve(turn, "Ramp down")); color(brightness=curve(t, [[0, 0], [0.75, 0], [0.7625, 1], [1, 0]]))` | angle wraps by default |
| Grow | `clip = time(); radius = space(shift=curve(clip, "Ramp up"), kind="radial"); color(brightness=curve(radius, "Step down"))` | heads turn on from the centre out over the whole clip: radius shifted by progress, then a step |
| Turning line | `turn = space(kind="angle"); t = time(every=2, duration=4, phase=curve(turn, "Ramp down")); color(brightness=curve(t, [[0, 0], [0.9, 0], [0.9, 1], [1, 1]]))` | duration = 2 × every: two opposite arms alive |
| Spiral | `radius = space(kind="radial"); turn = space(kind="angle"); t = time(every=4, phase=curve(turn, "Ramp down", low=curve(radius, "Ramp up"), high=curve(radius, "Ramp up", low=1, high=2))); color(brightness=curve(t, [[0, 0], [0.7, 0, [0.4, 0, 0.6, 1]], [0.85, 1, [0.4, 0, 0.6, 1]], [1, 0]]))` | phase by angle, moved by radius, bends the arm |
| Mirror | `halves = mirror(); t = time(every=2); place = space(heads=halves, shift=curve(t, "Ramp up", low=-0.1, high=0.5), scale=0.1); color(brightness=curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]))` | Chase over mirrored heads; after the mirror the place is 0 on the plane and 0.5 at the ends, so pills go out from the centre both ways |
| Kaleidoscope | `sides = mirror(normal=(1, 0, 0)); quarters = mirror(heads=sides, normal=(0, 0, 1)); turn = space(heads=quarters, kind="angle"); t = time(every=4, phase=curve(turn, "Ramp down")); color(brightness=curve(t, [[0, 0], [0.85, 0], [0.8575, 1], [1, 0]]))` | two mirrors, four-fold |

## Aim

| Name | Python | Chain |
|---|---|---|
| Position | `aim(direction=D)` | aim |
| Fan | `place = space(); aim(direction=D, yaw=curve(place, "Ramp up", low=-25, high=25))` | space → curve → yaw |
| Converge | `aim(point=(0, 3, 0), base="point")` | point base |
| Follow | `t = time(); aim(point=curve(t, "Ramp up", low=(-3, 3, 0), high=(3, 3, 0)), base="point")` | time → curve(vector) → point |
| Bloom | `t = time(); aim(point=curve(t, "Ramp up", low=(0, 0, 40), high=(0, 0, 7)), base="away")` | away base; the point comes down toward the rig |
| Tunnel | `aim(point=(0, 25, 1.5), base="point")` | a far point downstage |
| Sweep | `t = time(every=8); aim(direction=D, yaw=curve(t, "Sine", low=-45, high=45))` | time(every) → curve → yaw |
| Nod wave | `place = space(); t = time(every=4, phase=curve(place, "Ramp up", high=0.6)); aim(direction=D, pitch=curve(t, "Sine", low=-25, high=25))` | space → curve → time.phase |
| Circle | `t = time(every=4); aim(direction=D, yaw=curve(t, "Cosine", low=-18, high=18), pitch=curve(t, "Sine", low=-18, high=18))` | one time → two curves |
| Figure-8 | `t = time(every=4); aim(direction=D, yaw=curve(t, "Sine", low=-25, high=25), pitch=curve(t, "Double sine", low=-12.5, high=12.5))` | the same, pitch twice as fast |
| Pinwheel | `turn = space(kind="angle"); t = time(every=4, phase=curve(turn, "Ramp up")); aim(direction=D, yaw=curve(t, "Cosine", low=-20, high=20), pitch=curve(t, "Sine", low=-20, high=20))` | Circle with phase by angle |
| Scissor | `halves = mirror(); t = time(every=4); aim(heads=halves, direction=D, yaw=curve(t, "Sine", low=-30, high=30))` | mirrored heads yaw the other way |
| Up/down flip | `t = time(every=2); aim(direction=D, pitch=curve(t, "Square", low=-30, high=30))` | held pitch |
| Ballyhoo | `drift = noise(speed=4, scale=0.02); wander = noise(speed=4, scale=0.02); aim(direction=D, yaw=curve(drift, "Ramp up", low=-40, high=40), pitch=curve(wander, "Ramp up", low=-40, high=40))` | two noise nodes, two streams |

Motion presets ship with blend `offset`. Put a Position clip (`replace`)
under them.

## Strobe

| Name | Python | Chain |
|---|---|---|
| Strobe | `strobe(rate=0.9)` | strobe |
| Ramp | `t = time(); strobe(rate=curve(t, "Ramp up"))` | time → curve → rate |
| Strobe follows a band | `kick = audio(low_hz=40, high_hz=100); strobe(rate=curve(kick, "Ramp up", low=0.3, high=1))` | audio → curve → rate |

## Not one clip

| Name | Kind | How |
|---|---|---|
| Alternating diagonals | several clips | Two Diagonal slash clips, directions `(1, 0, 1)` and `(1, 0, -1)`, placed in turn. |
| Fireworks | several clips | A Ripple clip and a Sparkle rain clip at the same time. |
| Colored strobe | several clips | A Wash in the color, with a Strobe clip above it. |
| Sky lift | transition | Two Position clips overlap. The second fades in with `alpha=curve(time(), "Fade in")`. |
| Blackout | one clip or none | A Wash with `brightness=0` on `replace`, or place nothing. |
| Lean over crowd | one clip | Position with `direction=(0, 0.94, -0.34)`. |
| Strobe burst | one clip | A short Strobe clip. |
| Wobble strobe | agent's job | Measure the wobble rate with `luma.music`. Place Strobe clips, or a Strobe with `rate=curve(time(every=<rate>), "Square")`. |
| Color per pitch | agent's job | Analyse the pitch. Place one Wash clip per note, each in its color. |
| Level meter | agent's job | A bar that grows with the level is not one clip. Place Wipe clips on the peaks, or drive `brightness` with `curve(audio("Bass"), "Ramp up")`. |

## Change an effect

- Speed: change `every`. Overlap: set `duration` above `every`.
- Direction of travel: `direction=` on `space`, or swap "Ramp down" and
  "Ramp up" on the curve over space.
- Spread: the `low` and `high` of the curve over space. A small spread moves
  all heads almost together. On `delay` the spread is in beats.
- A separate effect per fixture: put `split()` into `heads`.
- Combine: multiply, `brightness=a * b`. `max(a, b)` keeps the brighter of
  two patterns; `1 - a` turns a pattern over.
- React to the music: replace a fixed number with
  `curve(audio("Kick"), "Ramp up", low=..., high=...)`, or multiply it in:
  `brightness=pill * curve(audio("Kick"), "Ramp up", low=0.3, high=1)`.
- Fade the whole clip: `alpha=curve(time(), "Fade in")`. Alpha is the clip's
  opacity over the light below; keep the pattern on `brightness`.
