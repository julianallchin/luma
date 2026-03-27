-- Sync engine: pending operations queue + pull state tracking

CREATE TABLE pending_ops (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    op_type TEXT NOT NULL CHECK(op_type IN ('upsert', 'delete')),
    table_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    payload_json TEXT,
    conflict_key TEXT NOT NULL DEFAULT 'id',
    tier INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    next_retry_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_pending_ops_next_retry ON pending_ops(next_retry_at);
CREATE INDEX idx_pending_ops_table_record ON pending_ops(table_name, record_id);
CREATE UNIQUE INDEX idx_pending_ops_dedup ON pending_ops(table_name, record_id, op_type);

CREATE TABLE sync_state (
    table_name TEXT PRIMARY KEY,
    last_pulled_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z'
);
