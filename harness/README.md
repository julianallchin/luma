# Harness

Reference data and image tools for native UI and renderer tests.

- `goldens/`: numerical contracts and reference images. Backend, scene and
  render tests read them. The renderer's reference images are in
  `gpui/crates/render/goldens/contracts/`.
- `fixture-specs/`: 50 `luma-fixture/0` specs copied from the fixtures
  library, chosen to cover its templates, bodies, mounts, faces and joints
  (and the fixtures Luma ships). `luma-fixture`'s tests build them and check
  `goldens/fixture-builds.json`; see `backend/crates/luma-fixture/README.md`.
- `gauntlet-chat/`: the chat style spec, its comet reference plates, and the
  app's captures beside them (`chat_captures.test.js` rewrites them).
- `gauntlet-te/`: the track editor interaction contract.
- `agent-gauntlet/`: the venue and show MCP gauntlet. See its README.
- `showcase/`: the staged library behind the website's screenshots: two
  made-up fixture definitions, cover art and one song, all made by
  `scripts/screenshots/make_assets.py` and read by
  `scripts/screenshots/showcase.test.js` through the fixture's `assets`.
- `shots/`: saved captures. Ignored by git.
