//! Editor selection, independent of venue ids and UI state.
//!
//! A [`SelectionTarget`] combines a graph node with its editor domain. The
//! domain is needed only for the legacy cross-type rule: a plain fixture click
//! drops stage-piece selection and vice versa, while shift-click can keep both.

use crate::NodeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ObjectKind {
    Fixture,
    StagePiece,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SelectionTarget {
    pub kind: ObjectKind,
    pub node: NodeId,
}

impl SelectionTarget {
    #[must_use]
    pub const fn new(kind: ObjectKind, node: NodeId) -> Self {
        Self { kind, node }
    }
}

/// Ordered multi-selection. The tail is the primary selection.
///
/// Keeping insertion order makes primary reassignment deterministic when a
/// shift-click toggles the current primary off.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    selected: Vec<SelectionTarget>,
}

impl Selection {
    #[must_use]
    pub fn selected(&self) -> &[SelectionTarget] {
        &self.selected
    }

    #[must_use]
    pub fn primary(&self) -> Option<SelectionTarget> {
        self.selected.last().copied()
    }

    #[must_use]
    pub fn contains(&self, target: SelectionTarget) -> bool {
        self.selected.contains(&target)
    }

    pub fn clear(&mut self) {
        self.selected.clear();
    }

    /// Apply the stage editor's click contract.
    ///
    /// Plain click replaces the complete mixed selection. Shift-click toggles
    /// one target without clearing objects of the other kind.
    pub fn click(&mut self, target: SelectionTarget, shift: bool) {
        if !shift {
            self.selected.clear();
            self.selected.push(target);
            return;
        }
        if let Some(index) = self.selected.iter().position(|item| *item == target) {
            self.selected.remove(index);
        } else {
            self.selected.push(target);
        }
    }

    /// Replace the selection with a marquee result, retaining the input order
    /// and ignoring duplicates. The final distinct target becomes primary.
    pub fn replace(&mut self, targets: impl IntoIterator<Item = SelectionTarget>) {
        self.selected.clear();
        for target in targets {
            if !self.selected.contains(&target) {
                self.selected.push(target);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(id: u32) -> SelectionTarget {
        SelectionTarget::new(ObjectKind::Fixture, NodeId(id))
    }

    fn piece(id: u32) -> SelectionTarget {
        SelectionTarget::new(ObjectKind::StagePiece, NodeId(id))
    }

    #[test]
    fn plain_click_replaces_and_clears_cross_type_selection() {
        let mut selection = Selection::default();
        selection.click(fixture(1), true);
        selection.click(piece(2), true);
        selection.click(fixture(3), false);
        assert_eq!(selection.selected(), &[fixture(3)]);
        assert_eq!(selection.primary(), Some(fixture(3)));
    }

    #[test]
    fn shift_click_toggles_without_clearing_the_other_type() {
        let mut selection = Selection::default();
        selection.click(fixture(1), true);
        selection.click(piece(2), true);
        selection.click(fixture(1), true);
        assert_eq!(selection.selected(), &[piece(2)]);
        selection.click(fixture(1), true);
        assert_eq!(selection.selected(), &[piece(2), fixture(1)]);
        assert_eq!(selection.primary(), Some(fixture(1)));
    }

    #[test]
    fn removing_the_primary_reveals_the_previous_insertion() {
        let mut selection = Selection::default();
        for target in [fixture(1), piece(2), fixture(3)] {
            selection.click(target, true);
        }
        selection.click(fixture(3), true);
        assert_eq!(selection.primary(), Some(piece(2)));
    }

    #[test]
    fn marquee_replacement_is_ordered_and_deduplicated() {
        let mut selection = Selection::default();
        selection.replace([fixture(1), piece(2), fixture(1), fixture(3)]);
        assert_eq!(selection.selected(), &[fixture(1), piece(2), fixture(3)]);
        assert_eq!(selection.primary(), Some(fixture(3)));
        selection.replace([]);
        assert_eq!(selection.primary(), None);
    }
}
