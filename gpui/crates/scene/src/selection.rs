//! Editor selection, generic over the id the editor names things by.
//!
//! The contract is the stage editor's click grammar — plain click replaces,
//! shift-click toggles, the tail is primary — and nothing else. The element
//! type is the caller's *stable identity* (the app uses its authored object
//! ids), which is what makes a selection survive a re-solve: an index into a
//! frame would name whatever inherited the slot.

/// Ordered multi-selection. The tail is the primary selection.
///
/// Keeping insertion order makes primary reassignment deterministic when a
/// shift-click toggles the current primary off.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection<T> {
    selected: Vec<T>,
}

impl<T> Default for Selection<T> {
    fn default() -> Self {
        Self {
            selected: Vec::new(),
        }
    }
}

impl<T: PartialEq + Clone> Selection<T> {
    #[must_use]
    pub fn selected(&self) -> &[T] {
        &self.selected
    }

    #[must_use]
    pub fn primary(&self) -> Option<&T> {
        self.selected.last()
    }

    #[must_use]
    pub fn contains(&self, target: &T) -> bool {
        self.selected.contains(target)
    }

    pub fn clear(&mut self) {
        self.selected.clear();
    }

    /// Drop everything the scene no longer has an object for. Called when a
    /// scene reloads, so a deleted piece cannot leave a name behind that the
    /// next pick resolves to something else.
    pub fn retain(&mut self, keep: impl Fn(&T) -> bool) {
        self.selected.retain(|target| keep(target));
    }

    /// Apply the stage editor's click contract.
    ///
    /// Plain click replaces the complete selection. Shift-click toggles one
    /// target.
    pub fn click(&mut self, target: T, shift: bool) {
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
    pub fn replace(&mut self, targets: impl IntoIterator<Item = T>) {
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

    #[test]
    fn plain_click_replaces_the_whole_selection() {
        let mut selection = Selection::default();
        selection.click("a", true);
        selection.click("b", true);
        selection.click("c", false);
        assert_eq!(selection.selected(), &["c"]);
        assert_eq!(selection.primary(), Some(&"c"));
    }

    #[test]
    fn shift_click_toggles() {
        let mut selection = Selection::default();
        selection.click("a", true);
        selection.click("b", true);
        selection.click("a", true);
        assert_eq!(selection.selected(), &["b"]);
        selection.click("a", true);
        assert_eq!(selection.selected(), &["b", "a"]);
        assert_eq!(selection.primary(), Some(&"a"));
    }

    #[test]
    fn removing_the_primary_reveals_the_previous_insertion() {
        let mut selection = Selection::default();
        for target in ["a", "b", "c"] {
            selection.click(target, true);
        }
        selection.click("c", true);
        assert_eq!(selection.primary(), Some(&"b"));
    }

    #[test]
    fn marquee_replacement_is_ordered_and_deduplicated() {
        let mut selection = Selection::default();
        selection.replace(["a", "b", "a", "c"]);
        assert_eq!(selection.selected(), &["a", "b", "c"]);
        assert_eq!(selection.primary(), Some(&"c"));
        selection.replace([]);
        assert_eq!(selection.primary(), None);
    }

    #[test]
    fn retain_drops_what_the_scene_lost() {
        let mut selection = Selection::default();
        selection.replace(["a", "b", "c"]);
        selection.retain(|t| *t != "b");
        assert_eq!(selection.selected(), &["a", "c"]);
    }
}
