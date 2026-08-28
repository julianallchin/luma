//! The shared add-track dialog: Luma library, source choice, and one normalized
//! DJ-library browser.
//!
//! # One palette, three routes
//!
//! Every route here is the same object — a header band carrying the filter and
//! the submit chip, a list, and a footer legend — and switching route changes
//! only what is *in* it. That is why all three share [`PALETTE_SIZE`]: a
//! picker that resized as you moved through it would read as three dialogs
//! taking turns, and the morph would spend its whole span animating the frame
//! instead of the content.
//!
//! The body is never given a height. It takes what the card has left after the
//! two bands, which makes "the body does not resize when the list goes sparse,
//! empty, or back to loading" true by construction rather than by a constant
//! someone has to keep in sync.
//!
//! # Navigator first
//!
//! The filter field holds focus, but it does not own the keyboard. Arrows,
//! Enter and Backspace-on-empty act on the *list*; only text keys reach the
//! field. This works because [`luma_ui::text_input::Mode::Search`] deliberately
//! leaves navigation keys unbound, so they bubble out of the field to the
//! card's own `on_key_down` — see [`Luma::add_tracks_key`].

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::*;
use gpui_component::{Icon, IconName};
use luma_lib::models::tracks::{TrackBrowserRow, TrackImportPhase, TrackImportProgress};
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float::{self, RowState};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::{glass, ladder};

use crate::{Luma, SourceLibrary, SourcePlaylist, SourceTrack, TrackImportRequest, TrackSource};

/// The one size every route takes — see the module docs. 680 wide is comet's
/// palette grammar; the height is the two bands plus a body deep enough for
/// eight rows, so a filtered list rarely needs the scrollbar to make sense.
const PALETTE_SIZE: MorphSize = MorphSize::new(680.0, 416.0);
/// Row height of the track lists. `uniform_list` virtualizes on a fixed row
/// height, so this is a layout fact rather than a style choice.
const ROW_HEIGHT: f32 = 44.0;
/// Album-art thumbnail edge. Sized to leave the row's padding either side of
/// it rather than to a round number.
const ART_SIZE: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    TrackBrowser,
    SourceLibrary,
}

/// Where a track can come from. A closed vocabulary, listed once: the menu's
/// rows, their labels and what each one does are all derived from it, so a
/// fourth source is one variant rather than four edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportChoice {
    EngineDj,
    Rekordbox,
    Files,
}

impl ImportChoice {
    const ALL: [Self; 3] = [Self::EngineDj, Self::Rekordbox, Self::Files];

    fn label(self) -> &'static str {
        match self {
            Self::EngineDj => "Engine DJ",
            Self::Rekordbox => "Rekordbox",
            Self::Files => "Files…",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::EngineDj => "Playlists from an Engine DJ database",
            Self::Rekordbox => "Playlists and crates from Rekordbox",
            Self::Files => "Audio files from this computer",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::EngineDj => "add-tracks-engine",
            Self::Rekordbox => "add-tracks-rekordbox",
            Self::Files => "add-tracks-files",
        }
    }
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
        RouteDescriptor::exact(self, PALETTE_SIZE.width, PALETTE_SIZE.height)
    }

    /// Where "back" goes from here — `None` at the root.
    fn parent(self) -> Option<Self> {
        match self {
            Self::TrackBrowser => None,
            Self::SourceLibrary => Some(Self::TrackBrowser),
        }
    }
}

pub(crate) struct AddTracks {
    instance_id: uuid::Uuid,
    venue_id: String,
    morph: MorphDialog<Route>,
    browser_rows: Rc<[TrackBrowserRow]>,
    /// [`Self::browser_rows`] narrowed to the filter, kept rather than derived.
    ///
    /// Deriving it per read meant walking and cloning the whole library on
    /// every frame, for every layer — and a route morph builds two. That is
    /// work proportional to the library on a path whose whole point is that
    /// `uniform_list` makes it proportional to the *viewport*: the list
    /// virtualizes what it renders, but nothing virtualizes what it is handed.
    /// The filter changes on keystrokes; the frames in between are free.
    browser_shown: Rc<[TrackBrowserRow]>,
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
    /// The source-choice route's first control. The other two routes focus
    /// their filter field instead, which owns its own handle.
    picker_focus: FocusHandle,
    /// One handle per header control. A `tab_index` alone makes an element a
    /// stop that cannot *hold* focus — gpui then settles focus on the nearest
    /// focusable ancestor (the dialog container) and the ring reads as broken.
    back_focus: FocusHandle,
    submit_focus: FocusHandle,
    close_focus: FocusHandle,
    /// The filter fields. A route renders its own; the one that is mounted
    /// takes focus, and the keys it leaves unbound reach [`Luma::add_tracks_key`].
    browser_filter: Entity<TextInput>,
    source_filter: Entity<TextInput>,
    /// Each field's own focus handle, taken once at construction.
    ///
    /// Render sees a `&Window` and no `&App`, and a field's handle lives
    /// inside its entity — so the handle is captured here, where the context
    /// to read it exists, rather than dragging one through every render
    /// signature to answer a question that never changes.
    browser_filter_focus: FocusHandle,
    source_filter_focus: FocusHandle,
    /// The typed query, mirrored out of the field above on every edit.
    ///
    /// The field is the editor and this is the model: filtering, the row
    /// cursor and the async source reads all run off a plain `String`, and
    /// nothing below has to reach into an entity (or hold an `&App`) to know
    /// what was typed. [`Luma::add_tracks_filter_changed`] is the only writer.
    browser_query: String,
    /// Which row the arrow keys are on, per list. Not a selection — see
    /// [`RowState`]; the browser has no selection at all and the source list's
    /// selection is [`Self::selected`].
    browser_active: usize,
    source_active: usize,
    browser_scroll: UniformListScrollHandle,
    source_scroll: UniformListScrollHandle,
    /// The import-source menu hanging off the header chip. A [`Popup`] so it
    /// leaves the same way a dialog does, rather than blinking out.
    source_menu: luma_ui::dialog::Popup<()>,
    /// Kept so the field subscriptions die with the dialog rather than firing
    /// into an overlay that has closed.
    _filter_subscriptions: [Subscription; 2],
}

impl AddTracks {
    fn new(venue_id: String, cx: &mut Context<Luma>) -> Self {
        let browser_filter = cx.new(|cx| TextInput::search("Search all tracks…", cx));
        let source_filter = cx.new(|cx| TextInput::search("Search source…", cx));
        let browser_filter_focus = browser_filter.read(cx).focus_handle(cx);
        let source_filter_focus = source_filter.read(cx).focus_handle(cx);
        // Edits are the only path from a field back into this state; every
        // other event a field emits is a caret or viewport move, which only
        // needs a repaint.
        let subscriptions = [
            cx.subscribe(&browser_filter, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let query = field.read(cx).text().to_string();
                    luma.add_tracks_filter_changed(query, cx);
                } else {
                    cx.notify();
                }
            }),
            cx.subscribe(&source_filter, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let query = field.read(cx).text().to_string();
                    luma.source_filter_changed(query, cx);
                } else {
                    cx.notify();
                }
            }),
        ];
        Self {
            instance_id: uuid::Uuid::new_v4(),
            venue_id,
            morph: MorphDialog::new(Route::TrackBrowser.descriptor(), PALETTE_SIZE),
            browser_rows: Rc::from(Vec::new()),
            browser_shown: Rc::from(Vec::new()),
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
            picker_focus: cx.focus_handle().tab_stop(true),
            back_focus: cx.focus_handle().tab_stop(true),
            submit_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            browser_filter,
            source_filter,
            browser_filter_focus,
            source_filter_focus,
            browser_query: String::new(),
            browser_active: 0,
            source_active: 0,
            browser_scroll: UniformListScrollHandle::new(),
            source_scroll: UniformListScrollHandle::new(),
            source_menu: luma_ui::dialog::Popup::default(),
            _filter_subscriptions: subscriptions,
        }
    }

    /// The filter field the mounted route puts focus in.
    fn active_filter(&self) -> Option<&Entity<TextInput>> {
        match self.morph.target_key() {
            Route::TrackBrowser => Some(&self.browser_filter),
            Route::SourceLibrary => Some(&self.source_filter),
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

    /// Recompute [`Self::browser_shown`]. Call wherever the library or the
    /// filter changes — the two writes are the whole contract.
    ///
    /// An empty query shares the backing rows instead of copying them, so the
    /// common case costs a refcount rather than a library.
    fn refilter_browser(&mut self) {
        let query = self.browser_query.trim().to_lowercase();
        self.browser_shown = if query.is_empty() {
            Rc::clone(&self.browser_rows)
        } else {
            self.browser_rows
                .iter()
                .filter(|track| {
                    [&track.title, &track.artist, &track.album]
                        .into_iter()
                        .flatten()
                        .any(|value| value.to_lowercase().contains(&query))
                })
                .cloned()
                .collect()
        };
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

    /// How many rows the arrow keys can walk on `route`.
    fn row_count(&self, route: Route) -> usize {
        match route {
            Route::TrackBrowser => self.browser_shown.len(),
            Route::SourceLibrary => self.source_rows.len(),
        }
    }

    /// Whether `route`'s filter field is empty — what decides if Backspace
    /// edits text or steps back out of the route.
    fn query_is_empty(&self, route: Route, cx: &App) -> bool {
        match route {
            Route::TrackBrowser => self.browser_filter.read(cx).is_empty(),
            Route::SourceLibrary => self.source_filter.read(cx).is_empty(),
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
        self.overlay
            .open(crate::shell::Overlay::AddTracks(Box::new(state)));
        cx.notify();
        self.refresh_open_track_browser(cx);
    }

    fn refresh_open_track_browser(&mut self, cx: &mut Context<Self>) {
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

    fn add_tracks_route(&mut self, route: Route, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.request(route, cx);
        }
    }

    /// Mirror an edit of the browser filter into the model, and put the row
    /// cursor back at the top — the row that was under it belonged to the
    /// previous result set.
    fn add_tracks_filter_changed(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.browser_query = query;
            state.refilter_browser();
            state.browser_active = 0;
            state.browser_scroll.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// The palette's keyboard. Everything here acts on the *list*; text keys
    /// never reach it, because the search field binds those and only those.
    fn add_tracks_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.as_open() else {
            return;
        };
        let route = *state.morph.target_key();
        // Read everything the borrow of `state` is needed for before the match
        // — every arm below takes `self` mutably.
        let query_empty = state.query_is_empty(route, cx);
        let menu_open = state.source_menu.is_open();
        let key = event.keystroke.key.as_str();
        let modified = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;

        match key {
            // Innermost first: escape closes the menu if one is open, and only
            // then the dialog — the same order the shell dismisses in.
            "escape" if menu_open => self.toggle_source_menu(cx),
            "escape" => self.dismiss_overlay(cx),
            "left" => self.add_tracks_back(route, cx),
            // ⌫ on an empty query is the same gesture as ←: the palette is a
            // navigator, and a query short enough to type is short enough to
            // delete your way out of.
            "backspace" if query_empty => self.add_tracks_back(route, cx),
            "up" => self.add_tracks_step(route, -1, cx),
            "down" => self.add_tracks_step(route, 1, cx),
            "enter" if modified => self.add_tracks_submit(route, cx),
            "enter" | "right" => self.add_tracks_activate(route, cx),
            _ => {}
        }
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
        };
        *active = (*active as isize + delta).rem_euclid(count as isize) as usize;
        scroll.scroll_to_item(*active, ScrollStrategy::Nearest);
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
                self.add_track_to_venue(track.id, cx);
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
        }
    }

    /// ⌘⏎ — the route's one committing action.
    fn add_tracks_submit(&mut self, route: Route, cx: &mut Context<Self>) {
        match route {
            // "Import" is a question — from where? — so the chord opens the
            // menu that asks it rather than guessing a source.
            Route::TrackBrowser => self.toggle_source_menu(cx),
            Route::SourceLibrary => self.import_source_selection(cx),
        }
    }

    /// Open or dismiss the import-source menu.
    fn toggle_source_menu(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        if state.source_menu.is_open() {
            state.source_menu.begin_close(cx);
        } else {
            state.source_menu.open(());
        }
        cx.notify();
    }

    /// Pick a source from the menu: the two DJ libraries open their browser
    /// route; files go straight to the platform picker, because there is no
    /// list to show first.
    fn choose_import_source(&mut self, choice: ImportChoice, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.source_menu.begin_close(cx);
        }
        match choice {
            ImportChoice::EngineDj => self.choose_source(true, cx),
            ImportChoice::Rekordbox => self.choose_source(false, cx),
            ImportChoice::Files => self.import_track_files(cx),
        }
    }

    /// The platform's own file picker, then the same import pipeline the DJ
    /// sources feed. A cancelled prompt is not an error: it is a person
    /// changing their mind, and it leaves the dialog exactly as it was.
    fn import_track_files(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                this.begin_track_import(TrackImportRequest::Files(paths), cx)
            })
            .ok();
        })
        .detach();
    }

    /// ← / ⌫-on-empty. At the root there is nowhere back to go, so the whole
    /// dialog closes — the same thing the gesture means one level down.
    fn add_tracks_back(&mut self, route: Route, cx: &mut Context<Self>) {
        match route.parent() {
            Some(parent) => self.add_tracks_route(parent, cx),
            None => self.dismiss_overlay(cx),
        }
    }

    fn choose_source(&mut self, engine: bool, cx: &mut Context<Self>) {
        let source = if engine {
            None
        } else {
            Some(TrackSource::Rekordbox)
        };
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
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
        let (instance_id, source_generation, row_generation) = match self.overlay.as_open() {
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
                        if let Some(crate::shell::Overlay::AddTracks(state)) =
                            this.overlay.open_mut()
                        {
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
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
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
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
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
        // The scope that was just chosen replaces the filter, so the field has
        // to stop showing a query that is no longer being applied. The echo
        // this provokes is absorbed by `source_filter_changed`'s guard.
        let filter = state.source_filter.clone();
        state.source_active = 0;
        filter.update(cx, |field, cx| field.set_text("", cx));
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
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
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

    /// The source library filters server-side, so an edit starts a fresh read
    /// rather than narrowing what is already in hand.
    fn source_filter_changed(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        // An edit that lands on the query already in force is not a new read.
        // This is what keeps `select_source_playlist`'s field-clear from
        // starting a second, racing read — and it is also the right rule on
        // its own: emptying the filter while a playlist is chosen should keep
        // that playlist, not silently widen to every track in the source.
        if query == state.source_query() {
            return;
        }
        let Some(source) = state.source.clone() else {
            return;
        };
        let instance_id = state.instance_id;
        let source_generation = state.source_generation;
        state.source_active = 0;
        state.source_scroll.scroll_to_item(0, ScrollStrategy::Top);
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
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
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
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            if !state.selected.remove(track_id) {
                state.selected.insert(track_id.to_string());
            }
            cx.notify();
        }
    }

    fn import_source_selection(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
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
        self.begin_track_import(
            TrackImportRequest::Source {
                source,
                track_ids: selected,
            },
            cx,
        );
    }

    /// Start `request` and follow it to completion.
    ///
    /// One path for every kind of import — a DJ library's selection and a
    /// folder of files differ only in what they name, and the progress,
    /// failure reporting and browser refresh after them are the same job.
    fn begin_track_import(&mut self, request: TrackImportRequest, cx: &mut Context<Self>) {
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
        let pending = self.library.import_tracks(request);
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
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.as_open() else {
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
    // The source menu leaves the same way the dialog does, and is reaped from
    // the same frame — see `Popup::tick_close`.
    if state.source_menu.tick_close() {
        window.request_animation_frame();
    }
    // Route controls are deliberately absent from paint-only morph copies.
    // Move focus to the host scope for the whole flight instead of leaving it
    // on the outgoing handle after that control has been unmounted. The
    // reducer hands the target route back only at commit, below.
    if state.morph.sample(now).animating {
        window.focus(dialog_focus, cx);
        return;
    }
    let arriving = state.morph.take_focus_after_commit();
    if std::mem::take(&mut state.initial_focus_pending) || arriving.is_some() {
        focus_route(state, window, cx);
    }
}

/// Put focus where the mounted route wants typing to land: its filter field if
/// it has one, its first control otherwise.
///
/// Deferred because the arriving route was only just committed — its elements
/// do not exist until the frame this schedules.
fn focus_route(state: &AddTracks, window: &mut Window, cx: &mut Context<Luma>) {
    let focus = match state.active_filter() {
        Some(filter) => filter.read(cx).focus_handle(cx),
        None => state.picker_focus.clone(),
    };
    window.defer(cx, move |window, cx| window.focus(&focus, cx));
}

pub(crate) fn render(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(Instant::now());
    let app = app.clone();
    let card_app = app.clone();
    morph::card(&sample, "Add tracks dialog", move |route, mode| {
        route_content(state, activity, *route, mode, &card_app, window, cx)
    })
}

#[allow(clippy::too_many_arguments)]
fn route_content(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let body = match route {
        Route::TrackBrowser => track_browser(state, mode, app, cx),
        Route::SourceLibrary => source_library(state, mode, app, cx),
    };
    palette(state, activity, route, mode, app, window, body)
}

// ---------------------------------------------------------------------------
// The palette frame
// ---------------------------------------------------------------------------

/// Header band, body, footer legend — the shape every route wears.
///
/// The body is handed no height: it takes whatever the card has left, which is
/// what keeps a loading, empty or filtered list from resizing the dialog.
#[allow(clippy::too_many_arguments)]
fn palette(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
    body: AnyElement,
) -> AnyElement {
    let interactive = mode == ContentMode::Interactive;
    let keys = app.clone();
    let mut frame = div().size_full().flex().flex_col().overflow_hidden();
    if interactive {
        // No `track_focus`: the dialog host already owns this card's focus
        // trap and tab group, and a second focus container inside it would add
        // a stop carrying no control. `on_key_down` fires for anything that
        // bubbles through, which is every key the focused field leaves unbound.
        frame = frame.on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.add_tracks_key(&event, cx));
        });
    }
    frame
        .child(header(state, route, mode, app, window))
        .child(div().flex_1().min_h_0().flex().child(body))
        .child(footer(activity, route))
        .into_any_element()
}

fn header(
    state: &AddTracks,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let interactive = mode == ContentMode::Interactive;
    let mut band = float::header_band();

    // Back sits where ← would take you, at the leading edge.
    if let Some(parent) = route.parent() {
        let back = app.clone();
        let cap = if interactive {
            float::key_cap_pressable(float::key_cap())
        } else {
            float::key_cap()
        };
        band = band.child(
            cap.id("add-tracks-back")
                .when(interactive, |cap| {
                    cap.track_focus(&state.back_focus)
                        .tab_index(0)
                        .on_click(move |_, _, cx| {
                            back.update(cx, |this, cx| this.add_tracks_route(parent, cx))
                        })
                })
                .child(Icon::new(IconName::ArrowLeft).size(px(12.5)))
                .agent_node(Role::Button, "Back")
                .agent_disabled(!interactive)
                .agent_focused(interactive && state.back_focus.is_focused(window)),
        );
    }

    band = band.child(filter_field(state, route, mode, window));

    if let Some(chip) = submit_chip(state, route, mode, app, window) {
        band = band.child(chip);
    }

    let close = app.clone();
    let cap = if interactive {
        float::key_cap_pressable(float::key_cap())
    } else {
        float::key_cap()
    };
    band.child(
        cap.id("add-tracks-close")
            .when(interactive, |cap| {
                cap.track_focus(&state.close_focus)
                    .tab_index(0)
                    .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
            })
            .child("esc")
            .agent_node(Role::Button, "Close")
            .agent_disabled(!interactive)
            .agent_focused(interactive && state.close_focus.is_focused(window)),
    )
}

/// The filter field, or the route's title where there is nothing to filter.
///
/// A paint-only morph copy renders the typed text as plain glyphs instead of
/// the field: a live [`TextInput`] would register a focus handle, and the
/// morph contract says an in-flight copy owns none.
fn filter_field(state: &AddTracks, route: Route, mode: ContentMode, window: &Window) -> AnyElement {
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    let (field, placeholder, query) = match route {
        Route::TrackBrowser => (
            Some(&state.browser_filter),
            "Search all tracks…",
            state.browser_query.as_str(),
        ),
        Route::SourceLibrary => (
            Some(&state.source_filter),
            "Search source…",
            state.source_query(),
        ),
    };
    let Some(field) = field else {
        return slot
            .font_weight(FontWeight::MEDIUM)
            .child(placeholder.to_string())
            .agent_node(Role::Text, placeholder)
            .into_any_element();
    };
    if mode != ContentMode::Interactive {
        return slot
            .text_color(if query.is_empty() {
                ladder::muted_foreground().into()
            } else {
                ladder::foreground_alpha(1.0)
            })
            .child(if query.is_empty() {
                placeholder.to_string()
            } else {
                query.to_string()
            })
            .agent_node(Role::Input, placeholder)
            .agent_disabled(true)
            .into_any_element();
    }
    let focused = match route {
        Route::TrackBrowser => state.browser_filter_focus.is_focused(window),
        Route::SourceLibrary => state.source_filter_focus.is_focused(window),
    };
    slot.child(field.clone())
        .agent_node(Role::Input, placeholder)
        .agent_focused(focused)
        .into_any_element()
}

/// The one committing action, as the lit key in a row of key caps.
fn submit_chip(
    state: &AddTracks,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Option<AnyElement> {
    let interactive = mode == ContentMode::Interactive;
    let (label, enabled, id, node) = match route {
        Route::TrackBrowser => (
            "Import tracks".to_string(),
            true,
            "add-tracks-import",
            "Import tracks",
        ),
        Route::SourceLibrary => (
            if state.selected.is_empty() {
                "Import".to_string()
            } else {
                format!("Import {}", state.selected.len())
            },
            !state.selected.is_empty(),
            "add-tracks-import-selected",
            "Import selected",
        ),
    };
    let submit = app.clone();
    let menu = (route == Route::TrackBrowser)
        .then(|| source_menu(state, app))
        .flatten();
    Some(
        float::btn_primary_chip()
            .relative()
            .id(id)
            .when(enabled && interactive, |chip| {
                chip.track_focus(&state.submit_focus).tab_index(0)
            })
            .when(!enabled || !interactive, |chip| {
                chip.opacity(float::INERT_OPACITY)
            })
            .when(enabled && interactive, |chip| {
                chip.on_click(move |_, _, cx| {
                    submit.update(cx, |this, cx| this.add_tracks_submit(route, cx))
                })
            })
            // The ⌘ glyph rather than an icon: this chip's whole job is to say
            // "the chord in the legend below does this".
            .child("⌘")
            .child(label)
            .children(menu)
            .agent_node(Role::Button, node)
            .agent_disabled(!enabled || !interactive)
            .agent_focused(enabled && interactive && state.submit_focus.is_focused(window))
            .into_any_element(),
    )
}

/// The height of [`float::btn_primary_chip`], which the source menu hangs off
/// — `anchored_below` needs it to know how far a menu that opens *upward* has
/// to clear the chip. The chip is cap-sized by construction, so the number
/// comes from the cap rather than being written down a second time.
const SUBMIT_CHIP_HEIGHT: f32 = float::KEY_CAP_HEIGHT;

/// The import-source menu, hanging off the header chip.
///
/// An actions menu, not a value picker: each row goes somewhere. It lives as a
/// child of the trigger so `anchored_below` can find the edge to hang from.
fn source_menu(state: &AddTracks, app: &Entity<Luma>) -> Option<AnyElement> {
    state.source_menu.get()?;
    let closing = state.source_menu.closing_since();
    let dismiss = app.clone();
    let mut card = float::popover_card()
        .w(px(248.0))
        .on_mouse_down_out(move |_, _, cx| {
            dismiss.update(cx, |this, cx| this.toggle_source_menu(cx));
        });
    for choice in ImportChoice::ALL {
        let pick = app.clone();
        card = card.child(
            float::menu_row(RowState::Rest, choice.id())
                .id(choice.id())
                .when(closing.is_none(), |row| {
                    row.on_click(move |_, _, cx| {
                        pick.update(cx, |this, cx| this.choose_import_source(choice, cx))
                    })
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().child(choice.label()))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(ladder::muted_foreground())
                                .child(choice.description()),
                        ),
                )
                .agent_node(Role::Row, choice.label()),
        );
    }
    Some(float::anchored_below(
        "add-tracks-source-menu",
        SUBMIT_CHIP_HEIGHT,
        card.into_any_element(),
    ))
}

/// The key legend, plus whatever the route has to say about an import in
/// flight. Import status lives *here* rather than in a band of its own: a
/// strip that appears and disappears mid-flow shifts everything under it, and
/// the footer is already reserved space on every route.
fn footer(activity: Option<&TrackImportActivity>, route: Route) -> Div {
    let mut band = float::footer_band()
        .child(float::key_hint_pair(
            IconName::ArrowUp,
            IconName::ArrowDown,
            "Navigate",
        ))
        .child(float::key_hint(IconName::ArrowLeft, "Back"))
        .child(float::key_hint(
            IconName::ArrowRight,
            match route {
                Route::SourceLibrary => "Select",
                _ => "Open",
            },
        ));
    band = band.child(float::key_hint_text("⌘↵", "Import"));
    band = band.child(div().flex_1().min_w_0());
    // A route's own failure is shown where its rows would have been, by
    // `retry_body` — the footer would be a second copy of the same sentence.
    // What lands here is import progress, which belongs to no list.
    if let Some(activity) = activity {
        band = band.child(import_status(activity));
    }
    band
}

/// Import progress as one quiet line in the footer legend.
fn import_status(activity: &TrackImportActivity) -> Div {
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
    let mut line = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(px(10.5))
        .text_color(ladder::muted_foreground())
        .child(
            div()
                .child(phase_label.clone())
                .agent_node(Role::Chip, phase_label),
        );
    if let Some(progress) = &activity.progress {
        let count = format!(
            "Track import progress: {}/{}",
            progress.done, progress.total
        );
        line = line.child(div().child(count.clone()).agent_node(Role::Text, count));
    }
    if let Some(error) = &activity.error {
        let label = format!("Track import error: {error}");
        line = line.child(
            div()
                .max_w(px(200.0))
                .truncate()
                .text_color(ladder::danger())
                .child(label.clone())
                .agent_node(Role::Text, label),
        );
    }
    line
}

// ---------------------------------------------------------------------------
// Route bodies
// ---------------------------------------------------------------------------

fn track_browser(
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
        // Distinct from the sidebar's "No tracks imported", which is about one
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
                                app.update(cx, |this, cx| this.add_track_to_venue(id, cx));
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

fn source_library(
    state: &AddTracks,
    mode: ContentMode,
    app: &Entity<Luma>,
    cx: &mut gpui::App,
) -> AnyElement {
    let list = if let Some(error) = &state.source_error {
        retry_body(format!("Source error: {error}"), mode, app)
    } else if !state.source_loaded {
        loading_body(
            "add-tracks-source-skeleton",
            "Loading source library…",
            app.entity_id(),
            cx,
        )
    } else if state.source_rows.is_empty() {
        empty_body("No source tracks")
    } else {
        source_rows(state, mode, app)
    };
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(div().flex_1().min_w_0().flex().flex_col().child(list))
        .child(source_rail(state, mode, app))
        .into_any_element()
}

fn source_rows(state: &AddTracks, mode: ContentMode, app: &Entity<Luma>) -> AnyElement {
    let rows = Rc::clone(&state.source_rows);
    let selected = Rc::new(state.selected.clone());
    let active = state.source_active;
    let scroll = state.source_scroll.clone();
    let app = app.clone();
    float::viewport()
        .child(
            uniform_list("source-tracks", rows.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let track = &rows[index];
                        let id = track.id.clone();
                        let checked = selected.contains(&id);
                        let title = source_title(track);
                        let app = app.clone();
                        let key = format!("add-tracks-source-track-{id}");
                        track_row(
                            RowState::of(checked, index == active),
                            &key,
                            // A DJ-library adapter exposes no art path, so
                            // these rows show the empty plate — same rhythm as
                            // the library list beside them.
                            None,
                            &title,
                            track.artist.as_deref(),
                            true,
                        )
                        .id(SharedString::from(key.clone()))
                        .when(mode == ContentMode::Interactive, |row| {
                            row.on_click(move |_, _, cx| {
                                app.update(cx, |this, cx| this.toggle_source_track(&id, cx));
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

fn source_rail(state: &AddTracks, mode: ContentMode, app: &Entity<Luma>) -> impl IntoElement {
    let interactive = mode == ContentMode::Interactive;
    let all = app.clone();
    float::rail()
        .id("add-tracks-source-rail")
        .overflow_y_scroll()
        .child(float::section_heading("Playlists"))
        .child(
            float::nav_row(
                RowState::of(state.active_playlist().is_none(), false),
                "add-tracks-source-all",
            )
            .id("add-tracks-source-all")
            .child(div().flex_1().min_w_0().truncate().child("All tracks"))
            .when(interactive, |row| {
                row.on_click(move |_, _, cx| {
                    all.update(cx, |this, cx| this.select_source_playlist(None, cx))
                })
            })
            .agent_node(Role::Row, "All tracks")
            .agent_disabled(!interactive),
        )
        .children(state.playlists.iter().map(|playlist| {
            let app = app.clone();
            let id = playlist.id.clone();
            let label = playlist.name.clone();
            let active = state.active_playlist() == Some(id.as_str());
            let key = format!("add-tracks-playlist-{id}");
            float::nav_row(RowState::of(active, false), key.clone())
                .id(SharedString::from(key))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(ladder::muted_foreground())
                        .child(format!("{}", playlist.track_count)),
                )
                .when(interactive, |row| {
                    row.on_click(move |_, _, cx| {
                        let id = id.clone();
                        app.update(cx, |this, cx| this.select_source_playlist(Some(id), cx));
                    })
                })
                .agent_node(Role::Row, label)
                .agent_disabled(!interactive)
        }))
}

// ---------------------------------------------------------------------------
// Shared row and state bodies
// ---------------------------------------------------------------------------

/// One track row. `selectable` adds the leading check glyph that makes a
/// multi-select list say so before anything is selected — without it, a
/// selected row and a merely-highlighted row differ only by a ring the user
/// has no reason to read as "chosen".
fn track_row(
    state: RowState,
    fade_key: &str,
    art: Option<&str>,
    title: &str,
    subtitle: Option<&str>,
    selectable: bool,
) -> Div {
    let mut row = float::menu_row(state, fade_key.to_string())
        .h(px(ROW_HEIGHT))
        .w_full();
    if selectable {
        let checked = state == RowState::Selected;
        row = row.child(
            div()
                .size(px(15.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(luma_ui::radius::CHIP))
                .border_1()
                .border_color(glass::hairline(if checked { 0.0 } else { 0.18 }))
                .when(checked, |box_| box_.bg(glass::ink(0.92)))
                .when(checked, |box_| {
                    box_.child(
                        Icon::new(IconName::Check)
                            .size(px(11.0))
                            .text_color(ladder::background()),
                    )
                }),
        );
    }
    // The same square whether or not there is art to put in it, so a list
    // where only some tracks have covers keeps one left edge. `tracks::album_art`
    // is the app's one recipe for this — art is a path on disk, read through
    // gpui's image cache, never inlined bytes (CLAUDE.md).
    row.child(crate::tracks::album_art(art, ART_SIZE)).child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(2.0))
            .child(
                div()
                    .truncate()
                    .text_size(px(12.5))
                    .child(title.to_string()),
            )
            .children(subtitle.map(|subtitle| {
                div()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(ladder::muted_foreground())
                    .child(subtitle.to_string())
            })),
    )
}

/// The line a list shows in place of rows it does not have.
///
/// Named, like [`loading_body`], because "empty" is a state a driver has to be
/// able to tell apart from "still loading" — and one line of grey prose is
/// otherwise indistinguishable from another.
fn empty_body(message: &'static str) -> AnyElement {
    float::viewport()
        .child(float::list().child(float::empty_row(message).agent_node(Role::Text, message)))
        .into_any_element()
}

/// Placeholder rows while a list loads.
///
/// The skeleton is a paint, but the *state* still has to be nameable: a
/// driver (and a screen reader) has no way to tell "loading" from "empty"
/// by looking at grey bars, so the label the rows stand in for rides along.
fn loading_body(
    id: &'static str,
    label: &'static str,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> AnyElement {
    float::viewport()
        .child(
            float::list().child(
                float::skeleton_rows(6, view, cx)
                    .id(id)
                    .agent_node(Role::Text, label),
            ),
        )
        .into_any_element()
}

fn retry_body(message: String, mode: ContentMode, app: &Entity<Luma>) -> AnyElement {
    let retry = app.clone();
    float::viewport()
        .child(
            float::list().child(
                float::error_row(message.clone())
                    .px(px(14.0))
                    .py(px(10.0))
                    .child(
                        float::btn("Retry", "add-tracks-retry")
                            .id("add-tracks-retry")
                            .when(mode == ContentMode::Interactive, |button| {
                                button.on_click(move |_, _, cx| {
                                    retry.update(cx, |this, cx| this.refresh_open_track_browser(cx))
                                })
                            })
                            .agent_node(Role::Button, "Retry")
                            .agent_disabled(mode != ContentMode::Interactive),
                    )
                    .agent_node(Role::Text, message),
            ),
        )
        .into_any_element()
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
