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

Three patterns make most effects:

- **A moving shape** (chase, bounce, sweep, meter): a curve over time (or
  audio) on `space.shift` slides the shape along the heads, with that
  curve's ease. `length` sets how much of the axis the shape covers.
- **A loop** (wave, spin): each head runs the same shape over time, shifted
  by its place. Put a curve over space on `time.phase`.
- **A one-shot** (wipe, dissolve, grow): each head starts later by its place.
  Put a curve over space on `time.delay`, and use a step shape.

A pill of length `w` that enters and leaves: `shift` runs from `-w` to 1
(`curve(time(k), "Ramp up", low=-w, high=1)`), `length=w`, and the pill
shape is `[[0, 0], [0, 1], [1, 1], [1, 0]]`. Name each node by its variable;
the names show on the cards.

## Color

| Name | Python | Chain |
|---|---|---|
| Wash | `color(color=(1, 1, 1))` | color |
| Pulse | `beat = clock(every=1); t = time(beat); drop = curve(t, "Drop"); color(brightness=drop)` | clock → time → curve → brightness |
| Breathe | `k = clock(every=4); t = time(k); swell = curve(t, "Swell"); color(brightness=swell)` | clock → time → curve → brightness |
| Fade | `t = time(); fade = curve(t, "Fade in"); color(alpha=fade)` | time → curve → alpha |
| Color fade | `t = time(); hue = curve(t, "Ramp up", gradient=[(0, "#b0400a"), (1, "#2449eb")]); color(color=hue)` | time → curve(color) → color |
| Rainbow | `k = clock(every=4); t = time(k); hue = curve(t, "Ramp up", gradient="Rainbow"); color(color=hue)` | clock → time → curve(color) |
| Gradient | `place = space(); hue = curve(place, "Ramp up", gradient="Sunset"); color(color=hue)` | space → curve(color) |
| Stepped palette | `k = clock(every=4); t = time(k); hue = curve(t, "Steps 4", gradient="Rainbow"); color(color=hue)` | clock → time → curve(color) |
| Two-color swap | `k = clock(every=2); t = time(k); hue = curve(t, "Square", gradient=[(0, "#ff2a00"), (1, "#0040ff")]); color(color=hue)` | clock → time → curve(color) |
| Follows a band | `kick = audio(40, 100); level = curve(kick, "Ramp up"); color(brightness=level)` | audio → curve → brightness |
| VU meter | `bass = audio(20, 250); level = curve(bass, "Ramp up"); bars = split(); height = space(bars, direction=(0, 0, 1), shift=level); meter = curve(height, "Step down"); color(brightness=meter)` | audio → curve → space.shift up each bar; Step down lights the heads below the level |
| Random heads | `k = clock(every=1); order = shuffle(clock=k); rank = space(order, kind="order"); half = curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)` | clock → shuffle → space(order) → curve with a jump |
| Random bars | `k = clock(every=1); bars = group(); order = shuffle(bars, clock=k); rank = space(order, kind="order"); half = curve(rank, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)` | clock → shuffle(group) → space → curve |
| Sparkle | `k = clock(every=0.125, duration=0.5); order = shuffle(clock=k); rank = space(order, kind="order"); pick = curve(rank, [[0, 1], [0.3, 1], [0.3, 0], [1, 0]]); t = time(k); spike = curve(t, "Spike"); color(brightness=[pick, spike])` | one clock; a random 30% × a spike over time |
| Build | `order = shuffle(); rank = space(order, kind="order"); start = curve(rank, "Ramp up"); t = time(delay=start); on = curve(t, "Step up"); color(brightness=on)` | shuffle → space(order) → curve → time.delay; time → step |
| Dissolve | `order = shuffle(); rank = space(order, kind="order"); stop = curve(rank, "Ramp down"); t = time(delay=stop); off = curve(t, "Step down"); color(brightness=off)` | as Build, heads go off in a random order |
| Clouds | `cloud = noise(speed=8, scale=0.5); hue = curve(cloud, "Ramp up", gradient="Ocean"); level = curve(cloud, "Ramp up", low=0.2, high=1); color(color=hue, brightness=level)` | one noise → two curves |
| Sparkle rain | `k = clock(every=0.25, duration=1); bars = group(); order = shuffle(bars, clock=k); columns = split(); t = time(k); fall = curve(t, "Ramp up", low=-0.3, high=1); drop = space(columns, direction=(0, 0, -1), shift=fall, length=0.3); streak = curve(drop, "Comet"); rank = space(order, kind="order"); pick = curve(rank, [[0, 0], [0, 1], [0.2, 1], [0.2, 0], [1, 0]]); color(brightness=streak, alpha=pick)` | needs vertical bars; each event's fall is a shift down each bar; the pick is a random 20% per bar |

## Movement

| Name | Python | Chain |
|---|---|---|
| Chase | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)` | time → curve → space.shift, length 0.2 → a pill that enters and leaves |
| Wave | `k = clock(every=2); place = space(); lag = curve(place, "Ramp down", low=0.5, high=1); t = time(k, phase=lag); swell = curve(t, [[0, 0, [0.4, 0, 0.6, 1]], [0.25, 1, [0.4, 0, 0.6, 1]], [0.5, 0], [1, 0]]); color(brightness=swell)` | Chase with a wide soft pulse |
| Bounce | `k = clock(every=4); t = time(k); move = curve(t, "Triangle", low=0, high=0.8); place = space(shift=move, length=0.2); pill = curve(place, "Soft"); color(brightness=pill)` | a there-and-back time curve on space.shift; a soft pill over the shifted space |
| Comet | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); tail = curve(place, "Comet"); color(brightness=tail)` | Chase with a Comet shape: a sharp head and a tail |
| Wipe | `k = clock(every=4); place = space(); start = curve(place, "Ramp up"); t = time(k, delay=start); on = curve(t, "Step up"); color(brightness=on)` | space → curve → time.delay; each head turns on in turn and stays on |
| Stepped chase | `k = clock(every=4); t = time(k); move = curve(t, "Steps 4", low=0, high=0.75); place = space(shift=move, length=0.25); block = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=block)` | a Steps 4 curve on space.shift: the block jumps a quarter at a time |
| Many pills | `k = clock(every=0.5, duration=2); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)` | Chase with four events alive at once |
| Wrapping chase | `k = clock(every=2); ring = space(wrap=True); lag = curve(ring, "Ramp down"); t = time(k, phase=lag); pill = curve(t, [[0, 0], [0.8, 0], [0.8, 1], [1, 1]]); color(brightness=pill)` | a ring: the pill leaves one end and enters the other |
| Colored pills | `k = clock(every=0.5, duration=2); t = time(k); move = curve(t, "Ramp up", low=-0.25, high=1); place = space(shift=move, length=0.25); age = time(k); hue = curve(age, "Ramp up", gradient="Rainbow"); pill = curve(place, "Soft"); color(color=hue, brightness=pill)` | each event has its own progress, so its own shift and its own color |
| Speed-up chase | `t = time(); every = curve(t, "Ramp down", low=0.25, high=2); life = curve(t, "Ramp down", low=0.5, high=2); k = clock(every=every, duration=life); age = time(k); move = curve(age, "Ramp up", low=-0.4, high=1); size = curve(t, "Ramp down", low=0.1, high=0.4); place = space(shift=move, length=size); tail = curve(place, "Comet"); color(brightness=tail)` | one time() feeds every, duration and the length; the shift runs on each event |
| Alternating sides | `k = clock(every=2); place = space(); lag = curve(place, [[0, 0.5], [0.5, 0.5], [0.5, 0], [1, 0]]); t = time(k, phase=lag); half = curve(t, [[0, 1], [0.5, 1], [0.5, 0], [1, 0]]); color(brightness=half)` | the two halves are half a turn apart |
| Diagonal slash | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.15, high=1); place = space(direction=(1, 0, 1), shift=move, length=0.15); line = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=line)` | Chase along a diagonal direction |
| Slash | `k=clock(every=2); cut=curve(time(k, delay=curve(space(direction=(-0.82,0,0.57)), "Ramp up", low=0, high=0.2)), "Step up"); d=space(direction=(0.57,0,0.82)); v=[[0,1],[0.68,0],[1,0.47]]; bloom=curve(time(k, delay=curve(d, v, low=0, high=0.9), length=curve(d, v, low=0.05, high=0.4)), "Ramp up"); fade=curve(time(k), [[0,1,"hold"],[0.2,1,"sine-out"],[1,0]]); color(brightness=[cut, bloom, fade])` | not a shipped preset. cut × bloom × fade: a fast cut across one axis, a bloom out from a line with a fade-in that grows with distance, then a fade for all |
| Ripple | `k = clock(every=2); t = time(k); move = curve(t, "Ramp up", low=-0.4, high=1); radius = space(shift=move, length=0.4, kind="radial"); ring = curve(radius, "Soft"); color(brightness=ring)` | Chase over radius: rings go out from the centre |
| Wrapping ripple | `k = clock(every=2); radius = space(kind="radial", wrap=True); lag = curve(radius, "Ramp down", low=-0.4, high=0.6); t = time(k, length=0.7143, phase=lag); ring = curve(t, [[0, 0], [0.6, 0, [0.4, 0, 0.6, 1]], [0.8, 1, [0.4, 0, 0.6, 1]], [1, 0]]); color(brightness=ring)` | rings come in again at the centre |
| Spin | `k = clock(every=2); turn = space(kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.75, 0], [0.7625, 1], [1, 0]]); color(brightness=arm)` | angle wraps by default |
| Grow | `radius = space(kind="radial"); start = curve(radius, "Ramp up"); t = time(delay=start); on = curve(t, "Step up"); color(brightness=on)` | Wipe over radius: heads turn on from the centre out |
| Turning line | `k = clock(every=2, duration=4); turn = space(kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.9, 0], [0.9, 1], [1, 1]]); color(brightness=arm)` | duration = 2 × every: two opposite arms alive |
| Spiral | `k = clock(every=4); radius = space(kind="radial"); inner = curve(radius, "Ramp up"); outer = curve(radius, "Ramp up", low=1, high=2); turn = space(kind="angle"); lag = curve(turn, "Ramp down", low=inner, high=outer); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.7, 0, [0.4, 0, 0.6, 1]], [0.85, 1, [0.4, 0, 0.6, 1]], [1, 0]]); color(brightness=arm)` | phase by angle, moved by radius, bends the arm |
| Mirror | `k = clock(every=2); halves = mirror(); t = time(k); move = curve(t, "Ramp up", low=-0.2, high=1); place = space(halves, shift=move, length=0.2); pill = curve(place, [[0, 0], [0, 1], [1, 1], [1, 0]]); color(brightness=pill)` | Chase over mirrored heads: pills from both ends meet |
| Kaleidoscope | `k = clock(every=4); sides = mirror(normal=(1, 0, 0)); quarters = mirror(sides, normal=(0, 0, 1)); turn = space(quarters, kind="angle"); lag = curve(turn, "Ramp down"); t = time(k, phase=lag); arm = curve(t, [[0, 0], [0.85, 0], [0.8575, 1], [1, 0]]); color(brightness=arm)` | two mirrors, four-fold |

## Aim

| Name | Python | Chain |
|---|---|---|
| Position | `aim(direction=D)` | aim |
| Fan | `place = space(); spread = curve(place, "Ramp up", low=-25, high=25); aim(direction=D, yaw=spread)` | space → curve → yaw |
| Converge | `aim(point=(0, 3, 0), base="point")` | point base |
| Follow | `t = time(); target = curve(t, "Ramp up", low=(-3, 3, 0), high=(3, 3, 0)); aim(point=target, base="point")` | time → curve(vector) → point |
| Bloom | `t = time(); source = curve(t, "Ramp up", low=(0, 0, 40), high=(0, 0, 7)); aim(point=source, base="away")` | away base; the point comes down toward the rig |
| Tunnel | `aim(point=(0, 25, 1.5), base="point")` | a far point downstage |
| Sweep | `k = clock(every=8); t = time(k); swing = curve(t, "Sine", low=-45, high=45); aim(direction=D, yaw=swing)` | clock → time → curve → yaw |
| Nod wave | `k = clock(every=4); place = space(); lag = curve(place, "Ramp up", high=0.6); t = time(k, phase=lag); nod = curve(t, "Sine", low=-25, high=25); aim(direction=D, pitch=nod)` | space → curve → time.phase |
| Circle | `k = clock(every=4); t = time(k); across = curve(t, "Cosine", low=-18, high=18); up = curve(t, "Sine", low=-18, high=18); aim(direction=D, yaw=across, pitch=up)` | one time → two curves |
| Figure-8 | `k = clock(every=4); t = time(k); across = curve(t, "Sine", low=-25, high=25); up = curve(t, "Double sine", low=-12.5, high=12.5); aim(direction=D, yaw=across, pitch=up)` | the same, pitch twice as fast |
| Pinwheel | `k = clock(every=4); turn = space(kind="angle"); lag = curve(turn, "Ramp up"); t = time(k, phase=lag); across = curve(t, "Cosine", low=-20, high=20); up = curve(t, "Sine", low=-20, high=20); aim(direction=D, yaw=across, pitch=up)` | Circle with phase by angle |
| Scissor | `k = clock(every=4); halves = mirror(); t = time(k); swing = curve(t, "Sine", low=-30, high=30); aim(heads=halves, direction=D, yaw=swing)` | mirrored heads yaw the other way |
| Up/down flip | `k = clock(every=2); t = time(k); flip = curve(t, "Square", low=-30, high=30); aim(direction=D, pitch=flip)` | held pitch |
| Ballyhoo | `drift = noise(speed=4, scale=0.02); across = curve(drift, "Ramp up", low=-40, high=40); wander = noise(speed=4, scale=0.02); up = curve(wander, "Ramp up", low=-40, high=40); aim(direction=D, yaw=across, pitch=up)` | two noise nodes, two streams |

Motion presets ship with blend `offset`. Put a Position clip (`replace`)
under them.

## Strobe

| Name | Python | Chain |
|---|---|---|
| Strobe | `strobe(rate=0.9)` | strobe |
| Ramp | `t = time(); rise = curve(t, "Ramp up"); strobe(rate=rise)` | time → curve → rate |
| Strobe follows a band | `kick = audio(40, 100); rate = curve(kick, "Ramp up", low=0.3, high=1); strobe(rate=rate)` | audio → curve → rate |

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
| Level meter | agent's job | A bar that grows with the level is not one clip. Place Wipe clips on the peaks, or drive `brightness` with `curve(audio("Bass"), "Ramp up")`. |

## Change an effect

- Speed: change `every`. Overlap: set `duration` above `every`.
- Direction of travel: `direction=` on `space`, or swap "Ramp down" and
  "Ramp up" on the curve over space.
- Spread: the `low` and `high` of the curve over space. A small spread moves
  all heads almost together.
- A separate effect per fixture: put `split()` into `heads`.
- Combine: put a list on brightness or alpha, `brightness=[a, b]`. The
  items multiply.
- React to the music: replace a fixed number with
  `curve(audio("Kick"), "Ramp up", low=..., high=...)`, or add it to a
  brightness list.
- Fade the whole clip: `alpha=curve(time(), "Fade in")`.
