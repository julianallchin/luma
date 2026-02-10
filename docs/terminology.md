# Luma Terminology

Canonical terms used throughout the codebase and documentation.

## Core Concepts

### Venue
A project container representing a physical space. Each venue is a `.luma` SQLite file containing fixture patches, groups, tags, and annotations. Venues are disposable and venue-specific — they translate abstract patterns into concrete hardware instructions.

- **Database:** `venues` table (in `luma.db` for metadata) + `.luma` project file for fixture data
- **Contains:** Patched fixtures, fixture groups, tags, annotations, venue-specific implementation overrides

### Patched Fixture
A fixture instance assigned to a venue at a specific DMX universe and address, with 3D position and rotation.

- **Database:** `fixtures` table (`venue_id` FK)
- **Fields:** universe, address, num_channels, manufacturer, model, mode_name, pos_x/y/z, rot_x/y/z

### Head
An independently-controllable segment of a multi-head fixture. A pixel bar with 12 pixels has 12 heads. A simple par can has 1 head. Head positions are computed from the fixture definition's physical dimensions and the fixture's 3D placement.

- **Addressing:** `fixture_id:head_index` (e.g., `"abc123:0"`, `"abc123:1"`)

### DMX Universe
A group of 512 DMX channels (addresses 1-512). Standard DMX-512 protocol term. A venue can span multiple universes.

- **Database:** `fixtures.universe` column
- **Not to be confused with:** `UniverseState` (runtime fixture output state, not a DMX universe)

### Group
A logical collection of fixtures organized by role and spatial position, not by DMX address. Groups are the bridge between abstract patterns and physical hardware — they enable venue portability.

- **Database:** `fixture_groups` table + `fixture_group_members` junction table
- **Spatial Axes:** axis_lr (left/right), axis_fb (front/back), axis_ab (above/below) — each -1.0 to +1.0

### Tag
A label attached to a group that patterns use for fixture selection. Two categories:

- **Auto-generated spatial tags:** `left`, `right`, `center`, `front`, `back`, `high`, `low`, `circular` (computed from fixture positions)
- **Capability tags:** `has_color`, `has_movement`, `has_strobe` (detected from fixture definitions)
- **User-defined tags:** Custom labels like `blinder`, `wash`, `spot`
- **Database:** `fixture_tags` table + `fixture_tag_assignments` junction, plus JSON `tags` array on `fixture_groups`

### Tag Expression
A boolean query over tags used by `select` nodes. Operators: `&` (AND), `|` (OR), `^` (XOR), `~` (NOT), `>` (fallback), parentheses. Example: `front & has_color`, `circular & ~blinder`, `has_movement > has_color`.

### Pattern
A reusable, venue-agnostic light behavior defined as a visual node graph. Stored in the global library (`luma.db`).

- **Database:** `patterns` table
- **Contains:** One or more implementations (graph versions)

### Implementation
A specific node graph version of a pattern. The `graph_json` field contains the serialized `Graph` (nodes, edges, pattern arguments).

- **Database:** `implementations` table (`pattern_id` FK)
- **Key field:** `graph_json` — JSON blob containing the full graph definition

### Graph
The node-and-edge data flow graph within an implementation. Nodes are processing units, edges connect output ports to input ports, and pattern arguments expose configurable parameters.

- **Structs:** `Graph`, `NodeInstance`, `Edge`, `PatternArgDef`
- **Execution:** Topological sort via petgraph, sequential node evaluation

### Signal
A 3D tensor — the fundamental data type flowing through pattern graphs. Three dimensions:

- **N (spatial):** Number of fixtures (or 1 for uniform)
- **T (temporal):** Time samples across the pattern duration
- **C (channel):** Data channels per sample (1=dimmer, 3=RGB, 4=RGBA, 2=pan/tilt, 12=chroma)
- **Memory layout:** Flat `Vec<f32>`, indexed as `data[n * (t * c) + t_idx * c + c_idx]`
- **Broadcasting:** Dimensions of size 1 expand to match larger operands (like NumPy)

### Annotation (Score)
A pattern placement on a track's timeline. Backend uses "score" (database tables: `scores`, `track_scores`), frontend uses "annotation." Same concept.

- **Fields:** start_time, end_time, z_index (stacking order), blend_mode, args (pattern argument values)
- **Database:** `scores` table (parent) + `track_scores` table (instances with timing)

### Blend Mode
How overlapping annotations combine during compositing. Applied per-layer in z-index order (bottom to top):

- **Replace** — top overwrites bottom
- **Add** — sum, clamped to 1.0
- **Multiply** — product (darkening)
- **Screen** — inverse multiply (lightening)
- **Max/Lighten** — take brightest
- **Min** — take dimmest
- **Value** — top brightness controls mix amount

### Track
An audio file with metadata and analysis data.

- **Database:** `tracks` table + `track_beats` (beat grid, BPM), `track_waveforms` (visual data), `track_stems` (separated audio), `track_roots` (chord analysis)

### PrimitiveState
Runtime output values for a single fixture/head at a single moment. Ephemeral, in-memory only.

- **Fields:** dimmer (0-1), color [R,G,B] (0-1), strobe (0-1), position [pan,tilt] (degrees), speed (0/1)

### UniverseState
A `HashMap<primitive_id, PrimitiveState>` — all fixture states at a single point in time. Despite the name, represents fixture output state, not a DMX universe.

## Data Flow

```
Global Library (luma.db)              Venue Project (.luma)
├── Patterns                          ├── Patched Fixtures
│   └── Implementations (graphs)      │   ├── DMX Universe + Address
├── Tracks                            │   └── 3D Position + Rotation
│   ├── Beats (BPM, beat grid)        ├── Groups + Tags
│   ├── Stems (drums/bass/vocals)     └── Annotations (Scores)
│   ├── Roots (chord analysis)            ├── Pattern reference
│   └── Waveforms                         ├── Time range + z-index
└── Categories                            ├── Blend mode
                                          └── Argument values
```

## File Locations

| Concept | Database | Models | Services | Commands | Frontend |
|---------|----------|--------|----------|----------|----------|
| Venue | `database/local/venues.rs` | `models/venues.rs` | — | `commands/venues.rs` | `features/venues/` |
| Fixture | `database/local/fixtures.rs` | `models/fixtures.rs` | `services/fixtures.rs` | `commands/fixtures.rs` | `features/universe/` |
| Group | `database/local/groups.rs` | `models/groups.rs` | `services/groups.rs` | `commands/groups.rs` | `features/universe/` |
| Tag | `database/local/tags.rs` | `models/tags.rs` | `services/tags.rs` | `commands/tags.rs` | `features/universe/` |
| Pattern | `database/local/patterns.rs` | `models/patterns.rs` | `services/community_patterns.rs` | `commands/patterns.rs` | `features/patterns/` |
| Annotation | `database/local/scores.rs` | `models/scores.rs` | — | `commands/scores.rs` | `features/track-editor/` |
| Track | `database/local/tracks.rs` | `models/tracks.rs` | `services/tracks.rs` | `commands/tracks.rs` | `features/tracks/` |
| Node Graph | — | `models/node_graph.rs` | — | `commands/node_graph.rs` | `features/patterns/` |
| Compositor | — | — | `compositor.rs` | — | — |
| DMX Engine | — | — | `fixtures/engine.rs` | — | — |

## See Also

- [User Guide](user-guide.md) — How to use Luma
- [Developer Guide](developer-guide.md) — Architecture and internals
- [Node Reference](node-reference.md) — All pattern graph node types
