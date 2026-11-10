# The presentation seam — handing a rendered stage to gpui

How a frame drawn by `luma-render` reaches the screen, and why it is shaped the
way it is.

The stage renders on its own thread into an offscreen texture. Where the
platform allows it, that texture is memory the window compositor can sample
directly, so the frame is written once, where it will be read: nothing is
copied and nothing is uploaded. Elsewhere the frame is read back to bytes and
painted as an image. The caller does not ask which it got: it receives a
`Presented` (`gpui/crates/render/src/viewport.rs`) and paints whichever variant
it holds.

---

## 1. The two paths

**Shared.** `Destination::Compositor` asks for a target the compositor can
address. `share::Shared::new` (`gpui/crates/render/src/share.rs`) allocates it,
the composite pass renders into it, and the frame is `Presented::Shared(Surface)`.
The app paints it with gpui's surface primitive (`Window::paint_surface`, or
`paint_upscaled_surface` for a frame rendered smaller than its bounds).

**Readback.** When `Shared::new` answers `None`, `Destination::Compositor` falls
back to `Destination::Bytes(Bgra)`:

```
composite pass → output texture
              → copy_texture_to_buffer  (GPU→CPU, pixel-linear)
              → map_async, memcpy rows out
              → RenderImage
              → PlatformAtlas::update   (CPU→GPU, pixel-linear, UI thread)
              → polychrome sprite draw
```

That is two full crossings of the memory bus per frame, the second one on the
UI thread. It is the only path on a device the compositor does not share: a
headless harness, a software fallback, a platform with no shared memory.

What "shared" means differs per platform, behind one `Shared` and one
`Surface`:

| platform | shared target | compositor's handle |
|---|---|---|
| macOS (Metal) | an `IOSurface`, wrapped as an `MTLTexture` by both devices | `CVPixelBuffer` |
| Linux, FreeBSD (wgpu) | an ordinary texture on the compositor's own device, which the renderer adopts (`device::DeviceContext::adopt`) | `wgpu::Texture` |
| anything else | none; `Surface` is uninhabited | — |

An HDR display (`DisplayRange::Hdr`) asks for a half-float linear target. On
wgpu that is shared like the SDR one. On macOS only BGRA8 is shared, so an HDR
request answers `None` and the frame is read back as SDR.

`LUMA_WITHHOLD_SHARED_SURFACES` forces the readback path. It exists because
every machine the tests run on can share memory, so without it the fallback
would never be executed. It is a coverage switch, not a rendering mode — a test
that sets it should be asserting the two paths draw the same thing.

## 2. Why gpui's surface primitive

gpui already has an external-surface path: `Primitive::Surface` /
`PaintSurface` / `Window::paint_surface`, with its own pipeline and per-instance
texture binding. It exists because Zed draws video frames. Upstream it is
macOS-only and hardcoded to planar YCbCr, because a camera is the only thing
that had ever fed it. Luma's gpui generalizes it: the surface carries its
own pixel format, which selects a plane layout and a fragment shader, and
`SurfaceHandle` names the compositor's shareable memory on every platform.

Z-ordering, clipping, content masks, batching and the damage/replay path all
work unchanged, because they were never format-specific.

Two alternatives were not taken:

- **An external-texture variant in the sprite atlas.** The atlas is a packed
  allocator: a tile is a sub-rectangle of a shared texture, and every sprite
  draw binds the one atlas texture and computes UVs from the tile bounds. An
  externally owned whole texture is none of those things, and admitting one
  carves exceptions into the invariant that makes the atlas an atlas.
- **A new external-texture primitive.** It would be a second primitive,
  element, shader and batch kind for a concept gpui already has.

The changes to gpui that this relies on are commits in
[lumashow/gpui](https://github.com/lumashow/gpui), listed in its `LUMA.md`.

## 3. How the Metal path works

```
IOSurface (one allocation)
 ├── MTLTexture, BGRA8Unorm_sRGB, RenderTarget  ← wgpu, renderer thread
 └── MTLTexture, BGRA8Unorm,      ShaderRead    ← gpui,  UI thread
```

The renderer's composite pass writes through the sRGB view, so the hardware
encodes exactly as it does into a staged target. The compositor's view is *not*
sRGB, so it samples the encoded bytes unchanged — which is what the polychrome
atlas does with those same bytes on the readback path. The asymmetry is what
makes the shared path and the readback path produce one picture rather than
two.

### Fencing

The readback copy is also a fence: a frame's pixels cannot be read until the
GPU has finished producing them, and the slot cannot be reused until they have
been read out. The shared path replaces both.

- **Completion** — `Queue::on_submitted_work_done` on the renderer's queue.
  A shared frame becomes presentable only after the queue says its submission is
  done, which is the same guarantee the buffer map provides.
- **Reuse** — `viewport.rs` reserves the last `RESERVED = 3` presented slots
  against being rendered into, out of `PRESENTATION_SLOTS = 5`, which leaves two
  for the renderer. The rule is `RESERVED >= drawables + 1`: the window's
  `CAMetalLayer` allows two drawables (`set_maximum_drawable_count(2)` in the
  `gpui_apple/src/metal_renderer.rs` in lumashow/gpui), each command buffer samples the
  surface that was current when it was encoded, and the slot is released in
  prepaint, one frame before the acquire that would bound it. The doc comment
  on `RESERVED` works the count out. Change either number and the other must be
  rechecked.

The reservation lives in the slot table rather than in the renderer because the
slot table is the only place that knows which frame reached the screen. Pushing
it down into `share.rs` would mean the renderer guessing at the compositor's
schedule.

The completion signal is a real fence: the renderer's own queue reports its
work done before the frame becomes presentable. The reservation is not — it is a
counting argument that two files must agree on, bounding how soon a surface can
be written again rather than ordering two devices against each other. Retaining
the `CVPixelBuffer` in the window command buffer's completed handler and
releasing the slot from that callback would replace it with a real fence.

On one wgpu device (Linux) the compositor's draw is ordered after the
renderer's submit by the queue itself. The reservation is harmless and
unnecessary there.

**One rule, one place.** Whether the renderer worker has somewhere to start a
frame is `Slots::startable_slot`, and both `can_begin` and `begin_latest` ask
it. A second predicate that restates the rule does not produce a wrong frame.
It produces a worker that spins on a queue it will not be allowed to begin: a
live-lock with no panic and no failing assertion.

### Headless capture

On macOS the pixel harness screenshots through
`HeadlessAppContext::capture_screenshot` → `Window::render_to_image` →
`MetalHeadlessRenderer`, which is the real Metal renderer rendering offscreen —
the same batch loop, the same `draw_surfaces`. So stage pixels appear in
headless screenshots on the shared path, and pixel tests cover it rather than
bypassing it.

## 4. What it costs

`LUMA_WITHHOLD_SHARED_SURFACES` makes this a controlled comparison: one binary,
one machine, one workload, the seam switched underneath. UI-thread `drawMs`
over 120 paced frames on macOS, release build, median of three runs:

| stage pane | shared | readback |
|---|---|---|
| 943×262 (0.25 Mpx) — median | **0.91 ms** | 1.17 ms |
| 943×262 — p95 | **1.09 ms** | 1.53 ms |
| 2303×518 (1.19 Mpx) — median | **0.95 ms** | 2.42 ms |
| 2303×518 — p95 | **1.14 ms** | 2.82 ms |

The shape matters more than the figures, which were taken on a loaded machine.
The readback path costs **~1.3 ms per megapixel** of viewport, every frame, on
the UI thread; the shared path is **flat** against a 4.8× change in viewport
area, because publishing a shared frame is `paint_surface` and nothing else. The
residual ~0.9 ms is gpui building the scene for the rest of the window.

A presentation cost only shows when a new frame lands for nearly every paint.
Repainting a frame already published is free on every path, so a measurement
that settles faster than the renderer completes frames reports no difference.
Quote the stage's pixel dimensions with any such timing.

## 5. Known issues

- **`core-video` 0.5.2 `CVPixelBuffer::get_io_surface` over-releases.**
  `CVPixelBufferGetIOSurface` follows the *Get* rule, but the binding wraps the
  result with `wrap_under_create_rule`, so dropping the returned `IOSurface`
  releases a reference nobody took. `share.rs` avoids it by creating the
  `IOSurface` itself and building the `CVPixelBuffer` from it.
- **`gpui_apple::draw_surfaces` creates its `CVMetalTexture` wrappers per
  surface per frame.** Fine at one surface; it would want a cache at many.
- **`io-surface` 0.16 is deprecated** in favour of `objc2-io-surface`, but
  `core-video` 0.5.2 — the version gpui pins — speaks the old one, so switching
  ours alone would only add a pointer cast.
