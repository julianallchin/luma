-- One vocabulary in `authored_revisions.actor`, not two. The mirror of
-- `src-tauri/migrations/20260827000001_authored_revision_actor_canonical.sql`;
-- the two halves must reach the same label for the same revision, or an
-- exact-replay upsert of an old row reads as an identity collision.
--
-- The previous migration's backfill recovers whichever provider wire id the
-- loop was configured with from the transcript, while a revision written after
-- it stores the model's stable key. `Actor::parse` reconciles the two on read;
-- this makes the stored bytes agree too.
--
-- Kept a separate file rather than folded into `20260827000000` because that
-- migration is already applied on a client, where sqlx records a checksum per
-- version and editing an applied file breaks every later launch. Nothing here
-- depends on the split, but the two migration sets stay symmetric.
--
-- The four ids below are exactly the provider wire ids that differ from their
-- key in `src-tauri/src/agent/model/mod.rs` MODELS (its `openrouter`,
-- `gateway` and `anthropic` columns), which is the same set `ModelId::parse`
-- accepts. Anything else — a retired model, a gateway-only id this build never
-- knew — passes through verbatim rather than being guessed at.
--
-- Disabled rather than dropped: the immutability guard exists for product
-- writes, not for a backfill of a column that did not exist when the rows were
-- written, and disabling keeps its definition rather than restating it.

ALTER TABLE public.authored_revisions DISABLE TRIGGER immutable_update_or_identical;

UPDATE public.authored_revisions
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

ALTER TABLE public.authored_revisions ENABLE TRIGGER immutable_update_or_identical;
