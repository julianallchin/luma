//! What the window shows of a venue, as pure logic.
//!
//! # The window is a venue
//!
//! Everything on screen belongs to the open venue, and switching venue swaps
//! the window. A venue is worked on in three [`Mode`]s — Rig, Arrange and
//! Live — and each mode shows at most one thing: Rig the venue page, Live its
//! Perform set, Arrange the one open score. [`Modes`] is that per-venue state;
//! [`Venues`] keeps every venue's, the one on screen live and the rest parked.
//! See `docs/specs/comet-shell.md` §2.
//!
//! # A body is its target
//!
//! [`Target`] names what a mode body shows, and **it is also the body's
//! identity**. Opening a target that is already open reveals it; opening a
//! different score replaces the open one, and the replaced body is handed back
//! so the caller can run its teardown.
//!
//! # Why the body is a type parameter
//!
//! [`Modes`] is slots, selection and healing. It is not an editor. The shell
//! instantiates it over the enum of real editor states; the tests here
//! instantiate it over a `&str`, which lets every rule be a plain unit test.
//!
//! # Switching is not closing
//!
//! [`Modes::open`] (a replaced score) and [`Modes::retain`] (a subject that
//! went away) hand bodies **back**; nothing else does. Switching mode or venue tears down nothing:
//! playback continues, a loop region stays armed, a turn keeps running.

use std::collections::HashMap;

/// One of the three ways a venue is worked on. The window shows one at a time.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Mode {
    /// The room: the stage, the venue builder, fixtures, groups and patch.
    Rig,
    /// The open score's timeline under the stage.
    #[default]
    Arrange,
    /// The live show: the decks being followed, the grid and the faders.
    Live,
}

impl Mode {
    pub(crate) const ALL: [Mode; 3] = [Mode::Rig, Mode::Arrange, Mode::Live];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Mode::Rig => "Rig",
            Mode::Arrange => "Arrange",
            Mode::Live => "Live",
        }
    }

    /// The key that shows this mode (`keymap`'s `ShowRig` / `ShowArrange` /
    /// `ShowLive`).
    pub(crate) fn key(self) -> &'static str {
        match self {
            Mode::Rig => "1",
            Mode::Arrange => "2",
            Mode::Live => "3",
        }
    }
}

/// What a mode body shows.
///
/// Closed on purpose: persistence, the agent's targeted opens and the status
/// dots all enumerate this, and a variant missing from one of them would be a
/// body that opens nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Target {
    /// One score of one track, in the venue the score belongs to: Arrange.
    Score {
        venue: String,
        track: String,
        score: String,
    },
    /// One venue's stage, fixture inventory and groups: Rig.
    Venue { venue: String },
    /// One venue's live show: Live.
    Perform { venue: String },
}

impl Target {
    /// The venue whose state this body belongs to.
    pub(crate) fn venue(&self) -> &str {
        match self {
            Self::Score { venue, .. } | Self::Venue { venue } | Self::Perform { venue } => venue,
        }
    }

    /// The mode that shows it.
    pub(crate) fn mode(&self) -> Mode {
        match self {
            Self::Score { .. } => Mode::Arrange,
            Self::Venue { .. } => Mode::Rig,
            Self::Perform { .. } => Mode::Live,
        }
    }

    /// The key context this body's root declares, nested *inside* the
    /// workspace's own context — see [`crate::keymap`].
    pub(crate) fn key_context(&self) -> &'static str {
        match self {
            Self::Score { .. } => crate::keymap::context::TRACK_EDITOR,
            Self::Venue { .. } => crate::keymap::context::PATCH,
            Self::Perform { .. } => crate::keymap::context::PERFORM,
        }
    }
}

/// One open body: what it shows, and the state showing it.
#[derive(Debug)]
pub(crate) struct Slot<B> {
    pub(crate) target: Target,
    pub(crate) body: B,
}

/// One venue's window state: the mode on screen and each mode's body.
#[derive(Debug)]
pub(crate) struct Modes<B> {
    mode: Mode,
    rig: Option<Slot<B>>,
    arrange: Option<Slot<B>>,
    live: Option<Slot<B>>,
}

impl<B> Default for Modes<B> {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            rig: None,
            arrange: None,
            live: None,
        }
    }
}

impl<B> Modes<B> {
    fn slot(&self, mode: Mode) -> &Option<Slot<B>> {
        match mode {
            Mode::Rig => &self.rig,
            Mode::Arrange => &self.arrange,
            Mode::Live => &self.live,
        }
    }

    fn slot_mut(&mut self, mode: Mode) -> &mut Option<Slot<B>> {
        match mode {
            Mode::Rig => &mut self.rig,
            Mode::Arrange => &mut self.arrange,
            Mode::Live => &mut self.live,
        }
    }

    /// The mode on screen.
    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    /// Put `mode` on screen. Nothing is built or torn down: a mode whose slot
    /// is empty shows its empty state until something is opened into it.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Reveal `target`, building its body only if it is not already open, and
    /// put its mode on screen.
    ///
    /// The one entry point every open gesture comes through. A mode holds one
    /// body, so opening a second score *replaces* the first: the replaced slot
    /// is handed back for its teardown. `build` is a closure because a body is
    /// an editor, and constructing one to throw it away would re-read a
    /// document for nothing.
    pub(crate) fn open(&mut self, target: Target, build: impl FnOnce() -> B) -> Option<Slot<B>> {
        let mode = target.mode();
        self.mode = mode;
        let slot = self.slot_mut(mode);
        if slot.as_ref().is_some_and(|open| open.target == target) {
            return None;
        }
        let body = build();
        slot.replace(Slot { target, body })
    }

    /// Close every body `keep` rejects, handing each back for its teardown —
    /// a subject that no longer exists takes its body with it.
    pub(crate) fn retain(&mut self, keep: impl Fn(&Target) -> bool) -> Vec<Slot<B>> {
        let mut dropped = Vec::new();
        for mode in Mode::ALL {
            let slot = self.slot_mut(mode);
            if slot.as_ref().is_some_and(|open| !keep(&open.target)) {
                dropped.extend(slot.take());
            }
        }
        dropped
    }

    /// The target on screen: the current mode's body, if it has one.
    pub(crate) fn active(&self) -> Option<&Target> {
        self.slot(self.mode).as_ref().map(|slot| &slot.target)
    }

    pub(crate) fn active_body(&self) -> Option<&B> {
        self.slot(self.mode).as_ref().map(|slot| &slot.body)
    }

    pub(crate) fn active_body_mut(&mut self) -> Option<&mut B> {
        self.slot_mut(self.mode).as_mut().map(|slot| &mut slot.body)
    }

    /// The body `mode` shows, whether or not that mode is on screen.
    pub(crate) fn of_mode(&self, mode: Mode) -> Option<&Slot<B>> {
        self.slot(mode).as_ref()
    }

    /// Every open body, Rig first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Slot<B>> {
        [&self.rig, &self.arrange, &self.live]
            .into_iter()
            .filter_map(Option::as_ref)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// The body showing `target`, whether or not its mode is on screen.
    pub(crate) fn body(&self, target: &Target) -> Option<&B> {
        self.slot(target.mode())
            .as_ref()
            .filter(|slot| slot.target == *target)
            .map(|slot| &slot.body)
    }

    pub(crate) fn body_mut(&mut self, target: &Target) -> Option<&mut B> {
        self.slot_mut(target.mode())
            .as_mut()
            .filter(|slot| slot.target == *target)
            .map(|slot| &mut slot.body)
    }
}

/// Every venue's [`Modes`] that is not on screen, keyed by venue id.
///
/// The live one stays a plain `Modes<B>` on the shell, so every call site that
/// asks the workspace something needs no venue: by the time it runs, the live
/// state is already the right one. [`Venues::focus`] swaps them.
///
/// Parked is not closed: switching venue tears nothing down, and a turn
/// running in a parked body's chat keeps running.
pub(crate) struct Venues<B> {
    parked: HashMap<String, Modes<B>>,
    /// The venue on screen. `None` before any venue is picked.
    current: Option<String>,
}

impl<B> Default for Venues<B> {
    fn default() -> Self {
        Self {
            parked: HashMap::new(),
            current: None,
        }
    }
}

impl<B> Venues<B> {
    /// The venue on screen.
    pub(crate) fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The body showing `target`, live or parked. Document replies and the
    /// agent's turns still belong to their body while it is parked.
    pub(crate) fn body<'a>(&'a self, live: &'a Modes<B>, target: &Target) -> Option<&'a B> {
        if self.current.as_deref() == Some(target.venue()) {
            return live.body(target);
        }
        self.parked.get(target.venue())?.body(target)
    }

    pub(crate) fn body_mut<'a>(
        &'a mut self,
        live: &'a mut Modes<B>,
        target: &Target,
    ) -> Option<&'a mut B> {
        if self.current.as_deref() == Some(target.venue()) {
            return live.body_mut(target);
        }
        self.parked.get_mut(target.venue())?.body_mut(target)
    }

    /// Every venue's state, the live one included, by venue.
    pub(crate) fn sets<'a>(
        &'a self,
        live: &'a Modes<B>,
    ) -> impl Iterator<Item = (&'a str, &'a Modes<B>)> {
        self.current
            .as_deref()
            .map(|venue| (venue, live))
            .into_iter()
            .chain(
                self.parked
                    .iter()
                    .map(|(venue, modes)| (venue.as_str(), modes)),
            )
    }

    /// Every open body in every venue.
    pub(crate) fn slots<'a>(&'a self, live: &'a Modes<B>) -> impl Iterator<Item = &'a Slot<B>> {
        self.sets(live).flat_map(|(_, modes)| modes.iter())
    }

    /// Put `venue`'s state on screen, parking whatever was there.
    ///
    /// Returns whether anything moved, so a caller deriving the venue every
    /// frame can notify only when it changed. Asking for the venue already on
    /// screen moves nothing — otherwise every frame would park and unpark it.
    pub(crate) fn focus(&mut self, venue: Option<String>, live: &mut Modes<B>) -> bool {
        if venue == self.current {
            return false;
        }
        let arriving = venue
            .as_ref()
            .and_then(|venue| self.parked.remove(venue))
            .unwrap_or_default();
        let leaving = std::mem::replace(live, arriving);
        if let Some(previous) = self.current.take() {
            self.parked.insert(previous, leaving);
        }
        self.current = venue;
        true
    }

    /// Close every body `keep` rejects, in every venue, handing each back so
    /// the caller can run its teardown.
    ///
    /// The leak rule: a body whose subject no longer exists — a deleted score,
    /// a track gone from the catalogue — can never be useful again. The live
    /// state is included, because a subject can vanish while it is on screen.
    pub(crate) fn retain(
        &mut self,
        live: &mut Modes<B>,
        keep: impl Fn(&Target) -> bool,
    ) -> Vec<Slot<B>> {
        let mut dropped = live.retain(&keep);
        for modes in self.parked.values_mut() {
            dropped.extend(modes.retain(&keep));
        }
        dropped
    }
}

impl crate::Luma {
    /// Keep the live state pointed at the open venue.
    ///
    /// Done at draw because a venue switch is a field assignment, and a
    /// gesture that forgot to ask would leave one venue's bodies on screen
    /// under another's name.
    pub(crate) fn sync_venue_modes(&mut self, cx: &mut gpui::Context<Self>) {
        let venue = self
            .songs
            .as_ref()
            .map(|songs| songs.venue_id().to_string());
        if venue.as_deref() != self.venues.current() {
            // Leaving a venue releases its songs' playback, the way leaving a
            // song does.
            self.park_track_audio(None, cx);
        }
        if self.venues.focus(venue, &mut self.workspace) {
            // The swapped-in state has its own mode, so the keyboard is owed
            // to a different element than the frame before.
            cx.notify();
        }
        self.sync_track_audio(cx);
        self.refresh_agent_bodies(cx);
    }

    /// Close every body `keep` rejects, in every venue, and run each one's
    /// teardown.
    pub(crate) fn close_bodies_where(
        &mut self,
        keep: impl Fn(&Target) -> bool,
        cx: &mut gpui::Context<Self>,
    ) {
        for slot in self.venues.retain(&mut self.workspace, keep) {
            self.teardown(slot.body, cx);
        }
        cx.notify();
    }

    /// Close the open scores of tracks this venue no longer has.
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
        self.close_bodies_where(
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

    fn rig(venue: &str) -> Target {
        Target::Venue {
            venue: venue.to_string(),
        }
    }

    fn live(venue: &str) -> Target {
        Target::Perform {
            venue: venue.to_string(),
        }
    }

    fn focus(venues: &mut Venues<&'static str>, live: &mut Modes<&'static str>, venue: &str) {
        venues.focus(Some(venue.to_string()), live);
    }

    #[test]
    fn opening_a_target_puts_its_mode_on_screen() {
        let mut modes: Modes<&str> = Modes::default();
        modes.open(rig("club"), || "rig");
        assert_eq!(modes.mode(), Mode::Rig);
        assert_eq!(modes.active(), Some(&rig("club")));
        modes.open(live("club"), || "live");
        assert_eq!(modes.mode(), Mode::Live);
        assert_eq!(modes.active_body(), Some(&"live"));
    }

    #[test]
    fn reopening_the_open_target_builds_nothing() {
        let mut modes: Modes<&str> = Modes::default();
        modes.open(score("club", "a"), || "a");
        let replaced = modes.open(score("club", "a"), || panic!("an open target was rebuilt"));
        assert!(replaced.is_none());
    }

    #[test]
    fn a_second_score_replaces_the_first_and_hands_it_back() {
        let mut modes: Modes<&str> = Modes::default();
        modes.open(score("club", "a"), || "a");
        let replaced = modes.open(score("club", "b"), || "b").map(|slot| slot.body);
        assert_eq!(replaced, Some("a"));
        assert_eq!(modes.active(), Some(&score("club", "b")));
        assert_eq!(modes.iter().count(), 1);
    }

    #[test]
    fn each_mode_keeps_its_own_body_across_a_mode_switch() {
        let mut modes: Modes<&str> = Modes::default();
        modes.open(rig("club"), || "rig");
        modes.open(score("club", "a"), || "a");
        modes.open(live("club"), || "live");
        modes.set_mode(Mode::Arrange);
        assert_eq!(modes.active_body(), Some(&"a"));
        modes.set_mode(Mode::Rig);
        assert_eq!(modes.active_body(), Some(&"rig"));
        assert_eq!(modes.iter().count(), 3);
    }

    #[test]
    fn a_mode_with_nothing_open_shows_nothing() {
        let mut modes: Modes<&str> = Modes::default();
        modes.open(rig("club"), || "rig");
        modes.set_mode(Mode::Arrange);
        assert_eq!(modes.active(), None);
        assert!(!modes.is_empty());
    }

    #[test]
    fn switching_modes_hands_back_nothing_to_tear_down() {
        let mut modes: Modes<&str> = Modes::default();
        modes.open(score("club", "a"), || "transport running");
        modes.set_mode(Mode::Live);
        modes.set_mode(Mode::Arrange);
        assert_eq!(modes.active_body(), Some(&"transport running"));
    }

    #[test]
    fn a_venue_gets_its_own_mode_and_bodies_back() {
        let mut venues = Venues::default();
        let mut on_screen = Modes::default();
        focus(&mut venues, &mut on_screen, "club");
        on_screen.open(score("club", "a"), || "a");
        on_screen.open(live("club"), || "live");

        focus(&mut venues, &mut on_screen, "hall");
        assert!(on_screen.is_empty(), "the hall inherited the club's bodies");
        on_screen.open(rig("hall"), || "rig");

        focus(&mut venues, &mut on_screen, "club");
        assert_eq!(on_screen.mode(), Mode::Live);
        on_screen.set_mode(Mode::Arrange);
        assert_eq!(on_screen.active(), Some(&score("club", "a")));
    }

    #[test]
    fn a_venue_with_nothing_open_still_keeps_its_mode() {
        let mut venues = Venues::default();
        let mut on_screen: Modes<&str> = Modes::default();
        focus(&mut venues, &mut on_screen, "club");
        on_screen.set_mode(Mode::Rig);
        focus(&mut venues, &mut on_screen, "hall");
        focus(&mut venues, &mut on_screen, "club");
        assert_eq!(on_screen.mode(), Mode::Rig);
    }

    #[test]
    fn asking_for_the_current_venue_moves_nothing() {
        let mut venues = Venues::default();
        let mut on_screen = Modes::default();
        assert!(venues.focus(Some("club".into()), &mut on_screen));
        on_screen.open(score("club", "a"), || "a");
        assert!(!venues.focus(Some("club".into()), &mut on_screen));
        assert_eq!(on_screen.active(), Some(&score("club", "a")));
    }

    #[test]
    fn a_parked_body_is_reachable_by_its_target() {
        let mut venues = Venues::default();
        let mut on_screen = Modes::default();
        focus(&mut venues, &mut on_screen, "club");
        on_screen.open(score("club", "a"), || "loading");
        focus(&mut venues, &mut on_screen, "hall");
        on_screen.open(score("hall", "b"), || "b");

        *venues
            .body_mut(&mut on_screen, &score("club", "a"))
            .unwrap() = "loaded";
        assert_eq!(
            venues.body(&on_screen, &score("club", "a")),
            Some(&"loaded")
        );
        assert_eq!(on_screen.active_body(), Some(&"b"));
    }

    #[test]
    fn retain_reaches_every_venue() {
        let mut venues = Venues::default();
        let mut on_screen = Modes::default();
        focus(&mut venues, &mut on_screen, "club");
        on_screen.open(score("club", "gone"), || "doomed");
        on_screen.open(rig("club"), || "rig");
        focus(&mut venues, &mut on_screen, "hall");
        on_screen.open(score("hall", "also-gone"), || "doomed too");

        let gone = |target: &Target| match target {
            Target::Score { score, .. } => score.contains("gone"),
            Target::Venue { .. } | Target::Perform { .. } => false,
        };
        let mut dropped: Vec<&str> = venues
            .retain(&mut on_screen, |target| !gone(target))
            .into_iter()
            .map(|slot| slot.body)
            .collect();
        dropped.sort_unstable();
        assert_eq!(dropped, vec!["doomed", "doomed too"]);
        assert_eq!(venues.slots(&on_screen).count(), 1);
    }

    #[test]
    fn every_target_declares_a_distinct_key_context() {
        let contexts = [
            score("v", "a").key_context(),
            rig("v").key_context(),
            live("v").key_context(),
        ];
        let unique: std::collections::HashSet<&&str> = contexts.iter().collect();
        assert_eq!(unique.len(), contexts.len());
    }
}
