//! The undo stack: where a document has been, and — after an undo — where it
//! was before it stepped back.
//!
//! Generic over the snapshot, because the stack's discipline is the same for
//! every editor that keeps whole-state snapshots (the track editor's clip
//! list, the graph editor's graph): a fresh edit branches, an undo exchanges
//! the present for the last checkpoint, a redo exchanges it back. What a
//! snapshot *is* — and what "this edit changed nothing" means for one — stays
//! with the editor that owns it; see [`History::abandon_if`].
//!
//! Whole snapshots rather than a log of inverse edits is the owning editor's
//! choice, made where its `Snapshot` is defined: when the document is already
//! the unit that gets written, a command's inverse is the document it
//! replaced, and every command gets undo for free instead of owing an inverse.

/// The two stacks, and nothing else: every method is an exchange against them,
/// so the invariant "undo and redo never lose the present" lives here once.
pub(crate) struct History<S> {
    past: Vec<S>,
    future: Vec<S>,
}

impl<S> Default for History<S> {
    fn default() -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
        }
    }
}

impl<S> History<S> {
    /// How far back an undo reaches. A stack that never forgot would grow for
    /// as long as the screen is open.
    const DEPTH: usize = 100;

    /// Mark a point to come back to. A fresh edit is a new branch, so whatever
    /// a previous undo left ahead of here is gone.
    pub(crate) fn record(&mut self, now: S) {
        self.future.clear();
        if self.past.len() >= Self::DEPTH {
            self.past.remove(0);
        }
        self.past.push(now);
    }

    /// Forget the last checkpoint when `untouched` says the edit it was taken
    /// for changed nothing — so a press that only selected, or a command with
    /// nothing to do, does not put a step on the stack that undoes to itself.
    /// The predicate is the caller's because only the snapshot's owner knows
    /// what "nothing ran" looks like (the track editor answers with pointer
    /// identity on its clip list).
    pub(crate) fn abandon_if(&mut self, untouched: impl FnOnce(&S) -> bool) {
        if self.past.last().is_some_and(untouched) {
            self.past.pop();
        }
    }

    /// Step back: exchange `now` for the last checkpoint. `None` — with `now`
    /// dropped and both stacks untouched — when there is nowhere to go.
    pub(crate) fn undo(&mut self, now: S) -> Option<S> {
        let was = self.past.pop()?;
        self.future.push(now);
        Some(was)
    }

    /// Step forward again: exchange `now` for the state the last undo left
    /// ahead of here.
    pub(crate) fn redo(&mut self, now: S) -> Option<S> {
        let next = self.future.pop()?;
        self.past.push(now);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exchange discipline: undo and redo trade the present for a stored
    /// state without ever losing either, and a dead end changes nothing.
    #[test]
    fn undo_and_redo_exchange_without_losing_the_present() {
        let mut history: History<u32> = History::default();
        assert_eq!(history.undo(9), None, "an empty past is a no-op");

        history.record(1);
        history.record(2);
        assert_eq!(history.undo(3), Some(2));
        assert_eq!(history.redo(2), Some(3));
        assert_eq!(history.undo(3), Some(2));
        assert_eq!(history.undo(2), Some(1));
        assert_eq!(history.undo(1), None);
        assert_eq!(history.redo(1), Some(2));
    }

    /// A fresh edit is a new branch: recording clears whatever an undo left
    /// ahead.
    #[test]
    fn recording_after_an_undo_drops_the_future() {
        let mut history: History<u32> = History::default();
        history.record(1);
        assert_eq!(history.undo(2), Some(1));
        history.record(1);
        assert_eq!(history.redo(1), None, "the branch ahead is gone");
    }

    /// The abandon predicate pops only when it answers yes, and only the last
    /// checkpoint is on offer.
    #[test]
    fn abandon_asks_the_owner_and_pops_at_most_one() {
        let mut history: History<u32> = History::default();
        history.record(1);
        history.record(2);
        history.abandon_if(|last| *last == 7);
        assert_eq!(history.undo(9), Some(2), "a refused abandon kept the step");
        history.abandon_if(|last| *last == 1);
        assert_eq!(history.undo(9), None, "the abandoned step is gone");
    }

    /// The stack forgets its oldest step past the depth, not its newest.
    #[test]
    fn depth_caps_the_past_from_the_old_end() {
        let mut history: History<usize> = History::default();
        for step in 0..(History::<usize>::DEPTH + 5) {
            history.record(step);
        }
        let mut reached = Vec::new();
        while let Some(step) = history.undo(0) {
            reached.push(step);
        }
        assert_eq!(reached.len(), History::<usize>::DEPTH);
        assert_eq!(*reached.last().unwrap(), 5, "the oldest steps were dropped");
    }
}
