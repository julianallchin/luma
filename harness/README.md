# Harness

Reference data and image tools for native UI and renderer tests.

- `goldens/`: numerical contracts and reference images. Backend, scene and
  render tests read them. `goldens/scenes-wgpu/` holds renderer reference
  images.
- `gauntlet-chat/`: the chat style spec, its comet reference plates, and the
  app's captures beside them (`chat_captures.test.js` rewrites them).
- `gauntlet-te/`: the track editor interaction contract.
- `agent-gauntlet/`: the venue and show MCP gauntlet. See its README.
- `showcase/`: the staged library behind the website's screenshots: two
  made-up fixture definitions, cover art and one song, all made by
  `scripts/screenshots/make_assets.py` and read by
  `scripts/screenshots/showcase.test.js` through the fixture's `assets`.
- `shots/`: saved captures. Ignored by git.

Render the renderer goldens:

```sh
cargo run --manifest-path gpui/Cargo.toml -p luma-render --release --bin render-goldens -- single-mover
```

Compare two capture directories:

```sh
cd harness && bun install
bun compare-shots.mjs goldens/scenes-wgpu DIR_B
```
