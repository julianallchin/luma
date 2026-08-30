-- Venue-scoped agent threads, remote half. Mirrors
-- src-tauri/migrations/20260901000000_venue_agent_threads.sql.
--
-- A venue thread is about one room: no track, no score, and no authored
-- document. Three guards therefore learn a third route, and the deletion
-- receipt's document reference becomes optional.

-- 1. The route.
DO $$
DECLARE
    route_check text;
BEGIN
    SELECT conname INTO route_check
      FROM pg_constraint
     WHERE conrelid = 'public.agent_threads'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%track_copilot%';
    IF route_check IS NULL THEN
        RAISE EXCEPTION 'agent_threads route check not found';
    END IF;
    EXECUTE format('ALTER TABLE public.agent_threads DROP CONSTRAINT %I', route_check);
END
$$;

ALTER TABLE public.agent_threads
    ADD CONSTRAINT agent_threads_route_check CHECK (
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
        OR
        (
            agent_kind = 'venue_rig'
            AND subject_kind = 'venue'
            AND coalesce(subject_id, '') <> ''
            AND venue_id = subject_id
            AND score_id IS NULL
            AND implementation_id IS NULL
        )
    );

-- 2. One preparation per assistant row of a *document* thread.
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
    IF NEW.role = 'assistant'
       AND NOT EXISTS (
           SELECT 1
             FROM public.agent_threads thread
            WHERE thread.id = NEW.created_in_thread_id
              AND thread.agent_kind = 'venue_rig'
       )
       AND NOT EXISTS (
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

-- 3. The deletion receipt names the scope the thread wrote to, and a venue
--    thread wrote to no document.
ALTER TABLE public.agent_thread_deletions
    ALTER COLUMN document_id DROP NOT NULL;

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
          LEFT JOIN public.authored_documents document
            ON document.document_id = NEW.document_id
           AND document.principal_key = NEW.principal_key
         WHERE thread.id = NEW.thread_id
           AND thread.owner_user_id = NEW.owner_user_id
           AND (NEW.document_id IS NULL) = (document.document_id IS NULL)
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
               OR
               (thread.agent_kind = 'venue_rig'
                AND NEW.document_id IS NULL)
           )
    ) THEN
        RAISE EXCEPTION 'agent thread deletion does not match its owned authored route'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
