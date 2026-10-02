//! Which venue's tab set is on screen, and where the others wait.
//!
//! # A venue is a project
//!
//! Each venue has its own tab set: its score tabs and its venue tab. Picking a
//! venue in the picker brings back the tabs that were open the last time that
//! venue was on screen, and leaves the previous venue's exactly as they were.
//! See `docs/specs/venue-tabs.md` rules 1 and 2.
//!
//! # The live set is not in here
//!
//! [`Tabs`] stays the one thing that knows about ordering, selection and
//! healing, and `Luma::workspace` stays a plain `Tabs<Body>` — the set on
//! screen. This module holds only the sets that are *not* on screen, and
//! [`ParkedTabs::focus`] swaps one for the other.
//!
//! That is why every call site that asks the workspace something needs no
//! venue: by the time it runs, the live set is already the right one.
//!
//! # Parked is not closed
//!
//! Switching venue parks a set; it does not tear it down. Closing a tab still
//! runs `Luma::teardown`, and switching venues still runs nothing — a turn
//! running in a parked tab's chat keeps running. The gesture that *does* hand
//! bodies back is the one where a tab's subject went away — see
//! [`ParkedTabs::retain`].

use std::collections::HashMap;

use crate::tabs::{Tab, Tabs, Target};

/// The tab sets that are not on screen, keyed by venue id.
pub(crate) struct ParkedTabs<B> {
    parked: HashMap<String, Tabs<B>>,
    /// The venue whose set is live right now. `None` before any venue is
    /// picked, which is the empty shell the venue picker opens over.
    current: Option<String>,
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
    /// The venue whose set is live.
    pub(crate) fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The body showing `target`, live or parked. Document replies and the
    /// agent's turns still belong to their tab while it is parked.
    pub(crate) fn body<'a>(&'a self, live: &'a Tabs<B>, target: &Target) -> Option<&'a B> {
        if self.current.as_deref() == Some(target.venue()) {
            return live.body(target);
        }
        self.parked.get(target.venue())?.body(target)
    }

    pub(crate) fn body_mut<'a>(
        &'a mut self,
        live: &'a mut Tabs<B>,
        target: &Target,
    ) -> Option<&'a mut B> {
        if self.current.as_deref() == Some(target.venue()) {
            return live.body_mut(target);
        }
        self.parked.get_mut(target.venue())?.body_mut(target)
    }

    /// Every tab set, the live one included, by venue.
    pub(crate) fn sets<'a>(
        &'a self,
        live: &'a Tabs<B>,
    ) -> impl Iterator<Item = (&'a str, &'a Tabs<B>)> {
        self.current
            .as_deref()
            .map(|venue| (venue, live))
            .into_iter()
            .chain(
                self.parked
                    .iter()
                    .map(|(venue, tabs)| (venue.as_str(), tabs)),
            )
    }

    /// Every open tab in every set.
    pub(crate) fn tabs<'a>(&'a self, live: &'a Tabs<B>) -> impl Iterator<Item = &'a Tab<B>> {
        self.sets(live).flat_map(|(_, tabs)| tabs.iter())
    }

    /// Put `venue`'s remembered tabs on screen, parking whatever was there.
    ///
    /// Returns whether anything moved, so a caller deriving the venue every
    /// frame can notify only when it actually changed. Asking for the venue
    /// that is already current is a no-op rather than a round trip through the
    /// map — otherwise every frame would park and unpark the live set, and any
    /// tab opened during that frame would be parked before it was ever drawn.
    pub(crate) fn focus(&mut self, venue: Option<String>, live: &mut Tabs<B>) -> bool {
        if venue == self.current {
            return false;
        }
        let arriving = venue
            .as_ref()
            .and_then(|venue| self.parked.remove(venue))
            .unwrap_or_default();
        let leaving = std::mem::replace(live, arriving);
        if let Some(previous) = self.current.take() {
            if !leaving.is_empty() {
                self.parked.insert(previous, leaving);
            }
        }
        self.current = venue;
        true
    }

    /// Close every tab `keep` rejects, in every set, handing each one back so
    /// the caller can run its teardown.
    ///
    /// This is the leak rule: a tab whose subject no longer exists — a deleted
    /// score, a track gone from the catalogue — has no gesture that could ever
    /// make it useful again, so nothing would otherwise drop it. The live set
    /// is included, because the subject can vanish while you are looking at it.
    pub(crate) fn retain(
        &mut self,
        live: &mut Tabs<B>,
        keep: impl Fn(&Target) -> bool,
    ) -> Vec<Tab<B>> {
        let mut dropped = live.retain(&keep);
        for tabs in self.parked.values_mut() {
            dropped.extend(tabs.retain(&keep));
        }
        self.parked.retain(|_, tabs| !tabs.is_empty());
        dropped
    }
}

impl crate::Luma {
    /// Keep the live set pointed at the sidebar's venue.
    ///
    /// Done at draw because a venue switch is a field assignment, and a
    /// gesture that forgot to ask would leave one venue's tabs on screen while
    /// the sidebar shows another.
    pub(crate) fn sync_venue_tabs(&mut self, cx: &mut gpui::Context<Self>) {
        let venue = self
            .sidebar
            .as_ref()
            .map(|browser| browser.venue_id().to_string());
        if venue.as_deref() != self.parked.current() {
            // Leaving a venue releases its songs' playback, the way leaving a
            // song does.
            self.park_track_audio(None, cx);
        }
        if self.parked.focus(venue, &mut self.workspace) {
            // The swapped-in set has its own active tab, so the keyboard is
            // owed to a different element than the frame before.
            cx.notify();
        }
        self.sync_track_audio(cx);
        self.refresh_agent_tabs(cx);
    }

    /// Close every tab `keep` rejects, in every venue, and run each one's
    /// teardown.
    pub(crate) fn close_tabs_where(
        &mut self,
        keep: impl Fn(&Target) -> bool,
        cx: &mut gpui::Context<Self>,
    ) {
        for tab in self.parked.retain(&mut self.workspace, keep) {
            self.teardown(tab.body, cx);
        }
        cx.notify();
    }

    /// Close the score tabs of tracks this venue no longer has.
    ///
    /// Called when a venue's rows land, which is the only moment the app
    /// learns a track is gone: "absent from the reloaded catalogue" *is* the
    /// deletion signal. Scoped to the one venue whose rows these are.
    pub(crate) fn prune_tracks(
        &mut self,
        venue_id: &str,
        live_tracks: &[String],
        cx: &mut gpui::Context<Self>,
    ) {
        self.close_tabs_where(
            |target| match target {
                Target::Score { venue, track, .. } if venue == venue_id => {
                    live_tracks.iter().any(|id| id == track)
                }
                _ => true,
            },
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(venue: &str, id: &str) -> Target {
        Target::Score {
            venue: venue.to_string(),
            track: format!("{id}-track"),
            score: id.to_string(),
        }
    }

    fn targets(tabs: &Tabs<&str>) -> Vec<Target> {
        tabs.iter().map(|tab| tab.target.clone()).collect()
    }

    fn focus(parked: &mut ParkedTabs<&'static str>, live: &mut Tabs<&'static str>, venue: &str) {
        parked.focus(Some(venue.to_string()), live);
    }

    #[test]
    fn a_venue_gets_its_own_tabs_back() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        focus(&mut parked, &mut live, "club");
        live.open(score("club", "a"), || "a");
        live.open(score("club", "b"), || "b");

        focus(&mut parked, &mut live, "hall");
        assert!(live.is_empty(), "the hall inherited the club's tabs");
        live.open(score("hall", "c"), || "c");

        focus(&mut parked, &mut live, "club");
        assert_eq!(targets(&live), vec![score("club", "a"), score("club", "b")]);
    }

    #[test]
    fn the_front_tab_comes_back_with_the_set() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        focus(&mut parked, &mut live, "club");
        live.open(score("club", "a"), || "a");
        live.open(score("club", "b"), || "b");
        live.select(&score("club", "a"));

        focus(&mut parked, &mut live, "hall");
        focus(&mut parked, &mut live, "club");
        assert_eq!(live.active(), Some(&score("club", "a")));
    }

    #[test]
    fn asking_for_the_current_venue_moves_nothing() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        assert!(parked.focus(Some("club".into()), &mut live));
        live.open(score("club", "a"), || "a");
        assert!(!parked.focus(Some("club".into()), &mut live));
        assert_eq!(targets(&live), vec![score("club", "a")]);
    }

    #[test]
    fn switching_venues_hands_back_nothing_to_tear_down() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        focus(&mut parked, &mut live, "club");
        live.open(score("club", "a"), || "turn running");
        focus(&mut parked, &mut live, "hall");
        focus(&mut parked, &mut live, "club");
        assert_eq!(live.active_body(), Some(&"turn running"));
    }

    #[test]
    fn a_parked_body_is_reachable_by_its_target() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        focus(&mut parked, &mut live, "club");
        live.open(score("club", "a"), || "loading");
        focus(&mut parked, &mut live, "hall");
        live.open(score("hall", "b"), || "b");

        *parked.body_mut(&mut live, &score("club", "a")).unwrap() = "loaded";
        assert_eq!(parked.body(&live, &score("club", "a")), Some(&"loaded"));
        assert_eq!(live.active_body(), Some(&"b"));
    }

    #[test]
    fn retain_reaches_every_set_and_forgets_emptied_ones() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        focus(&mut parked, &mut live, "club");
        live.open(score("club", "gone"), || "doomed");
        focus(&mut parked, &mut live, "hall");
        live.open(score("hall", "kept"), || "kept");
        live.open(score("hall", "also-gone"), || "doomed too");

        let gone = |target: &Target| match target {
            Target::Score { score, .. } => score.contains("gone"),
            Target::Venue { .. } => false,
        };
        let mut dropped: Vec<&str> = parked
            .retain(&mut live, |target| !gone(target))
            .into_iter()
            .map(|tab| tab.body)
            .collect();
        dropped.sort_unstable();
        assert_eq!(dropped, vec!["doomed", "doomed too"]);
        assert_eq!(targets(&live), vec![score("hall", "kept")]);
        assert_eq!(parked.sets(&live).count(), 1);
    }

    #[test]
    fn sets_lists_the_live_set_under_its_venue() {
        let mut parked = ParkedTabs::default();
        let mut live = Tabs::default();
        focus(&mut parked, &mut live, "club");
        live.open(score("club", "a"), || "a");
        focus(&mut parked, &mut live, "hall");
        live.open(score("hall", "b"), || "b");
        let mut venues: Vec<&str> = parked.sets(&live).map(|(venue, _)| venue).collect();
        venues.sort_unstable();
        assert_eq!(venues, vec!["club", "hall"]);
        assert_eq!(parked.tabs(&live).count(), 2);
    }
}
