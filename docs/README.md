# Docs

How Luma's systems work and why. User-facing docs are the website (`www/`),
kept short on purpose; build and test know-how is in
[gpui/BUILD.md](../gpui/BUILD.md) and each crate's README. A doc says at its top whether it is built; where a doc and the code
disagree, the code is right and the doc needs fixing.

`specs/` hold contracts code is checked against; `design/` holds how a system
works and the decisions behind it.

## Start here

- [design/overview.md](design/overview.md) — the stack, where the data lives, and the repository layout
- [design/design-decisions.md](design/design-decisions.md) — the main architectural choices and the reasons for them
- [glossary.md](glossary.md) — the terms the code and the docs use

## Patterns and playback

- [specs/clip-graphs.md](specs/clip-graphs.md) — every clip is a small typed graph: node kinds, types, checks, lowering
- [specs/composable-patterns.md](specs/composable-patterns.md) — the evaluation and persistence contract under clip graphs
- [design/node-graph-engine.md](design/node-graph-engine.md) — scores, clip graphs, typed signals, validation, preparation and musical time
- [design/compositor.md](design/compositor.md) — scenes, per-frame compositing, blend modes and Live mixing
- [design/selection-system.md](design/selection-system.md) — group references, cells, positions in a clip graph and rig UV
- [design/dmx-pipeline.md](design/dmx-pipeline.md) — lighting state to DMX channel values, strobe, color wheels, Art-Net and USB output
- [specs/aim.md](specs/aim.md) — the room frame, the pan/tilt solver, moving in black, motor lag
- [design/lasers.md](design/lasers.md) — beam shows from laser projectors: settings, the window, figures, the solver, DAC output
- [design/laser-hardware.md](design/laser-hardware.md) — bringing a projector and its DAC up on the bench: what is unverified, and writing a driver
- [design/live-control.md](design/live-control.md) — controls, controllers, MIDI and OSC bindings, Live and its Perform set
- [design/score-video.md](design/score-video.md) — recording a score as video (`luma-cli record`, Export show)

## Venues and the editor

- [design/venue-graph.md](design/venue-graph.md) — a venue as a tree of relations; poses derived by the snap resolver
- [design/venue-workspace.md](design/venue-workspace.md) — Rig's venue page: live scene, fixture table, groups
- [design/venue-authoring-api.md](design/venue-authoring-api.md) — the agent's Python API for building rigs
- [specs/venue-builder-gauntlet.md](specs/venue-builder-gauntlet.md) — the acceptance contract for the venue graph's rig building
- [design/editor-core.md](design/editor-core.md) — the stage builder's editor kernel: selection, input, tools
- [design/fixture-library.md](design/fixture-library.md) — the Luma fixture library: `lib:` fixtures, how they load, draw and drive, and what is still lowered
- [design/fixture-system.md](design/fixture-system.md) — QLC+ fixture definitions, channel types and head layout

## Rendering

- [specs/wgpu-renderer.md](specs/wgpu-renderer.md) — the renderer's architecture
- [design/stage-rendering-quality.md](design/stage-rendering-quality.md) — quality targets and how they are met
- [design/haze-lit-interval-cache.md](design/haze-lit-interval-cache.md) — the haze path
- [design/clouds-prior-art.md](design/clouds-prior-art.md) — research behind the sky-cloud layer
- [design/presentation-seam.md](design/presentation-seam.md) — handing a rendered stage to gpui
- [design/visualizer-fullscreen.md](design/visualizer-fullscreen.md) — fullscreen stage

## App shell

- [specs/comet-shell.md](specs/comet-shell.md) — the shell's contract: modes, per-venue state, the song list, chrome, surfaces
- [specs/agent-chat-gpui.md](specs/agent-chat-gpui.md) — the agent chat panel

## Agent

- [design/agent-code-execution.md](design/agent-code-execution.md) — Python workspaces, the sandbox, score authoring
- [design/subagents.md](design/subagents.md) — subagents
- [design/skills.md](design/skills.md) — skills
- [design/prompt-caching.md](design/prompt-caching.md) — prompt caching
- [specs/ipc-manifest.md](specs/ipc-manifest.md) — every dispatch command, generated from the command table

## Data, sync and analysis

- [design/sync.md](design/sync.md) — PowerSync row sync, uploads, media transfer
- [design/data-layer.md](design/data-layer.md) — source and derived tables, soft deletes, ids, the history log
- [design/database.md](design/database.md) — the local SQLite database, migrations, row sync, deletes, history and media
- [design/genre-model.md](design/genre-model.md) — the genre model: how its weights arrive
- [backend/src/preprocessing/README.md](../backend/src/preprocessing/README.md) — the track analysis DAG
- [design/import-pipeline.md](design/import-pipeline.md) — track import, Python workers, and the StageLinQ and Pro DJ Link integrations
