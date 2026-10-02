-- Every clip plays a shipped form, so a score has no local definitions.
--
-- Apply after the clip-forms change set, which deletes these rows, and after
-- `deploy/powersync/sync-config.yaml` no longer names `score_definitions`: a sync rule
-- over a dropped table fails its whole stream.
--
-- A draft stores a whole score document. Documents written before this change
-- carry `version` and `definitions`, which no longer decode, so every draft
-- goes. A draft is a subagent's unmerged scratch copy.
--
-- Idempotent.

begin;

drop table if exists public.score_definitions;

delete from public.drafts;

commit;

notify pgrst, 'reload schema';
