CREATE TABLE IF NOT EXISTS implementations (
    pattern_id INTEGER PRIMARY KEY,
    graph_json TEXT NOT NULL DEFAULT '{"nodes":[],"edges":[]}',
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER IF NOT EXISTS implementations_updated_at
AFTER UPDATE ON implementations
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE implementations
    SET updated_at = CURRENT_TIMESTAMP
    WHERE pattern_id = OLD.pattern_id;
END;
