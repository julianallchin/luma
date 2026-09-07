-- Turn context replaces creation-time subject restrictions; owner admission remains unchanged.
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

    ) THEN
        RAISE EXCEPTION 'authored turn preparation does not match its owned document and revision'
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END
$$;

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
       AND COALESCE((
           WITH RECURSIVE lineage AS (
               SELECT id, parent_message_id, depth, role, parts_json
               FROM public.agent_thread_messages WHERE id = NEW.parent_message_id
               UNION ALL
               SELECT parent.id, parent.parent_message_id, parent.depth, parent.role, parent.parts_json
               FROM public.agent_thread_messages parent JOIN lineage child ON parent.id = child.parent_message_id
           )
           SELECT COALESCE(part.value #>> '{data,scope,agentKind}', 'unbound')
           FROM lineage, jsonb_array_elements(lineage.parts_json::jsonb) part
           WHERE lineage.role = 'user' AND part.value ->> 'type' = 'data-luma-context'
           ORDER BY lineage.depth DESC LIMIT 1
       ), (SELECT agent_kind FROM public.agent_threads WHERE id = NEW.created_in_thread_id))
       IN ('track_copilot', 'pattern_graph')
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

