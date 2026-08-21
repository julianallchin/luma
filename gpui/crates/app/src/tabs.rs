//! What the workspace panel is showing, as pure logic.
//!
//! # A tab is its target
//!
//! [`Target`] names the thing a tab shows, and **it is also the tab's
//! identity**. That one rule buys three behaviours that would otherwise be
//! three rules:
//!
//! - opening a target that already has a tab *reveals* that tab rather than
//!   minting a second view of one thing ([`Tabs::open`] is idempotent),
//! - the visualizer and the universe are singletons per venue for free, since
//!   there is only one `Visualizer { venue }` value per venue,
//! - "the graph agent edited a pattern, surface its tab" is a call with no
//!   question attached — whether the tab already existed is not the caller's
//!   problem.
//!
//! A second identity (a `TabId` beside the target) would be a second key that
//! could disagree with the first, which is exactly the drift the shell
//! redesign removes. See `docs/specs/comet-shell.md` §2.3 and its deviation
//! note.
//!
//! # Why the body is a type parameter
//!
//! [`Tabs`] is ordering, selection and healing. It is not a track editor. The
//! shell instantiates it over the enum of real editor states; the tests here
//! instantiate it over a `&str`, which is what lets every rule — idempotent
//! open, dead-tab healing, close-then-heal, reorder — be a plain unit test
//! instead of a windowed one.
//!
//! # Switching is not closing
//!
//! [`Tabs::close`] hands the body **back**; nothing else does. That asymmetry
//! is the whole seam behind the shell's teardown rule: switching tabs tears
//! down nothing (playback continues, a loop region stays armed), and closing
//! runs the state's own close semantics — which the caller can only do if it
//! is given the state to run them on.

// The model lands before the shell that renders it, so that the shell swap is
// one commit of wiring over logic that is already proven. Delete this line in
// the same commit that mounts `Tabs` — after that, an unused method here is a
// real finding.
#![allow(dead_code)]

/// What a workspace tab shows.
///
/// Closed on purpose: the `+` menu, the picker cards, the keymap and the
/// agent's targeted opens all enumerate this, and a variant that existed in one
/// of those lists and not the others would be a card that opens nothing.
///
/// Ids are `String` rather than newtypes because every id in this crate is a
/// `String` today — minting four newtypes here would put a second id vocabulary
/// at the wrong layer and make every call site a conversion. Recorded as a
/// deviation in the spec rather than silently dropped.
///
/// The fields are what today's *gestures* can name — a key wider than any
/// gesture would force every call site to invent the missing half. A track row
/// names `(track, venue)` and the score is resolved from that pair; a pattern
/// row names a pattern and the implementation arrives with the document. When
/// a gesture that names a score or an implementation exists, the key widens in
/// the same change that adds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// One track's timeline, against the named venue's score for it.
    TrackEditor { track: String, venue: String },
    /// One pattern's node graph.
    Graph { pattern: String },
    /// One venue's rig in 3D. Singleton per venue.
    Visualizer { venue: String },
    /// One venue's DMX patch. Singleton per venue.
    Universe { venue: String },
}

impl Target {
    /// The key context this tab's root declares, nested *inside* the
    /// workspace's own context. That nesting is what lets the track editor's
    /// whole binding block survive the shell swap character-for-character —
    /// see [`crate::keymap`].
    pub(crate) fn key_context(&self) -> &'static str {
        match self {
            Self::TrackEditor { .. } => crate::keymap::context::TRACK_EDITOR,
            Self::Graph { .. } => crate::keymap::context::GRAPH,
            Self::Visualizer { .. } => crate::keymap::context::VISUALIZER,
            Self::Universe { .. } => crate::keymap::context::UNIVERSE,
        }
    }

    /// What the `+` menu and the empty-state picker call this kind of tab.
    pub(crate) fn kind_label(&self) -> &'static str {
        match self {
            Self::TrackEditor { .. } => "Track Editor",
            Self::Graph { .. } => "Pattern",
            Self::Visualizer { .. } => "Visualizer",
            Self::Universe { .. } => "Universe",
        }
    }

    /// A stable, unique element-id fragment for this tab's chip and body.
    ///
    /// Derived from the target rather than from a counter, for the same reason
    /// the target is the identity: a chip whose id changed when the strip was
    /// reordered would lose its hover and its drag mid-gesture.
    pub(crate) fn element_key(&self) -> String {
        match self {
            Self::TrackEditor { track, venue } => format!("track:{track}:{venue}"),
            Self::Graph { pattern } => format!("graph:{pattern}"),
            Self::Visualizer { venue } => format!("visualizer:{venue}"),
            Self::Universe { venue } => format!("universe:{venue}"),
        }
    }

    /// The venue this tab dies with, when it dies with one.
    ///
    /// A track editor does not: its score names a venue, but the tab is about
    /// the track, and closing somebody's open timeline because they glanced at
    /// another room would be the shell throwing work away — which is the thing
    /// this redesign exists to stop. Only the two rig views are a view *of* a
    /// venue. This is the spec's open question 2, answered in the type rather
    /// than at the call site that asks.
    pub(crate) fn venue(&self) -> Option<&str> {
        match self {
            Self::Visualizer { venue } | Self::Universe { venue } => Some(venue),
            Self::TrackEditor { .. } | Self::Graph { .. } => None,
        }
    }
}

/// One open tab: what it shows, and the state showing it.
#[derive(Debug)]
pub(crate) struct Tab<B> {
    pub(crate) target: Target,
    pub(crate) body: B,
}

/// The workspace panel's open tabs, in strip order.
///
/// The active tab is stored as a *target*, so a close or a reorder cannot leave
/// the selection pointing at the wrong tab. A stored target that is no longer
/// open is not an error state — see [`Tabs::active`], which heals it the way
/// comet's `resolved_right_active` does.
#[derive(Debug)]
pub(crate) struct Tabs<B> {
    open: Vec<Tab<B>>,
    /// The reader's last pick. May name a tab that has since closed; it is
    /// never *read* without healing, so it cannot render a dead surface.
    picked: Option<Target>,
}

impl<B> Default for Tabs<B> {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            picked: None,
        }
    }
}

impl<B> Tabs<B> {
    /// Reveal `target`, building its body only if it is not already open.
    ///
    /// The one entry point: every gesture that opens anything — a menu card, a
    /// clip double-click, an agent tool call — comes through here, so "was it
    /// already open" is never the caller's problem. `build` is a closure rather
    /// than a value because a tab body is an editor, and constructing one only
    /// to throw it away because the tab existed would re-read a document for
    /// nothing.
    pub(crate) fn open(&mut self, target: Target, build: impl FnOnce() -> B) {
        if !self.open.iter().any(|tab| tab.target == target) {
            let body = build();
            self.open.push(Tab {
                target: target.clone(),
                body,
            });
        }
        self.picked = Some(target);
    }

    /// Close `target`, handing back its state so the caller can run whatever
    /// teardown that state needs — see the module docs. `None` means it was not
    /// open, which is not a failure: a double-click on a ✕ is two closes of one
    /// tab.
    pub(crate) fn close(&mut self, target: &Target) -> Option<B> {
        let index = self.open.iter().position(|tab| &tab.target == target)?;
        let tab = self.open.remove(index);
        // Land on the neighbour that took its place, else the new last tab. The
        // eye was on that chip's row; sending it to the first tab instead would
        // be a jump nobody asked for.
        if self.picked.as_ref() == Some(target) {
            self.picked = self
                .open
                .get(index)
                .or_else(|| self.open.last())
                .map(|tab| tab.target.clone());
        }
        Some(tab.body)
    }

    /// Close every tab whose target answers `doomed`, handing back their states
    /// in strip order so the caller tears each one down.
    pub(crate) fn close_where(&mut self, doomed: impl Fn(&Target) -> bool) -> Vec<B> {
        let condemned: Vec<Target> = self
            .open
            .iter()
            .filter(|tab| doomed(&tab.target))
            .map(|tab| tab.target.clone())
            .collect();
        condemned
            .iter()
            .filter_map(|target| self.close(target))
            .collect()
    }

    /// Make `target` the visible tab. A target that is not open is ignored —
    /// [`Tabs::open`] is the call that adds one, and a `select` that silently
    /// opened would be a second way to open.
    pub(crate) fn select(&mut self, target: &Target) {
        if self.open.iter().any(|tab| &tab.target == target) {
            self.picked = Some(target.clone());
        }
    }

    /// Select the `index`th tab in strip order (`⌘1`…`⌘9`). Out of range is a
    /// no-op: ⌘7 with four tabs open should do nothing, not wrap and not close.
    pub(crate) fn select_index(&mut self, index: usize) {
        if let Some(tab) = self.open.get(index) {
            self.picked = Some(tab.target.clone());
        }
    }

    /// Move the tab at `from` so that it sits at `to`, clamped into range.
    /// Reordering never changes which tab is active, because the selection is a
    /// target and not a position.
    pub(crate) fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.open.len() || from == to {
            return;
        }
        // Clamped against the *pre-removal* length, so a drop past the last
        // chip means "the end" rather than one short of it.
        let to = to.min(self.open.len() - 1);
        let tab = self.open.remove(from);
        self.open.insert(to, tab);
    }

    /// The target that actually renders: the stored pick when it still exists,
    /// else the first remaining tab, else nothing (the picker).
    ///
    /// Healing on *read* rather than fixing on every mutation is what makes
    /// "never render a dead surface" a property of the type instead of a rule
    /// each mutator has to remember.
    pub(crate) fn active(&self) -> Option<&Target> {
        let picked = self.picked.as_ref()?;
        self.open
            .iter()
            .find(|tab| &tab.target == picked)
            .or_else(|| self.open.first())
            .map(|tab| &tab.target)
    }

    /// The visible tab's state.
    pub(crate) fn active_body(&self) -> Option<&B> {
        let active = self.active()?.clone();
        self.open
            .iter()
            .find(|tab| tab.target == active)
            .map(|tab| &tab.body)
    }

    pub(crate) fn active_body_mut(&mut self) -> Option<&mut B> {
        let active = self.active()?.clone();
        self.open
            .iter_mut()
            .find(|tab| tab.target == active)
            .map(|tab| &mut tab.body)
    }

    /// Every open tab, in strip order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Tab<B>> {
        self.open.iter()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab<B>> {
        self.open.iter_mut()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// The body showing `target`, whether or not it is the visible tab — the
    /// agent's targeted opens address a tab by what it is about.
    pub(crate) fn body_mut(&mut self, target: &Target) -> Option<&mut B> {
        self.open
            .iter_mut()
            .find(|tab| &tab.target == target)
            .map(|tab| &mut tab.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Target {
        Target::TrackEditor {
            track: id.to_string(),
            venue: "venue".to_string(),
        }
    }

    fn graph(pattern: &str) -> Target {
        Target::Graph {
            pattern: pattern.to_string(),
        }
    }

    fn targets<B>(tabs: &Tabs<B>) -> Vec<Target> {
        tabs.iter().map(|tab| tab.target.clone()).collect()
    }

    #[test]
    fn empty_workspace_has_no_active_tab() {
        let tabs: Tabs<&str> = Tabs::default();
        assert_eq!(tabs.active(), None);
        assert!(tabs.is_empty());
    }

    #[test]
    fn opening_the_same_target_twice_reveals_the_first_tab() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "first");
        tabs.open(track("b"), || "second");
        tabs.open(track("a"), || panic!("an open target must not be rebuilt"));

        assert_eq!(targets(&tabs), vec![track("a"), track("b")]);
        assert_eq!(tabs.active(), Some(&track("a")));
    }

    #[test]
    fn two_patterns_are_two_tabs_and_one_pattern_is_one() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(graph("pulse"), || "one");
        tabs.open(graph("wash"), || "two");
        tabs.open(graph("pulse"), || {
            panic!("an open pattern must not be rebuilt")
        });
        assert_eq!(tabs.iter().count(), 2);
    }

    #[test]
    fn a_venue_has_exactly_one_visualizer() {
        let mut tabs: Tabs<&str> = Tabs::default();
        let target = Target::Visualizer {
            venue: "aurora".to_string(),
        };
        tabs.open(target.clone(), || "one");
        tabs.open(target, || panic!("the visualizer is a singleton per venue"));
        assert_eq!(tabs.iter().count(), 1);
    }

    #[test]
    fn close_hands_back_the_state_for_teardown() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "playing");
        assert_eq!(tabs.close(&track("a")), Some("playing"));
        assert_eq!(tabs.close(&track("a")), None);
    }

    #[test]
    fn switching_tabs_hands_back_nothing_to_tear_down() {
        // The teardown seam is `close` and only `close`: there is no way to
        // express "switch away and stop the transport", which is what makes the
        // shell's switch-keeps-playing rule structural rather than remembered.
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "transport running");
        tabs.open(track("b"), || "b");
        tabs.select(&track("a"));
        tabs.select(&track("b"));
        assert_eq!(tabs.iter().count(), 2);
        assert_eq!(
            tabs.body_mut(&track("a")).copied(),
            Some("transport running")
        );
    }

    #[test]
    fn closing_the_active_tab_lands_on_its_neighbour() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        tabs.open(track("c"), || "c");
        tabs.select(&track("b"));

        tabs.close(&track("b"));
        assert_eq!(tabs.active(), Some(&track("c")));

        tabs.close(&track("c"));
        assert_eq!(tabs.active(), Some(&track("a")));

        tabs.close(&track("a"));
        assert_eq!(tabs.active(), None);
    }

    #[test]
    fn closing_an_inactive_tab_leaves_the_selection_alone() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        tabs.select(&track("a"));
        tabs.close(&track("b"));
        assert_eq!(tabs.active(), Some(&track("a")));
    }

    #[test]
    fn a_stale_pick_heals_to_the_first_remaining_tab() {
        // The pick is reachable only through `active`, so a target removed
        // behind the selection's back still cannot render a dead surface.
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        tabs.picked = Some(track("gone"));
        assert_eq!(tabs.active(), Some(&track("a")));
    }

    #[test]
    fn reorder_moves_the_chip_without_moving_the_selection() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        tabs.open(track("c"), || "c");
        tabs.select(&track("a"));

        tabs.reorder(0, 2);
        assert_eq!(targets(&tabs), vec![track("b"), track("c"), track("a")]);
        assert_eq!(tabs.active(), Some(&track("a")));
    }

    #[test]
    fn reorder_out_of_range_is_a_no_op() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        tabs.reorder(9, 0);
        tabs.reorder(0, 9);
        assert_eq!(targets(&tabs), vec![track("b"), track("a")]);
    }

    #[test]
    fn select_index_addresses_strip_order_and_ignores_the_rest() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        tabs.select_index(0);
        assert_eq!(tabs.active(), Some(&track("a")));
        tabs.select_index(8);
        assert_eq!(tabs.active(), Some(&track("a")));
    }

    #[test]
    fn select_never_opens() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.select(&track("never-opened"));
        assert_eq!(tabs.active(), Some(&track("a")));
        assert_eq!(tabs.iter().count(), 1);
    }

    #[test]
    fn leaving_a_venue_closes_only_that_venues_rig_tabs() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "editor");
        tabs.open(
            Target::Visualizer {
                venue: "aurora".to_string(),
            },
            || "aurora rig",
        );
        tabs.open(
            Target::Universe {
                venue: "glasshouse".to_string(),
            },
            || "glasshouse patch",
        );

        let dropped = tabs.close_where(|target| target.venue() == Some("aurora"));
        assert_eq!(dropped, vec!["aurora rig"]);
        assert_eq!(tabs.iter().count(), 2);
    }

    #[test]
    fn element_keys_are_unique_per_target() {
        let keys = [
            track("a").element_key(),
            track("b").element_key(),
            graph("p").element_key(),
            Target::Visualizer {
                venue: "v".to_string(),
            }
            .element_key(),
            Target::Universe {
                venue: "v".to_string(),
            }
            .element_key(),
        ];
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn every_target_declares_a_distinct_key_context() {
        let contexts = [
            track("a").key_context(),
            graph("p").key_context(),
            Target::Visualizer {
                venue: "v".to_string(),
            }
            .key_context(),
            Target::Universe {
                venue: "v".to_string(),
            }
            .key_context(),
        ];
        let unique: std::collections::HashSet<&&str> = contexts.iter().collect();
        assert_eq!(unique.len(), contexts.len());
    }

    #[test]
    fn the_active_body_follows_the_selection() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        assert_eq!(tabs.active_body(), Some(&"b"));
        tabs.select(&track("a"));
        assert_eq!(tabs.active_body(), Some(&"a"));
        *tabs.active_body_mut().unwrap() = "edited";
        assert_eq!(tabs.active_body(), Some(&"edited"));
    }

    #[test]
    fn a_body_is_reachable_by_target_without_selecting_it() {
        let mut tabs: Tabs<&str> = Tabs::default();
        tabs.open(track("a"), || "a");
        tabs.open(track("b"), || "b");
        *tabs.body_mut(&track("a")).unwrap() = "touched by the agent";
        assert_eq!(tabs.active(), Some(&track("b")));
        assert_eq!(
            tabs.body_mut(&track("a")).copied(),
            Some("touched by the agent")
        );
    }
}
