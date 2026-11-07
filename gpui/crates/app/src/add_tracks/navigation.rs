//! Opening the dialog and driving it: which route is up, the palette's
//! keyboard, and the import-source menu.

use gpui::prelude::*;
use gpui::*;
use luma_ui::float;

use super::{AddTracks, ImportChoice, Purpose, Route};
use crate::Luma;

impl Luma {
    pub(crate) fn show_add_tracks(&mut self, cx: &mut Context<Self>) {
        self.open_add_tracks(Route::TrackBrowser, Purpose::AddToVenue, cx);
    }

    /// The same dialog, to choose a song for Perform's virtual deck.
    pub(crate) fn show_track_to_play(
        &mut self,
        target: crate::workspace::Target,
        cx: &mut Context<Self>,
    ) {
        self.open_add_tracks(Route::TrackBrowser, Purpose::PlayVirtual(target), cx);
    }

    /// The status bar's import item: the dialog, opened on the import's progress.
    pub(crate) fn show_track_import(&mut self, cx: &mut Context<Self>) {
        if let Some(activity) = &mut self.track_import {
            activity.seen = true;
        }
        self.open_add_tracks(Route::Progress, Purpose::AddToVenue, cx);
    }

    fn open_add_tracks(&mut self, route: Route, purpose: Purpose, cx: &mut Context<Self>) {
        let Some(venue_id) = self
            .songs
            .as_ref()
            .map(|tracks| tracks.venue_id().to_string())
        else {
            return;
        };
        let state = AddTracks::new(venue_id, route, purpose, cx);
        self.overlay
            .open(crate::shell::Overlay::AddTracks(Box::new(state)));
        cx.notify();
        self.refresh_open_track_browser(cx);
    }

    pub(super) fn add_tracks_route(&mut self, route: Route, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.request(route, cx);
        }
    }

    /// The palette's keyboard. Everything here acts on the *list*; text keys
    /// never reach it, because the search field binds those and only those.
    pub(super) fn add_tracks_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.as_open() else {
            return;
        };
        let route = *state.morph.target_key();
        // Read everything the borrow of `state` is needed for before the match
        // — every arm below takes `self` mutably.
        let query_empty = state.query_is_empty(route, cx);
        let menu_open = state.source_menu.is_open();

        match float::Nav::of(&event.keystroke, query_empty) {
            // Innermost first: escape closes the menu if one is open, and only
            // then the dialog — the same order the shell dismisses in.
            Some(float::Nav::Dismiss) if menu_open => self.toggle_source_menu(cx),
            Some(float::Nav::Dismiss) => self.dismiss_overlay(cx),
            Some(float::Nav::Back) => self.add_tracks_back(route, cx),
            Some(float::Nav::Step(delta)) => self.add_tracks_step(route, delta, cx),
            Some(float::Nav::Submit) => self.add_tracks_submit(route, cx),
            Some(float::Nav::Open) => self.add_tracks_activate(route, cx),
            None => {}
        }
    }

    /// Space in the source list: preview the cursor row. `false` — so the
    /// field types a space — when the person is typing a query rather than
    /// walking the list; see [`AddTracks::source_navigating`].
    pub(super) fn add_tracks_space(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.as_open() else {
            return false;
        };
        if *state.morph.target_key() != Route::SourceLibrary
            || !(state.source_navigating || state.source_filter.read(cx).is_empty())
        {
            return false;
        }
        let Some(id) = state
            .source_rows
            .get(state.source_active)
            .map(|track| track.id.clone())
        else {
            return false;
        };
        self.toggle_source_preview(&id, cx);
        true
    }

    /// Move the row cursor, wrapping, and keep it on screen.
    fn add_tracks_step(&mut self, route: Route, delta: isize, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        let count = state.row_count(route);
        if count == 0 {
            return;
        }
        let (active, scroll) = match route {
            Route::TrackBrowser => (&mut state.browser_active, &state.browser_scroll),
            Route::SourceLibrary => (&mut state.source_active, &state.source_scroll),
            Route::Progress => return,
        };
        *active = (*active as isize + delta).rem_euclid(count as isize) as usize;
        scroll.scroll_to_item(*active, ScrollStrategy::Nearest);
        if route == Route::SourceLibrary {
            state.source_navigating = true;
        }
        cx.notify();
    }

    /// Enter / → on the row under the cursor.
    fn add_tracks_activate(&mut self, route: Route, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.as_open() else {
            return;
        };
        match route {
            Route::TrackBrowser => {
                let Some(track) = state.browser_shown.get(state.browser_active).cloned() else {
                    return;
                };
                self.pick_library_track(track.id, cx);
            }
            // The source list is multi-select: activating a row toggles it,
            // and ⌘⏎ is what commits the set.
            Route::SourceLibrary => {
                let Some(id) = state
                    .source_rows
                    .get(state.source_active)
                    .map(|track| track.id.clone())
                else {
                    return;
                };
                self.toggle_source_track(&id, cx);
            }
            Route::Progress => {}
        }
    }

    /// ⌘⏎ — the route's one committing action.
    pub(super) fn add_tracks_submit(&mut self, route: Route, cx: &mut Context<Self>) {
        match route {
            // "Import" is a question — from where? — so the chord opens the
            // menu that asks it rather than guessing a source.
            Route::TrackBrowser if !self.picking_to_play() => self.toggle_source_menu(cx),
            Route::TrackBrowser => {}
            Route::SourceLibrary => self.import_source_selection(cx),
            Route::Progress => self.dismiss_overlay(cx),
        }
    }

    /// The dialog is open to choose a song to play, not to add one.
    fn picking_to_play(&self) -> bool {
        matches!(
            self.overlay.as_open(),
            Some(crate::shell::Overlay::AddTracks(state))
                if matches!(state.purpose, Purpose::PlayVirtual(_))
        )
    }

    /// Open or dismiss the import-source menu.
    pub(super) fn toggle_source_menu(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        state.source_menu.toggle();
        cx.notify();
    }

    /// Pick a source from the menu: the two DJ libraries open their browser
    /// route; files go straight to the platform picker, because there is no
    /// list to show first.
    pub(super) fn choose_import_source(&mut self, choice: ImportChoice, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.source_menu.close();
        }
        match choice {
            ImportChoice::EngineDj => self.choose_source(true, cx),
            ImportChoice::Rekordbox => self.choose_source(false, cx),
            ImportChoice::Files => self.import_track_files(false, cx),
            ImportChoice::Folder => self.import_track_files(true, cx),
        }
    }

    /// ← / ⌫-on-empty. At the root there is nowhere back to go, so the whole
    /// dialog closes — the same thing the gesture means one level down.
    fn add_tracks_back(&mut self, route: Route, cx: &mut Context<Self>) {
        match route.parent() {
            Some(parent) => self.add_tracks_route(parent, cx),
            None => self.dismiss_overlay(cx),
        }
    }
}
