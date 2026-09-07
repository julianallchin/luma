-- MIDI controller: cues, modifiers, bindings
-- Cues and bindings are venue-scoped and synced like scores/annotations.
-- No cascade deletes (local is source of truth during sync).

CREATE TABLE cues (
    id TEXT PRIMARY KEY,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL,
    pattern_id TEXT NOT NULL,
    args_json TEXT NOT NULL DEFAULT '{}',
    z_index INTEGER NOT NULL DEFAULT 1,
    blend_mode TEXT NOT NULL DEFAULT 'Replace',
    default_target_json TEXT NOT NULL DEFAULT '"All"',
    execution_mode_json TEXT NOT NULL DEFAULT '"Loop"',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id),
    FOREIGN KEY (pattern_id) REFERENCES patterns(id)
);

CREATE TABLE midi_modifiers (
    id TEXT PRIMARY KEY,
    venue_id TEXT NOT NULL,
    name TEXT NOT NULL,
    input_json TEXT NOT NULL,
    groups_json TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id)
);

CREATE TABLE midi_bindings (
    id TEXT PRIMARY KEY,
    venue_id TEXT NOT NULL,
    trigger_json TEXT NOT NULL,
    required_modifiers_json TEXT NOT NULL DEFAULT '[]',
    exclusive INTEGER NOT NULL DEFAULT 0,
    mode_json TEXT NOT NULL DEFAULT '"Toggle"',
    action_json TEXT NOT NULL,
    target_override_json TEXT,
    display_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (venue_id) REFERENCES venues(id)
);

CREATE INDEX idx_cues_venue ON cues(venue_id);
CREATE INDEX idx_midi_modifiers_venue ON midi_modifiers(venue_id);
CREATE INDEX idx_midi_bindings_venue ON midi_bindings(venue_id);
