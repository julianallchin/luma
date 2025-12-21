# Luma Domain Terminology

This document defines the canonical terminology used throughout the Luma codebase.

## Core Concepts

### Venue
A physical location or project container with patched fixtures. Replaces the old "project" concept where each project had its own SQLite database. Now all venues exist in a unified database that can sync to Supabase.

- **Database:** `venues` table
- **Contains:** Patched fixtures, venue-specific implementation overrides

### Patched Fixture
A fixture instance assigned to a venue at a specific DMX universe and address, with optional 3D position/rotation data.

- **Database:** `fixtures` table (has `venue_id` foreign key)
- **Fields:** universe, address, num_channels, manufacturer, model, mode_name, label, pos_x/y/z, rot_x/y/z

### DMX Universe
A group of 512 DMX channels (addresses 1-512). Standard DMX protocol term. A venue can have multiple universes.

- **Database:** `fixtures.universe` column
- **Not to be confused with:** `UniverseState` (runtime output state)

### Pattern
A reusable light behavior template with a name, description, and optional category.

- **Database:** `patterns` table
- **Contains:** One or more Implementations
- **Has:** `default_implementation_id` pointing to the default graph

### Implementation
A specific graph-based version of a pattern. Patterns can have multiple implementations (e.g., "minimal", "full", "venue-specific").

- **Database:** `implementations` table
- **Contains:** `graph_json` with nodes and edges
- **Linked to:** Pattern via `pattern_id`

### Graph
The node-and-edge visual representation within an implementation. Contains nodes (processing units), edges (connections), and pattern arguments.

- **Database:** Stored as JSON in `implementations.graph_json`
- **Types:** `Graph`, `NodeInstance`, `Edge`, `PortDef`, `PatternArgDef`

### Score
A pattern placed on a track's timeline with start/end time, z-index, blend mode, and arguments.

- **Database:** `scores` table (parent) + `track_scores` table (instances)
- **⚠️ Known issue:** Frontend currently calls these "annotations" - needs unification

### Track
An audio file with metadata and analysis data (beats, waveform, stems, roots).

- **Database:** `tracks` table + related tables (`track_beats`, `track_waveforms`, `track_stems`, `track_roots`)

### Fixture State (UniverseState)
Runtime output values for fixtures at any given moment. Ephemeral, in-memory only.

- **Structs:** `UniverseState`, `PrimitiveState`
- **Fields:** dimmer, color, strobe, position, speed
- **Note:** Named `UniverseState` but represents fixture output state, not a DMX universe

## Data Relationships

```
Venue
  ├── Patched Fixtures (venue_id FK)
  │     └── DMX Universe + Address
  │
  └── Venue Implementation Overrides
        └── Pattern → Implementation selection

Track
  ├── Track Beats (beat grid, BPM)
  ├── Track Waveforms (preview/full samples)
  ├── Track Stems (drums, vocals, bass, etc.)
  └── Scores
        └── Track Scores (pattern placements on timeline)

Pattern
  ├── Category (optional)
  └── Implementations
        └── Graph (nodes, edges, args)
```

## Known Terminology Issues

1. **Score vs Annotation**: Backend uses "score", frontend uses "annotation" for the same concept
2. **UniverseState naming**: Could be confused with DMX Universe; represents fixture output state
3. **Implementation not exposed**: No explicit `Implementation` type in Rust API, only implicit via `graph_json`

## File Locations

| Concept | Database Layer | Models | Commands | Frontend Store |
|---------|---------------|--------|----------|----------------|
| Venue | `database/local/venues.rs` | `models/venues.rs` | `commands/venues.rs` | `features/venues/` |
| Fixture | `database/local/fixtures.rs` | `fixtures/models.rs` | `commands/fixtures.rs` | `features/universe/stores/` |
| Pattern | `database/local/patterns.rs` | `models/patterns.rs` | `commands/patterns.rs` | `features/patterns/stores/` |
| Score | `database/local/scores.rs` | `models/scores.rs` | `commands/scores.rs` | `features/track-editor/stores/` |
| Track | `database/local/tracks.rs` | `models/tracks.rs` | `commands/tracks.rs` | `features/tracks/stores/` |
