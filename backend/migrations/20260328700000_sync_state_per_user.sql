-- Make sync_state per-user so multiple users on the same machine
-- don't share pull cursors.

-- Recreate with composite PK (uid, table_name).
-- Existing rows are dropped — first sync after migration does a full pull.
DROP TABLE IF EXISTS sync_state;

CREATE TABLE sync_state (
    uid TEXT NOT NULL,
    table_name TEXT NOT NULL,
    last_pulled_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z',
    PRIMARY KEY (uid, table_name)
);
