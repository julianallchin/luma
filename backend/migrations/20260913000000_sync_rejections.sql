-- Uploads Supabase refused for good: a policy said no, a constraint said no, or
-- the request was malformed. PowerSync's queue is strictly ordered, so an entry
-- the server will never accept has to leave it or every later write waits
-- behind it forever. It lands here instead, where recovery can find it.
--
-- Local only. Never synced.
CREATE TABLE IF NOT EXISTS sync_rejections (
    id          TEXT PRIMARY KEY,
    table_name  TEXT NOT NULL,
    row_id      TEXT NOT NULL,
    op          TEXT NOT NULL,
    data_json   TEXT,
    status      INTEGER NOT NULL,
    body        TEXT,
    at          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sync_rejections_at ON sync_rejections(at DESC);
