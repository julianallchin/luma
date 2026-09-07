-- Which head an agent turn writes to.
--
-- Every thread but one writes to the live document head. A subagent thread —
-- one spawned by another thread's turn — writes to the private head of the
-- workspace it owns, and the live head moves only later, at `merge_workspace`.
-- Recording the answer on the preparation is what lets the two halves of a
-- turn agree without asking the thread twice: `finalize_turn` replays the head
-- the preparation named instead of re-deriving it, so a workspace retired
-- between the halves can never redirect a child's writes onto the live
-- document.
--
-- Deliberately not a foreign key. A workspace is a local disposable directory
-- and never syncs; a preparation does. A preparation pulled from another
-- device therefore names a workspace this host has never had — which is
-- exactly the signal `recover_turns` uses to leave it alone.
ALTER TABLE authored_turn_preparations ADD COLUMN workspace_id TEXT;

-- A subagent's transcript is a thread of its own: an ordinary `agent_threads`
-- row whose messages are ordinary message rows. There is no second transcript
-- store and no milestone row — `parent_thread_id` is the whole of the
-- relationship, and `parent_call_id` names the tool call in the parent that
-- spawned it, so the start chip and the child thread find each other without
-- an index of their own.
--
-- No foreign key, matching `forked_from_thread_id`: thread rows are mutable
-- and sync in `sync_seq` order, so a parent updated after its child was
-- created legitimately arrives second on a fresh device.
ALTER TABLE agent_threads ADD COLUMN parent_thread_id TEXT;
ALTER TABLE agent_threads ADD COLUMN parent_call_id TEXT;

CREATE INDEX idx_agent_threads_parent ON agent_threads(parent_thread_id);

-- A child edits the same document as its parent — that is what makes merging
-- its workspace back into the parent's meaningful — and is never its own
-- parent, which is also what makes the deletion walk terminate. A parent that
-- is simply absent is tolerated: a pulled child may arrive before the row it
-- names.
CREATE TRIGGER agent_thread_parent_shares_its_scope
BEFORE INSERT ON agent_threads FOR EACH ROW
WHEN NEW.parent_thread_id IS NOT NULL
 AND (
     NEW.parent_thread_id = NEW.id
     OR EXISTS (
         SELECT 1 FROM agent_threads parent
         WHERE parent.id = NEW.parent_thread_id
           AND (
               parent.owner_user_id IS NOT NEW.owner_user_id
               OR parent.agent_kind <> NEW.agent_kind
               OR parent.subject_id IS NOT NEW.subject_id
               OR parent.implementation_id IS NOT NEW.implementation_id
               OR parent.venue_id IS NOT NEW.venue_id
               OR parent.score_id IS NOT NEW.score_id
           )
     )
 )
BEGIN
    SELECT RAISE(ABORT, 'subagent thread must share its parent''s authored scope');
END;
