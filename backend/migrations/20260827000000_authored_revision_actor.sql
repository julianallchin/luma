-- Who produced a revision, as a stable label the row keeps forever.
--
-- `author_name`/`author_email` were never authorship: they are the constant
-- "Luma"/"authored-state@luma.local" baked into the deterministic revision id.
-- `actor` is the real answer — 'user', a model key ('claude-opus-5'), or an
-- external MCP client ('client:<name>/<version>[:<model>]') — and it is
-- deliberately NOT part of the revision hash: both this client and
-- `private.expected_revision_id` on the server re-derive stored ids, so a new
-- hashed field would invalidate every revision that already exists.
--
-- A thread may be deleted; the label stays on the revision.

ALTER TABLE authored_revisions ADD COLUMN actor TEXT NOT NULL DEFAULT 'unknown';

-- Which model a thread's writes should be attributed to, restamped at the
-- start of every turn by the loop that just resolved it, or once at `open` by
-- an external MCP client. NULL means "whatever this host's session actor is".
ALTER TABLE agent_threads ADD COLUMN actor TEXT;

-- Backfill. The immutability trigger guards product writes, not this one
-- migration adding a column that did not exist when the rows were written, so
-- it is dropped and restored inside the same transaction.
DROP TRIGGER authored_revision_is_immutable;

-- A revision with no thread came from the editor: the human wrote it.
UPDATE authored_revisions SET actor = 'user' WHERE thread_id IS NULL;

-- A revision with a thread came from an agent turn. The model that served the
-- turn is recorded in the transcript's `data-pi-message` parts, so prefer the
-- one on this revision's own assistant row, then the last one the thread
-- recorded at all, and fall back to the bare fact that an agent wrote it.
UPDATE authored_revisions
   SET actor = coalesce(
       (SELECT json_extract(part.value, '$.data.model')
          FROM agent_thread_messages message, json_each(message.parts_json) part
         WHERE message.id = authored_revisions.assistant_message_id
           AND json_extract(part.value, '$.type') = 'data-pi-message'
           AND json_type(part.value, '$.data.model') = 'text'
         ORDER BY part.key DESC
         LIMIT 1),
       (SELECT json_extract(part.value, '$.data.model')
          FROM agent_thread_messages message, json_each(message.parts_json) part
         WHERE message.created_in_thread_id = authored_revisions.thread_id
           AND json_extract(part.value, '$.type') = 'data-pi-message'
           AND json_type(part.value, '$.data.model') = 'text'
         ORDER BY message.depth DESC, part.key DESC
         LIMIT 1),
       'agent')
 WHERE thread_id IS NOT NULL;

CREATE TRIGGER authored_revision_is_immutable
BEFORE UPDATE ON authored_revisions FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision is immutable');
END;
