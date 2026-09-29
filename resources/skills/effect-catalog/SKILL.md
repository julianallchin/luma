---
name: effect-catalog
description: Every lighting effect Luma knows, as the Python that builds its clip graph, the node chain, and the shipped clip preset name. Color, movement, aim and strobe effects, and the effects that are not one clip (several clips, a transition, or the agent's own analysis). Read this to find how to build a look.
---

# Effect catalog

Each row is one clip. The name is the shipped clip preset: `preset("Chase")`
gives the same graph with its name and blend mode. The Python builds the graph with the
bare builders; pass it to `edit.add_clip(graph, name=..., ...)`. Change the
numbers to fit the music. Read `node-cards` for what each node does.

`D` below is the default aim `(0, 0.766, -0.643)`. Shapes and gradients are
preset names (see `node-cards`).

## Color

| Name | Python | Chain |
|---|---|---|
| Wash | `color(color=(1,1,1))` | color |
| Pulse | `color(brightness=curve(time(clock(every=1)), "Drop"))` | clock → time → curve → brightness |
| Breathe | `color(brightness=curve(time(clock(every=4)), "Swell"))` | clock → time → curve → brightness |
| Fade | `color(alpha=curve(time(), "Fade in"))` | time → curve → alpha |
| Color fade | `color(color=curve(time(), "Ramp up", gradient=[(0,"#b0400a"),(1,"#2449eb")]))` | time → curve(color) → color |
| Rainbow | `color(color=curve(time(clock(every=4)), "Ramp up", gradient="Rainbow"))` | clock → time → curve(color) |
| Gradient | `color(color=curve(space(), "Ramp up", gradient="Sunset"))` | space → curve(color) |
| Stepped palette | `color(color=curve(time(clock(every=4)), "Steps 4", gradient="Rainbow"))` | clock → time → curve(color) |
| Two-color swap | `color(color=curve(time(clock(every=2)), "Square", gradient=[(0,"#ff2a00"),(1,"#0040ff")]))` | clock → time → curve(color) |
| Follows a band | `color(brightness=curve(audio(40, 100), "Ramp up"))` | audio → curve → brightness |
| VU meter | `color(brightness=curve(space(split(), direction=(0,0,1), offset=0, width=curve(audio(20,250), "Ramp up")), "On"))` | audio → curve → space.width; split → space → curve → brightness |
| Random heads | `k=clock(every=1); color(brightness=curve(space(shuffle(clock=k), kind="order", offset=0, width=0.5), "On"))` | clock → shuffle → space(order) → curve |
| Random bars | `k=clock(every=1); color(brightness=curve(space(shuffle(group(), k), kind="order", offset=0, width=0.5), "On"))` | clock → shuffle(group) → space → curve |
| Sparkle | `k=clock(every=0.125, duration=0.5); color(brightness=curve(space(shuffle(clock=k), kind="order", offset=0, width=0.3), "On"), alpha=curve(time(k), "Spike"))` | one clock → shuffle → space → curve → brightness; time → curve → alpha |
| Build | `color(brightness=curve(space(shuffle(), kind="order", offset=0, width=curve(time(), "Ramp up")), "On"))` | time → curve → space.width; shuffle → space → curve |
| Dissolve | as Build with `"Ramp down"` on the width curve | same |
| Clouds | `n=noise(speed=8, scale=0.5); color(color=curve(n, "Ramp up", gradient="Ocean"), brightness=curve(n, "Ramp up", low=0.2, high=1))` | one noise → two curves |
| Sparkle rain | `k=clock(every=0.25, duration=1); fall=curve(space(split(), direction=(0,0,-1), offset=curve(time(k), "Ramp up", low=-0.3, high=1), width=0.3), "Comet"); color(brightness=fall, alpha=curve(space(shuffle(group(), k), kind="order", offset=0, width=0.2), "On"))` | needs vertical bars; one clock drives the fall and the pick per bar |

## Movement

| Name | Python | Chain |
|---|---|---|
| Chase | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))` | clock → time → curve → space.offset; space → curve |
| Wave | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-1, high=1), width=1), "Soft"))` | same, a wide soft stroke |
| Bounce | `k=clock(every=4); color(brightness=curve(space(offset=curve(time(k), "Triangle", low=0, high=0.8), width=0.2), "Soft"))` | offset goes there and back |
| Comet | Chase with `"Comet"` as the stroke shape | same |
| Wipe | `k=clock(every=4); color(brightness=curve(space(offset=0, width=curve(time(k), "Ramp up")), "On"))` | time → curve → space.width |
| Stepped chase | `k=clock(every=4); color(brightness=curve(space(offset=curve(time(k), "Steps 4", low=0, high=0.75), width=0.25), "On"))` | held offsets |
| Many pills | `k=clock(every=0.5, duration=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))` | four events alive at once |
| Wrapping chase | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Ramp up"), width=0.2, wrap=True), "On"))` | wrap on a line |
| Colored pills | `k=clock(every=0.5, duration=2); s=space(offset=curve(time(k), "Ramp up", low=-0.25, high=1), width=0.25); color(color=curve(time(k), "Ramp up", gradient="Rainbow"), brightness=curve(s, "Soft"))` | each event has its own progress, so its own color |
| Speed-up chase | `t=time(); k=clock(every=curve(t, "Ramp down", low=0.25, high=2), duration=curve(t, "Ramp down", low=0.5, high=2)); color(brightness=curve(space(offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=curve(t, "Ramp down", low=0.1, high=0.4)), "Comet"))` | one time() feeds every, duration and width; events multiply as they shrink |
| Alternating sides | `k=clock(every=2); color(brightness=curve(space(offset=curve(time(k), "Square", low=0, high=0.5), width=0.5), "On"))` | half the axis, swapping |
| Diagonal slash | `k=clock(every=2); color(brightness=curve(space(direction=(1,0,1), offset=curve(time(k), "Ramp up", low=-0.15, high=1), width=0.15), "On"))` | a diagonal line direction |
| Ripple | `k=clock(every=2); color(brightness=curve(space(kind="radial", offset=curve(time(k), "Ramp up", low=-0.4, high=1), width=0.4), "Soft"))` | radial stroke |
| Wrapping ripple | Ripple with `wrap=True` | rings come in again at the centre |
| Spin | `k=clock(every=2); color(brightness=curve(space(kind="angle", offset=curve(time(k), "Ramp up"), width=0.25), "Comet"))` | angle wraps by default |
| Grow | `color(brightness=curve(space(kind="radial", offset=0, width=curve(time(), "Ramp up")), "On"))` | width grows from the centre |
| Turning line | `k=clock(every=2, duration=4); color(brightness=curve(space(kind="angle", offset=curve(time(k), "Ramp up"), width=0.1), "On"))` | duration = 2 × every: two opposite arms alive |
| Spiral | `k=clock(every=4); t=time(k, phase=curve(space(kind="radial"), "Ramp up")); color(brightness=curve(space(kind="angle", offset=curve(t, "Ramp up"), width=0.3), "Soft"))` | phase by radius bends the arm |
| Mirror | `m=mirror(); k=clock(every=2); color(brightness=curve(space(m, offset=curve(time(k), "Ramp up", low=-0.2, high=1), width=0.2), "On"))` | pills from both ends meet |
| Kaleidoscope | `m=mirror(mirror(normal=(1,0,0)), normal=(0,0,1)); k=clock(every=4); color(brightness=curve(space(m, kind="angle", offset=curve(time(k), "Ramp up"), width=0.15), "Comet"))` | two mirrors, four-fold |

## Aim

| Name | Python | Chain |
|---|---|---|
| Position | `aim(direction=D)` | aim |
| Fan | `aim(direction=D, yaw=curve(space(), "Ramp up", low=-25, high=25))` | space → curve → yaw |
| Converge | `aim(base="point", point=(0, 3, 0))` | point base |
| Follow | `aim(base="point", point=curve(time(), "Ramp up", low=(-3,3,0), high=(3,3,0)))` | time → curve(vector) → point |
| Bloom | `aim(base="away", point=curve(time(), "Ramp up", low=(0,0,40), high=(0,0,7)))` | away base; the point comes down toward the rig |
| Tunnel | `aim(base="point", point=(0, 25, 1.5))` | a far point downstage |
| Sweep | `aim(direction=D, yaw=curve(time(clock(every=8)), "Sine", low=-45, high=45))` | clock → time → curve → yaw |
| Nod wave | `k=clock(every=4); aim(direction=D, pitch=curve(time(k, phase=curve(space(), "Ramp up", high=0.6)), "Sine", low=-25, high=25))` | space → curve → time.phase |
| Circle | `t=time(clock(every=4)); aim(direction=D, yaw=curve(t, "Cosine", low=-18, high=18), pitch=curve(t, "Sine", low=-18, high=18))` | one time → two curves |
| Figure-8 | `t=time(clock(every=4)); aim(direction=D, yaw=curve(t, "Sine", low=-25, high=25), pitch=curve(t, "Double sine", low=-12.5, high=12.5))` | the same, pitch twice as fast |
| Pinwheel | `t=time(clock(every=4), phase=curve(space(kind="angle"), "Ramp up")); aim(direction=D, yaw=curve(t, "Cosine", low=-20, high=20), pitch=curve(t, "Sine", low=-20, high=20))` | Circle with phase by angle |
| Scissor | `m=mirror(); aim(heads=m, direction=D, yaw=curve(time(clock(every=4)), "Sine", low=-30, high=30))` | mirrored heads yaw the other way |
| Up/down flip | `aim(direction=D, pitch=curve(time(clock(every=2)), "Square", low=-30, high=30))` | held pitch |
| Ballyhoo | `aim(direction=D, yaw=curve(noise(speed=4, scale=0.02), "Ramp up", low=-40, high=40), pitch=curve(noise(speed=4, scale=0.02), "Ramp up", low=-40, high=40))` | two noise nodes, two streams |

Motion presets ship with blend `offset`. Put a Position clip (`replace`)
under them.

## Strobe

| Name | Python | Chain |
|---|---|---|
| Strobe | `strobe(rate=0.9)` | strobe |
| Ramp | `strobe(rate=curve(time(), "Ramp up"))` | time → curve → rate |
| Strobe follows a band | `strobe(rate=curve(audio(40, 100), "Ramp up", low=0.3, high=1))` | audio → curve → rate |

## Not one clip

| Name | Kind | How |
|---|---|---|
| Alternating diagonals | several clips | Two Diagonal slash clips, directions `(1,0,1)` and `(1,0,-1)`, placed in turn. |
| Fireworks | several clips | A Ripple clip and a Sparkle rain clip at the same time. |
| Colored strobe | several clips | A Wash in the color, with a Strobe clip above it. |
| Sky lift | transition | Two Position clips overlap. The second fades in with `alpha=curve(time(), "Fade in")`. |
| Blackout | one clip or none | A Wash with `brightness=0` on `replace`, or place nothing. |
| Lean over crowd | one clip | Position with `direction=(0, 0.94, -0.34)`. |
| Strobe burst | one clip | A short Strobe clip. |
| Wobble strobe | agent's job | Measure the wobble rate with `luma.music`. Place Strobe clips, or a Strobe with `rate=curve(time(clock(every=<rate>)), "Square")`. |
| Color per pitch | agent's job | Analyse the pitch. Place one Wash clip per note, each in its color. |

## Change an effect

- Speed: change `every`. Overlap: set `duration` above `every`.
- Direction of travel: `direction=` on `space`, or a "Ramp down" offset with a
  flipped stroke shape.
- A separate effect per fixture: put `split()` into `heads`.
- React to the music: replace a fixed number with
  `curve(audio("Kick"), "Ramp up", low=..., high=...)`.
- Fade the whole clip: `alpha=curve(time(), "Fade in")`.
