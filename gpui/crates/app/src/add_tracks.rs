//! The shared add-track dialog: Luma library, source choice, and one normalized
//! DJ-library browser. Route children own their descriptors; the morph reducer
//! owns geometry, choreography, and focus commitment.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::*;
use gpui_component::{scroll::ScrollableElement as _, Icon, IconName};
use luma_lib::models::tracks::{TrackBrowserRow, TrackImportPhase, TrackImportProgress};
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{ladder, Enabled};

use crate::{Luma, SourceLibrary, SourcePlaylist, SourceTrack, TrackImportRequest, TrackSource};

const BROWSER_SIZE: MorphSize = MorphSize::new(680.0, 480.0);
const SOURCE_PICKER_SIZE: MorphSize = MorphSize::new(420.0, 260.0);
const SOURCE_LIBRARY_SIZE: MorphSize = MorphSize::new(760.0, 520.0);
const ROW_HEIGHT: f32 = 46.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    TrackBrowser,
    ImportSource,
    SourceLibrary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceRowScope {
    All,
    Playlist(String),
    Search(String),
}

pub(crate) struct TrackImportActivity {
    generation: u64,
    import_id: Option<String>,
    progress: Option<TrackImportProgress>,
    active: bool,
    error: Option<String>,
}

impl Route {
    fn descriptor(self) -> RouteDescriptor<Self> {
        let size = match self {
            Self::TrackBrowser => BROWSER_SIZE,
            Self::ImportSource => SOURCE_PICKER_SIZE,
            Self::SourceLibrary => SOURCE_LIBRARY_SIZE,
        };
        RouteDescriptor::exact(self, size.width, size.height)
    }
}

pub(crate) struct AddTracks {
    instance_id: uuid::Uuid,
    venue_id: String,
    morph: MorphDialog<Route>,
    browser_rows: Rc<[TrackBrowserRow]>,
    browser_query: String,
    browser_loaded: bool,
    browser_error: Option<String>,
    source: Option<TrackSource>,
    source_library: Option<SourceLibrary>,
    playlists: Rc<[SourcePlaylist]>,
    source_scope: SourceRowScope,
    source_rows: Rc<[SourceTrack]>,
    source_loaded: bool,
    source_error: Option<String>,
    source_generation: u64,
    source_row_generation: u64,
    selected: HashSet<String>,
    browser_generation: u64,
    initial_focus_pending: bool,
    browser_focus: FocusHandle,
    picker_focus: FocusHandle,
    source_focus: FocusHandle,
}

impl AddTracks {
    fn new(venue_id: String, cx: &mut Context<Luma>) -> Self {
        Self {
            instance_id: uuid::Uuid::new_v4(),
            venue_id,
            morph: MorphDialog::new(Route::TrackBrowser.descriptor(), BROWSER_SIZE),
            browser_rows: Rc::from(Vec::new()),
            browser_query: String::new(),
            browser_loaded: false,
            browser_error: None,
            source: None,
            source_library: None,
            playlists: Rc::from(Vec::new()),
            source_scope: SourceRowScope::All,
            source_rows: Rc::from(Vec::new()),
            source_loaded: false,
            source_error: None,
            source_generation: 0,
            source_row_generation: 0,
            selected: HashSet::new(),
            browser_generation: 0,
            initial_focus_pending: true,
            browser_focus: cx.focus_handle().tab_stop(true),
            picker_focus: cx.focus_handle().tab_stop(true),
            source_focus: cx.focus_handle().tab_stop(true),
        }
    }

    fn request(&mut self, route: Route, cx: &mut Context<Luma>) {
        self.morph.request(
            route.descriptor(),
            Instant::now(),
            luma_ui::motion::reduced_motion(cx),
        );
        cx.notify();
    }

    fn browser_matches(&self) -> Vec<TrackBrowserRow> {
        let query = self.browser_query.trim().to_lowercase();
        self.browser_rows
            .iter()
            .filter(|track| {
                query.is_empty()
                    || [&track.title, &track.artist, &track.album]
                        .into_iter()
                        .flatten()
                        .any(|value| value.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    fn source_query(&self) -> &str {
        match &self.source_scope {
            SourceRowScope::Search(query) => query,
            SourceRowScope::All | SourceRowScope::Playlist(_) => "",
        }
    }

    fn active_playlist(&self) -> Option<&str> {
        match &self.source_scope {
            SourceRowScope::Playlist(id) => Some(id),
            SourceRowScope::All | SourceRowScope::Search(_) => None,
        }
    }

    fn begin_source_row_read(&mut self, scope: SourceRowScope) -> u64 {
        self.source_row_generation = self
            .source_row_generation
            .checked_add(1)
            .expect("source row generation exhausted");
        self.source_scope = scope;
        self.source_loaded = false;
        self.source_error = None;
        self.selected.clear();
        self.source_row_generation
    }
}

impl Luma {
    pub(crate) fn show_add_tracks(&mut self, cx: &mut Context<Self>) {
        let Some(venue_id) = self
            .sidebar
            .as_ref()
            .map(|tracks| tracks.venue_id().to_string())
        else {
            return;
        };
        let state = AddTracks::new(venue_id, cx);
        self.overlay = Some(crate::shell::Overlay::AddTracks(Box::new(state)));
        cx.notify();
        self.refresh_open_track_browser(cx);
    }

    fn refresh_open_track_browser(&mut self, cx: &mut Context<Self>) {
        let (instance_id, generation) = match &mut self.overlay {
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
                let Some(crate::shell::Overlay::AddTracks(state)) = &mut this.overlay else {
                    return;
                };
                if state.instance_id != instance_id || state.browser_generation != generation {
                    return;
                }
                state.browser_loaded = true;
                match result {
                    Ok(rows) => state.browser_rows = rows.into(),
                    Err(error) => state.browser_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add_tracks_route(&mut self, route: Route, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay {
            state.request(route, cx);
        }
    }

    fn add_tracks_search(&mut self, key: &Keystroke, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay else {
            return;
        };
        match key.key.as_str() {
            "backspace" => {
                state.browser_query.pop();
            }
            "escape" => state.browser_query.clear(),
            _ => {
                if let Some(value) = &key.key_char {
                    state.browser_query.push_str(value);
                }
            }
        }
        cx.notify();
    }

    fn choose_source(&mut self, engine: bool, cx: &mut Context<Self>) {
        let source = if engine {
            None
        } else {
            Some(TrackSource::Rekordbox)
        };
        if let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay {
            state.source_generation = state
                .source_generation
                .checked_add(1)
                .expect("source generation exhausted");
            state.source = source.clone();
            state.source_library = None;
            state.playlists = Rc::from(Vec::new());
            state.source_rows = Rc::from(Vec::new());
            state.begin_source_row_read(SourceRowScope::All);
            state.request(Route::SourceLibrary, cx);
        }
        let (instance_id, source_generation, row_generation) = match &self.overlay {
            Some(crate::shell::Overlay::AddTracks(state)) => (
                state.instance_id,
                state.source_generation,
                state.source_row_generation,
            ),
            _ => return,
        };
        let engine_source = engine.then(|| self.library.default_engine_dj_source());
        cx.spawn(async move |this, cx| {
            let source = match (source, engine_source) {
                (Some(source), None) => Ok(source),
                (None, Some(source)) => source.await,
                _ => unreachable!("one source choice"),
            };
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if let Some(crate::shell::Overlay::AddTracks(state)) = &mut this.overlay {
                            if state.instance_id != instance_id
                                || state.source_generation != source_generation
                            {
                                return;
                            }
                            state.source_loaded = true;
                            state.source_error = Some(error.to_string());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let Ok(library) =
                this.read_with(cx, |this, _| this.library.source_library(source.clone()))
            else {
                return;
            };
            let library = library.await;
            let Ok(playlists) =
                this.read_with(cx, |this, _| this.library.source_playlists(source.clone()))
            else {
                return;
            };
            let playlists = playlists.await;
            let Ok(tracks) =
                this.read_with(cx, |this, _| this.library.source_tracks(source.clone()))
            else {
                return;
            };
            let tracks = tracks.await;
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = &mut this.overlay else {
                    return;
                };
                if state.instance_id != instance_id || state.source_generation != source_generation
                {
                    return;
                }
                state.source = Some(source);
                match library {
                    Ok(library) => state.source_library = Some(library),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                match playlists {
                    Ok(playlists) => state.playlists = playlists.into(),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                if state.source_row_generation == row_generation {
                    state.source_loaded = true;
                    match tracks {
                        Ok(tracks) => state.source_rows = tracks.into(),
                        Err(error) => state.source_error = Some(error.to_string()),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_source_playlist(&mut self, playlist_id: Option<String>, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay else {
            return;
        };
        let Some(source) = state.source.clone() else {
            return;
        };
        let instance_id = state.instance_id;
        let source_generation = state.source_generation;
        let row_generation = state.begin_source_row_read(match &playlist_id {
            Some(id) => SourceRowScope::Playlist(id.clone()),
            None => SourceRowScope::All,
        });
        let all = playlist_id
            .is_none()
            .then(|| self.library.source_tracks(source.clone()));
        let playlist = playlist_id
            .as_deref()
            .map(|id| self.library.source_playlist_tracks(source, id));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match (all, playlist) {
                (Some(all), None) => all.await,
                (None, Some(playlist)) => playlist.await,
                _ => unreachable!("one source track read"),
            };
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = &mut this.overlay else {
                    return;
                };
                if state.instance_id != instance_id
                    || state.source_generation != source_generation
                    || state.source_row_generation != row_generation
                {
                    return;
                }
                state.source_loaded = true;
                match result {
                    Ok(rows) => state.source_rows = rows.into(),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn source_search(&mut self, key: &Keystroke, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay else {
            return;
        };
        let Some(source) = state.source.clone() else {
            return;
        };
        let instance_id = state.instance_id;
        let source_generation = state.source_generation;
        let mut query = state.source_query().to_string();
        match key.key.as_str() {
            "backspace" => {
                query.pop();
            }
            "escape" => query.clear(),
            _ => {
                if let Some(value) = &key.key_char {
                    query.push_str(value);
                }
            }
        }
        let row_generation = state.begin_source_row_read(if query.is_empty() {
            SourceRowScope::All
        } else {
            SourceRowScope::Search(query.clone())
        });
        let all = query
            .is_empty()
            .then(|| self.library.source_tracks(source.clone()));
        let search = (!query.is_empty()).then(|| self.library.search_source_tracks(source, &query));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match (all, search) {
                (Some(all), None) => all.await,
                (None, Some(search)) => search.await,
                _ => unreachable!("one source search read"),
            };
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = &mut this.overlay else {
                    return;
                };
                if state.instance_id != instance_id
                    || state.source_generation != source_generation
                    || state.source_row_generation != row_generation
                {
                    return;
                }
                state.source_loaded = true;
                match result {
                    Ok(rows) => state.source_rows = rows.into(),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_source_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay {
            if !state.selected.remove(track_id) {
                state.selected.insert(track_id.to_string());
            }
            cx.notify();
        }
    }

    fn import_source_selection(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = &mut self.overlay else {
            return;
        };
        let Some(source) = state.source.clone() else {
            return;
        };
        if state.selected.is_empty()
            || self
                .track_import
                .as_ref()
                .is_some_and(|activity| activity.active)
        {
            return;
        }
        let selected: Vec<String> = state.selected.iter().cloned().collect();
        state.source_error = None;
        self.next_track_import = self
            .next_track_import
            .checked_add(1)
            .expect("track import generation exhausted");
        let generation = self.next_track_import;
        self.track_import = Some(TrackImportActivity {
            generation,
            import_id: None,
            progress: None,
            active: true,
            error: None,
        });
        let mut progress = self.library.import_progress();
        let pending = self.library.import_tracks(TrackImportRequest::Source {
            source,
            track_ids: selected,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if let Some(activity) = &mut this.track_import {
                            if activity.generation == generation {
                                activity.active = false;
                                activity.error = Some(error.to_string());
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let import_id = result.import_id.clone();
            this.update(cx, |this, cx| {
                if let Some(activity) = &mut this.track_import {
                    if activity.generation == generation {
                        activity.import_id = Some(import_id.clone());
                        activity.error = (!result.failures.is_empty()).then(|| {
                            result
                                .failures
                                .iter()
                                .map(|failure| failure.message.as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        });
                    }
                }
                this.refresh_open_track_browser(cx);
                cx.notify();
            })
            .ok();
            loop {
                let event = match progress.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if event.import_id != import_id {
                    continue;
                }
                let complete = event.phase == TrackImportPhase::Complete;
                this.update(cx, |this, cx| {
                    if let Some(activity) = &mut this.track_import {
                        if activity.generation == generation {
                            activity.progress = Some(event.clone());
                            activity.active = !complete;
                            if let Some(error) = &event.error {
                                activity.error = Some(error.clone());
                            }
                        }
                    }
                    if complete {
                        this.refresh_open_track_browser(cx);
                    }
                    cx.notify();
                })
                .ok();
                if complete {
                    break;
                }
            }
        })
        .detach();
    }

    fn add_track_to_venue(&mut self, track_id: String, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = &self.overlay else {
            return;
        };
        let instance_id = state.instance_id;
        let venue_id = state.venue_id.clone();
        let Some(venue_generation) = self
            .sidebar
            .as_ref()
            .filter(|sidebar| sidebar.venue_id() == venue_id)
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
                        this.with_tracks_for_venue(&venue_id, venue_generation, cx, |sidebar| {
                            sidebar.replace_rows(rows)
                        });
                    }
                    if matches!(
                        &this.overlay,
                        Some(crate::shell::Overlay::AddTracks(state))
                            if state.instance_id == instance_id
                    ) {
                        this.overlay = None;
                    }
                    cx.notify();
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    if let Some(crate::shell::Overlay::AddTracks(state)) = &mut this.overlay {
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

pub(crate) fn tick(
    state: &mut AddTracks,
    dialog_focus: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<Luma>,
) {
    let now = Instant::now();
    if state.morph.tick(now, luma_ui::motion::reduced_motion(cx)) {
        window.request_animation_frame();
    }
    // Route controls are deliberately absent from paint-only morph copies.
    // Move focus to the host scope for the whole flight instead of leaving it
    // on the outgoing handle after that control has been unmounted. The
    // reducer hands the target route back only at commit, below.
    if state.morph.sample(now).animating {
        window.focus(dialog_focus, cx);
    }
    if std::mem::take(&mut state.initial_focus_pending) {
        let focus = state.browser_focus.clone();
        window.defer(cx, move |window, cx| window.focus(&focus, cx));
    }
    if let Some(route) = state.morph.take_focus_after_commit() {
        let focus = match route {
            Route::TrackBrowser => state.browser_focus.clone(),
            Route::SourceLibrary => state.source_focus.clone(),
            Route::ImportSource => state.picker_focus.clone(),
        };
        window.defer(cx, move |window, cx| window.focus(&focus, cx));
    }
}

pub(crate) fn render(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let sample = state.morph.sample(Instant::now());
    let app = app.clone();
    let card_app = app.clone();
    morph::card(&sample, "Add tracks dialog", move |route, mode| {
        route_content(state, activity, *route, mode, &card_app, window)
    })
}

fn route_content(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    match route {
        Route::TrackBrowser => track_browser(state, activity, mode, app, window).into_any_element(),
        Route::ImportSource => import_source(state, mode, app, window).into_any_element(),
        Route::SourceLibrary => {
            source_library(state, activity, mode, app, window).into_any_element()
        }
    }
}

fn toolbar(title: &str, back: Option<Route>, mode: ContentMode, app: &Entity<Luma>) -> Div {
    let mut bar = div()
        .h(px(46.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .bg(luma_ui::glass::band())
        .border_b_1()
        .border_color(luma_ui::glass::hairline(0.08));
    if let Some(route) = back {
        let app = app.clone();
        bar = bar.child(
            morph_button("Back", Enabled::Yes, mode)
                .id("add-tracks-back")
                .when(mode == ContentMode::Interactive, |button| {
                    button.on_click(move |_, _, cx| {
                        app.update(cx, |this, cx| this.add_tracks_route(route, cx))
                    })
                })
                .agent_node(Role::Button, "Back")
                .agent_disabled(mode != ContentMode::Interactive),
        );
    }
    let close = app.clone();
    bar.child(
        div()
            .text_size(px(14.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(title.to_string()),
    )
    .child(div().flex_1())
    .child(
        morph_button("Close", Enabled::Yes, mode)
            .id("add-tracks-close")
            .when(mode == ContentMode::Interactive, |button| {
                button
                    .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
            })
            .agent_node(Role::Button, "Close")
            .agent_disabled(mode != ContentMode::Interactive),
    )
}

fn import_activity(activity: &TrackImportActivity) -> Div {
    let phase = activity
        .progress
        .as_ref()
        .map(|progress| match progress.phase {
            TrackImportPhase::Importing => "importing",
            TrackImportPhase::Analyzing => "analyzing",
            TrackImportPhase::Complete => "complete",
        })
        .unwrap_or("importing");
    let phase_label = format!("Track import phase: {phase}");
    div()
        .h(px(30.0))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(16.0))
        .text_size(px(10.0))
        .text_color(ladder::muted_foreground())
        .child(
            div()
                .child(phase_label.clone())
                .agent_node(Role::Chip, phase_label),
        )
        .children(activity.progress.as_ref().map(|progress| {
            let count = format!(
                "Track import progress: {}/{}",
                progress.done, progress.total
            );
            div().child(count.clone()).agent_node(Role::Text, count)
        }))
        .children(activity.error.as_ref().map(|error| {
            let label = format!("Track import error: {error}");
            div()
                .text_color(ladder::danger())
                .child(label.clone())
                .agent_node(Role::Text, label)
        }))
}

fn track_browser(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let rows: Rc<[TrackBrowserRow]> = state.browser_matches().into();
    let search_text = if state.browser_query.is_empty() {
        "Search all tracks…"
    } else {
        &state.browser_query
    };
    let search = app.clone();
    let import = app.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(luma_ui::glass::dialog())
        .child(toolbar("Add tracks", None, mode, app))
        .children(activity.map(import_activity))
        .child(
            div()
                .id("all-track-search")
                .h(px(38.0))
                .mx(px(10.0))
                .mt(px(10.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(8.0))
                .bg(luma_ui::glass::wash(0.04))
                .border_1()
                .border_color(luma_ui::glass::hairline(0.08))
                .when(mode == ContentMode::Interactive, |field| {
                    field
                        .track_focus(&state.browser_focus)
                        .key_context(crate::keymap::context::TEXT_INPUT)
                        .on_key_down(move |event, _, cx| {
                            let key = event.keystroke.clone();
                            search.update(cx, |this, cx| this.add_tracks_search(&key, cx));
                        })
                })
                .child(search_text.to_string())
                .agent_node(Role::Input, "Search all tracks…")
                .agent_focused(
                    mode == ContentMode::Interactive && state.browser_focus.is_focused(window),
                ),
        )
        .child(match &state.browser_error {
            Some(error) => {
                luma_ui::plate(format!("Failed to load tracks: {error}"), ladder::danger())
            }
            None if !state.browser_loaded => luma_ui::plate(
                "Loading all tracks…".to_string(),
                ladder::muted_foreground(),
            ),
            None if rows.is_empty() && state.browser_query.is_empty() => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    primary_morph_button("Import tracks", Enabled::Yes, mode)
                        .id("add-tracks-import-empty")
                        .when(mode == ContentMode::Interactive, |button| {
                            button.on_click(move |_, _, cx| {
                                import.update(cx, |this, cx| {
                                    this.add_tracks_route(Route::ImportSource, cx)
                                })
                            })
                        })
                        .agent_node(Role::Button, "Import tracks")
                        .agent_disabled(mode != ContentMode::Interactive),
                )
                .into_any_element(),
            None if rows.is_empty() => {
                luma_ui::plate("No matching tracks".to_string(), ladder::muted_foreground())
            }
            None => all_track_rows(rows, mode, app),
        })
        .when(
            state.browser_loaded && !state.browser_rows.is_empty(),
            |root| {
                let import = app.clone();
                root.child(
                    div()
                        .h(px(54.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_end()
                        .px(px(16.0))
                        .bg(luma_ui::glass::band())
                        .border_t_1()
                        .border_color(luma_ui::glass::hairline(0.08))
                        .child(
                            primary_morph_button("Import tracks", Enabled::Yes, mode)
                                .id("add-tracks-import-bottom")
                                .when(mode == ContentMode::Interactive, |button| {
                                    button.on_click(move |_, _, cx| {
                                        import.update(cx, |this, cx| {
                                            this.add_tracks_route(Route::ImportSource, cx)
                                        })
                                    })
                                })
                                .agent_node(Role::Button, "Import tracks")
                                .agent_disabled(mode != ContentMode::Interactive),
                        ),
                )
            },
        )
}

fn all_track_rows(
    rows: Rc<[TrackBrowserRow]>,
    mode: ContentMode,
    app: &Entity<Luma>,
) -> AnyElement {
    let app = app.clone();
    uniform_list("all-luma-tracks", rows.len(), move |range, _, _| {
        range
            .map(|index| {
                let track = &rows[index];
                let id = track.id.clone();
                let app = app.clone();
                row_shell(&track_title(track), track.artist.as_deref())
                    .id(SharedString::from(format!("add-track-{id}")))
                    .when(mode == ContentMode::Interactive, |row| {
                        row.on_click(move |_, _, cx| {
                            let id = id.clone();
                            app.update(cx, |this, cx| this.add_track_to_venue(id, cx));
                        })
                    })
                    .agent_node(Role::Row, track_title(track))
                    .agent_disabled(mode != ContentMode::Interactive)
            })
            .collect()
    })
    .size_full()
    .into_any_element()
}

fn import_source(state: &AddTracks, mode: ContentMode, app: &Entity<Luma>, window: &Window) -> Div {
    let engine = app.clone();
    let rekordbox = app.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(luma_ui::glass::dialog())
        .child(toolbar(
            "Import source",
            Some(Route::TrackBrowser),
            mode,
            app,
        ))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .p(px(12.0))
                .child(
                    source_choice(
                        "Engine DJ",
                        "Browse playlists from an Engine DJ database",
                        IconName::FolderOpen,
                        mode,
                    )
                    .id("add-tracks-engine")
                    .when(mode == ContentMode::Interactive, |button| {
                        button.track_focus(&state.picker_focus)
                    })
                    .when(mode == ContentMode::Interactive, |button| {
                        button.on_click(move |_, _, cx| {
                            engine.update(cx, |this, cx| this.choose_source(true, cx))
                        })
                    })
                    .agent_node(Role::Button, "Engine DJ")
                    .agent_disabled(mode != ContentMode::Interactive)
                    .agent_focused(
                        mode == ContentMode::Interactive && state.picker_focus.is_focused(window),
                    ),
                )
                .child(
                    source_choice(
                        "Rekordbox",
                        "Open playlists and crates from Rekordbox",
                        IconName::BookOpen,
                        mode,
                    )
                    .id("add-tracks-rekordbox")
                    .when(mode == ContentMode::Interactive, |button| {
                        button.on_click(move |_, _, cx| {
                            rekordbox.update(cx, |this, cx| this.choose_source(false, cx))
                        })
                    })
                    .agent_node(Role::Button, "Rekordbox")
                    .agent_disabled(mode != ContentMode::Interactive),
                ),
        )
}

fn source_choice(title: &str, description: &str, icon: IconName, mode: ContentMode) -> Div {
    div()
        .h(px(76.0))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .bg(luma_ui::glass::wash(0.035))
        .when(mode == ContentMode::Interactive, |row| {
            row.tab_index(0)
                .cursor_pointer()
                .hover(|hover| hover.bg(luma_ui::glass::wash(0.08)))
        })
        .child(
            div()
                .size(px(38.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.0))
                .bg(luma_ui::glass::wash(0.07))
                .text_color(ladder::muted_foreground())
                .child(Icon::new(icon).size(px(17.0))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.0))
                        .text_color(ladder::muted_foreground())
                        .child(description.to_string()),
                ),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size(px(14.0))
                .text_color(ladder::muted_foreground()),
        )
}

fn source_library(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let search = app.clone();
    let import = app.clone();
    let title = match &state.source {
        Some(TrackSource::EngineDj { .. }) => "Engine DJ library",
        Some(TrackSource::Rekordbox) => "Rekordbox library",
        None => "Source library",
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(luma_ui::glass::dialog())
        .child(toolbar(title, Some(Route::ImportSource), mode, app))
        .children(activity.map(import_activity))
        .child(
            div()
                .h(px(38.0))
                .flex_none()
                .flex()
                .items_center()
                .px(px(16.0))
                .text_size(px(11.0))
                .text_color(ladder::muted_foreground())
                .child(
                    state
                        .source_library
                        .as_ref()
                        .map(|library| format!("{} tracks", library.track_count))
                        .unwrap_or_else(|| "Opening library…".to_string()),
                )
                .child(div().flex_1()),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(source_sidebar(state, mode, app))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .id("source-search")
                                .h(px(38.0))
                                .m(px(10.0))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .rounded(px(8.0))
                                .bg(luma_ui::glass::wash(0.04))
                                .border_1()
                                .border_color(luma_ui::glass::hairline(0.08))
                                .when(mode == ContentMode::Interactive, |field| {
                                    field
                                        .track_focus(&state.source_focus)
                                        .key_context(crate::keymap::context::TEXT_INPUT)
                                        .on_key_down(move |event, _, cx| {
                                            let key = event.keystroke.clone();
                                            search.update(cx, |this, cx| {
                                                this.source_search(&key, cx)
                                            });
                                        })
                                })
                                .child(if state.source_query().is_empty() {
                                    "Search source…".to_string()
                                } else {
                                    state.source_query().to_string()
                                })
                                .agent_node(Role::Input, "Search source…")
                                .agent_focused(
                                    mode == ContentMode::Interactive
                                        && state.source_focus.is_focused(window),
                                ),
                        )
                        .child(match &state.source_error {
                            Some(error) => {
                                luma_ui::plate(format!("Source error: {error}"), ladder::danger())
                            }
                            None if !state.source_loaded => luma_ui::plate(
                                "Loading source library…".to_string(),
                                ladder::muted_foreground(),
                            ),
                            None if state.source_rows.is_empty() => luma_ui::plate(
                                "No source tracks".to_string(),
                                ladder::muted_foreground(),
                            ),
                            None => source_rows(state, mode, app),
                        }),
                ),
        )
        .child(
            div()
                .h(px(54.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(12.0))
                .px(px(16.0))
                .bg(luma_ui::glass::band())
                .border_t_1()
                .border_color(luma_ui::glass::hairline(0.08))
                .child(format!("{} selected", state.selected.len()))
                .child(
                    primary_morph_button(
                        if activity.is_some_and(|activity| activity.active) {
                            "Importing…"
                        } else {
                            "Import selected"
                        },
                        if state.selected.is_empty()
                            || activity.is_some_and(|activity| activity.active)
                        {
                            Enabled::No
                        } else {
                            Enabled::Yes
                        },
                        mode,
                    )
                    .id("add-tracks-import-selected")
                    .when(
                        mode == ContentMode::Interactive
                            && !state.selected.is_empty()
                            && !activity.is_some_and(|activity| activity.active),
                        |button| {
                            button.on_click(move |_, _, cx| {
                                import.update(cx, |this, cx| this.import_source_selection(cx))
                            })
                        },
                    )
                    .agent_node(Role::Button, "Import selected")
                    .agent_disabled(
                        mode != ContentMode::Interactive
                            || state.selected.is_empty()
                            || activity.is_some_and(|activity| activity.active),
                    ),
                ),
        )
}

fn source_sidebar(state: &AddTracks, mode: ContentMode, app: &Entity<Luma>) -> impl IntoElement {
    let all = app.clone();
    div()
        .id("add-tracks-source-sidebar")
        .w(px(196.0))
        .flex_none()
        .overflow_y_scrollbar()
        .border_r_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .p(px(8.0))
        .gap(px(2.0))
        .child(
            row_shell("All tracks", None)
                .id("add-tracks-source-all")
                .when(state.active_playlist().is_none(), |row| {
                    row.bg(luma_ui::glass::wash(0.10))
                })
                .when(mode == ContentMode::Interactive, |row| {
                    row.on_click(move |_, _, cx| {
                        all.update(cx, |this, cx| this.select_source_playlist(None, cx))
                    })
                })
                .agent_node(Role::Row, "All tracks")
                .agent_disabled(mode != ContentMode::Interactive),
        )
        .children(state.playlists.iter().map(|playlist| {
            let app = app.clone();
            let id = playlist.id.clone();
            let label = playlist.name.clone();
            let active = state.active_playlist() == Some(id.as_str());
            row_shell(&label, Some(&format!("{} tracks", playlist.track_count)))
                .id(SharedString::from(format!("add-tracks-playlist-{id}")))
                .when(active, |row| row.bg(luma_ui::glass::wash(0.10)))
                .when(mode == ContentMode::Interactive, |row| {
                    row.on_click(move |_, _, cx| {
                        let id = id.clone();
                        app.update(cx, |this, cx| this.select_source_playlist(Some(id), cx));
                    })
                })
                .agent_node(Role::Row, label)
                .agent_disabled(mode != ContentMode::Interactive)
        }))
}

fn source_rows(state: &AddTracks, mode: ContentMode, app: &Entity<Luma>) -> AnyElement {
    let rows = Rc::clone(&state.source_rows);
    let selected = Rc::new(state.selected.clone());
    let app = app.clone();
    uniform_list("source-tracks", rows.len(), move |range, _, _| {
        range
            .map(|index| {
                let track = &rows[index];
                let id = track.id.clone();
                let checked = selected.contains(&id);
                let app = app.clone();
                row_shell(&source_title(track), track.artist.as_deref())
                    .id(SharedString::from(format!("add-tracks-source-track-{id}")))
                    .when(checked, |row| row.bg(luma_ui::glass::wash(0.10)))
                    .when(mode == ContentMode::Interactive, |row| {
                        row.on_click(move |_, _, cx| {
                            app.update(cx, |this, cx| this.toggle_source_track(&id, cx));
                        })
                    })
                    .agent_node(Role::Row, source_title(track))
                    .agent_disabled(mode != ContentMode::Interactive)
            })
            .collect()
    })
    .size_full()
    .into_any_element()
}

fn row_shell(title: &str, subtitle: Option<&str>) -> Div {
    div()
        .h(px(ROW_HEIGHT))
        .w_full()
        .px(px(10.0))
        .flex()
        .flex_col()
        .justify_center()
        .overflow_hidden()
        .rounded(px(8.0))
        .hover(|row| row.bg(luma_ui::glass::wash(0.06)))
        .child(
            div()
                .truncate()
                .text_size(px(12.0))
                .child(title.to_string()),
        )
        .children(subtitle.map(|subtitle| {
            div()
                .truncate()
                .text_size(px(10.0))
                .text_color(ladder::muted_foreground())
                .child(subtitle.to_string())
        }))
}

/// The morph's incoming and outgoing route copies are visual samples only.
/// `luma_button` makes an enabled control a tab stop even before an `on_click`
/// is attached, so those copies need a separate inert rendering rather than
/// merely omitting the listener.
fn morph_button(label: &str, enabled: Enabled, mode: ContentMode) -> Div {
    div()
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .text_size(px(13.0))
        .text_color(ladder::muted_foreground())
        .when(
            mode == ContentMode::Interactive && enabled == Enabled::Yes,
            |button| {
                button.tab_index(0).cursor_pointer().hover(|state| {
                    state
                        .bg(luma_ui::glass::wash(0.08))
                        .text_color(ladder::foreground())
                })
            },
        )
        .when(enabled == Enabled::No, |button| {
            button.opacity(ladder::DISABLED_OPACITY)
        })
        .child(label.to_string())
}

fn primary_morph_button(label: &str, enabled: Enabled, mode: ContentMode) -> Div {
    div()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .bg(ladder::foreground())
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(ladder::background())
        .when(
            mode == ContentMode::Interactive && enabled == Enabled::Yes,
            |button| {
                button
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(|state| state.opacity(0.9))
            },
        )
        .when(enabled == Enabled::No, |button| button.opacity(0.38))
        .child(label.to_string())
}

fn track_title(track: &TrackBrowserRow) -> String {
    track
        .title
        .clone()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            std::path::Path::new(&track.file_path)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled")
                .to_string()
        })
}

fn source_title(track: &SourceTrack) -> String {
    track
        .title
        .clone()
        .or_else(|| track.filename.clone())
        .unwrap_or_else(|| "Untitled".to_string())
}
