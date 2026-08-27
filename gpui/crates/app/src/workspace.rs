//! Which tab set is on screen, and where the others wait.
//!
//! # The strip belongs to a subject
//!
//! A tab strip used to be the app's — one set of tabs, whatever you were doing.
//! It is now the *subject's*: picking a track in the sidebar brings back the
//! tabs that were open the last time that track was the subject, and leaves the
//! previous track's exactly as they were. The subject is a [`TabScope`].
//!
//! # The live set is not in here
//!
//! [`Tabs`] stays the one thing that knows about ordering, selection and
//! healing, and `Luma::workspace` stays a plain `Tabs<Body>` — the set on
//! screen. This module holds only the sets that are *not* on screen, and
//! [`ParkedTabs::focus`] swaps one for the other.
//!
//! That is why every call site that asks the workspace something is unchanged:
//! there is no scope to thread through `active()`, `open()` or `close()`,
//! because by the time they run the live set is already the right one. A
//! wrapper delegating a dozen methods to "the current scope" would have put
//! that question at every call site instead of answering it once here.
//!
//! # Parked is not closed
//!
//! Switching away parks a set; it does not tear it down. That is the shell's
//! "nothing is destroyed to show something else" rule, one level up: closing a
//! tab still runs `Luma::teardown`, and switching subjects still runs nothing.
//! The two gestures that *do* hand bodies back are the two where the subject
//! itself went away — see [`ParkedTabs::retain`] and
//! [`ParkedTabs::close_where`].

use std::collections::HashMap;

use crate::tabs::{Tabs, Target};

/// Whose tab strip is on screen.
///
/// A track when one is picked, else the room itself — which is what makes the
/// strip reachable before any track is chosen, and is where a patch tab opened
/// from an empty sidebar lands.
///
/// **The venue is part of the key even for a track.** A track id alone would
/// identify the scope perfectly well, but then "drop everything belonging to
/// the room I just left" could not be asked of the key, and answering it would
/// mean keeping a second track-to-venue map beside this one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TabScope {
    Track { track: String, venue: String },
    Venue { venue: String },
}

impl TabScope {
    /// The room this scope belongs to. Total, which is what lets a venue
    /// switch be a predicate over keys rather than a lookup.
    pub(crate) fn venue(&self) -> &str {
        match self {
            Self::Track { venue, .. } | Self::Venue { venue } => venue,
        }
    }

    /// The track this scope is about, when it is about one.
    pub(crate) fn track(&self) -> Option<&str> {
        match self {
            Self::Track { track, .. } => Some(track),
            Self::Venue { .. } => None,
        }
    }
}

/// The tab sets that are not on screen, keyed by whose they are.
pub(crate) struct ParkedTabs<B> {
    parked: HashMap<TabScope, Tabs<B>>,
    /// Whose set is live right now. `None` before any subject exists, which is
    /// the empty shell the venue picker opens over.
    current: Option<TabScope>,
}

impl<B> Default for ParkedTabs<B> {
    fn default() -> Self {
        Self {
            parked: HashMap::new(),
            current: None,
        }
    }
}

impl<B> ParkedTabs<B> {
    /// Put `scope`'s remembered tabs on screen, parking whatever was there.
    ///
    /// Returns whether anything moved, so a caller deriving the scope every
    /// frame can notify only when it actually changed. Asking for the scope
    /// that is already current is a no-op rather than a round trip through the
    /// map — otherwise every frame would park and unpark the live set, and any
    /// tab opened during that frame would be parked before it was ever drawn.
    pub(crate) fn focus(&mut self, scope: Option<TabScope>, live: &mut Tabs<B>) -> bool {
        if scope == self.current {
            return false;
        }
        let arriving = scope
            .as_ref()
            .and_then(|scope| self.parked.remove(scope))
            .unwrap_or_default();
        let leaving = std::mem::replace(live, arriving);
        if let Some(previous) = self.current.take() {
            // An empty set is still a memory: it says "this track had nothing
            // open", which is different from "this track was never visited"
            // only in ways nothing can observe. Parking it anyway keeps the
            // map's contents a function of where the eye has been, which is
            // what the pruning rules below are stated over.
            self.parked.insert(previous, leaving);
        }
        self.current = scope;
        true
    }

    /// Forget every scope `keep` rejects, handing back their tab states so the
    /// caller can run each one's teardown.
    ///
    /// This is the leak rule: a scope whose subject no longer exists — a
    /// deleted track, a room that is gone — has no gesture that could ever
    /// bring it back on screen, so nothing would otherwise drop it. The live
    /// set is included, because the subject can vanish while you are looking
    /// at it.
    pub(crate) fn retain(
        &mut self,
        live: &mut Tabs<B>,
        keep: impl Fn(&TabScope) -> bool,
    ) -> Vec<B> {
        let doomed: Vec<TabScope> = self
            .parked
            .keys()
            .filter(|scope| !keep(scope))
            .cloned()
            .collect();
        let mut dropped: Vec<B> = doomed
            .iter()
            .filter_map(|scope| self.parked.remove(scope))
            .flat_map(Tabs::into_bodies)
            .collect();
        if let Some(current) = self.current.as_ref() {
            if !keep(current) {
                self.current = None;
                dropped.extend(std::mem::take(live).into_bodies());
            }
        }
        dropped
    }

    /// Close every tab answering `doomed`, in every scope including the live
    /// one, and hand their states back.
    ///
    /// Separate from [`Self::retain`] because it is a different question: that
    /// one drops whole subjects, this one drops one *kind of view* wherever it
    /// is parked. A patch tab for a room you have left is the case — the room
    /// still exists and its track scopes are still worth remembering, but a
    /// view of that room's rig is not.
    pub(crate) fn close_where(
        &mut self,
        live: &mut Tabs<B>,
        doomed: impl Fn(&Target) -> bool,
    ) -> Vec<B> {
        let mut dropped = live.close_where(&doomed);
        for tabs in self.parked.values_mut() {
            dropped.extend(tabs.close_where(&doomed));
        }
        dropped
    }
}

impl crate::Luma {
    /// Whose tab strip belongs on screen this frame: the picked track, else
    /// the room, else nothing.
    ///
    /// Read from the *sidebar* rather than from the tabs, and that direction
    /// matters: the strip is a consequence of what is picked, so deriving it
    /// from what the strip is already showing would make the two define each
    /// other and never move.
    pub(crate) fn tab_scope(&self) -> Option<TabScope> {
        let browser = self.sidebar.as_ref()?;
        let venue = browser.venue_id().to_string();
        Some(match &self.selected_track {
            Some(track) => TabScope::Track {
                track: track.clone(),
                venue,
            },
            None => TabScope::Venue { venue },
        })
    }

    /// Keep the strip pointed at the picked subject.
    ///
    /// Done at draw, for the reason [`crate::Luma::sync_chat`] is: a navigation
    /// is a field assignment, and a gesture that forgot to ask would leave one
    /// track's tabs on screen while the sidebar says another is picked. Every
    /// gesture that changes the subject therefore only has to set
    /// `selected_track`, which is what it was already doing.
    pub(crate) fn sync_workspace_scope(&mut self, cx: &mut gpui::Context<Self>) {
        let scope = self.tab_scope();
        if self.parked.focus(scope, &mut self.workspace) {
            // The swapped-in set has its own active tab, so the keyboard is
            // owed to a different element than the frame before.
            cx.notify();
        }
    }

    /// Forget the tab sets of tracks this venue no longer has.
    ///
    /// Called when a venue's rows land, which is the only moment the app
    /// learns a track is gone — there is no delete gesture in this shell yet,
    /// so "absent from the reloaded catalogue" *is* the deletion signal. Scoped
    /// to the one venue whose rows these are: another room's tracks are not
    /// missing, they are merely not in this list.
    pub(crate) fn prune_tab_scopes(
        &mut self,
        venue_id: &str,
        live_tracks: &[String],
        cx: &mut gpui::Context<Self>,
    ) {
        let dropped = self.parked.retain(&mut self.workspace, |scope| {
            if scope.venue() != venue_id {
                return true;
            }
            scope
                .track()
                .is_none_or(|track| live_tracks.iter().any(|id| id == track))
        });
        for body in dropped {
            self.teardown(body, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_scope(track: &str, venue: &str) -> TabScope {
        TabScope::Track {
            track: track.to_string(),
            venue: venue.to_string(),
        }
    }

    fn editor(track: &str, venue: &str) -> Target {
        Target::TrackEditor {
            track: track.to_string(),
            venue: venue.to_string(),
        }
    }

    fn patch(venue: &str) -> Target {
        Target::Universe {
            venue: venue.to_string(),
        }
    }

    fn targets(tabs: &Tabs<&str>) -> Vec<Target> {
        tabs.iter().map(|tab| tab.target.clone()).collect()
    }

    #[test]
    fn a_subject_gets_its_own_tabs_back() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();

        parked.focus(Some(track_scope("a", "room")), &mut live);
        live.open(editor("a", "room"), || "editor a");

        parked.focus(Some(track_scope("b", "room")), &mut live);
        assert!(live.is_empty(), "track b inherited track a's tabs");
        live.open(editor("b", "room"), || "editor b");

        parked.focus(Some(track_scope("a", "room")), &mut live);
        assert_eq!(targets(&live), vec![editor("a", "room")]);
    }

    #[test]
    fn the_selection_comes_back_with_the_set() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        parked.focus(Some(track_scope("a", "room")), &mut live);
        live.open(editor("a", "room"), || "editor");
        live.open(patch("room"), || "patch");
        live.select(&editor("a", "room"));

        parked.focus(Some(track_scope("b", "room")), &mut live);
        parked.focus(Some(track_scope("a", "room")), &mut live);
        assert_eq!(live.active(), Some(&editor("a", "room")));
    }

    #[test]
    fn asking_for_the_current_scope_moves_nothing() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        assert!(parked.focus(Some(track_scope("a", "room")), &mut live));
        live.open(editor("a", "room"), || "editor");

        assert!(!parked.focus(Some(track_scope("a", "room")), &mut live));
        assert_eq!(
            targets(&live),
            vec![editor("a", "room")],
            "re-focusing the live scope parked the set it was already showing"
        );
    }

    #[test]
    fn switching_subjects_hands_back_nothing_to_tear_down() {
        // Parked is not closed: the teardown seam stays `Tabs::close`.
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        parked.focus(Some(track_scope("a", "room")), &mut live);
        live.open(editor("a", "room"), || "transport running");
        parked.focus(Some(track_scope("b", "room")), &mut live);

        parked.focus(Some(track_scope("a", "room")), &mut live);
        assert_eq!(live.active_body(), Some(&"transport running"));
    }

    #[test]
    fn a_vanished_track_takes_its_whole_set_with_it() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        parked.focus(Some(track_scope("gone", "room")), &mut live);
        live.open(editor("gone", "room"), || "doomed");
        parked.focus(Some(track_scope("kept", "room")), &mut live);
        live.open(editor("kept", "room"), || "kept");

        let dropped = parked.retain(&mut live, |scope| scope.track() != Some("gone"));
        assert_eq!(dropped, vec!["doomed"]);
        assert_eq!(targets(&live), vec![editor("kept", "room")]);
    }

    #[test]
    fn a_subject_that_vanishes_while_on_screen_is_dropped_too() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        parked.focus(Some(track_scope("gone", "room")), &mut live);
        live.open(editor("gone", "room"), || "doomed");

        let dropped = parked.retain(&mut live, |scope| scope.track() != Some("gone"));
        assert_eq!(dropped, vec!["doomed"]);
        assert!(live.is_empty());
        // Focusing "no subject" is a no-op precisely because `retain` already
        // put the scope there — a set whose subject vanished must not stay
        // current, or the next pick would park it under a dead key.
        assert!(
            !parked.focus(None, &mut live),
            "the vanished subject is still the current scope"
        );
    }

    #[test]
    fn leaving_a_room_drops_its_patch_wherever_it_was_parked() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        parked.focus(Some(track_scope("a", "old")), &mut live);
        live.open(editor("a", "old"), || "old editor");
        live.open(patch("old"), || "old patch");
        parked.focus(Some(track_scope("b", "new")), &mut live);
        live.open(patch("new"), || "new patch");

        let dropped = parked.close_where(&mut live, |target| {
            target.venue().is_some_and(|owner| owner != "new")
        });
        assert_eq!(dropped, vec!["old patch"]);

        // The room's track work is remembered, not thrown away.
        parked.focus(Some(track_scope("a", "old")), &mut live);
        assert_eq!(targets(&live), vec![editor("a", "old")]);
    }

    #[test]
    fn a_venue_switch_can_be_asked_of_the_keys_alone() {
        let mut parked: ParkedTabs<&str> = ParkedTabs::default();
        let mut live: Tabs<&str> = Tabs::default();
        parked.focus(Some(track_scope("a", "old")), &mut live);
        live.open(editor("a", "old"), || "old");
        parked.focus(
            Some(TabScope::Venue {
                venue: "new".into(),
            }),
            &mut live,
        );

        let dropped = parked.retain(&mut live, |scope| scope.venue() == "new");
        assert_eq!(dropped, vec!["old"]);
    }
}
