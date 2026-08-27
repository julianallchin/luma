-- Mirror of the local `authored_revisions.actor` column so a revision's
-- authorship round-trips through sync.
--
-- Not part of `private.expected_revision_id`: the revision id is a hash over
-- content and operation metadata that both halves re-derive, and adding a
-- field to it would invalidate every revision already stored. `actor` is a
-- provenance label carried beside the identity, not part of it.

ALTER TABLE public.authored_revisions
    ADD COLUMN actor text NOT NULL DEFAULT 'unknown';

-- The same backfill the client applies, so the two stores agree on history
-- they both already hold — otherwise a fresh install would pull `unknown` over
-- rows every other device had recovered, and an exact-replay upsert of an old
-- revision would look like an identity collision.
--
-- Disabled rather than dropped: the immutability guard exists for product
-- writes, not for this one migration adding a column that did not exist when
-- the rows were written, and disabling keeps its definition rather than
-- restating it.
ALTER TABLE public.authored_revisions DISABLE TRIGGER immutable_update_or_identical;

UPDATE public.authored_revisions
   SET actor = 'user'
 WHERE thread_id IS NULL;

-- The model that served a turn is recorded in the transcript's
-- `data-pi-message` parts: prefer the one on this revision's own assistant
-- row, then the last one the thread recorded at all, and fall back to the bare
-- fact that an agent wrote it.
UPDATE public.authored_revisions revision
   SET actor = coalesce(
       (SELECT part.value->'data'->>'model'
          FROM public.agent_thread_messages message,
               LATERAL jsonb_array_elements(message.parts_json::jsonb)
                   WITH ORDINALITY AS part(value, ord)
         WHERE message.id = revision.assistant_message_id
           AND part.value->>'type' = 'data-pi-message'
           AND jsonb_typeof(part.value->'data'->'model') = 'string'
         ORDER BY part.ord DESC
         LIMIT 1),
       (SELECT part.value->'data'->>'model'
          FROM public.agent_thread_messages message,
               LATERAL jsonb_array_elements(message.parts_json::jsonb)
                   WITH ORDINALITY AS part(value, ord)
         WHERE message.created_in_thread_id = revision.thread_id
           AND part.value->>'type' = 'data-pi-message'
           AND jsonb_typeof(part.value->'data'->'model') = 'string'
         ORDER BY message.depth DESC, part.ord DESC
         LIMIT 1),
       'agent')
 WHERE revision.thread_id IS NOT NULL;

ALTER TABLE public.authored_revisions ENABLE TRIGGER immutable_update_or_identical;
