-- Add sync columns (uid, version, synced_at) to cues, midi_modifiers, midi_bindings
-- so they participate in the Supabase pull/push sync engine.
-- SQLite doesn't support ADD COLUMN with expressions well for existing data,
-- so we recreate each table.

-- ── cues ─────────────────────────────────────────────────────────────────────

CREATE TABLE cues_new (
    id                  TEXT PRIMARY KEY,
    uid                 TEXT,
    venue_id            TEXT NOT NULL,
    name                TEXT NOT NULL,
    pattern_id          TEXT NOT NULL,
    args_json           TEXT NOT NULL DEFAULT '{}',
    z_index             INTEGER NOT NULL DEFAULT 1,
    blend_mode          TEXT NOT NULL DEFAULT 'Replace',
    default_target_json TEXT NOT NULL DEFAULT '"All"',
    execution_mode_json TEXT NOT NULL DEFAULT '"Loop"',
    display_order       INTEGER NOT NULL DEFAULT 0,
    display_x           INTEGER NOT NULL DEFAULT 0,
    display_y           INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version             INTEGER NOT NULL DEFAULT 1,
    synced_at           TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id),
    FOREIGN KEY (pattern_id) REFERENCES patterns(id)
);

INSERT INTO cues_new (id, venue_id, name, pattern_id, args_json, z_index, blend_mode,
                      default_target_json, execution_mode_json, display_order, display_x, display_y,
                      created_at, updated_at)
SELECT id, venue_id, name, pattern_id, args_json, z_index, blend_mode,
       default_target_json, execution_mode_json, display_order, display_x, display_y,
       created_at, updated_at
FROM cues;

DROP TABLE cues;
ALTER TABLE cues_new RENAME TO cues;

CREATE INDEX idx_cues_venue ON cues(venue_id);

CREATE TRIGGER cues_updated_at AFTER UPDATE ON cues FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE cues SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

-- ── midi_modifiers ────────────────────────────────────────────────────────────

CREATE TABLE midi_modifiers_new (
    id          TEXT PRIMARY KEY,
    uid         TEXT,
    venue_id    TEXT NOT NULL,
    name        TEXT NOT NULL,
    input_json  TEXT NOT NULL,
    groups_json TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version     INTEGER NOT NULL DEFAULT 1,
    synced_at   TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id)
);

INSERT INTO midi_modifiers_new (id, venue_id, name, input_json, groups_json, created_at, updated_at)
SELECT id, venue_id, name, input_json, groups_json, created_at, updated_at
FROM midi_modifiers;

DROP TABLE midi_modifiers;
ALTER TABLE midi_modifiers_new RENAME TO midi_modifiers;

CREATE INDEX idx_midi_modifiers_venue ON midi_modifiers(venue_id);

CREATE TRIGGER midi_modifiers_updated_at AFTER UPDATE ON midi_modifiers FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE midi_modifiers SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

-- ── midi_bindings ─────────────────────────────────────────────────────────────

CREATE TABLE midi_bindings_new (
    id                      TEXT PRIMARY KEY,
    uid                     TEXT,
    venue_id                TEXT NOT NULL,
    trigger_json            TEXT NOT NULL,
    required_modifiers_json TEXT NOT NULL DEFAULT '[]',
    exclusive               INTEGER NOT NULL DEFAULT 0,
    mode_json               TEXT NOT NULL DEFAULT '"Toggle"',
    action_json             TEXT NOT NULL,
    target_override_json    TEXT,
    display_order           INTEGER NOT NULL DEFAULT 0,
    created_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    version                 INTEGER NOT NULL DEFAULT 1,
    synced_at               TEXT,
    FOREIGN KEY (venue_id) REFERENCES venues(id)
);

INSERT INTO midi_bindings_new (id, venue_id, trigger_json, required_modifiers_json, exclusive,
                               mode_json, action_json, target_override_json, display_order,
                               created_at, updated_at)
SELECT id, venue_id, trigger_json, required_modifiers_json, exclusive,
       mode_json, action_json, target_override_json, display_order,
       created_at, updated_at
FROM midi_bindings;

DROP TABLE midi_bindings;
ALTER TABLE midi_bindings_new RENAME TO midi_bindings;

CREATE INDEX idx_midi_bindings_venue ON midi_bindings(venue_id);

CREATE TRIGGER midi_bindings_updated_at AFTER UPDATE ON midi_bindings FOR EACH ROW
    WHEN OLD.version = NEW.version
    BEGIN UPDATE midi_bindings SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), version = OLD.version + 1 WHERE id = OLD.id; END;

-- ── delete sync triggers ──────────────────────────────────────────────────────

-- tier 1
CREATE TRIGGER sync_delete_midi_modifiers AFTER DELETE ON midi_modifiers FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'midi_modifiers', OLD.id, 1, CURRENT_TIMESTAMP);
END;

-- tier 2
CREATE TRIGGER sync_delete_cues AFTER DELETE ON cues FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'cues', OLD.id, 2, CURRENT_TIMESTAMP);
END;

CREATE TRIGGER sync_delete_midi_bindings AFTER DELETE ON midi_bindings FOR EACH ROW
BEGIN
    INSERT OR REPLACE INTO pending_ops (op_type, table_name, record_id, tier, next_retry_at)
    VALUES ('delete', 'midi_bindings', OLD.id, 2, CURRENT_TIMESTAMP);
END;
