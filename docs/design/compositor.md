# Compositor

The compositor combines the clips of one score into one lighting state per
fixture head. It evaluates each frame on request. It does not store a
precomputed time series.

Files: `backend/src/compositor.rs`, `backend/src/eval/scene.rs`,
`backend/src/eval/composite.rs`, `backend/src/render_engine.rs`.

## Scenes

`build_score_scene` loads a score and compiles it into a `Scene`. A scene is a
list of `CompiledAnnotation` values. Each one has:

| Field | Meaning |
|-------|---------|
| `plan` | The compiled graph for the clip |
| `span` | Active time range `[start, end)`, in track seconds |
| `z_index` | Layer order |
| `blend_mode` | Blend mode of the clip |

`Scene::new` sorts the annotations by `z_index`, lowest first.

`Scene::render` takes a list of times and a scope:

- `Single`: one annotation alone, with no compositing and no span check. The
  editor preview uses this.
- `Composite`: all annotations that are active at each time, in layer order.

A clip is active when `start <= t < end`.

## Frame compositing

For each time, compositing starts with an empty frame. Each active annotation
is then evaluated and added with `composite_frame`. Only the capabilities that
the clip's graph writes are changed. The other capabilities of lower layers
stay.

Each layer carries its alpha per head: the clip's opacity. The clip's values
blend with the values under them by the blend mode, and the result mixes with
the values under them by alpha. Alpha 0 is no clip, in every mode. A head with
no value yet blends against no light.

| Capability | Rule |
|------------|------|
| Color and dimmer | `blend_light_alpha`, on the light (color times dimmer) |
| Strobe | `blend_value_alpha` with the clip's blend mode, clamped to 0–1 |
| Aim | `replace`: `blend_aim`; `offset`: `offset_aim` (below) |
| Position | The top layer replaces the value |
| Speed | Binary: above 0.5 is 1, else 0 |

Laser clips draw elements into the frame's laser list instead.

A head that no clip writes has these defaults: dimmer 0, color white, strobe
0, position (0, 0), speed 1.

## Blend modes

The modes are defined in `backend/crates/patterns/src/blend.rs`. `blend_value`
blends one scalar channel. `base` is the lower value and `top` is the clip
value.

| Mode | Formula |
|------|---------|
| `replace` | `top` |
| `add` | `min(base + top, 1)` |
| `multiply` | `base * top` |
| `screen` | `1 - (1 - base) * (1 - top)` |
| `max` | `max(base, top)` |
| `min` | `min(base, top)` |
| `subtract` | `max(base - top, 0)` |
| `below` | `top` |

The default blend mode of a clip is `replace`.

An aim clip takes `replace` or `offset` only, a laser clip `replace` or
`add`, and no other clip takes `offset` (`blend_modes` in
`backend/crates/patterns/src/clip_graph/check.rs`). A
`replace` aim blends toward the clip's aim by alpha along the shortest arc
(`blend_aim`). An `offset` aim gives a turn per head instead: the aim node's
yaw and pitch. `offset_frame` turns the aim under the clip
by it, with every angle times alpha (`offset_aim`). Over no aim the turn
starts from the head's home.

`blend_light` blends one head's light. The light is the color times the
dimmer, in linear Rec. 2020. No light under a head is a dimmer of 0.

- `replace` is the top light: the top color and the top dimmer. Black on
  `replace` paints black. Opacity is alpha's job: `blend_light_alpha` mixes
  the result with the light below by alpha.
- `below` is the top dimmer in the color already under the head: a layer
  that takes its color from the layers below it. Over nothing that color is
  white.
- Every other mode applies `blend_value` to each channel of the light. The
  new dimmer is the peak channel, and the color is the light divided by it.
  So `add` and `screen` over black equal `add` and `screen` over nothing, and
  `multiply` or `subtract` over nothing stay nothing.

Where a blend leaves no light, the head keeps the color under it, so a
`below` layer higher up still finds it.

`offset` never reaches `blend_value`: an `offset` clip goes through
`offset_frame` instead.

## Sampling

`RenderEngine::sample(t)` returns the lighting state at one time. The native
visualizer calls it for each frame it draws. The desktop output loop calls it
every 23 ms with the audio playback time (`run_output` in
`backend/src/dispatch/services.rs`).

## Perform mode

When decks are live, `sample` uses the perform mix instead of the editor scene
(`backend/src/render_engine.rs`):

1. Each deck with a volume above zero renders its own scene at the deck time.
2. Dimmer, color, strobe and speed are averaged, weighted by deck volume.
   Position and aim come from the deck with the highest volume.
3. The venue's Perform set composites over that mix, or over the editor scene
   when no deck is followed (`backend/src/perform/live.rs`). It runs on the
   followed deck's time plus the latency, or on the tapped tempo. Each
   pattern's pad is a button node wired into its graph's alpha, so the graph
   fades itself in and out; the set scales it by its level times the Perform
   master. The venue's base color is written under the layers into each
   light with no dimmer, so a `below` layer finds it.

The control bank (`backend/src/controls.rs`) holds every live value the set
reads: faders, buttons, chosen groups, list values and the tapped tempo.
Every input writes there, so an on-screen pad, a MIDI key and an OSC message
are the same press.

See [Live](https://luma.show/docs/live).
