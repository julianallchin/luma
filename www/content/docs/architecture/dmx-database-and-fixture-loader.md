---
title: Dmx Database And Fixture Loader
---

# DMX database and QXF loader plan

Rough plan for ingesting QLC+ `.qxf` fixture definitions into in-memory Rust types and wiring them to DMX patching, without persisting the XML data in the database.

## Goals
- Parse `.qxf` files in `resources/fixtures/**` into strongly typed Rust structs.
- Keep fixture definitions in memory; database only stores venue-specific patch data (universe, start address, selected mode, fixture instance name).
- Expose enough structure for UI: channel labels/presets, capabilities with DMX ranges, heads grouping, physical layout metadata.

## Data sources and scope
- Fixture definitions: XML in `resources/fixtures/*/*.qxf`.
- We do **not** persist fixture XML to SQLite; the DB remains for patch records only.
- Later: optional caching of parsed fixtures on disk if needed for startup speed; not a blocker now.

## Parser approach
- Dependency: `quick-xml` with serde features. Use `quick_xml::de::from_reader` after stripping DOCTYPE if needed.
- Structs (PascalCase mapping, `#[serde(default)]` on Vec/Option):
  - `FixtureDefinition { creator, manufacturer, model, fixture_type, channel: Vec<Channel>, mode: Vec<Mode>, physical: Option<Physical> }`
  - `Channel { name, preset, group: Option<Group>, capabilities: Vec<Capability> }`
  - `Capability { min, max, preset, res, color, color2, label }`
  - `Mode { name, channels: Vec<ModeChannel>, heads: Vec<Head> }`
  - `Physical` children: `Bulb`, `Dimensions`, `Lens`, `Focus`, `Layout`, `Technical`.
- Loader helper: `load_fixture(path: &Path) -> Result<FixtureDefinition>`; wrap errors; optionally return `FixtureHandle { id, path, definition }`.
- Consider a thin domain-normalizer to:
  - Expand color hex strings to RGB.
  - Map `Group`/`Preset` to enum variants.
  - Build per-head channel maps for easy lookup (e.g., head 0 → RGB[A]/dimmer/strobe channels).

## DMX patching data model (DB)
- Table idea: `dmx_patch(id PK, fixture_path TEXT, manufacturer TEXT, model TEXT, mode TEXT, universe INTEGER, address INTEGER, label TEXT, created_at, updated_at)`.
- No XML stored; `fixture_path` is the pointer back to the resource file, plus cached manufacturer/model/mode for quick listing.
- On load, resolve `fixture_path` → parse → locate `mode` → validate channel span fits `address..address+mode_len`.

## UI/engine usage
- Present selectable modes from `definition.mode`.
- Use `channel.preset` and `group` to choose control widgets (color picker, pan/tilt joystick, dimmer sliders, gobo dropdown).
- Use `capabilities` ranges to render segmented sliders/dropdowns with labels/icons (`res`) and colors (`color`/`color2`).
- Use `heads` + `layout` to visualize multi-head bars/panels and to map pixel outputs to DMX channels.
- Use `physical` for rig planning (weight, power, connector) and layout drawing.

## Open questions / decisions
- Do we need per-fixture caching on disk, or is parsing on-demand fast enough?
- Should we expose a “capability lookup” API (QLC-style) for engine modules?
- How to handle fine channels (Group Byte=1) and 16-bit joins? Need a pairing strategy.
- Asset handling for `Capability.res` (gobo icons): bundle existing assets or ignore initially?

## Next steps
1) Add `quick-xml` dependency and define parser structs under `src-tauri/src/fixtures/qxf.rs` (or `models/fixtures` if we want TS export later).
2) Build `loader.rs` with a `load_fixture` helper plus XML sanitization (doctype strip).
3) Add a small domain layer to compute head/channel maps and normalize colors.
4) Sketch DB table for patch records; add migrations only after loader works.
5) Wire a sample CLI/dev command to load a fixture and dump a summary for validation.***
