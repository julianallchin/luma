-- One vocabulary in `authored_revisions.actor`, not two.
--
-- The previous migration's backfill recovers whichever provider wire id the
-- loop was configured with from the transcript, while a revision written after
-- it stores the model's stable key — so raw SQL over the column would see
-- `anthropic/claude-opus-5` on backfilled rows and `claude-opus-5` on new ones.
-- `Actor::parse` already reconciles the two on read; this makes the stored
-- bytes agree too.
--
-- Separate from `20260827000000` because that migration has already been
-- applied: sqlx records a checksum per version, and editing an applied file
-- fails every later launch.
--
-- The four ids below are exactly the provider wire ids that differ from their
-- key in `src-tauri/src/agent/model/mod.rs` MODELS (its `openrouter`,
-- `gateway` and `anthropic` columns), which is the same set `ModelId::parse`
-- accepts. Anything else — a retired model, a gateway-only id this build never
-- knew — passes through verbatim rather than being guessed at.
--
-- The immutability trigger guards product writes, not a backfill of a column
-- that did not exist when the rows were written, so it is dropped and restored
-- inside this transaction.

DROP TRIGGER authored_revision_is_immutable;

UPDATE authored_revisions
   SET actor = CASE actor
       WHEN 'anthropic/claude-opus-5' THEN 'claude-opus-5'
       WHEN 'moonshotai/kimi-k3-fast' THEN 'kimi-k3-fast'
       WHEN 'x-ai/grok-4.5'           THEN 'grok-4.5'
       WHEN 'xai/grok-4.5'            THEN 'grok-4.5'
   END
 WHERE actor IN (
     'anthropic/claude-opus-5',
     'moonshotai/kimi-k3-fast',
     'x-ai/grok-4.5',
     'xai/grok-4.5'
 );

CREATE TRIGGER authored_revision_is_immutable
BEFORE UPDATE ON authored_revisions FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'authored revision is immutable');
END;
