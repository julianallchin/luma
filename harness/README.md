# Harness

Reference data, fonts and image tools for native UI and renderer tests.

- `fonts/`: Inter fonts. The native app embeds them, so they must stay here.
- `goldens/`: numerical contracts and reference images. Backend, scene and
  render tests read them. `goldens/scenes-wgpu/` holds renderer reference
  images.
- `gauntlet-chat/`: the chat style spec, its comet reference plates, and the
  app's captures beside them (`chat_captures.test.js` rewrites them).
- `gauntlet-te/`: the track editor interaction contract.
- `agent-gauntlet/`: the venue and show MCP gauntlet. See its README.
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
