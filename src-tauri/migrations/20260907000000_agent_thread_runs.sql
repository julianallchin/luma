-- Local receipts for this device's cloud execution claims. Maintained through
-- claim/release RPCs rather than general row sync; credentials stay in state.db.
CREATE TABLE agent_thread_runs (
    owner_user_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    PRIMARY KEY (owner_user_id, thread_id)
);
