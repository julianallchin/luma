//! The Luma library route: every track already imported, filtered as you
//! type, and what picking one does.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::*;
use luma_ui::dialog::morph::ContentMode;
use luma_ui::float::{self, RowState};
use luma_ui::node::{AgentNode, Instrument, Role};

use super::rows::{empty_body, loading_body, retry_body, track_row, track_title};
use super::{AddTracks, Purpose};
use crate::Luma;

impl Luma {
    /// A library row picked, by click or Enter: what that means depends on
    /// why the dialog is open.
    pub(super) fn pick_library_track(&mut self, track_id: String, cx: &mut Context<Self>) {
        let play = match self.overlay.as_open() {
            Some(crate::shell::Overlay::AddTracks(state)) => match &state.purpose {
                Purpose::PlayVirtual(target) => Some(target.clone()),
                Purpose::AddToVenue => None,
            },
            _ => None,
        };
        match play {
            Some(target) => {
                self.dismiss_overlay(cx);
                self.play_virtual(&target, &track_id, cx);
            }
            None => self.add_track_to_venue(track_id, cx),
        }
    }

    pub(super) fn refresh_open_track_browser(&mut self, cx: &mut Context<Self>) {
        let (instance_id, generation) = match self.overlay.open_mut() {
            Some(crate::shell::Overlay::AddTracks(state)) => {
                state.browser_generation = state
                    .browser_generation
                    .checked_add(1)
                    .expect("track browser generation exhausted");
                state.browser_loaded = false;
                state.browser_error = None;
                (state.instance_id, state.browser_generation)
            }
            _ => return,
        };
        let pending = self.library.all_tracks();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.instance_id != instance_id || state.browser_generation != generation {
                    return;
                }
                state.browser_loaded = true;
                match result {
                    Ok(rows) => {
                        state.browser_rows = rows.into();
                        state.refilter_browser();
                    }
                    Err(error) => state.browser_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Mirror an edit of the browser filter into the model, and put the row
    /// cursor back at the top — the row that was under it belonged to the
    /// previous result set.
    pub(super) fn add_tracks_filter_changed(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.browser_query = query;
            state.refilter_browser();
            state.browser_active = 0;
            state.browser_scroll.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
        }
    }

    fn add_track_to_venue(&mut self, track_id: String, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.as_open() else {
            return;
        };
        let instance_id = state.instance_id;
        let venue_id = state.venue_id.clone();
        let Some(venue_generation) = self
            .songs
            .as_ref()
            .filter(|songs| songs.venue_id() == venue_id)
            .map(crate::tracks::Tracks::load_generation)
        else {
            return;
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let pending = self
            .library
            .ensure_track_in_venue(&request_id, &track_id, &venue_id, None);
        cx.spawn(async move |this, cx| match pending.await {
            Ok(_) => {
                let Ok(rows) = this.read_with(cx, |this, _| this.library.tracks(&venue_id)) else {
                    return;
                };
                let rows = rows.await;
                this.update(cx, |this, cx| {
                    if let Ok(rows) = rows {
                        this.with_tracks_for_venue(&venue_id, venue_generation, cx, |songs| {
                            songs.replace_rows(rows)
                        });
                    }
                    // The track joined the venue, so the dialog has done its
                    // job — but only if this is still the same dialog.
                    if matches!(
                        this.overlay.as_open(),
                        Some(crate::shell::Overlay::AddTracks(state))
                            if state.instance_id == instance_id
                    ) {
                        this.close_overlay(cx);
                    }
                    cx.notify();
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() {
                        if state.instance_id != instance_id {
                            return;
                        }
                        state.browser_error = Some(error.to_string());
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }
}

pub(super) fn track_browser(
    state: &AddTracks,
    mode: ContentMode,
    app: &Entity<Luma>,
    cx: &mut gpui::App,
) -> AnyElement {
    let rows = Rc::clone(&state.browser_shown);
    if let Some(error) = &state.browser_error {
        return retry_body(format!("Failed to load tracks: {error}"), mode, app);
    }
    if !state.browser_loaded {
        return loading_body(
            "add-tracks-browser-skeleton",
            "Loading all tracks…",
            app.entity_id(),
            cx,
        );
    }
    if rows.is_empty() {
        // Distinct from the song list's "No tracks imported", which is about one
        // venue: this list is the whole library, and a driver (or a reader)
        // needs to be able to tell the two sentences apart.
        return empty_body(if state.browser_query.is_empty() {
            "No tracks in your library"
        } else {
            "No matching tracks"
        });
    }
    let active = state.browser_active;
    let scroll = state.browser_scroll.clone();
    let app = app.clone();
    float::viewport()
        .child(
            uniform_list("all-luma-tracks", rows.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let track = &rows[index];
                        let id = track.id.clone();
                        let title = track_title(track);
                        let app = app.clone();
                        let key = format!("add-track-{id}");
                        track_row(
                            RowState::of(false, index == active),
                            &key,
                            track.album_art_path.as_deref(),
                            &title,
                            track.artist.as_deref(),
                            false,
                        )
                        .id(SharedString::from(key.clone()))
                        .when(mode == ContentMode::Interactive, |row| {
                            row.on_click(move |_, _, cx| {
                                let id = id.clone();
                                app.update(cx, |this, cx| this.pick_library_track(id, cx));
                            })
                        })
                        .agent_node(Role::Row, title)
                        .agent_disabled(mode != ContentMode::Interactive)
                    })
                    .collect()
            })
            .track_scroll(&scroll)
            .size_full()
            .px(px(8.0)),
        )
        .into_any_element()
}
