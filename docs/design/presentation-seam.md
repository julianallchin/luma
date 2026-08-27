# The presentation seam — handing a rendered stage to gpui

How a frame drawn by `luma-render`'s wgpu device reaches the screen, and why it
is shaped the way it is.

This picks up where `volumetrics-v2.md` §4 "The presentation seam (vendor patch)"
left off. That round removed the copies *around* the handoff; this one removes
the handoff.

---

## 1. What was there

The stage renders on its own thread, on its own wgpu device, into an offscreen
texture. To get those pixels into a gpui window the frame took this route:

```
composite pass → output texture
              → copy_texture_to_buffer  (GPU→CPU, pixel-linear)
              → map_async, memcpy rows out
              → RenderImage
              → PlatformAtlas::update   (CPU→GPU, pixel-linear, UI thread)
              → polychrome sprite draw
```

Two full crossings of the memory bus per frame, the second one on the UI thread.
At 2560×1440 that is ~15 MB down and ~15 MB back up, sixty times a second. The
upload half measures ~1.3 ms per megapixel of viewport on the UI thread —
~1.5 ms per frame at the full-screen pane, and growing with every pixel the
window gains (§4).

Both crossings exist for one reason: gpui had no way to be handed memory it did
not own.

## 2. The three designs

### (a) Teach the sprite atlas about external textures

`PlatformAtlas::update` already exists as a vendor patch. Grow it an
`IOSurface` variant, let the sprite pipeline sample it, keep the `ImageId`
identity the publish path already uses. Smallest conceptual change.

**Rejected.** The atlas is a *packed allocator*: a tile is a sub-rectangle of a
shared texture, placed by `etagere`, and every sprite draw binds the one atlas
texture for its `AtlasTextureId` and computes UVs from the tile bounds. An
externally-owned whole texture is none of those things. Admitting one means a
texture id that no allocator issued, a tile whose bounds are the whole texture,
and a sprite shader that branches on which kind of texture it is sampling —
three exceptions carved into the invariant that makes the atlas an atlas, in
service of a caller that does not want packing at all. That is the definition of
patch accretion: the interface would grow a case that only one caller can use
and that every other caller must now reason around.

### (b) A new external-surface primitive in the fork

`Primitive::ExternalTexture`, a `paint_texture` on `Window`, an element, a
pipeline, a shader, and an arm in each of the three backends' exhaustive
matches.

**Rejected, but for a better reason than cost.** gpui *already has this
primitive*. `Primitive::Surface` / `PaintSurface` / `Window::paint_surface` /
`elements::surface` is a first-class external-surface path with its own Metal
pipeline, its own shader, and its own per-instance texture binding — it exists
because Zed draws video frames. Adding a second one would be the "second way to
do something" failure in its purest form: two primitives, two elements, two
shaders, two batch kinds, for one concept.

### (c) Generalize the primitive that exists — **chosen**

`PaintSurface` carries a `CVPixelBuffer`, which is a thin wrapper over an
`IOSurface`: exactly the allocation two Metal devices can share. The only thing
standing between it and our use case was one line:

```rust
assert_eq!(
    surface.image_buffer.get_pixel_format(),
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
);
```

The path was hardcoded to planar YCbCr because a camera is the only thing that
had ever fed it. So the change is not "add a case" — it is **delete a
special case**: the pixel format stops being an assumption and becomes what it
should always have been, a fact the surface carries about itself, selecting a
plane layout and a fragment shader.

What that bought:

- No new gpui type, no new `Window` method, no new element, no new
  `Primitive` variant, no `PrimitiveBatch` arm, no cbindgen export, and
  therefore **no arms to add to the wgpu or DirectX backends**.
- Z-ordering, clipping, content masks, batching and the damage/replay path all
  work already, because they were never format-specific.
- The vendor diff is one pipeline, one fragment shader, and a plane loop.

The general lesson, and the reason this was worth the search: the cheapest way
to add a capability to a fork is often to find the assumption that is already
almost the feature, and delete it.

## 3. How it works

```
IOSurface (one allocation)
 ├── MTLTexture, BGRA8Unorm_sRGB, RenderTarget  ← wgpu, renderer thread
 └── MTLTexture, BGRA8Unorm,      ShaderRead    ← gpui,  UI thread
```

The renderer's composite pass writes through the sRGB view, so the hardware
encodes exactly as it does into a staged target. The compositor's view is *not*
sRGB, so it samples the encoded bytes unchanged — which is precisely what the
polychrome atlas does with those same bytes today. The asymmetry is what makes
the shared path and the CPU path produce one picture rather than two.

Nothing is copied, and nothing is uploaded. The frame is written once, where it
will be read.

### Fencing

The copy that was removed was also, incidentally, a fence: a frame's pixels
could not be read until the GPU had finished producing them, and the slot could
not be reused until they had been read out. Both had to be replaced.

- **Completion** — `Queue::on_submitted_work_done` on the renderer's queue.
  A shared frame becomes presentable only after the queue says its submission is
  done, which is the same guarantee the buffer map used to provide.
- **Reuse** — `viewport.rs` reserves the presented slot *and its predecessor*
  (`Slots::reserved`, `RESERVED = 2`) against being rendered into. One is the
  frame on screen. The second is the frame before it: the UI takes a frame in
  prepaint, and the already-encoded window frame that still refers to the
  previous one has not finished yet. Two reserved out of `PRESENTATION_SLOTS = 4`
  leaves two for the renderer — the same headroom the three-slot readback seam
  had, and less memory, because the three readback buffers are gone.

The reservation lives in the slot table rather than in the renderer because the
slot table is the only place that knows which frame reached the screen. Pushing
it down into `share.rs` would mean the renderer guessing at the compositor's
schedule.

Note what this is and is not. The completion signal is a real fence: the
renderer's own queue reports its work done before the frame becomes presentable.
The reservation is not — it is a scheduling guarantee, bounding how soon a
surface can be written again rather than ordering two devices against each
other. Two generations of margin against a compositor that is at most one frame
behind is the same bet every `IOSurface` display pipeline on the platform makes.
If it ever needs to be a fence, the mechanism is a shared `MTLSharedEvent`
signalled by the renderer's command buffer and waited on by the compositor's,
which would mean the vendor patch growing a wait — the reason it is not there
now is that nothing has asked for it.

**One rule, one place.** The renderer worker used to decide whether to park or
loop with its own predicate — "is any slot not `Rendering`?" — which restated
`begin_latest`'s selection rule instead of asking it. Adding reservations made
the two disagree: with a job queued and every *unreserved* slot busy, the worker
believed work was startable, `continue`d, and spun forever on a queue
`begin_latest` would not let it begin. That live-lock has no panic and no wrong
pixel; it presents as a wedged UI several layers away (it was caught by a track
browser test timing out, on a screen with no stage on it). Both callers now go
through `Slots::startable_slot`. The duplicated predicate was a smell before
this change and merely harmless; the lesson is that "harmless duplication" is
only ever harmless until someone adds a case to one copy.

### The CPU path is still there

`Destination::Compositor` falls back to `Destination::Bytes(Bgra)` whenever
`share::Shared::new` returns `None` — a non-Metal adapter, a software fallback,
a platform that is not macOS. The caller never asks which it got: it receives a
`Presented`, and paints whichever variant it holds.

`LUMA_WITHHOLD_SHARED_SURFACES` forces that fallback. It exists because every
machine the tests run on can share memory, so without it the fallback would
never be executed again. It is a coverage switch, not a rendering mode — a test
that sets it should be asserting the two paths draw the same thing.

### Headless capture

The pixel harness screenshots through `HeadlessAppContext::capture_screenshot` →
`Window::render_to_image` → `MetalHeadlessRenderer`, which is the real Metal
renderer rendering offscreen — the same `render_frame`, the same batch loop, the
same `draw_surfaces`. So stage pixels appear in headless screenshots on the
shared path without the harness knowing anything changed, and pixel tests cover
the new path rather than bypassing it.

## 4. What it measured, and what nearly failed to measure it

`LUMA_WITHHOLD_SHARED_SURFACES` makes this a controlled comparison: one binary,
one machine, one workload, the seam switched underneath. UI-thread `drawMs`
over 120 paced frames, release build, median of three runs:

| stage pane | shared | readback |
|---|---|---|
| 943×262 (0.25 Mpx) — median | **0.91 ms** | 1.17 ms |
| 943×262 — p95 | **1.09 ms** | 1.53 ms |
| 2303×518 (1.19 Mpx) — median | **0.95 ms** | 2.42 ms |
| 2303×518 — p95 | **1.14 ms** | 2.82 ms |

The shape matters more than the figures. The readback path costs
**~1.3 ms per megapixel** of viewport, every frame, on the UI thread; the
shared path is **flat** against a 4.8× change in viewport area, because
publishing a shared frame is `paint_surface` and nothing else. The residual
~0.9 ms is gpui building the scene for the rest of the window — the floor this
seam can no longer contribute to.

Captured at machine load ~12, well above the load < 5 an absolute number needs.
Both sides of every pair ran under the same load and the comparison is
within-run, so the ratio and the scaling are sound while the absolute
milliseconds are inflated.

### The measurement that could not see it

The obvious instrument gave a null result, and the reason is worth recording
because it would otherwise be rediscovered.

`visualizer_budget` measures a camera drag. It reported ~0.51 ms median on
*both* paths at *both* sizes — flat even across a five-fold change in viewport
area, which for a pixel-linear cost is impossible unless the cost is not being
measured. Two things were true at once:

- `drawMs` times `Window::draw` — building a scene — and not `Window::present`.
  The harness says so in `FrameTiming`'s doc. The atlas upload *is* inside
  `Window::draw`, so it was in principle visible.
- But a drag settles far faster than the renderer completes frames, so most
  settles repainted a frame already published. Repainting a frame you already
  published is free on every path, and the median was measuring exactly that.

`visualizer_present_budget` exists for this: it paces settles at 20 ms so a new
frame lands for nearly every one, which is the only condition under which a
publish appears in the number at all. Both tests now report the stage's pixel
dimensions alongside the timing, because a presentation cost quoted without the
area it was paid for cannot be compared to anything — which is how the null
result nearly got written down as "no change".

This also puts a caveat on the earlier round's 12.3 → 7.2 ms table in
`volumetrics-v2.md`: that measurement was of a drag, so it captured the copies
that round removed from *prepaint* and could not have captured the atlas upload
in paint. The 7.2 ms it attributed to "the `replace_region` upload itself" was
not that.

## 5. Vendor patch surface

Cumulative, including the two patches from the previous round.

| file | what |
|---|---|
| `gpui/src/platform.rs` | `PlatformAtlas::update` (previous round) |
| `gpui/src/window.rs` | `Window::update_image` (previous round) |
| `gpui_apple/src/metal_atlas.rs` | Metal `update` via `replace_region` (previous round) |
| `gpui/src/gpui.rs` | re-export `core_video` |
| `gpui_apple/src/metal_renderer.rs` | `bgra_surfaces_pipeline_state`; plane/pipeline dispatch in `draw_surfaces`; `SurfaceInputIndex::BgraTexture` |
| `gpui_apple/src/shaders.metal` | `bgra_surface_fragment` |

The `core_video` re-export repairs a leak that predates this work:
`Window::paint_surface` and `PaintSurface` name `CVPixelBuffer` in their public
signatures while the crate providing it is private, so a caller had to depend on
`core-video` itself and guess gpui's exact version — a version that is an
implementation detail is the wrong thing to make callers guess.

## 6. Smells found and not fixed

- **`core-video` 0.5.2 `CVPixelBuffer::get_io_surface` over-releases.**
  `CVPixelBufferGetIOSurface` follows the *Get* rule, but the binding wraps the
  result with `wrap_under_create_rule`, so dropping the returned `IOSurface`
  releases a reference nobody took. `share.rs` avoids it by creating the
  `IOSurface` itself and building the `CVPixelBuffer` from it, which it wanted
  to do anyway. Worth reporting upstream.
- **`gpui_apple::draw_surfaces` creates its `CVMetalTexture` wrappers per
  surface per frame.** Fine at one surface; it would want a cache at many.
- **`io-surface` 0.16 is deprecated** in favour of `objc2-io-surface`, but
  `core-video` 0.5.2 — the version gpui pins — speaks the old one, so switching
  ours alone would only add a pointer cast.
