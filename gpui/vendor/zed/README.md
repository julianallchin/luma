# Vendored GPUI snapshot

This directory contains the 23-package GPUI dependency closure used by Luma.
It is a `cargo vendor`-normalized snapshot of Zed base `32a0e813`, with the
following local commits applied:

- `c4e9293096`: backdrop blur, horizontal edge fade, and macOS glass fixes
- `2b23084c75`: cached Gaussian kernels for filtered dialog content

Beyond those commits, the snapshot carries **local edits made in place**. A
refresh that does not re-apply them silently reintroduces the bug they fix, or
deletes the feature they are.

`gpui/vendor/` has exactly one recorded commit, so the inventory is not a list
anyone maintains — it is a diff:

```sh
git diff -- gpui/vendor/     # must be empty after a refresh + re-apply
```

Trust that, not the `LUMA LOCAL EDIT` markers in the source. The markers are
navigation, and they can only ever find edits someone remembered to mark; the
diff is complete by construction. What the diff currently holds:

- **Zero-copy presentation of a foreign renderer's frames** — the reason most of
  this exists. One `Window::paint_surface(bounds, SurfaceHandle)` on every
  platform (`gpui/src/scene.rs` defines `SurfaceHandle` and `PaintSurface.source`,
  `gpui/src/window.rs` the method), then per compositor:
  - Metal: `gpui_apple/src/metal_renderer.rs` (`bgra_surfaces_pipeline_state`
    and the BGRA branch in `draw_surfaces`, `SurfaceInputIndex::BgraTexture`),
    `gpui_apple/src/shaders.metal` (`bgra_surface_fragment`), and
    `gpui/src/gpui.rs` (the `core_video` re-export, so callers need not pin the
    crate version themselves).
  - wgpu (Linux): the renderer draws on the compositor's own device, so the
    surface is a plain texture. `gpui/src/gpui.rs` (`WgpuDevice`, the `wgpu`
    re-export), `gpui/src/platform.rs` (`PlatformWindow::wgpu_device`),
    `gpui_wgpu/src/wgpu_context.rs` (`shared_device`; the device also asks for
    `TIMESTAMP_QUERY` and WebGPU-default limits so a renderer can live on it),
    `gpui_wgpu/src/wgpu_renderer.rs` (`wgpu_device`, and `PrimitiveBatch::Surfaces`
    actually drawn: upstream's stub and its never-wired YCbCr layout are gone,
    surfaces are an instance array plus a per-draw texture like polychrome
    sprites), the three `gpui_wgpu/src/shaders*.wgsl` (`Surface`, `load_surface`,
    the single-texture `fs_surface`), and `gpui_linux/src/linux/{x11,wayland}/window.rs`
    (the trait method).
  - `gpui_wgpu` is on **wgpu 30** (upstream: 29) so the compositor and
    `luma_render` share one `wgpu::Device` type; the two API deltas are
    `SurfaceConfiguration::color_space` and `Queue::present`.
- **X11 Mailbox presentation**, matching Wayland's preference in
  `gpui_linux/src/linux/x11/window.rs`. The X11 refresh timer paces frames;
  FIFO backpressure can block that timer past its next deadline and halve the
  delivered rate. The existing surface-capability check falls back to FIFO
  when Mailbox is unavailable.
- **In-place atlas tile refresh**, so a live viewport can publish under one
  stable `ImageId` instead of churning a full-screen texture every frame. Three
  parts, and all three are load-bearing: `gpui/src/platform.rs` (the trait
  method, whose default drops the tile so unpatched backends stay correct),
  `gpui_apple/src/metal_atlas.rs` (the Metal backend), and `gpui/src/window.rs`
  (`Window::update_image`, the public entry point the app actually calls).
  Re-applying the first two without the third leaves them dead code.
- **`set_maximum_drawable_count(2)`** in `gpui_apple/src/metal_renderer.rs`, down
  from upstream's 3. It is coupled to `luma_render::viewport::RESERVED` — see
  the comment at both sites.
- **Within-window backdrop blur on wgpu** — `gpui_wgpu/src/backdrop.rs` and
  `backdrop.wgsl`, integrated at scene draw-order boundaries in `wgpu_renderer.rs`.
  Frames with glass use a sampleable color target; each visible backdrop is
  padded, downsampled, Gaussian-blurred in two passes, and replaced inside its
  rounded clip before drawing the foreground. Scratch targets grow in 64-pixel
  increments and are released after 60 frames without glass. Normal frames
  still draw directly to the surface. Device recovery drops all blur resources.
  Backdrop operations carry element opacity, and both Metal and wgpu blend
  between sharp and blurred pixels at a constant Gaussian radius. Dialog
  dismissal fades and scales the painted subtree with the shared spring;
  `Window::paint_scaled_layer` / `Scene::append_scaled` transform its primitives
  and clip bounds together without adding an offscreen content pass.
  Linux enables the existing `luma_ui::dialog::frosted` primitive and floating
  tint tokens; desktop-window translucency remains a separate macOS feature.
  Run the exact compositor's GPU pixel tests from the workspace with
  `cargo +1.97.1 test --manifest-path gpui/Cargo.toml -p luma-ui --test backdrop`.
- **Color emoji font selection** in `gpui_wgpu/src/cosmic_text_system.rs`.
  Color emoji faces are valid without a Latin `m` glyph. Explicit emoji
  presentation selectors choose the emoji face for the whole grapheme,
  including characters also covered by the primary text font.
- **Resolved anchor offsets** in `gpui/src/elements/anchored.rs`. Popover
  motion receives the final anchor after collision flips, so its translation
  follows the displayed side without affecting the placement calculation.
- **`filter_profile`** in `gpui_apple/src/metal_renderer.rs`.

The workspace keeps its upstream Git dependency declarations so
`gpui-component` and Luma resolve one GPUI type identity; `[patch]` redirects
the three public roots into this closed path graph. To refresh the snapshot,
check out the recorded fork revision, vendor the GPUI package closure into a
scratch directory, preserve the upstream `crates/*` and `tooling/perf`
hierarchy, then replace this directory and verify with:

```sh
CARGO_NET_OFFLINE=true cargo metadata --locked
CARGO_NET_OFFLINE=true cargo check -p luma-ui --locked
```

Upstream license files and Cargo package metadata are preserved alongside the
source. `zlog`, `ztracing`, and `ztracing_macro` declare GPL-3.0-or-later; this
was already part of the remote GPUI dependency graph and must remain in the
application's distribution-license review.
