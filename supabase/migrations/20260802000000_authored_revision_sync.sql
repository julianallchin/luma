-- Relational authored history, immutable conversation traces, and
-- server-ordered authored-head convergence.
--
-- `sync_seq` is allocated under a transactional row lock. Unlike a PostgreSQL
-- sequence, this cannot commit out of order: a transaction holding N must
-- commit before another transaction can allocate N+1. A client may therefore
-- persist `sync_seq > cursor` without a client clock or a missed late commit.

CREATE SCHEMA IF NOT EXISTS private;
CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA extensions;

CREATE TABLE private.luma_sync_clock (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    value bigint NOT NULL CHECK (value >= 0)
);
INSERT INTO private.luma_sync_clock (singleton, value)
VALUES (true, 0)
ON CONFLICT (singleton) DO NOTHING;

CREATE OR REPLACE FUNCTION private.next_sync_seq()
RETURNS bigint
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, private
AS $$
DECLARE
    allocated bigint;
BEGIN
    UPDATE private.luma_sync_clock
       SET value = value + 1
     WHERE singleton
     RETURNING value INTO allocated;
    IF allocated IS NULL THEN
        RAISE EXCEPTION 'sync clock is not initialized' USING ERRCODE = '55000';
    END IF;
    RETURN allocated;
END
$$;

CREATE OR REPLACE FUNCTION private.current_principal_key()
RETURNS text
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
    SELECT CASE
        WHEN auth.uid() IS NULL THEN NULL
        ELSE 'signed-in:' || auth.uid()::text
    END
$$;

CREATE OR REPLACE FUNCTION private.bump_sync_seq()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, private
AS $$
BEGIN
    NEW.sync_seq := private.next_sync_seq();
    RETURN NEW;
END
$$;

-- Client-visible tables use a constant placeholder default and a BEFORE
-- INSERT trigger. Calling a SECURITY DEFINER allocator from a column default
-- would require granting clients EXECUTE on that helper; trigger execution
-- keeps the clock capability private while still overriding any client value.

-- Install the commit-ordered cursor on the pre-existing relational registry.
-- Deployments may omit individual feature tables, so the block is tolerant.
DO $$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'venues', 'tracks', 'fixtures', 'patterns', 'fixture_groups',
        'midi_modifiers', 'scores', 'track_beats', 'track_roots',
        'track_stems', 'track_drum_onsets', 'track_bar_classifications',
        'fixture_group_members', 'cues', 'midi_bindings'
    ] LOOP
        IF to_regclass('public.' || relation) IS NULL THEN
            CONTINUE;
        END IF;
        EXECUTE format(
            'ALTER TABLE public.%I ADD COLUMN IF NOT EXISTS sync_seq bigint',
            relation
        );
        EXECUTE format(
            'UPDATE public.%I SET sync_seq = private.next_sync_seq() WHERE sync_seq IS NULL',
            relation
        );
        EXECUTE format(
            'ALTER TABLE public.%I ALTER COLUMN sync_seq SET DEFAULT 0, ALTER COLUMN sync_seq SET NOT NULL',
            relation
        );
        EXECUTE format(
            'CREATE UNIQUE INDEX IF NOT EXISTS %I ON public.%I(sync_seq)',
            'idx_' || relation || '_sync_seq', relation
        );
        EXECUTE format('DROP TRIGGER IF EXISTS sync_seq_bump ON public.%I', relation);
        EXECUTE format(
            'CREATE TRIGGER sync_seq_bump BEFORE INSERT OR UPDATE ON public.%I FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq()',
            relation
        );
    END LOOP;
END
$$;

CREATE TABLE public.authored_documents (
    document_id text PRIMARY KEY,
    document_kind text NOT NULL CHECK (document_kind IN ('track_score', 'pattern_graph')),
    principal_key text NOT NULL,
    subject_id text NOT NULL,
    track_id text,
    venue_id text,
    score_id text,
    implementation_id text,
    archived_at text,
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    UNIQUE (principal_key, document_id),
    CHECK (principal_key = private.current_principal_key()),
    CHECK (
        octet_length(principal_key) BETWEEN 1 AND 4096
        AND octet_length(subject_id) BETWEEN 1 AND 4096
        AND (track_id IS NULL OR octet_length(track_id) BETWEEN 1 AND 4096)
        AND (venue_id IS NULL OR octet_length(venue_id) BETWEEN 1 AND 4096)
        AND (score_id IS NULL OR octet_length(score_id) BETWEEN 1 AND 4096)
        AND (implementation_id IS NULL OR octet_length(implementation_id) BETWEEN 1 AND 4096)
    ),
    CHECK (
        (document_kind = 'track_score' AND subject_id = track_id
         AND track_id IS NOT NULL AND venue_id IS NOT NULL
         AND score_id IS NOT NULL AND implementation_id IS NULL)
        OR
        (document_kind = 'pattern_graph' AND track_id IS NULL AND venue_id IS NULL
         AND score_id IS NULL AND implementation_id IS NOT NULL)
    )
);
CREATE UNIQUE INDEX authored_documents_track_scope
    ON public.authored_documents(principal_key, track_id, venue_id, score_id)
    WHERE document_kind = 'track_score';
CREATE UNIQUE INDEX authored_documents_pattern_scope
    ON public.authored_documents(principal_key, subject_id, implementation_id)
    WHERE document_kind = 'pattern_graph';
CREATE UNIQUE INDEX authored_documents_sync_seq ON public.authored_documents(sync_seq);

CREATE TABLE public.authored_revisions (
    revision_id text PRIMARY KEY,
    document_id text NOT NULL,
    principal_key text NOT NULL,
    parent_count smallint NOT NULL CHECK (parent_count BETWEEN 0 AND 2),
    content_hash text NOT NULL CHECK (content_hash ~ '^sha256:[0-9a-f]{64}$'),
    operation_kind text NOT NULL,
    operation_id text,
    message text NOT NULL,
    author_name text NOT NULL,
    author_email text NOT NULL,
    authored_at text NOT NULL,
    thread_id text,
    assistant_message_id text,
    restored_revision_id text,
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    FOREIGN KEY (principal_key, document_id)
        REFERENCES public.authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, restored_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    CHECK (principal_key = private.current_principal_key()),
    CHECK (assistant_message_id IS NULL OR thread_id IS NOT NULL),
    UNIQUE (document_id, revision_id),
    UNIQUE (principal_key, revision_id),
    UNIQUE (principal_key, document_id, revision_id)
);
CREATE UNIQUE INDEX authored_revisions_operation
    ON public.authored_revisions(document_id, operation_kind, operation_id)
    WHERE operation_id IS NOT NULL;
CREATE UNIQUE INDEX authored_revisions_assistant_message
    ON public.authored_revisions(assistant_message_id)
    WHERE assistant_message_id IS NOT NULL;
CREATE UNIQUE INDEX authored_revisions_sync_seq ON public.authored_revisions(sync_seq);

CREATE TABLE public.authored_revision_files (
    revision_id text NOT NULL,
    principal_key text NOT NULL,
    path text NOT NULL,
    content_hash text NOT NULL CHECK (content_hash ~ '^sha256:[0-9a-f]{64}$'),
    content bytea NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (revision_id, path),
    FOREIGN KEY (principal_key, revision_id)
        REFERENCES public.authored_revisions(principal_key, revision_id),
    CHECK (principal_key = private.current_principal_key())
);
CREATE UNIQUE INDEX authored_revision_files_sync_seq
    ON public.authored_revision_files(sync_seq);

CREATE TABLE public.authored_revision_parents (
    principal_key text NOT NULL,
    document_id text NOT NULL,
    revision_id text NOT NULL,
    parent_order smallint NOT NULL CHECK (parent_order IN (0, 1)),
    parent_revision_id text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    CHECK (principal_key = private.current_principal_key()),
    CHECK (revision_id <> parent_revision_id),
    PRIMARY KEY (revision_id, parent_order),
    UNIQUE (revision_id, parent_revision_id),
    FOREIGN KEY (principal_key, document_id, revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, parent_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id)
);
CREATE INDEX authored_revision_parents_parent
    ON public.authored_revision_parents(principal_key, document_id, parent_revision_id);
CREATE UNIQUE INDEX authored_revision_parents_sync_seq
    ON public.authored_revision_parents(sync_seq);

-- No client INSERT/UPDATE policy exists for heads. Only integration RPCs move
-- this projection; clients pull it and apply the referenced revision.
CREATE TABLE public.authored_document_heads (
    document_id text PRIMARY KEY,
    principal_key text NOT NULL,
    revision_id text NOT NULL,
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    updated_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    FOREIGN KEY (principal_key, document_id)
        REFERENCES public.authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id)
);
CREATE UNIQUE INDEX authored_document_heads_sync_seq
    ON public.authored_document_heads(sync_seq);
CREATE TRIGGER authored_document_heads_sync_seq
    BEFORE INSERT OR UPDATE ON public.authored_document_heads
    FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq();

CREATE TABLE public.authored_operation_outcomes (
    principal_key text NOT NULL,
    document_id text NOT NULL,
    operation_kind text NOT NULL CHECK (
        operation_kind IN (
            'create_score', 'create_pattern', 'score_edit', 'graph_edit',
            'restore', 'workspace_commit', 'workspace_merge', 'pattern_fork'
        )
    ),
    operation_id text NOT NULL,
    request_fingerprint text NOT NULL,
    base_revision_id text,
    status text NOT NULL CHECK (status IN ('committed', 'conflicted')),
    result_revision_id text,
    conflicts_json text,
    result_json text,
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (document_id, operation_kind, operation_id),
    FOREIGN KEY (principal_key, document_id)
        REFERENCES public.authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, base_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, result_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    CHECK (principal_key = private.current_principal_key()),
    CHECK (
        (status = 'committed' AND result_revision_id IS NOT NULL AND conflicts_json IS NULL)
        OR (status = 'conflicted' AND result_revision_id IS NULL AND conflicts_json IS NOT NULL)
    )
);
CREATE UNIQUE INDEX authored_operation_outcomes_sync_seq
    ON public.authored_operation_outcomes(sync_seq);

CREATE TABLE public.authored_head_proposals (
    proposal_id text PRIMARY KEY,
    principal_key text NOT NULL,
    document_id text NOT NULL,
    device_id text NOT NULL,
    operation_id text NOT NULL,
    base_revision_id text,
    proposed_revision_id text NOT NULL,
    server_proposal_seq bigint NOT NULL UNIQUE,
    created_at text NOT NULL,
    sync_seq bigint NOT NULL UNIQUE,
    FOREIGN KEY (principal_key, document_id)
        REFERENCES public.authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, base_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, proposed_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    UNIQUE (principal_key, document_id, device_id, operation_id)
);
CREATE INDEX authored_head_proposals_order
    ON public.authored_head_proposals(principal_key, document_id, server_proposal_seq);

CREATE TABLE public.authored_head_integrations (
    proposal_id text PRIMARY KEY,
    principal_key text NOT NULL,
    document_id text NOT NULL,
    prior_revision_id text,
    result_revision_id text,
    resolution_kind text NOT NULL CHECK (resolution_kind IN (
        'fast_forward', 'already_ancestor', 'structural', 'whole_proposal',
        'quarantined_noop', 'cancelled_archived'
    )),
    server_integration_seq bigint NOT NULL UNIQUE,
    integrated_at text NOT NULL,
    sync_seq bigint NOT NULL UNIQUE,
    FOREIGN KEY (proposal_id) REFERENCES public.authored_head_proposals(proposal_id),
    FOREIGN KEY (principal_key, document_id, prior_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, result_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    CHECK (
        (resolution_kind = 'cancelled_archived'
            AND prior_revision_id IS NULL AND result_revision_id IS NULL)
        OR (resolution_kind = 'quarantined_noop'
            AND prior_revision_id IS NOT DISTINCT FROM result_revision_id)
        -- An initial server head may adopt a parented offline history tip via
        -- whole-proposal fallback, so both of these terminal resolutions can
        -- validly have no prior server revision.
        OR (resolution_kind IN ('fast_forward', 'whole_proposal')
            AND result_revision_id IS NOT NULL)
        OR (resolution_kind NOT IN (
                'cancelled_archived', 'quarantined_noop',
                'fast_forward', 'whole_proposal'
            )
            AND prior_revision_id IS NOT NULL AND result_revision_id IS NOT NULL)
    )
);

-- Archive requests are append-only traces. Several devices may race to
-- archive one document; the first transition wins, while every request still
-- receives its own immutable receipt.
CREATE TABLE public.authored_document_archives (
    archive_id text PRIMARY KEY,
    principal_key text NOT NULL,
    document_id text NOT NULL,
    device_id text NOT NULL,
    operation_id text NOT NULL,
    -- NULL records that the requesting device had no local revision. It does
    -- not assert that the server is headless; final_revision_id captures the
    -- locked server head and may therefore be non-NULL.
    requested_revision_id text,
    final_revision_id text,
    server_archive_seq bigint NOT NULL UNIQUE,
    archived_at text NOT NULL,
    sync_seq bigint NOT NULL UNIQUE,
    FOREIGN KEY (principal_key, document_id)
        REFERENCES public.authored_documents(principal_key, document_id),
    FOREIGN KEY (principal_key, document_id, requested_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, final_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    UNIQUE (principal_key, document_id, device_id, operation_id)
);

-- Conversation backup. A thread row is terminal once deleting. Transcript
-- messages and append receipts remain after deletion, so none of those traces
-- has a foreign key to the lifecycle row.
CREATE TABLE public.agent_threads (
    id text PRIMARY KEY,
    owner_user_id text NOT NULL CHECK (owner_user_id = auth.uid()::text),
    agent_kind text NOT NULL,
    subject_kind text,
    subject_id text,
    implementation_id text,
    venue_id text,
    score_id text,
    title text,
    lifecycle_state text NOT NULL CHECK (lifecycle_state IN ('active', 'deleting')),
    forked_from_thread_id text,
    forked_at_message_id text,
    created_at text NOT NULL,
    updated_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    UNIQUE (owner_user_id, id),
    CHECK (
        (
            agent_kind = 'track_copilot'
            AND subject_kind = 'track'
            AND coalesce(subject_id, '') <> ''
            AND coalesce(venue_id, '') <> ''
            AND coalesce(score_id, '') <> ''
            AND implementation_id IS NULL
        )
        OR
        (
            agent_kind = 'pattern_graph'
            AND subject_kind = 'pattern'
            AND coalesce(subject_id, '') <> ''
            AND coalesce(implementation_id, '') <> ''
            AND score_id IS NULL
            AND (venue_id IS NULL OR venue_id <> '')
        )
    )
);
CREATE UNIQUE INDEX agent_threads_sync_seq ON public.agent_threads(sync_seq);
CREATE TRIGGER agent_threads_sync_seq
    BEFORE INSERT OR UPDATE ON public.agent_threads
    FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq();

CREATE TABLE public.agent_thread_messages (
    id text PRIMARY KEY,
    owner_user_id text NOT NULL,
    principal_key text NOT NULL,
    created_in_thread_id text NOT NULL,
    parent_message_id text,
    depth bigint NOT NULL CHECK (depth >= 0),
    role text NOT NULL,
    parts_json text NOT NULL CHECK (jsonb_typeof(parts_json::jsonb) = 'array'),
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    FOREIGN KEY (parent_message_id) REFERENCES public.agent_thread_messages(id),
    CHECK (owner_user_id = auth.uid()::text),
    CHECK (principal_key = 'signed-in:' || owner_user_id),
    CHECK ((parent_message_id IS NULL AND depth = 0)
        OR (parent_message_id IS NOT NULL AND depth > 0))
);
CREATE UNIQUE INDEX agent_thread_messages_sync_seq
    ON public.agent_thread_messages(sync_seq);

-- This is a server projection. An append receipt advances it in server commit
-- order; clients never upload a blind head snapshot.
CREATE TABLE public.agent_thread_transcript_heads (
    thread_id text PRIMARY KEY REFERENCES public.agent_threads(id),
    owner_user_id text NOT NULL,
    head_message_id text REFERENCES public.agent_thread_messages(id),
    message_count bigint NOT NULL CHECK (message_count >= 0),
    updated_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    CHECK ((head_message_id IS NULL AND message_count = 0)
        OR (head_message_id IS NOT NULL AND message_count > 0))
);
CREATE UNIQUE INDEX agent_thread_transcript_heads_sync_seq
    ON public.agent_thread_transcript_heads(sync_seq);
CREATE TRIGGER agent_thread_transcript_heads_sync_seq
    BEFORE INSERT OR UPDATE ON public.agent_thread_transcript_heads
    FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq();

CREATE TABLE public.agent_thread_message_appends (
    thread_id text NOT NULL,
    owner_user_id text NOT NULL,
    principal_key text NOT NULL,
    operation_id text NOT NULL,
    request_fingerprint text NOT NULL,
    base_head_message_id text REFERENCES public.agent_thread_messages(id),
    first_message_id text NOT NULL REFERENCES public.agent_thread_messages(id),
    result_head_message_id text NOT NULL REFERENCES public.agent_thread_messages(id),
    message_count bigint NOT NULL CHECK (message_count > 0),
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (thread_id, operation_id),
    CHECK (owner_user_id = auth.uid()::text),
    CHECK (principal_key = 'signed-in:' || owner_user_id)
);
CREATE UNIQUE INDEX agent_thread_message_appends_sync_seq
    ON public.agent_thread_message_appends(sync_seq);

CREATE TABLE public.authored_turn_preparations (
    thread_id text NOT NULL,
    assistant_message_id text NOT NULL,
    owner_user_id text NOT NULL,
    principal_key text NOT NULL,
    document_id text NOT NULL,
    prepared_revision_id text NOT NULL,
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (thread_id, assistant_message_id),
    UNIQUE (assistant_message_id),
    FOREIGN KEY (principal_key, document_id, prepared_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    CHECK (owner_user_id = auth.uid()::text),
    CHECK (principal_key = 'signed-in:' || owner_user_id)
);
CREATE UNIQUE INDEX authored_turn_preparations_sync_seq
    ON public.authored_turn_preparations(sync_seq);

CREATE TABLE public.authored_turn_outcomes (
    thread_id text NOT NULL,
    assistant_message_id text NOT NULL,
    owner_user_id text NOT NULL,
    principal_key text NOT NULL,
    document_id text NOT NULL,
    prepared_revision_id text NOT NULL,
    status text NOT NULL CHECK (status IN ('committed', 'conflicted')),
    result_revision_id text,
    conflicts_json text CHECK (
        conflicts_json IS NULL OR jsonb_typeof(conflicts_json::jsonb) = 'array'
    ),
    created_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (thread_id, assistant_message_id),
    FOREIGN KEY (thread_id, assistant_message_id)
        REFERENCES public.authored_turn_preparations(thread_id, assistant_message_id),
    FOREIGN KEY (principal_key, document_id, prepared_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    FOREIGN KEY (principal_key, document_id, result_revision_id)
        REFERENCES public.authored_revisions(principal_key, document_id, revision_id),
    CHECK (owner_user_id = auth.uid()::text),
    CHECK (principal_key = 'signed-in:' || owner_user_id),
    CHECK ((status = 'committed' AND result_revision_id IS NOT NULL AND conflicts_json IS NULL)
        OR (status = 'conflicted' AND result_revision_id IS NULL AND conflicts_json IS NOT NULL))
);
CREATE UNIQUE INDEX authored_turn_outcomes_sync_seq
    ON public.authored_turn_outcomes(sync_seq);

CREATE TABLE public.agent_thread_deletions (
    thread_id text PRIMARY KEY,
    owner_user_id text NOT NULL,
    principal_key text NOT NULL,
    document_id text NOT NULL,
    deleted_at text NOT NULL,
    sync_seq bigint NOT NULL DEFAULT 0,
    FOREIGN KEY (owner_user_id, thread_id)
        REFERENCES public.agent_threads(owner_user_id, id),
    FOREIGN KEY (principal_key, document_id)
        REFERENCES public.authored_documents(principal_key, document_id),
    CHECK (owner_user_id = auth.uid()::text),
    CHECK (principal_key = 'signed-in:' || owner_user_id)
);
CREATE UNIQUE INDEX agent_thread_deletions_sync_seq
    ON public.agent_thread_deletions(sync_seq);

-- SQLite rejects an edge whose ordinal is outside the immutable parent_count.
-- Enforce the same shape at ingress so a malformed owner upload can never
-- become the first unmaterializable row on another client's parent cursor.
CREATE OR REPLACE FUNCTION private.guard_authored_revision_parent_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM public.authored_revisions revision
         WHERE revision.principal_key = NEW.principal_key
           AND revision.document_id = NEW.document_id
           AND revision.revision_id = NEW.revision_id
           AND NEW.parent_order < revision.parent_count
    ) THEN
        RAISE EXCEPTION 'authored parent edge exceeds immutable revision shape'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_revision_parent_insert
    BEFORE INSERT ON public.authored_revision_parents
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_revision_parent_insert();

-- Exact response-loss replay is legal. Any change to product bytes under an
-- existing identity is a hard collision and remains queued client-side.
CREATE OR REPLACE FUNCTION private.immutable_update_or_identical()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF (to_jsonb(NEW) - 'sync_seq') IS DISTINCT FROM (to_jsonb(OLD) - 'sync_seq') THEN
        RAISE EXCEPTION 'immutable row identity collision in %', TG_TABLE_NAME
            USING ERRCODE = '23505';
    END IF;
    NEW.sync_seq := OLD.sync_seq;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION private.reject_delete()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    RAISE EXCEPTION 'immutable row cannot be deleted from %', TG_TABLE_NAME
        USING ERRCODE = '23514';
END
$$;

DO $$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'authored_revisions', 'authored_revision_files',
        'authored_revision_parents', 'authored_operation_outcomes',
        'authored_head_proposals', 'authored_head_integrations',
        'authored_document_archives', 'agent_thread_messages',
        'agent_thread_message_appends', 'authored_turn_preparations',
        'authored_turn_outcomes', 'agent_thread_deletions'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER immutable_assign_sync_seq BEFORE INSERT ON public.%I FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq()',
            relation
        );
        EXECUTE format(
            'CREATE TRIGGER immutable_update_or_identical BEFORE UPDATE ON public.%I FOR EACH ROW EXECUTE FUNCTION private.immutable_update_or_identical()',
            relation
        );
        EXECUTE format(
            'CREATE TRIGGER immutable_reject_delete BEFORE DELETE ON public.%I FOR EACH ROW EXECUTE FUNCTION private.reject_delete()',
            relation
        );
    END LOOP;
END
$$;

CREATE OR REPLACE FUNCTION private.guard_authored_document_update()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF (to_jsonb(NEW) - ARRAY['sync_seq', 'archived_at'])
        IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['sync_seq', 'archived_at'])
    THEN
        RAISE EXCEPTION 'authored document identity collision' USING ERRCODE = '23505';
    END IF;
    IF OLD.archived_at IS NOT NULL THEN
        -- A stale create replay carries NULL. Preserve the terminal server
        -- value instead of turning a harmless retry into an immortal queue op.
        NEW.archived_at := OLD.archived_at;
    ELSIF NEW.archived_at IS NOT NULL AND NOT EXISTS (
        SELECT 1
          FROM public.authored_document_archives archive
         WHERE archive.principal_key = NEW.principal_key
           AND archive.document_id = NEW.document_id
           AND archive.archived_at = NEW.archived_at
    ) THEN
        RAISE EXCEPTION 'authored archive transition requires its immutable archive fact'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.archived_at IS NOT DISTINCT FROM OLD.archived_at THEN
        NEW.sync_seq := OLD.sync_seq;
    ELSE
        NEW.sync_seq := private.next_sync_seq();
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_document_update
    BEFORE UPDATE ON public.authored_documents
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_document_update();

CREATE OR REPLACE FUNCTION private.guard_authored_document_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    catalog_route_is_live_and_owned boolean;
BEGIN
    -- Bound identity material before hashing it. The relational route is the
    -- document identity; accepting an unbounded value here would make the
    -- SECURITY DEFINER trigger an avoidable allocation oracle.
    IF coalesce(octet_length(NEW.principal_key), 0) NOT BETWEEN 1 AND 4096
       OR coalesce(octet_length(NEW.subject_id), 0) NOT BETWEEN 1 AND 4096
       OR (NEW.track_id IS NOT NULL
           AND octet_length(NEW.track_id) NOT BETWEEN 1 AND 4096)
       OR (NEW.venue_id IS NOT NULL
           AND octet_length(NEW.venue_id) NOT BETWEEN 1 AND 4096)
       OR (NEW.score_id IS NOT NULL
           AND octet_length(NEW.score_id) NOT BETWEEN 1 AND 4096)
       OR (NEW.implementation_id IS NOT NULL
           AND octet_length(NEW.implementation_id) NOT BETWEEN 1 AND 4096)
    THEN
        RAISE EXCEPTION 'authored document route fields must contain 1 to 4096 UTF-8 bytes'
            USING ERRCODE = '22023';
    END IF;
    IF NEW.document_id IS DISTINCT FROM private.expected_document_id(
        NEW.document_kind,
        NEW.principal_key,
        NEW.subject_id,
        NEW.track_id,
        NEW.venue_id,
        NEW.score_id,
        NEW.implementation_id
    ) THEN
        RAISE EXCEPTION 'authored document id does not match its immutable scope'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.archived_at IS NOT NULL THEN
        RAISE EXCEPTION 'new authored document cannot arrive archived; use archive RPC'
            USING ERRCODE = '23514';
    END IF;
    -- Exact response-loss replay of an existing document must still reach the
    -- UPDATE guard after its catalog row has been tombstoned. A missing catalog
    -- row is also legal: a document created and archived entirely offline must
    -- be able to upload its immutable history before the archive RPC. When a
    -- catalog row does exist, lock it with the archive RPC and reject foreign,
    -- mismatched, or terminal routes.
    IF NOT EXISTS (
        SELECT 1
          FROM public.authored_documents existing
         WHERE existing.document_id = NEW.document_id
           AND existing.principal_key = NEW.principal_key
    ) THEN
        PERFORM pg_advisory_xact_lock(hashtextextended(
            'luma.authored-route.v1:' || NEW.principal_key || ':'
            || NEW.document_kind || ':' || CASE
                WHEN NEW.document_kind = 'track_score' THEN NEW.score_id
                ELSE NEW.subject_id
            END,
            0
        ));
        IF NEW.document_kind = 'track_score' THEN
            IF EXISTS (
                SELECT 1
                  FROM public.authored_documents terminal
                 WHERE terminal.principal_key = NEW.principal_key
                   AND terminal.document_kind = 'track_score'
                   AND terminal.score_id = NEW.score_id
                   AND terminal.archived_at IS NOT NULL
            ) THEN
                RAISE EXCEPTION 'new score document conflicts with a terminal authored route'
                    USING ERRCODE = '23514';
            END IF;
            SELECT score.uid::text = auth.uid()::text
                   AND score.track_id::text = NEW.track_id
                   AND score.venue_id::text = NEW.venue_id
                   AND score.deleted_at IS NULL
              INTO catalog_route_is_live_and_owned
              FROM public.scores score
             WHERE score.id::text = NEW.score_id
             FOR UPDATE;
            IF FOUND AND catalog_route_is_live_and_owned IS NOT TRUE THEN
                RAISE EXCEPTION 'new score document conflicts with its catalog route'
                    USING ERRCODE = '23514';
            END IF;
        ELSIF NEW.document_kind = 'pattern_graph' THEN
            IF EXISTS (
                SELECT 1
                  FROM public.authored_documents terminal
                 WHERE terminal.principal_key = NEW.principal_key
                   AND terminal.document_kind = 'pattern_graph'
                   AND terminal.subject_id = NEW.subject_id
            ) AND NOT EXISTS (
                SELECT 1
                  FROM public.authored_documents live
                 WHERE live.principal_key = NEW.principal_key
                   AND live.document_kind = 'pattern_graph'
                   AND live.subject_id = NEW.subject_id
                   AND live.archived_at IS NULL
            ) THEN
                RAISE EXCEPTION 'new graph document conflicts with a terminal authored route'
                    USING ERRCODE = '23514';
            END IF;
            SELECT pattern.uid::text = auth.uid()::text
                   AND pattern.deleted_at IS NULL
              INTO catalog_route_is_live_and_owned
              FROM public.patterns pattern
             WHERE pattern.id::text = NEW.subject_id
             FOR UPDATE;
            IF FOUND AND catalog_route_is_live_and_owned IS NOT TRUE THEN
                RAISE EXCEPTION 'new graph document conflicts with its catalog route'
                    USING ERRCODE = '23514';
            END IF;
        END IF;
    END IF;
    NEW.sync_seq := private.next_sync_seq();
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_document_insert
    BEFORE INSERT ON public.authored_documents
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_document_insert();
CREATE TRIGGER authored_documents_reject_delete
    BEFORE DELETE ON public.authored_documents
    FOR EACH ROW EXECUTE FUNCTION private.reject_delete();
CREATE TRIGGER authored_document_heads_reject_delete
    BEFORE DELETE ON public.authored_document_heads
    FOR EACH ROW EXECUTE FUNCTION private.reject_delete();

CREATE OR REPLACE FUNCTION private.preserve_catalog_tombstone()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF OLD.deleted_at IS NOT NULL THEN
        NEW.deleted_at := OLD.deleted_at;
    END IF;
    RETURN NEW;
END
$$;

-- The catalog row is only a projection, but it must preserve the authored
-- archive's terminal effect when an offline client later replays an older
-- score or pattern snapshot.
DO $$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY['scores', 'patterns'] LOOP
        IF to_regclass('public.' || relation) IS NULL THEN
            CONTINUE;
        END IF;
        EXECUTE format(
            'CREATE TRIGGER preserve_catalog_tombstone BEFORE UPDATE ON public.%I FOR EACH ROW EXECUTE FUNCTION private.preserve_catalog_tombstone()',
            relation
        );
    END LOOP;
END
$$;

CREATE OR REPLACE FUNCTION private.guard_agent_thread_update()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF (to_jsonb(NEW) - ARRAY['sync_seq', 'title', 'lifecycle_state', 'updated_at'])
        IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['sync_seq', 'title', 'lifecycle_state', 'updated_at'])
    THEN
        RAISE EXCEPTION 'agent thread identity collision' USING ERRCODE = '23505';
    END IF;
    IF OLD.lifecycle_state = 'deleting' THEN
        NEW.lifecycle_state := 'deleting';
    ELSIF NEW.lifecycle_state = 'deleting' AND NOT EXISTS (
        SELECT 1
          FROM public.agent_thread_deletions deletion
         WHERE deletion.thread_id = NEW.id
           AND deletion.owner_user_id = NEW.owner_user_id
    ) THEN
        RAISE EXCEPTION 'agent thread deletion requires its immutable deletion fact'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_agent_thread_update
    BEFORE UPDATE ON public.agent_threads
    FOR EACH ROW EXECUTE FUNCTION private.guard_agent_thread_update();

CREATE OR REPLACE FUNCTION private.guard_agent_thread_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF NEW.lifecycle_state <> 'active' THEN
        -- An offline create followed by delete may coalesce its mutable thread
        -- snapshot to `deleting` before the first upload. Materialize the
        -- routing row as active; the separately queued immutable deletion fact
        -- below performs the only terminal transition.
        NEW.lifecycle_state := 'active';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_agent_thread_insert
    BEFORE INSERT ON public.agent_threads
    FOR EACH ROW EXECUTE FUNCTION private.guard_agent_thread_insert();

CREATE OR REPLACE FUNCTION private.initialize_agent_transcript_head()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    prefix_depth bigint;
    latest_head text;
    latest_depth bigint;
    latest_updated_at text;
BEGIN
    IF NEW.forked_at_message_id IS NULL THEN
        INSERT INTO public.agent_thread_transcript_heads
            (thread_id, owner_user_id, head_message_id, message_count, updated_at)
        VALUES (NEW.id, NEW.owner_user_id, NULL, 0, NEW.updated_at);
    ELSE
        SELECT depth INTO prefix_depth
          FROM public.agent_thread_messages
         WHERE id = NEW.forked_at_message_id
           AND owner_user_id = NEW.owner_user_id
           AND principal_key = 'signed-in:' || NEW.owner_user_id;
        IF prefix_depth IS NULL THEN
            -- The thread snapshot may beat an unsynced prefix node. Keep an
            -- empty projection until that immutable node arrives.
            INSERT INTO public.agent_thread_transcript_heads
                (thread_id, owner_user_id, head_message_id, message_count, updated_at)
            VALUES (NEW.id, NEW.owner_user_id, NULL, 0, NEW.updated_at);
        ELSE
            INSERT INTO public.agent_thread_transcript_heads
                (thread_id, owner_user_id, head_message_id, message_count, updated_at)
            VALUES (
                NEW.id, NEW.owner_user_id, NEW.forked_at_message_id,
                prefix_depth + 1, NEW.updated_at
            );
        END IF;
    END IF;

    -- A failed thread upload must not let later immutable append receipts lose
    -- their projection update. If receipts committed before this routing row,
    -- install the latest server-ordered result now.
    SELECT receipt.result_head_message_id, message.depth, receipt.created_at
      INTO latest_head, latest_depth, latest_updated_at
      FROM public.agent_thread_message_appends receipt
      JOIN public.agent_thread_messages message
        ON message.id = receipt.result_head_message_id
       AND message.owner_user_id = receipt.owner_user_id
       AND message.principal_key = receipt.principal_key
     WHERE receipt.thread_id = NEW.id
       AND receipt.owner_user_id = NEW.owner_user_id
       AND receipt.principal_key = 'signed-in:' || NEW.owner_user_id
     ORDER BY receipt.sync_seq DESC
     LIMIT 1;
    IF latest_head IS NOT NULL THEN
        UPDATE public.agent_thread_transcript_heads
           SET head_message_id = latest_head,
               message_count = latest_depth + 1,
               updated_at = latest_updated_at
         WHERE thread_id = NEW.id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER initialize_agent_transcript_head
    AFTER INSERT ON public.agent_threads
    FOR EACH ROW EXECUTE FUNCTION private.initialize_agent_transcript_head();

CREATE OR REPLACE FUNCTION private.resolve_waiting_fork_heads()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
    UPDATE public.agent_thread_transcript_heads head
       SET head_message_id = NEW.id,
           message_count = NEW.depth + 1,
           updated_at = thread.updated_at
      FROM public.agent_threads thread
     WHERE thread.id = head.thread_id
       AND thread.owner_user_id = NEW.owner_user_id
       AND thread.lifecycle_state = 'active'
       AND thread.forked_at_message_id = NEW.id
       AND head.head_message_id IS NULL
       AND head.message_count = 0;
    RETURN NEW;
END
$$;
CREATE TRIGGER resolve_waiting_fork_heads
    AFTER INSERT ON public.agent_thread_messages
    FOR EACH ROW EXECUTE FUNCTION private.resolve_waiting_fork_heads();

CREATE OR REPLACE FUNCTION private.guard_agent_message_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
DECLARE
    parent_depth bigint;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM public.agent_threads thread
         WHERE thread.id = NEW.created_in_thread_id
           AND thread.owner_user_id = NEW.owner_user_id
    ) THEN
        RAISE EXCEPTION 'transcript message requires its existing owned origin thread'
            USING ERRCODE = '23503';
    END IF;
    IF NEW.parent_message_id IS NULL THEN
        IF NEW.depth <> 0 THEN
            RAISE EXCEPTION 'root transcript message must have depth zero'
                USING ERRCODE = '23514';
        END IF;
    ELSE
        SELECT depth INTO parent_depth
          FROM public.agent_thread_messages
         WHERE id = NEW.parent_message_id
           AND owner_user_id = NEW.owner_user_id
           AND principal_key = NEW.principal_key;
        IF parent_depth IS NULL OR NEW.depth <> parent_depth + 1 THEN
            RAISE EXCEPTION 'transcript parent is missing, foreign, or has invalid depth'
                USING ERRCODE = '23503';
        END IF;
    END IF;
    IF NEW.role = 'assistant' AND NOT EXISTS (
        SELECT 1
          FROM public.authored_turn_preparations preparation
         WHERE preparation.assistant_message_id = NEW.id
           AND preparation.thread_id = NEW.created_in_thread_id
           AND preparation.owner_user_id = NEW.owner_user_id
           AND preparation.principal_key = NEW.principal_key
    ) THEN
        RAISE EXCEPTION 'assistant message requires its immutable turn preparation'
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_agent_message_insert
    BEFORE INSERT ON public.agent_thread_messages
    FOR EACH ROW EXECUTE FUNCTION private.guard_agent_message_insert();

CREATE OR REPLACE FUNCTION private.guard_authored_turn_preparation_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM public.agent_threads thread
          JOIN public.authored_documents document
            ON document.document_id = NEW.document_id
           AND document.principal_key = NEW.principal_key
          JOIN public.authored_revisions revision
            ON revision.revision_id = NEW.prepared_revision_id
           AND revision.document_id = NEW.document_id
           AND revision.principal_key = NEW.principal_key
         WHERE thread.id = NEW.thread_id
           AND thread.owner_user_id = NEW.owner_user_id
           AND revision.thread_id = NEW.thread_id
           AND revision.operation_kind = 'agent_turn_prepare'
           AND revision.operation_id = NEW.assistant_message_id
           AND revision.assistant_message_id IS NULL
           AND (
               (thread.agent_kind = 'track_copilot'
                AND document.document_kind = 'track_score'
                AND document.track_id = thread.subject_id
                AND document.venue_id = thread.venue_id
                AND document.score_id = thread.score_id)
               OR
               (thread.agent_kind = 'pattern_graph'
                AND document.document_kind = 'pattern_graph'
                AND document.subject_id = thread.subject_id
                AND document.implementation_id = thread.implementation_id)
           )
    ) THEN
        RAISE EXCEPTION 'authored turn preparation does not match its owned thread route and revision'
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_turn_preparation_insert
    BEFORE INSERT ON public.authored_turn_preparations
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_turn_preparation_insert();

CREATE OR REPLACE FUNCTION private.guard_authored_turn_outcome_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM public.authored_turn_preparations preparation
          JOIN public.agent_thread_messages message
            ON message.id = preparation.assistant_message_id
         WHERE preparation.thread_id = NEW.thread_id
           AND preparation.assistant_message_id = NEW.assistant_message_id
           AND preparation.owner_user_id = NEW.owner_user_id
           AND preparation.principal_key = NEW.principal_key
           AND preparation.document_id = NEW.document_id
           AND preparation.prepared_revision_id = NEW.prepared_revision_id
           AND message.created_in_thread_id = NEW.thread_id
           AND message.owner_user_id = NEW.owner_user_id
           AND message.principal_key = NEW.principal_key
           AND message.role = 'assistant'
           AND (
               NEW.status = 'conflicted'
               OR EXISTS (
                   SELECT 1
                     FROM public.authored_revisions result
                    WHERE result.revision_id = NEW.result_revision_id
                      AND result.document_id = NEW.document_id
                      AND result.principal_key = NEW.principal_key
                      AND result.operation_kind = 'agent_turn'
                      AND result.operation_id = NEW.assistant_message_id
                      AND result.thread_id = NEW.thread_id
                      AND result.assistant_message_id = NEW.assistant_message_id
                      AND EXISTS (
                          SELECT 1
                            FROM public.authored_revision_parents parent
                           WHERE parent.revision_id = result.revision_id
                             AND parent.principal_key = result.principal_key
                             AND parent.document_id = result.document_id
                             AND parent.parent_order = 1
                             AND parent.parent_revision_id = NEW.prepared_revision_id
                      )
               )
           )
    ) THEN
        RAISE EXCEPTION 'authored turn outcome lacks its persisted assistant revision'
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_turn_outcome_insert
    BEFORE INSERT ON public.authored_turn_outcomes
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_turn_outcome_insert();

CREATE OR REPLACE FUNCTION private.advance_agent_transcript_from_append()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    first_parent text;
    first_depth bigint;
    result_depth bigint;
    cursor_id text;
    reaches_first boolean := false;
BEGIN
    SELECT parent_message_id, depth INTO first_parent, first_depth
     FROM public.agent_thread_messages
     WHERE id = NEW.first_message_id
       AND owner_user_id = NEW.owner_user_id
       AND principal_key = NEW.principal_key
       AND created_in_thread_id = NEW.thread_id;
    SELECT depth INTO result_depth
     FROM public.agent_thread_messages
     WHERE id = NEW.result_head_message_id
       AND owner_user_id = NEW.owner_user_id
       AND principal_key = NEW.principal_key
       AND created_in_thread_id = NEW.thread_id;
    IF first_depth IS NULL OR result_depth IS NULL
       OR first_parent IS DISTINCT FROM NEW.base_head_message_id
       OR result_depth - first_depth + 1 <> NEW.message_count
    THEN
        RAISE EXCEPTION 'append receipt does not describe a contiguous owned range'
            USING ERRCODE = '23514';
    END IF;
    cursor_id := NEW.result_head_message_id;
    WHILE cursor_id IS NOT NULL LOOP
        IF cursor_id = NEW.first_message_id THEN
            reaches_first := true;
            EXIT;
        END IF;
        SELECT parent_message_id INTO cursor_id
          FROM public.agent_thread_messages
         WHERE id = cursor_id
           AND owner_user_id = NEW.owner_user_id
           AND principal_key = NEW.principal_key
           AND created_in_thread_id = NEW.thread_id;
    END LOOP;
    IF NOT reaches_first THEN
        RAISE EXCEPTION 'append result does not descend from its first message'
            USING ERRCODE = '23514';
    END IF;
    -- Every valid append is integrated. The commit-ordered later append wins
    -- the head projection even when it was authored from a stale base; its
    -- losing sibling remains an immutable, restorable trace.
    UPDATE public.agent_thread_transcript_heads head
       SET head_message_id = NEW.result_head_message_id,
           message_count = result_depth + 1,
           updated_at = NEW.created_at
      FROM public.agent_threads thread
     WHERE head.thread_id = NEW.thread_id
       AND thread.id = head.thread_id
       AND thread.owner_user_id = NEW.owner_user_id
       AND thread.lifecycle_state = 'active';
    RETURN NEW;
END
$$;
CREATE TRIGGER advance_agent_transcript_from_append
    AFTER INSERT ON public.agent_thread_message_appends
    FOR EACH ROW EXECUTE FUNCTION private.advance_agent_transcript_from_append();

CREATE OR REPLACE FUNCTION private.apply_agent_thread_deletion()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
    UPDATE public.agent_threads
       SET lifecycle_state = 'deleting', updated_at = NEW.deleted_at
     WHERE id = NEW.thread_id AND owner_user_id = NEW.owner_user_id;
    RETURN NEW;
END
$$;
CREATE TRIGGER apply_agent_thread_deletion
    AFTER INSERT ON public.agent_thread_deletions
    FOR EACH ROW EXECUTE FUNCTION private.apply_agent_thread_deletion();

CREATE OR REPLACE FUNCTION private.guard_agent_thread_deletion_scope()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM public.agent_threads thread
          JOIN public.authored_documents document
            ON document.document_id = NEW.document_id
           AND document.principal_key = NEW.principal_key
         WHERE thread.id = NEW.thread_id
           AND thread.owner_user_id = NEW.owner_user_id
           AND (
               (thread.agent_kind = 'track_copilot'
                AND document.document_kind = 'track_score'
                AND document.track_id = thread.subject_id
                AND document.venue_id = thread.venue_id
                AND document.score_id = thread.score_id)
               OR
               (thread.agent_kind = 'pattern_graph'
                AND document.document_kind = 'pattern_graph'
                AND document.subject_id = thread.subject_id
                AND document.implementation_id = thread.implementation_id)
           )
    ) THEN
        RAISE EXCEPTION 'agent thread deletion does not match its owned authored route'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_agent_thread_deletion_scope
    BEFORE INSERT ON public.agent_thread_deletions
    FOR EACH ROW EXECUTE FUNCTION private.guard_agent_thread_deletion_scope();

-- RLS. Product traces are owner-readable and allow only insert / exact replay.
-- Heads, proposal outcomes, and archive outcomes are select-only because their
-- writes are serialized by the three RPCs below.
DO $$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'authored_documents', 'authored_revisions', 'authored_revision_files',
        'authored_revision_parents', 'authored_operation_outcomes',
        'agent_thread_messages', 'agent_thread_message_appends',
        'authored_turn_preparations', 'authored_turn_outcomes',
        'agent_thread_deletions'
    ] LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', relation);
        EXECUTE format(
            'CREATE POLICY owner_select ON public.%I FOR SELECT TO authenticated USING (principal_key = private.current_principal_key())',
            relation
        );
        EXECUTE format(
            'CREATE POLICY owner_insert ON public.%I FOR INSERT TO authenticated WITH CHECK (principal_key = private.current_principal_key())',
            relation
        );
        EXECUTE format(
            'CREATE POLICY owner_exact_replay ON public.%I FOR UPDATE TO authenticated USING (principal_key = private.current_principal_key()) WITH CHECK (principal_key = private.current_principal_key())',
            relation
        );
    END LOOP;

    FOREACH relation IN ARRAY ARRAY[
        'authored_document_heads', 'authored_head_proposals',
        'authored_head_integrations', 'authored_document_archives'
    ] LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', relation);
        EXECUTE format(
            'CREATE POLICY owner_select ON public.%I FOR SELECT TO authenticated USING (principal_key = private.current_principal_key())',
            relation
        );
    END LOOP;
END
$$;

ALTER TABLE public.agent_threads ENABLE ROW LEVEL SECURITY;
CREATE POLICY agent_threads_owner_select ON public.agent_threads
    FOR SELECT TO authenticated USING (owner_user_id = auth.uid()::text);
CREATE POLICY agent_threads_owner_insert ON public.agent_threads
    FOR INSERT TO authenticated WITH CHECK (owner_user_id = auth.uid()::text);
CREATE POLICY agent_threads_owner_update ON public.agent_threads
    FOR UPDATE TO authenticated
    USING (owner_user_id = auth.uid()::text)
    WITH CHECK (owner_user_id = auth.uid()::text);

ALTER TABLE public.agent_thread_transcript_heads ENABLE ROW LEVEL SECURITY;
CREATE POLICY agent_transcript_heads_owner_select
    ON public.agent_thread_transcript_heads
    FOR SELECT TO authenticated USING (owner_user_id = auth.uid()::text);

GRANT USAGE ON SCHEMA private TO authenticated;
REVOKE ALL ON TABLE private.luma_sync_clock FROM PUBLIC, authenticated;

CREATE OR REPLACE FUNCTION private.hash_field(value bytea)
RETURNS bytea
LANGUAGE sql
IMMUTABLE
STRICT
SET search_path = pg_catalog
AS $$
    SELECT int8send(octet_length(value)::bigint) || value
$$;

-- This is the byte-for-byte PostgreSQL implementation of
-- AuthoredDocumentId::derive. The domain is raw bytes ending in NUL; every
-- identity component, including the kind, is prefixed by an unsigned u64
-- big-endian byte length through hash_field.
CREATE OR REPLACE FUNCTION private.expected_document_id(
    document_kind text,
    principal_key text,
    subject_id text,
    track_id text,
    venue_id text,
    score_id text,
    implementation_id text
)
RETURNS text
LANGUAGE plpgsql
IMMUTABLE
SET search_path = pg_catalog, private, extensions
AS $$
DECLARE
    identity_bytes bytea;
BEGIN
    IF document_kind = 'track_score'
       AND subject_id IS NOT DISTINCT FROM track_id
       AND track_id IS NOT NULL
       AND venue_id IS NOT NULL
       AND score_id IS NOT NULL
       AND implementation_id IS NULL
    THEN
        identity_bytes := private.hash_field(convert_to(document_kind, 'UTF8'))
            || private.hash_field(convert_to(principal_key, 'UTF8'))
            || private.hash_field(convert_to(track_id, 'UTF8'))
            || private.hash_field(convert_to(venue_id, 'UTF8'))
            || private.hash_field(convert_to(score_id, 'UTF8'));
    ELSIF document_kind = 'pattern_graph'
          AND track_id IS NULL
          AND venue_id IS NULL
          AND score_id IS NULL
          AND implementation_id IS NOT NULL
    THEN
        identity_bytes := private.hash_field(convert_to(document_kind, 'UTF8'))
            || private.hash_field(convert_to(principal_key, 'UTF8'))
            || private.hash_field(convert_to(subject_id, 'UTF8'))
            || private.hash_field(convert_to(implementation_id, 'UTF8'));
    ELSE
        RETURN NULL;
    END IF;

    RETURN 'ad-' || encode(
        extensions.digest(
            decode('6c756d612e617574686f7265642d646f63756d656e742e763100', 'hex')
            || identity_bytes,
            'sha256'
        ),
        'hex'
    );
END
$$;

-- Cross-runtime contract vectors. Fail the migration rather than deploy a
-- server whose canonical route IDs differ from the Rust authored-state core.
DO $$
BEGIN
    IF private.expected_document_id(
        'track_score', 'signed-in:user-a', 'track-a',
        'track-a', 'venue-a', 'score-a', NULL
    ) <> 'ad-5d78d30274abf38a2d9af6dab42ed7577eaf8b617e5847e8b792dcdd3d58eb94'
    THEN
        RAISE EXCEPTION 'PostgreSQL track document identity vector does not match Rust';
    END IF;
    IF private.expected_document_id(
        'pattern_graph', 'signed-in:user-a', 'pattern-a',
        NULL, NULL, NULL, 'implementation-a'
    ) <> 'ad-a937e9175da900a0bb4522038184659e6f142a44690ef8cbc0be8f6b7cf494df'
    THEN
        RAISE EXCEPTION 'PostgreSQL graph document identity vector does not match Rust';
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION private.hash_optional(value text)
RETURNS bytea
LANGUAGE sql
IMMUTABLE
SET search_path = pg_catalog, private
AS $$
    SELECT CASE
        WHEN value IS NULL THEN decode('00', 'hex')
        ELSE decode('01', 'hex') || private.hash_field(convert_to(value, 'UTF8'))
    END
$$;

CREATE OR REPLACE FUNCTION private.expected_file_hash(content bytea)
RETURNS text
LANGUAGE sql
IMMUTABLE
STRICT
SET search_path = pg_catalog, extensions, private
AS $$
    SELECT 'sha256:' || encode(
        extensions.digest(
            decode('6c756d612e617574686f7265642d66696c652d636f6e74656e742e763100', 'hex')
            || private.hash_field(content),
            'sha256'
        ),
        'hex'
    )
$$;

-- Validate each immutable file before it receives a server cursor. Closure
-- validation still proves the canonical document shape, but direct row sync
-- must independently reject malformed or resource-exhausting unattached
-- files so no client can publish a row that SQLite can never consume.
CREATE OR REPLACE FUNCTION private.guard_authored_revision_file_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    existing_file_count bigint;
    existing_total_bytes bigint;
BEGIN
    -- Serialize file additions for one immutable revision before enforcing
    -- aggregate bounds; otherwise concurrent distinct-path inserts could each
    -- observe a stale count/total and jointly exceed the Rust limits.
    PERFORM 1
      FROM public.authored_revisions revision
     WHERE revision.principal_key = NEW.principal_key
       AND revision.revision_id = NEW.revision_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'authored revision file requires its parent revision'
            USING ERRCODE = '23503';
    END IF;

    -- An INSERT .. ON CONFLICT exact replay still executes BEFORE INSERT.
    -- Existing paths consume no additional quota; let the immutable UPDATE
    -- trigger below decide whether the replay is identical or a collision.
    IF EXISTS (
        SELECT 1
          FROM public.authored_revision_files existing
         WHERE existing.revision_id = NEW.revision_id
           AND existing.path = NEW.path
           AND existing.principal_key = NEW.principal_key
    ) THEN
        RETURN NEW;
    END IF;

    IF octet_length(NEW.content) > 16777216
       OR octet_length(convert_to(NEW.path, 'UTF8')) NOT BETWEEN 1 AND 1024
       OR left(NEW.path, 1) = '/'
       OR position(chr(92) IN NEW.path) > 0
       OR NEW.path ~ '(^|/)(\.|\.\.)(/|$)'
       OR NEW.path ~ '//'
       OR NEW.path ~ '(^|/)[^/]*[. ](/|$)'
       OR position(':' IN NEW.path) > 0
       OR NEW.content_hash <> private.expected_file_hash(NEW.content)
    THEN
        RAISE EXCEPTION 'authored revision file has invalid bytes, path, or content hash'
            USING ERRCODE = '23514';
    END IF;

    SELECT count(*), coalesce(sum(octet_length(content)), 0)
      INTO existing_file_count, existing_total_bytes
      FROM public.authored_revision_files
     WHERE principal_key = NEW.principal_key
       AND revision_id = NEW.revision_id;
    IF existing_file_count >= 4096
       OR existing_total_bytes + octet_length(NEW.content) > 67108864
    THEN
        RAISE EXCEPTION 'authored revision exceeds immutable file count or byte bounds'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_revision_file_insert
    BEFORE INSERT ON public.authored_revision_files
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_revision_file_insert();

CREATE OR REPLACE FUNCTION private.expected_manifest_hash(owner_key text, revision text)
RETURNS text
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, private, extensions
AS $$
    SELECT 'sha256:' || encode(
        extensions.digest(
            decode('6c756d612e617574686f7265642d636f6e74656e742d6d616e69666573742e763100', 'hex')
            || coalesce(
                string_agg(
                    private.hash_field(convert_to(path, 'UTF8'))
                    || private.hash_field(content),
                    ''::bytea
                    ORDER BY convert_to(path, 'UTF8')
                ),
                ''::bytea
            ),
            'sha256'
        ),
        'hex'
    )
    FROM public.authored_revision_files
    WHERE principal_key = owner_key AND revision_id = revision
$$;

CREATE OR REPLACE FUNCTION private.expected_revision_id(
    owner_key text,
    document text,
    revision text
)
RETURNS text
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, private, extensions
AS $$
DECLARE
    row_data public.authored_revisions%ROWTYPE;
    parent_bytes bytea;
BEGIN
    SELECT * INTO row_data
      FROM public.authored_revisions
     WHERE principal_key = owner_key
       AND document_id = document
       AND revision_id = revision;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    SELECT coalesce(
        string_agg(
            private.hash_field(convert_to(parent_revision_id, 'UTF8')),
            ''::bytea ORDER BY parent_order
        ),
        ''::bytea
    ) INTO parent_bytes
    FROM public.authored_revision_parents
    WHERE principal_key = owner_key
      AND document_id = document
      AND revision_id = revision;

    RETURN 'rv-' || encode(
        extensions.digest(
            decode('6c756d612e617574686f7265642d7265766973696f6e2e763100', 'hex')
            || private.hash_field(convert_to(document, 'UTF8'))
            || int8send(row_data.parent_count::bigint)
            || parent_bytes
            || private.hash_field(convert_to(row_data.content_hash, 'UTF8'))
            || private.hash_field(convert_to(row_data.operation_kind, 'UTF8'))
            || private.hash_optional(row_data.operation_id)
            || private.hash_field(convert_to(row_data.message, 'UTF8'))
            || private.hash_field(convert_to(row_data.author_name, 'UTF8'))
            || private.hash_field(convert_to(row_data.author_email, 'UTF8'))
            || private.hash_field(convert_to(row_data.authored_at, 'UTF8'))
            || private.hash_optional(row_data.thread_id)
            || private.hash_optional(row_data.assistant_message_id)
            || private.hash_optional(row_data.restored_revision_id),
            'sha256'
        ),
        'hex'
    );
END
$$;

CREATE OR REPLACE FUNCTION private.is_revision_ancestor(
    owner_key text,
    document text,
    ancestor text,
    descendant text
)
RETURNS boolean
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
    WITH RECURSIVE lineage(revision_id) AS (
        SELECT descendant
        UNION
        SELECT edge.parent_revision_id
          FROM public.authored_revision_parents edge
          JOIN lineage ON lineage.revision_id = edge.revision_id
         WHERE edge.principal_key = owner_key
           AND edge.document_id = document
    )
    SELECT EXISTS (SELECT 1 FROM lineage WHERE revision_id = ancestor)
$$;

CREATE OR REPLACE FUNCTION private.assert_revision_closed(
    owner_key text,
    document text,
    revision text
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    declared_parents integer;
    document_kind text;
    declared_manifest text;
    file_count integer;
    total_bytes bigint;
    bad_file boolean;
    has_cycle boolean;
BEGIN
    SELECT authored_revisions.parent_count, authored_documents.document_kind,
           authored_revisions.content_hash
      INTO declared_parents, document_kind, declared_manifest
      FROM public.authored_revisions
      JOIN public.authored_documents
        ON authored_documents.document_id = authored_revisions.document_id
       AND authored_documents.principal_key = authored_revisions.principal_key
     WHERE authored_revisions.principal_key = owner_key
       AND authored_revisions.document_id = document
       AND authored_revisions.revision_id = revision;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'revision % is not owned by document %', revision, document
            USING ERRCODE = '23503';
    END IF;

    IF declared_parents <> (
        SELECT count(*) FROM public.authored_revision_parents
         WHERE principal_key = owner_key AND document_id = document
           AND revision_id = revision
    ) THEN
        RAISE EXCEPTION 'revision % parent closure is incomplete', revision
            USING ERRCODE = '23514';
    END IF;
    IF EXISTS (
        SELECT 1 FROM generate_series(0, declared_parents - 1) expected(parent_order)
        WHERE NOT EXISTS (
            SELECT 1 FROM public.authored_revision_parents edge
             WHERE edge.principal_key = owner_key AND edge.document_id = document
               AND edge.revision_id = revision
               AND edge.parent_order = expected.parent_order
        )
    ) THEN
        RAISE EXCEPTION 'revision % parent order is not dense', revision
            USING ERRCODE = '23514';
    END IF;

    WITH RECURSIVE lineage(revision_id) AS (
        SELECT parent_revision_id
          FROM public.authored_revision_parents
         WHERE principal_key = owner_key AND document_id = document
           AND revision_id = revision
        UNION
        SELECT edge.parent_revision_id
          FROM public.authored_revision_parents edge
          JOIN lineage ON lineage.revision_id = edge.revision_id
         WHERE edge.principal_key = owner_key AND edge.document_id = document
    )
    SELECT EXISTS (SELECT 1 FROM lineage WHERE revision_id = revision)
      INTO has_cycle;
    IF has_cycle THEN
        RAISE EXCEPTION 'revision % participates in a parent cycle', revision
            USING ERRCODE = '23514';
    END IF;

    SELECT count(*), coalesce(sum(octet_length(content)), 0), coalesce(bool_or(
               content_hash <> private.expected_file_hash(content)
               OR octet_length(content) > 16777216
               OR octet_length(convert_to(path, 'UTF8')) > 1024
               OR path = '' OR left(path, 1) = '/' OR position(chr(92) IN path) > 0
               OR path ~ '(^|/)(\.|\.\.)(/|$)'
               OR path ~ '//'
               OR path ~ '(^|/)[^/]*[. ](/|$)'
               OR position(':' IN path) > 0
           ), false)
      INTO file_count, total_bytes, bad_file
      FROM public.authored_revision_files
     WHERE principal_key = owner_key AND revision_id = revision;
    IF bad_file OR file_count > 4096 OR total_bytes > 67108864 THEN
        RAISE EXCEPTION 'revision % has invalid file bytes, paths, or bounds', revision
            USING ERRCODE = '23514';
    END IF;
    IF document_kind = 'track_score' AND (
        file_count <> 1 OR NOT EXISTS (
            SELECT 1 FROM public.authored_revision_files
             WHERE principal_key = owner_key AND revision_id = revision
               AND path = 'score.luma'
        )
    ) THEN
        RAISE EXCEPTION 'score revision % has an invalid file shape', revision
            USING ERRCODE = '23514';
    END IF;
    IF document_kind = 'pattern_graph' AND (
        file_count <> 2 OR NOT EXISTS (
            SELECT 1 FROM public.authored_revision_files
             WHERE principal_key = owner_key AND revision_id = revision
               AND path = 'graph.json'
        ) OR NOT EXISTS (
            SELECT 1 FROM public.authored_revision_files
             WHERE principal_key = owner_key AND revision_id = revision
               AND path = 'layout.json'
        )
    ) THEN
        RAISE EXCEPTION 'graph revision % has an invalid file shape', revision
            USING ERRCODE = '23514';
    END IF;
    IF declared_manifest <> private.expected_manifest_hash(owner_key, revision) THEN
        RAISE EXCEPTION 'revision % manifest hash does not match its canonical bytes', revision
            USING ERRCODE = '23514';
    END IF;
    IF revision <> private.expected_revision_id(owner_key, document, revision) THEN
        RAISE EXCEPTION 'revision % id does not match its content and metadata', revision
            USING ERRCODE = '23514';
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION private.assert_revision_closure(
    owner_key text,
    document text,
    tip_revision text
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    reachable_revision text;
BEGIN
    -- UNION (not UNION ALL) makes the walk finite even for malformed cyclic
    -- input. The single-node validator then detects any cycle at each member,
    -- while also proving every ancestor's parent/file/hash closure.
    FOR reachable_revision IN
        WITH RECURSIVE closure(revision_id) AS (
            SELECT tip_revision
            UNION
            SELECT edge.parent_revision_id
              FROM public.authored_revision_parents edge
              JOIN closure ON closure.revision_id = edge.revision_id
             WHERE edge.principal_key = owner_key
               AND edge.document_id = document
        )
        SELECT revision_id FROM closure
    LOOP
        PERFORM private.assert_revision_closed(
            owner_key, document, reachable_revision
        );
    END LOOP;
END
$$;

-- Audit identities use the same bounded ASCII token grammar as Rust. These
-- helpers run inside SECURITY DEFINER ingress paths but remain private and
-- non-callable by clients.
CREATE OR REPLACE FUNCTION private.assert_audit_token(value text, field_name text)
RETURNS void
LANGUAGE plpgsql
IMMUTABLE
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF value IS NULL
       OR octet_length(value) NOT BETWEEN 1 AND 256
       OR value !~ '^[A-Za-z0-9_.:-]+$'
    THEN
        RAISE EXCEPTION 'invalid %', field_name USING ERRCODE = '22023';
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION private.assert_rfc3339(value text, field_name text)
RETURNS void
LANGUAGE plpgsql
IMMUTABLE
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF value IS NULL
       OR octet_length(value) NOT BETWEEN 1 AND 64
       OR value !~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt][0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]+)?([Zz]|[+-][0-9]{2}:[0-9]{2})$'
    THEN
        RAISE EXCEPTION '% must be an RFC 3339 timestamp', field_name
            USING ERRCODE = '22007';
    END IF;
    BEGIN
        PERFORM value::timestamptz;
    EXCEPTION
        WHEN datetime_field_overflow OR invalid_datetime_format THEN
            RAISE EXCEPTION '% must be an RFC 3339 timestamp', field_name
                USING ERRCODE = '22007';
    END;
END
$$;

-- Keep revision operation kinds forward-compatible while enforcing the exact
-- metadata grammar used to hash, list, restore, and render history locally.
CREATE OR REPLACE FUNCTION private.guard_authored_revision_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, private
AS $$
BEGIN
    IF octet_length(NEW.operation_kind) NOT BETWEEN 1 AND 64
       OR NEW.operation_kind !~ '^[a-z0-9_]+$'
    THEN
        RAISE EXCEPTION 'revision operation kind must be lower snake case'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.operation_id IS NOT NULL THEN
        PERFORM private.assert_audit_token(NEW.operation_id, 'revision operation id');
    END IF;
    IF octet_length(NEW.message) > 8192
       OR octet_length(NEW.author_name) > 1024
       OR octet_length(NEW.author_email) > 1024
    THEN
        RAISE EXCEPTION 'revision display metadata exceeds immutable bounds'
            USING ERRCODE = '23514';
    END IF;
    PERFORM private.assert_rfc3339(NEW.authored_at, 'revision authored_at');
    IF NEW.thread_id IS NOT NULL THEN
        PERFORM private.assert_audit_token(NEW.thread_id, 'revision thread id');
    END IF;
    IF NEW.assistant_message_id IS NOT NULL THEN
        PERFORM private.assert_audit_token(
            NEW.assistant_message_id, 'revision assistant message id'
        );
        IF NEW.thread_id IS NULL THEN
            RAISE EXCEPTION 'assistant message revision metadata requires a thread id'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER guard_authored_revision_insert
    BEFORE INSERT ON public.authored_revisions
    FOR EACH ROW EXECUTE FUNCTION private.guard_authored_revision_insert();

-- RPC 1/3: publish one immutable local head advance. Revision closure must be
-- present first. The server assigns order; the originating device is not
-- special after this transaction, so any other online owner client can finish
-- the proposal.
CREATE OR REPLACE FUNCTION public.submit_authored_head_proposal(
    proposal_id text,
    document_id text,
    device_id text,
    operation_id text,
    base_revision_id text,
    proposed_revision_id text,
    created_at text
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    input_proposal_id ALIAS FOR $1;
    input_document_id ALIAS FOR $2;
    input_device_id ALIAS FOR $3;
    input_operation_id ALIAS FOR $4;
    input_base_revision_id ALIAS FOR $5;
    input_proposed_revision_id ALIAS FOR $6;
    input_created_at ALIAS FOR $7;
    owner_key text := private.current_principal_key();
    document_archive text;
    existing public.authored_head_proposals%ROWTYPE;
    integration public.authored_head_integrations%ROWTYPE;
    proposal_sequence bigint;
    integration_sequence bigint;
    current_head text;
    earliest boolean;
    status text;
    now_text text := to_char(clock_timestamp() AT TIME ZONE 'UTC',
                             'YYYY-MM-DD"T"HH24:MI:SS.US"Z"');
BEGIN
    IF owner_key IS NULL THEN
        RAISE EXCEPTION 'authentication required' USING ERRCODE = '42501';
    END IF;
    IF coalesce(input_proposal_id, '') = '' OR coalesce(input_document_id, '') = ''
       OR coalesce(input_device_id, '') = '' OR coalesce(input_operation_id, '') = ''
       OR coalesce(input_proposed_revision_id, '') = '' OR coalesce(input_created_at, '') = ''
    THEN
        RAISE EXCEPTION 'proposal identity and audit fields are required'
            USING ERRCODE = '22023';
    END IF;
    PERFORM private.assert_audit_token(input_proposal_id, 'proposal id');
    PERFORM private.assert_audit_token(input_device_id, 'proposal device id');
    PERFORM private.assert_audit_token(input_operation_id, 'proposal operation id');
    PERFORM private.assert_rfc3339(input_created_at, 'proposal created_at');

    SELECT archived_at INTO document_archive
      FROM public.authored_documents
     WHERE principal_key = owner_key AND authored_documents.document_id = input_document_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'authored document is missing or owned by another principal'
            USING ERRCODE = '42501';
    END IF;

    SELECT * INTO existing
      FROM public.authored_head_proposals
     WHERE authored_head_proposals.proposal_id = input_proposal_id;
    IF FOUND THEN
        IF existing.principal_key <> owner_key
           OR existing.document_id <> input_document_id
           OR existing.device_id <> input_device_id
           OR existing.operation_id <> input_operation_id
           OR existing.base_revision_id IS DISTINCT FROM input_base_revision_id
           OR existing.proposed_revision_id <> input_proposed_revision_id
           OR existing.created_at <> input_created_at
        THEN
            RAISE EXCEPTION 'proposal id is already bound to different input'
                USING ERRCODE = '23505';
        END IF;
    ELSE
        PERFORM private.assert_revision_closure(
            owner_key, input_document_id, input_proposed_revision_id
        );
        IF input_base_revision_id IS NULL THEN
            IF (SELECT revision.parent_count
                  FROM public.authored_revisions revision
                 WHERE revision.principal_key = owner_key
                   AND revision.document_id = input_document_id
                   AND revision.revision_id = input_proposed_revision_id) <> 0
            THEN
                RAISE EXCEPTION 'a parented proposal must record its base revision'
                    USING ERRCODE = '23514';
            END IF;
        ELSE
            IF NOT private.is_revision_ancestor(
                owner_key, input_document_id, input_base_revision_id,
                input_proposed_revision_id
            ) THEN
                RAISE EXCEPTION 'proposal base is not an ancestor of its tip'
                    USING ERRCODE = '23514';
            END IF;
        END IF;
        proposal_sequence := private.next_sync_seq();
        INSERT INTO public.authored_head_proposals (
            proposal_id, principal_key, document_id, device_id, operation_id,
            base_revision_id, proposed_revision_id, server_proposal_seq,
            created_at, sync_seq
        ) VALUES (
            input_proposal_id, owner_key, input_document_id, input_device_id,
            input_operation_id, input_base_revision_id, input_proposed_revision_id,
            proposal_sequence, input_created_at, proposal_sequence
        ) RETURNING * INTO existing;

        IF document_archive IS NOT NULL THEN
            integration_sequence := private.next_sync_seq();
            INSERT INTO public.authored_head_integrations (
                proposal_id, principal_key, document_id, prior_revision_id,
                result_revision_id, resolution_kind, server_integration_seq,
                integrated_at, sync_seq
            ) VALUES (
                input_proposal_id, owner_key, input_document_id, NULL, NULL,
                'cancelled_archived', integration_sequence, now_text,
                integration_sequence
            );
        END IF;
    END IF;

    SELECT * INTO integration
      FROM public.authored_head_integrations
     WHERE authored_head_integrations.proposal_id = input_proposal_id;
    SELECT revision_id INTO current_head
      FROM public.authored_document_heads
     WHERE authored_document_heads.document_id = input_document_id
       AND principal_key = owner_key;
    SELECT NOT EXISTS (
        SELECT 1
          FROM public.authored_head_proposals earlier
         WHERE earlier.principal_key = owner_key
           AND earlier.document_id = input_document_id
           AND earlier.server_proposal_seq < existing.server_proposal_seq
           AND NOT EXISTS (
               SELECT 1 FROM public.authored_head_integrations done
                WHERE done.proposal_id = earlier.proposal_id
           )
    ) INTO earliest;
    status := CASE
        WHEN integration.proposal_id IS NULL THEN 'pending'
        WHEN integration.resolution_kind = 'quarantined_noop' THEN 'quarantined_noop'
        WHEN integration.resolution_kind = 'cancelled_archived' THEN 'cancelled_archived'
        ELSE 'integrated'
    END;
    RETURN jsonb_build_object(
        'proposal_id', existing.proposal_id,
        'document_id', existing.document_id,
        'proposal_seq', existing.server_proposal_seq,
        'status', status,
        'base_revision_id', existing.base_revision_id,
        'proposed_revision_id', existing.proposed_revision_id,
        'current_head_revision_id', current_head,
        'is_earliest_pending', earliest AND integration.proposal_id IS NULL
    );
END
$$;

-- RPC 2/3: serialize the earliest pending proposal against the locked current
-- head. Domain-aware clients may supply a structural merge revision; every
-- other path is still total (whole proposal or quarantined no-op). A stale
-- calculation is returned to the caller for immediate recomputation and never
-- mutates the head.
CREATE OR REPLACE FUNCTION public.integrate_authored_head_proposal(
    proposal_id text,
    expected_head_revision_id text,
    resolution text,
    result_revision_id text
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    input_proposal_id ALIAS FOR $1;
    input_expected_head_revision_id ALIAS FOR $2;
    input_resolution ALIAS FOR $3;
    input_result_revision_id ALIAS FOR $4;
    owner_key text := private.current_principal_key();
    proposal public.authored_head_proposals%ROWTYPE;
    existing public.authored_head_integrations%ROWTYPE;
    document_archive text;
    current_head text;
    earliest_id text;
    integration_sequence bigint;
    now_text text := to_char(clock_timestamp() AT TIME ZONE 'UTC',
                             'YYYY-MM-DD"T"HH24:MI:SS.US"Z"');
    proposal_status text;
    outcome text;
BEGIN
    IF owner_key IS NULL THEN
        RAISE EXCEPTION 'authentication required' USING ERRCODE = '42501';
    END IF;
    PERFORM private.assert_audit_token(input_proposal_id, 'proposal id');
    SELECT * INTO proposal
      FROM public.authored_head_proposals
     WHERE authored_head_proposals.proposal_id = input_proposal_id
       AND principal_key = owner_key;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'proposal is missing or owned by another principal'
            USING ERRCODE = '42501';
    END IF;
    SELECT archived_at INTO document_archive
      FROM public.authored_documents
     WHERE document_id = proposal.document_id AND principal_key = owner_key
     FOR UPDATE;

    SELECT * INTO existing
      FROM public.authored_head_integrations
     WHERE authored_head_integrations.proposal_id = input_proposal_id;
    SELECT revision_id INTO current_head
      FROM public.authored_document_heads
     WHERE document_id = proposal.document_id AND principal_key = owner_key;
    IF existing.proposal_id IS NOT NULL THEN
        proposal_status := CASE
            WHEN existing.resolution_kind = 'quarantined_noop' THEN 'quarantined_noop'
            WHEN existing.resolution_kind = 'cancelled_archived' THEN 'cancelled_archived'
            ELSE 'integrated'
        END;
        RETURN jsonb_build_object(
            'proposal_id', proposal.proposal_id,
            'document_id', proposal.document_id,
            'outcome', 'already_resolved',
            'proposal_status', proposal_status,
            'current_head_revision_id', current_head,
            'integrated_revision_id', existing.result_revision_id,
            'resolution', existing.resolution_kind,
            'integration_seq', existing.server_integration_seq,
            'integrated_at', existing.integrated_at
        );
    END IF;

    SELECT pending.proposal_id INTO earliest_id
      FROM public.authored_head_proposals pending
     WHERE pending.principal_key = owner_key
       AND pending.document_id = proposal.document_id
       AND NOT EXISTS (
           SELECT 1 FROM public.authored_head_integrations done
            WHERE done.proposal_id = pending.proposal_id
       )
     ORDER BY pending.server_proposal_seq
     LIMIT 1;
    IF earliest_id IS DISTINCT FROM proposal.proposal_id THEN
        RETURN jsonb_build_object(
            'proposal_id', proposal.proposal_id,
            'document_id', proposal.document_id,
            'outcome', 'not_earliest',
            'proposal_status', 'pending',
            'current_head_revision_id', current_head,
            'integrated_revision_id', NULL,
            'resolution', NULL,
            'integration_seq', NULL,
            'integrated_at', NULL
        );
    END IF;

    IF document_archive IS NOT NULL THEN
        integration_sequence := private.next_sync_seq();
        INSERT INTO public.authored_head_integrations (
            proposal_id, principal_key, document_id, prior_revision_id,
            result_revision_id, resolution_kind, server_integration_seq,
            integrated_at, sync_seq
        ) VALUES (
            proposal.proposal_id, owner_key, proposal.document_id, NULL, NULL,
            'cancelled_archived', integration_sequence, now_text,
            integration_sequence
        ) RETURNING * INTO existing;
        RETURN jsonb_build_object(
            'proposal_id', proposal.proposal_id,
            'document_id', proposal.document_id,
            'outcome', 'archived',
            'proposal_status', 'cancelled_archived',
            'current_head_revision_id', current_head,
            'integrated_revision_id', NULL,
            'resolution', 'cancelled_archived',
            'integration_seq', existing.server_integration_seq,
            'integrated_at', existing.integrated_at
        );
    END IF;

    IF current_head IS DISTINCT FROM input_expected_head_revision_id THEN
        RETURN jsonb_build_object(
            'proposal_id', proposal.proposal_id,
            'document_id', proposal.document_id,
            'outcome', 'stale',
            'proposal_status', 'pending',
            'current_head_revision_id', current_head,
            'integrated_revision_id', NULL,
            'resolution', NULL,
            'integration_seq', NULL,
            'integrated_at', NULL
        );
    END IF;
    IF input_resolution NOT IN (
        'fast_forward', 'already_ancestor', 'structural',
        'whole_proposal', 'quarantined_noop'
    ) THEN
        RAISE EXCEPTION 'unknown integration resolution %', input_resolution
            USING ERRCODE = '22023';
    END IF;

    IF input_resolution = 'quarantined_noop' THEN
        IF input_result_revision_id IS DISTINCT FROM current_head THEN
            RAISE EXCEPTION 'quarantine must preserve the current head'
                USING ERRCODE = '23514';
        END IF;
    ELSE
        IF input_result_revision_id IS NULL THEN
            RAISE EXCEPTION 'integration result revision is required'
                USING ERRCODE = '22023';
        END IF;
        PERFORM private.assert_revision_closed(
            owner_key, proposal.document_id, input_result_revision_id
        );
    END IF;

    IF input_resolution = 'fast_forward' THEN
        IF input_result_revision_id <> proposal.proposed_revision_id
           OR (current_head IS NULL AND proposal.base_revision_id IS NOT NULL)
           OR (current_head IS NOT NULL AND NOT private.is_revision_ancestor(
                owner_key, proposal.document_id, current_head,
                proposal.proposed_revision_id
           ))
        THEN
            RAISE EXCEPTION 'invalid fast-forward integration'
                USING ERRCODE = '23514';
        END IF;
    ELSIF input_resolution = 'already_ancestor' THEN
        IF current_head IS NULL OR input_result_revision_id <> current_head
           OR NOT private.is_revision_ancestor(
                owner_key, proposal.document_id,
                proposal.proposed_revision_id, current_head
           )
        THEN
            RAISE EXCEPTION 'invalid already-ancestor integration'
                USING ERRCODE = '23514';
        END IF;
    ELSIF input_resolution = 'structural' THEN
        IF current_head IS NULL OR NOT EXISTS (
            SELECT 1
              FROM public.authored_revisions merged
             WHERE merged.principal_key = owner_key
               AND merged.document_id = proposal.document_id
               AND merged.revision_id = input_result_revision_id
               AND merged.parent_count = 2
               AND EXISTS (
                   SELECT 1 FROM public.authored_revision_parents parent
                    WHERE parent.revision_id = merged.revision_id
                      AND parent.parent_order = 0
                      AND parent.parent_revision_id = current_head
               )
               AND EXISTS (
                   SELECT 1 FROM public.authored_revision_parents parent
                    WHERE parent.revision_id = merged.revision_id
                      AND parent.parent_order = 1
                      AND parent.parent_revision_id = proposal.proposed_revision_id
               )
        ) THEN
            RAISE EXCEPTION 'structural result must merge current then proposal'
                USING ERRCODE = '23514';
        END IF;
    ELSIF input_resolution = 'whole_proposal' THEN
        IF input_result_revision_id <> proposal.proposed_revision_id THEN
            RAISE EXCEPTION 'whole-proposal fallback must select the proposal tip'
                USING ERRCODE = '23514';
        END IF;
    END IF;

    IF input_result_revision_id IS NOT DISTINCT FROM current_head THEN
        NULL;
    ELSIF current_head IS NULL THEN
        INSERT INTO public.authored_document_heads (
            document_id, principal_key, revision_id, generation, updated_at
        ) VALUES (
            proposal.document_id, owner_key, input_result_revision_id, 0, now_text
        );
    ELSE
        UPDATE public.authored_document_heads
           SET revision_id = input_result_revision_id,
               generation = generation + 1,
               updated_at = now_text
         WHERE document_id = proposal.document_id
           AND principal_key = owner_key
           AND revision_id = current_head;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'locked authored head moved unexpectedly'
                USING ERRCODE = '40001';
        END IF;
    END IF;

    integration_sequence := private.next_sync_seq();
    INSERT INTO public.authored_head_integrations (
        proposal_id, principal_key, document_id, prior_revision_id,
        result_revision_id, resolution_kind, server_integration_seq,
        integrated_at, sync_seq
    ) VALUES (
        proposal.proposal_id, owner_key, proposal.document_id, current_head,
        input_result_revision_id, input_resolution, integration_sequence,
        now_text, integration_sequence
    ) RETURNING * INTO existing;
    proposal_status := CASE WHEN input_resolution = 'quarantined_noop'
        THEN 'quarantined_noop' ELSE 'integrated' END;
    outcome := CASE WHEN input_resolution = 'quarantined_noop'
        THEN 'quarantined_noop' ELSE 'integrated' END;
    RETURN jsonb_build_object(
        'proposal_id', proposal.proposal_id,
        'document_id', proposal.document_id,
        'outcome', outcome,
        'proposal_status', proposal_status,
        'current_head_revision_id', input_result_revision_id,
        'integrated_revision_id', input_result_revision_id,
        'resolution', input_resolution,
        'integration_seq', existing.server_integration_seq,
        'integrated_at', existing.integrated_at
    );
END
$$;

-- RPC 3/3: archive is terminal. The document row is locked before the
-- transition, every pending proposal is terminally cancelled in server order,
-- and every racing archive request remains an immutable trace.
CREATE OR REPLACE FUNCTION public.archive_authored_document(
    archive_id text,
    document_id text,
    device_id text,
    operation_id text,
    requested_revision_id text,
    archived_at text
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public, private
AS $$
DECLARE
    input_archive_id ALIAS FOR $1;
    input_document_id ALIAS FOR $2;
    input_device_id ALIAS FOR $3;
    input_operation_id ALIAS FOR $4;
    input_requested_revision_id ALIAS FOR $5;
    input_archived_at ALIAS FOR $6;
    owner_key text := private.current_principal_key();
    document_archive text;
    document_kind text;
    document_subject_id text;
    document_score_id text;
    canonical_archive text;
    current_head text;
    existing public.authored_document_archives%ROWTYPE;
    archive_sequence bigint;
    integration_sequence bigint;
    pending public.authored_head_proposals%ROWTYPE;
    cancelled_count bigint := 0;
    now_text text := to_char(clock_timestamp() AT TIME ZONE 'UTC',
                             'YYYY-MM-DD"T"HH24:MI:SS.US"Z"');
    receipt_status text;
BEGIN
    IF owner_key IS NULL THEN
        RAISE EXCEPTION 'authentication required' USING ERRCODE = '42501';
    END IF;
    IF coalesce(input_archive_id, '') = '' OR coalesce(input_document_id, '') = ''
       OR coalesce(input_device_id, '') = '' OR coalesce(input_operation_id, '') = ''
       OR (input_requested_revision_id IS NOT NULL
           AND input_requested_revision_id = '')
       OR coalesce(input_archived_at, '') = ''
    THEN
        RAISE EXCEPTION 'archive identity and audit fields are required'
            USING ERRCODE = '22023';
    END IF;
    PERFORM private.assert_audit_token(input_archive_id, 'archive id');
    PERFORM private.assert_audit_token(input_device_id, 'archive device id');
    PERFORM private.assert_audit_token(input_operation_id, 'archive operation id');
    PERFORM private.assert_rfc3339(input_archived_at, 'archive archived_at');

    SELECT authored_documents.archived_at,
           authored_documents.document_kind,
           authored_documents.subject_id,
           authored_documents.score_id
      INTO document_archive, document_kind, document_subject_id, document_score_id
      FROM public.authored_documents
     WHERE principal_key = owner_key
       AND authored_documents.document_id = input_document_id
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'authored document is missing or owned by another principal'
            USING ERRCODE = '42501';
    END IF;
    -- New sibling routes and terminal archive checks share this transaction
    -- lock even when their catalog projection does not exist yet. Thus an
    -- entirely offline create/archive can upload in order, while a later stale
    -- sibling can never slip in after the route became terminal.
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'luma.authored-route.v1:' || owner_key || ':' || document_kind || ':'
        || CASE
            WHEN document_kind = 'track_score' THEN document_score_id
            ELSE document_subject_id
        END,
        0
    ));
    IF document_kind = 'pattern_graph' THEN
        -- Sibling implementation archives can be submitted concurrently.
        -- Serialize their terminal check on the shared catalog row so the
        -- last committer always observes every prior sibling archive and
        -- publishes the pattern tombstone.
        PERFORM 1
          FROM public.patterns
         WHERE id::text = document_subject_id
           AND uid::text = auth.uid()::text
         FOR UPDATE;
    END IF;
    SELECT revision_id INTO current_head
      FROM public.authored_document_heads
     WHERE principal_key = owner_key
       AND authored_document_heads.document_id = input_document_id;

    SELECT * INTO existing
      FROM public.authored_document_archives
     WHERE authored_document_archives.archive_id = input_archive_id;
    IF FOUND THEN
        IF existing.principal_key <> owner_key
           OR existing.document_id <> input_document_id
           OR existing.device_id <> input_device_id
           OR existing.operation_id <> input_operation_id
           OR existing.requested_revision_id IS DISTINCT FROM input_requested_revision_id
           OR existing.archived_at <> input_archived_at
        THEN
            RAISE EXCEPTION 'archive id is already bound to different input'
                USING ERRCODE = '23505';
        END IF;
        canonical_archive := document_archive;
        RETURN jsonb_build_object(
            'archive_id', existing.archive_id,
            'document_id', existing.document_id,
            'status', CASE WHEN existing.archived_at = canonical_archive
                THEN 'archived' ELSE 'already_archived' END,
            'final_revision_id', existing.final_revision_id,
            'cancelled_proposal_count', 0,
            'archive_seq', existing.server_archive_seq,
            'document_archived_at', canonical_archive
        );
    END IF;

    IF input_requested_revision_id IS NOT NULL THEN
        PERFORM private.assert_revision_closed(
            owner_key, input_document_id, input_requested_revision_id
        );
    END IF;
    IF document_archive IS NULL THEN
        canonical_archive := input_archived_at;
        receipt_status := 'archived';
    ELSE
        canonical_archive := document_archive;
        receipt_status := 'already_archived';
    END IF;

    archive_sequence := private.next_sync_seq();
    INSERT INTO public.authored_document_archives (
        archive_id, principal_key, document_id, device_id, operation_id,
        requested_revision_id, final_revision_id, server_archive_seq,
        archived_at, sync_seq
    ) VALUES (
        input_archive_id, owner_key, input_document_id, input_device_id,
        input_operation_id, input_requested_revision_id, current_head,
        archive_sequence, input_archived_at, archive_sequence
    ) RETURNING * INTO existing;

    IF document_archive IS NULL THEN
        -- The immutable fact exists before the mutable terminal projection.
        -- The document guard proves this exact row authorizes the transition;
        -- no caller-settable session flag participates in authority.
        UPDATE public.authored_documents
           SET archived_at = canonical_archive
         WHERE principal_key = owner_key
           AND authored_documents.document_id = input_document_id
           AND authored_documents.archived_at IS NULL;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'locked authored document archive moved unexpectedly'
                USING ERRCODE = '40001';
        END IF;
    END IF;

    -- The ordinary catalog tombstone is a projection of this terminal fact,
    -- not a second authority. Publish it in the same server transaction so
    -- completion never depends on the submitting device remaining online.
    IF document_kind = 'track_score' THEN
        UPDATE public.scores
           SET deleted_at = canonical_archive::timestamptz
         WHERE id::text = document_score_id
           AND uid::text = auth.uid()::text
           AND deleted_at IS NULL;
    ELSIF document_kind = 'pattern_graph'
          AND NOT EXISTS (
              SELECT 1
                FROM public.authored_documents sibling
               WHERE sibling.principal_key = owner_key
                 AND sibling.document_kind = 'pattern_graph'
                 AND sibling.subject_id = document_subject_id
                 AND sibling.archived_at IS NULL
          )
    THEN
        UPDATE public.patterns
           SET deleted_at = canonical_archive::timestamptz
         WHERE id::text = document_subject_id
           AND uid::text = auth.uid()::text
           AND deleted_at IS NULL;
    END IF;

    FOR pending IN
        SELECT proposal.*
          FROM public.authored_head_proposals proposal
         WHERE proposal.principal_key = owner_key
           AND proposal.document_id = input_document_id
           AND NOT EXISTS (
               SELECT 1 FROM public.authored_head_integrations integrated
                WHERE integrated.proposal_id = proposal.proposal_id
           )
         ORDER BY proposal.server_proposal_seq
         FOR UPDATE
    LOOP
        integration_sequence := private.next_sync_seq();
        INSERT INTO public.authored_head_integrations (
            proposal_id, principal_key, document_id, prior_revision_id,
            result_revision_id, resolution_kind, server_integration_seq,
            integrated_at, sync_seq
        ) VALUES (
            pending.proposal_id, owner_key, input_document_id, NULL, NULL,
            'cancelled_archived', integration_sequence, now_text,
            integration_sequence
        );
        cancelled_count := cancelled_count + 1;
    END LOOP;

    RETURN jsonb_build_object(
        'archive_id', existing.archive_id,
        'document_id', existing.document_id,
        'status', receipt_status,
        'final_revision_id', existing.final_revision_id,
        'cancelled_proposal_count', cancelled_count,
        'archive_seq', existing.server_archive_seq,
        'document_archived_at', canonical_archive
    );
END
$$;

-- PostgREST surface. These are the only new public RPCs. Helper functions stay
-- in the non-exposed private schema.
REVOKE ALL ON FUNCTION private.next_sync_seq() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.current_principal_key() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.bump_sync_seq() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.immutable_update_or_identical() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.reject_delete() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_revision_parent_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_document_update() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_document_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.preserve_catalog_tombstone() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_agent_thread_update() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_agent_thread_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.initialize_agent_transcript_head() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.resolve_waiting_fork_heads() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_agent_message_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_turn_preparation_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_turn_outcome_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.advance_agent_transcript_from_append() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.apply_agent_thread_deletion() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_agent_thread_deletion_scope() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.hash_field(bytea) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.expected_document_id(
    text, text, text, text, text, text, text
) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.hash_optional(text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.expected_file_hash(bytea) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_revision_file_insert() FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.expected_manifest_hash(text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.expected_revision_id(text, text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.is_revision_ancestor(text, text, text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.assert_revision_closed(text, text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.assert_revision_closure(text, text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.assert_audit_token(text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.assert_rfc3339(text, text) FROM PUBLIC, authenticated;
REVOKE ALL ON FUNCTION private.guard_authored_revision_insert() FROM PUBLIC, authenticated;
-- RLS policies invoke this zero-argument identity projection. It reveals only
-- the caller's own principal and is the sole private helper callable by an
-- authenticated role.
GRANT EXECUTE ON FUNCTION private.current_principal_key() TO authenticated;

REVOKE ALL ON FUNCTION public.submit_authored_head_proposal(
    text, text, text, text, text, text, text
) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.integrate_authored_head_proposal(
    text, text, text, text
) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.archive_authored_document(
    text, text, text, text, text, text
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.submit_authored_head_proposal(
    text, text, text, text, text, text, text
) TO authenticated;
GRANT EXECUTE ON FUNCTION public.integrate_authored_head_proposal(
    text, text, text, text
) TO authenticated;
GRANT EXECUTE ON FUNCTION public.archive_authored_document(
    text, text, text, text, text, text
) TO authenticated;

GRANT SELECT, INSERT, UPDATE ON
    public.authored_documents,
    public.authored_revisions,
    public.authored_revision_files,
    public.authored_revision_parents,
    public.authored_operation_outcomes,
    public.agent_thread_messages,
    public.agent_thread_message_appends,
    public.authored_turn_preparations,
    public.authored_turn_outcomes,
    public.agent_thread_deletions
TO authenticated;
GRANT SELECT ON
    public.authored_document_heads,
    public.authored_head_proposals,
    public.authored_head_integrations,
    public.authored_document_archives,
    public.agent_thread_transcript_heads
TO authenticated;
GRANT SELECT, INSERT, UPDATE ON public.agent_threads TO authenticated;
