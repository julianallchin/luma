-- Durable idempotency for host-owned authored containers. The schema is
-- intentionally generic so clip creation can join the same mechanism without
-- another request ledger or compatibility layer.
CREATE TABLE authored_state_creations (
    principal_key      TEXT NOT NULL,
    creation_kind      TEXT NOT NULL,
    request_id         TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    subject_id         TEXT NOT NULL,
    auxiliary_id       TEXT,
    commit_id          TEXT NOT NULL,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    PRIMARY KEY (principal_key, creation_kind, request_id),
    UNIQUE (principal_key, creation_kind, subject_id)
);
