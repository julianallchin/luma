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
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::*;
use gpui_component::{Icon, IconNamed};
use luma_lib::models::tracks::{
    StageOutcome, TrackAnalysisEvent, TrackBrowserRow, TrackImportPhase, TrackImportProgress,
};
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::{glass, ladder};

use crate::{Luma, SourceLibrary, SourcePlaylist, SourceTrack, TrackImportRequest, TrackSource};

mod art;
mod preview;

pub(crate) use art::ArtCache;
pub(crate) use preview::{reap_preview, SourcePreview};

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
    /// The import in flight, song by song. Where the dialog goes the moment
    /// an import starts, so "Import" visibly does something.
    Progress,
}

/// Where a track can come from. A closed vocabulary, listed once: the menu's
/// rows, their labels and what each one does are all derived from it, so a
/// fourth source is one variant rather than four edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportChoice {
    EngineDj,
    Rekordbox,
    Files,
    Folder,
}

impl ImportChoice {
    const ALL: [Self; 4] = [Self::EngineDj, Self::Rekordbox, Self::Files, Self::Folder];

    fn label(self) -> &'static str {
        match self {
            Self::EngineDj => "Engine DJ",
            Self::Rekordbox => "Rekordbox",
            Self::Files => "Files…",
            Self::Folder => "Folder…",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::EngineDj => "Playlists from an Engine DJ database",
            Self::Rekordbox => "Playlists and crates from Rekordbox",
            Self::Files => "Audio files from this computer",
            Self::Folder => "Every audio file in a folder",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::EngineDj => "add-tracks-engine",
            Self::Rekordbox => "add-tracks-rekordbox",
            Self::Files => "add-tracks-files",
            Self::Folder => "add-tracks-folder",
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
    /// The venue the dialog was adding to when the import started. Every
    /// imported song joins it, which is what the person asked for by
    /// importing from there.
    venue_id: Option<String>,
    /// One per imported song, known once the files are in the library.
    /// Empty while they are still being copied in.
    items: Vec<ImportItem>,
    /// Whether the progress page has been looked at since the import ended,
    /// so the sidebar keeps a failed import on screen until it has been.
    seen: bool,
}

pub(crate) struct ImportItem {
    track_id: String,
    title: String,
    art: Option<String>,
    analysis: ItemAnalysis,
}

/// One song's analysis as the scheduler has reported it: the stages it was
/// queued with, and which of them have started and settled.
///
/// Status is *derived* from this, never stored, so "Ready" cannot be written
/// down while a stage is still out — see [`Self::status`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ItemAnalysis {
    /// Every stage the song will report. Empty until the scheduler queues it.
    stages: Vec<String>,
    /// Started and not yet settled, in the order they started.
    running: Vec<String>,
    settled: Vec<String>,
    /// `stage: reason`, one per failed stage.
    failures: Vec<String>,
    /// The import has ended: whatever has not settled now never will.
    ended: bool,
}

/// What a row says about its song.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ItemStatus {
    Queued,
    Analyzing {
        running: Vec<String>,
        settled: usize,
        total: usize,
    },
    Ready,
    Failed(String),
}

impl ItemStatus {
    fn finished(&self) -> bool {
        matches!(self, Self::Ready | Self::Failed(_))
    }
}

impl ItemAnalysis {
    fn status(&self) -> ItemStatus {
        let total = self.stages.len();
        let all_settled = total > 0 && self.settled.len() >= total;
        if all_settled || self.ended {
            if !self.failures.is_empty() {
                return ItemStatus::Failed(self.failures.join("; "));
            }
            if total > 0 && !all_settled {
                return ItemStatus::Failed("Analysis stopped before it finished".to_string());
            }
            // Never queued and the import is over: the song was already in
            // the library, so there was nothing to analyse.
            return ItemStatus::Ready;
        }
        if self.running.is_empty() && self.settled.is_empty() {
            ItemStatus::Queued
        } else {
            ItemStatus::Analyzing {
                running: self.running.clone(),
                settled: self.settled.len(),
                total,
            }
        }
    }

    fn apply(&mut self, event: &TrackAnalysisEvent) {
        match event {
            TrackAnalysisEvent::Queued { stages } => self.stages = stages.clone(),
            TrackAnalysisEvent::StageStarted { stage } => {
                if !self.settled.contains(stage) && !self.running.contains(stage) {
                    self.running.push(stage.clone());
                }
            }
            TrackAnalysisEvent::StageSettled { stage, outcome } => {
                self.running.retain(|running| running != stage);
                if self.settled.contains(stage) {
                    return;
                }
                self.settled.push(stage.clone());
                if let StageOutcome::Failed(error) = outcome {
                    self.failures
                        .push(format!("{}: {error}", stage_label(stage)));
                }
            }
        }
    }
}

/// A stage's wire name as a person reads it. The wire names are stable
/// identifiers (`beat_grid`), not prose; one that is not listed here shows
/// as itself rather than disappearing.
fn stage_label(stage: &str) -> &str {
    match stage {
        "beat_grid" => "beats",
        "stems" => "stems",
        "mert" => "features",
        "roots" => "chords",
        "n2n" => "drums",
        "classifier" => "sections",
        "genre" => "genre",
        "waveform" => "waveform",
        other => other,
    }
}

impl TrackImportActivity {
    /// Fold one progress event into the per-song analyses.
    ///
    /// Only analysis events touch a song: phase one's copy events name the
    /// song they copied too, and reading one of those as "analysis started"
    /// is exactly how a song used to show Ready while it was still analysing.
    fn apply(&mut self, event: &TrackImportProgress) {
        if event.phase == TrackImportPhase::Complete {
            for item in &mut self.items {
                item.analysis.ended = true;
            }
            return;
        }
        let (Some(track_id), Some(analysis)) = (&event.track_id, &event.analysis) else {
            return;
        };
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|item| &item.track_id == track_id)
        {
            item.analysis.apply(analysis);
        }
    }

    fn failed(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.analysis.status(), ItemStatus::Failed(_)))
            .count()
    }

    /// The one line that says where the import is — the sidebar row and the
    /// progress page's title both read it.
    ///
    /// Once the songs are known it is counted off their rows, so the header
    /// and the list cannot disagree: it reads as finished exactly when every
    /// row does.
    fn summary(&self) -> String {
        if !self.items.is_empty() {
            let total = self.items.len();
            let finished = self
                .items
                .iter()
                .filter(|item| item.analysis.status().finished())
                .count();
            if finished < total {
                return if total == 1 {
                    "Analyzing 1 song…".to_string()
                } else {
                    format!("Analyzing {total} songs · {finished} of {total} done")
                };
            }
            let failed = self.failed();
            return if failed > 0 {
                format!("{} of {total} failed to import", failed)
            } else {
                format!("Imported {}", plural(total, "song"))
            };
        }
        if self.active {
            let (done, total) = self
                .progress
                .as_ref()
                .map_or((0, 0), |progress| (progress.done, progress.total));
            if total == 0 {
                "Importing songs…".to_string()
            } else {
                format!("Importing {} of {total} songs…", (done + 1).min(total))
            }
        } else if self.error.is_some() {
            "Import failed".to_string()
        } else {
            "Nothing was imported".to_string()
        }
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

impl Route {
    fn descriptor(self) -> RouteDescriptor<Self> {
        RouteDescriptor::exact(self, PALETTE_SIZE.width, PALETTE_SIZE.height)
            .with_transition(morph::direction(self.parent().is_some()))
    }

    /// Where "back" goes from here — `None` at the root.
    fn parent(self) -> Option<Self> {
        match self {
            Self::TrackBrowser => None,
            Self::SourceLibrary | Self::Progress => Some(Self::TrackBrowser),
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
    /// The arrows have moved the source list's cursor since the query was
    /// last edited. Space previews the cursor row then, and types a space
    /// otherwise — a query has to be able to contain one.
    source_navigating: bool,
    browser_scroll: UniformListScrollHandle,
    source_scroll: UniformListScrollHandle,
    /// The import-source menu hanging off the header chip. A [`Popup`] so it
    /// leaves the same way a dialog does, rather than blinking out.
    source_menu: float::MenuVisibility,
    /// Kept so the field subscriptions die with the dialog rather than firing
    /// into an overlay that has closed.
    _filter_subscriptions: [Subscription; 2],
}

impl AddTracks {
    fn new(venue_id: String, route: Route, cx: &mut Context<Luma>) -> Self {
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
            morph: MorphDialog::new(route.descriptor(), PALETTE_SIZE),
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
            source_navigating: false,
            browser_scroll: UniformListScrollHandle::new(),
            source_scroll: UniformListScrollHandle::new(),
            source_menu: float::MenuVisibility::Closed,
            _filter_subscriptions: subscriptions,
        }
    }

    /// The filter field the mounted route puts focus in.
    fn active_filter(&self) -> Option<&Entity<TextInput>> {
        match self.morph.target_key() {
            Route::TrackBrowser => Some(&self.browser_filter),
            Route::SourceLibrary => Some(&self.source_filter),
            Route::Progress => None,
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
            Route::Progress => 0,
        }
    }

    /// Whether `route`'s filter field is empty — what decides if Backspace
    /// edits text or steps back out of the route.
    fn query_is_empty(&self, route: Route, cx: &App) -> bool {
        match route {
            Route::TrackBrowser => self.browser_filter.read(cx).is_empty(),
            Route::SourceLibrary => self.source_filter.read(cx).is_empty(),
            Route::Progress => true,
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
        self.open_add_tracks(Route::TrackBrowser, cx);
    }

    /// The sidebar's import row: the dialog, opened on the import's progress.
    fn show_track_import(&mut self, cx: &mut Context<Self>) {
        if let Some(activity) = &mut self.track_import {
            activity.seen = true;
        }
        self.open_add_tracks(Route::Progress, cx);
    }

    fn open_add_tracks(&mut self, route: Route, cx: &mut Context<Self>) {
        let Some(venue_id) = self
            .sidebar
            .as_ref()
            .map(|tracks| tracks.venue_id().to_string())
        else {
            return;
        };
        let state = AddTracks::new(venue_id, route, cx);
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
    fn add_tracks_space(&mut self, cx: &mut Context<Self>) -> bool {
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
            Route::Progress => {}
        }
    }

    /// ⌘⏎ — the route's one committing action.
    fn add_tracks_submit(&mut self, route: Route, cx: &mut Context<Self>) {
        match route {
            // "Import" is a question — from where? — so the chord opens the
            // menu that asks it rather than guessing a source.
            Route::TrackBrowser => self.toggle_source_menu(cx),
            Route::SourceLibrary => self.import_source_selection(cx),
            Route::Progress => self.dismiss_overlay(cx),
        }
    }

    /// Open or dismiss the import-source menu.
    fn toggle_source_menu(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        state.source_menu.toggle();
        cx.notify();
    }

    /// Pick a source from the menu: the two DJ libraries open their browser
    /// route; files go straight to the platform picker, because there is no
    /// list to show first.
    fn choose_import_source(&mut self, choice: ImportChoice, cx: &mut Context<Self>) {
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

    /// The platform's own file picker, then the same import pipeline the DJ
    /// sources feed. A cancelled prompt is not an error: it is a person
    /// changing their mind, and it leaves the dialog exactly as it was.
    fn import_track_files(&mut self, folder: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            // The Linux portal picker is folder-only when `directories` is
            // set, which greys out every file, so files and folders are two
            // separate prompts.
            files: !folder,
            directories: folder,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let paths = audio_files_in(paths);
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
        state.source_navigating = false;
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
        let venue_id = match self.overlay.open_mut() {
            Some(crate::shell::Overlay::AddTracks(state)) => {
                state.request(Route::Progress, cx);
                Some(state.venue_id.clone())
            }
            _ => None,
        };
        self.track_import = Some(TrackImportActivity {
            generation,
            import_id: None,
            progress: None,
            active: true,
            error: None,
            venue_id,
            items: Vec::new(),
            seen: false,
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
                let mut venue = None;
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
                        activity.items = result
                            .tracks
                            .iter()
                            .map(|track| ImportItem {
                                track_id: track.id.clone(),
                                title: track
                                    .title
                                    .clone()
                                    .filter(|title| !title.is_empty())
                                    .or_else(|| track.source_filename.clone())
                                    .unwrap_or_else(|| track.file_path.clone()),
                                art: track.album_art_path.clone(),
                                analysis: ItemAnalysis::default(),
                            })
                            .collect();
                        venue = activity.venue_id.clone();
                    }
                }
                if let Some(venue_id) = venue {
                    let track_ids = result.tracks.iter().map(|track| track.id.clone()).collect();
                    this.add_imported_to_venue(venue_id, track_ids, cx);
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
                            activity.apply(&event);
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

    /// Put every song an import brought in into the venue it was imported
    /// from, then show them in the sidebar. The dialog stays open: it is
    /// showing the import's progress, which is still going.
    fn add_imported_to_venue(
        &mut self,
        venue_id: String,
        track_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if track_ids.is_empty() {
            return;
        }
        let pending: Vec<_> = track_ids
            .iter()
            .map(|track_id| {
                let request_id = uuid::Uuid::new_v4().to_string();
                self.library
                    .ensure_track_in_venue(&request_id, track_id, &venue_id, None)
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut errors = Vec::new();
            for added in pending {
                if let Err(error) = added.await {
                    errors.push(error.to_string());
                }
            }
            let Ok(rows) = this.read_with(cx, |this, _| this.library.tracks(&venue_id)) else {
                return;
            };
            let rows = rows.await;
            this.update(cx, |this, cx| {
                let generation = this
                    .sidebar
                    .as_ref()
                    .filter(|sidebar| sidebar.venue_id() == venue_id)
                    .map(crate::tracks::Tracks::load_generation);
                if let (Ok(rows), Some(generation)) = (rows, generation) {
                    this.with_tracks_for_venue(&venue_id, generation, cx, |sidebar| {
                        sidebar.replace_rows(rows)
                    });
                }
                if !errors.is_empty() {
                    if let Some(activity) = &mut this.track_import {
                        let message = format!("Could not add to venue: {}", errors.join("\n"));
                        activity.error = Some(match activity.error.take() {
                            Some(error) => format!("{error}\n{message}"),
                            None => message,
                        });
                    }
                }
                cx.notify();
            })
            .ok();
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
    if state
        .source_menu
        .tick_close(luma_ui::motion::reduced_motion(cx))
    {
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
        None => state.submit_focus.clone(),
    };
    window.defer(cx, move |window, cx| window.focus(&focus, cx));
}

/// What the dialog reads from the shell besides its own state.
pub(crate) struct Ambient<'a> {
    pub(crate) activity: Option<&'a TrackImportActivity>,
    pub(crate) preview: Option<&'a SourcePreview>,
    pub(crate) art: &'a ArtCache,
}

pub(crate) fn render(
    state: &AddTracks,
    ambient: Ambient<'_>,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(Instant::now());
    let app = app.clone();
    let card_app = app.clone();
    morph::card(&sample, "Add tracks dialog", move |route, mode| {
        route_content(state, &ambient, *route, mode, &card_app, window, cx)
    })
}

#[allow(clippy::too_many_arguments)]
fn route_content(
    state: &AddTracks,
    ambient: &Ambient<'_>,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let activity = ambient.activity;
    let body = match route {
        Route::TrackBrowser => track_browser(state, mode, app, cx),
        Route::SourceLibrary => source_library(state, ambient, mode, app, cx),
        Route::Progress => import_progress(activity, app, cx),
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
        // Bound above the search field (`keymap::init_field_overrides`), so
        // each one hands the key back to the field when it is not about a
        // preview.
        let space = app.clone();
        let back = app.clone();
        let forward = app.clone();
        frame = frame
            .on_action(move |_: &crate::keymap::ToggleSourcePreview, _, cx| {
                if !space.update(cx, |this, cx| this.add_tracks_space(cx)) {
                    cx.propagate();
                }
            })
            .on_action(move |_: &crate::keymap::SeekSourcePreviewBack, _, cx| {
                if !back.update(cx, |this, cx| {
                    this.nudge_source_preview(-preview::SEEK_STEP, cx)
                }) {
                    cx.propagate();
                }
            })
            .on_action(move |_: &crate::keymap::SeekSourcePreviewForward, _, cx| {
                if !forward.update(cx, |this, cx| {
                    this.nudge_source_preview(preview::SEEK_STEP, cx)
                }) {
                    cx.propagate();
                }
            });
    }
    frame
        .child(header(state, activity, route, mode, app, window))
        .child(div().flex_1().min_h_0().flex().child(body))
        .child(footer(activity, route))
        .into_any_element()
}

fn header(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
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

    band = band.child(filter_field(state, activity, route, mode, window));

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
fn filter_field(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    window: &Window,
) -> AnyElement {
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    if route == Route::Progress {
        let title = activity.map_or_else(|| "No import running".to_string(), |a| a.summary());
        return slot
            .truncate()
            .font_weight(FontWeight::MEDIUM)
            .child(title.clone())
            .agent_node(Role::Text, title)
            .into_any_element();
    }
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
        Route::Progress => unreachable!("the progress route has a title, not a field"),
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
        Route::Progress => false,
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
        Route::Progress => ("Done".to_string(), true, "add-tracks-done", "Done"),
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
    if !state.source_menu.is_shown() {
        return None;
    }
    let dismiss = app.clone();
    let mut card = float::popover_card().w(px(248.0));
    for choice in ImportChoice::ALL {
        let pick = app.clone();
        card = card.child(
            float::menu_row(RowState::Rest, choice.id())
                .id(choice.id())
                .on_click(move |_, _, cx| {
                    pick.update(cx, |this, cx| this.choose_import_source(choice, cx))
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
    Some(match state.source_menu.exit() {
        Some(progress) => float::anchored_below_closing(
            "add-tracks-source-menu",
            SUBMIT_CHIP_HEIGHT,
            card.into_any_element(),
            progress,
        ),
        None => float::anchored_below(
            "add-tracks-source-menu",
            SUBMIT_CHIP_HEIGHT,
            float::Dismiss::on_press_out(move |_, cx| {
                dismiss.update(cx, |this, cx| this.toggle_source_menu(cx));
            }),
            card.into_any_element(),
        ),
    })
}

/// The key legend, plus whatever the route has to say about an import in
/// flight. Import status lives *here* rather than in a band of its own: a
/// strip that appears and disappears mid-flow shifts everything under it, and
/// the footer is already reserved space on every route.
fn footer(activity: Option<&TrackImportActivity>, route: Route) -> Div {
    if route == Route::Progress {
        // The page is the progress, so the footer does not repeat it.
        return float::footer_band()
            .child(float::key_hint(IconName::ArrowLeft, "Add more"))
            .child(float::key_hint_text("⌘↵", "Done"));
    }
    let mut band = float::nav_legend(match route {
        Route::SourceLibrary => "Select",
        Route::TrackBrowser | Route::Progress => "Open",
    });
    if route == Route::SourceLibrary {
        band = band.child(float::key_hint_text("space", "Preview"));
    }
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

/// The import, one row per song: copying first, then each song's analysis.
fn import_progress(
    activity: Option<&TrackImportActivity>,
    app: &Entity<Luma>,
    cx: &mut gpui::App,
) -> AnyElement {
    let Some(activity) = activity else {
        return empty_body("No import running");
    };
    if activity.items.is_empty() {
        if activity.active {
            return loading_body(
                "add-tracks-import-skeleton",
                "Copying songs into the library…",
                app.entity_id(),
                cx,
            );
        }
        let message = activity
            .error
            .clone()
            .unwrap_or_else(|| "Nothing was imported".to_string());
        return float::viewport()
            .child(
                float::list()
                    .child(float::error_row(message.clone()).agent_node(Role::Text, message)),
            )
            .into_any_element();
    }
    let rows: Vec<(String, String, Option<String>, ItemStatus)> = activity
        .items
        .iter()
        .map(|item| {
            (
                item.track_id.clone(),
                item.title.clone(),
                item.art.clone(),
                item.analysis.status(),
            )
        })
        .collect();
    let rows: Rc<[_]> = rows.into();
    float::viewport()
        .child(
            uniform_list("track-import-progress", rows.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let (id, title, art, status) = &rows[index];
                        let key = format!("import-{id}");
                        let (label, subtitle) = import_status_words(status);
                        track_row(
                            RowState::Rest,
                            &key,
                            art.as_deref(),
                            title,
                            subtitle.as_deref(),
                            false,
                        )
                        .child(import_status_mark(status, &label))
                        .id(SharedString::from(key))
                        .agent_node(Role::Row, format!("{title}: {label}"))
                    })
                    .collect()
            })
            .size_full()
            .px(px(8.0)),
        )
        .into_any_element()
}

/// A row's status word, and the line under its title.
///
/// The word is what the row's automation label carries, so a script reads
/// the same sentence a person does: `Analyzing · beats, stems (2 of 8)`.
fn import_status_words(status: &ItemStatus) -> (String, Option<String>) {
    match status {
        ItemStatus::Queued => ("Queued".to_string(), None),
        ItemStatus::Analyzing {
            running,
            settled,
            total,
        } => {
            let stages = running
                .iter()
                .map(|stage| stage_label(stage))
                .collect::<Vec<_>>()
                .join(", ");
            let label = if stages.is_empty() {
                format!("Analyzing ({settled} of {total})")
            } else {
                format!("Analyzing · {stages} ({settled} of {total})")
            };
            let subtitle = (!stages.is_empty()).then(|| capitalized(&stages));
            (label, subtitle)
        }
        ItemStatus::Ready => ("Ready".to_string(), None),
        ItemStatus::Failed(reason) => ("Failed".to_string(), Some(reason.clone())),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Width of the trailing status column, so the stage bar under a word has
/// one length on every row and rows that change state do not shift.
const STATUS_WIDTH: f32 = 96.0;
/// The stage bar's thickness: a hairline you can read a fill on.
const STATUS_BAR_HEIGHT: f32 = 3.0;

/// The trailing column: the status word, and while analysing, how many of
/// the song's stages have settled as a thin bar under it. Stage-count rather
/// than a percentage because no stage reports fractional progress — a bar
/// that crept inside a stage would be inventing it.
fn import_status_mark(status: &ItemStatus, label: &str) -> Div {
    let (word, color): (&str, Hsla) = match status {
        ItemStatus::Queued => ("Queued", ladder::muted_foreground().into()),
        ItemStatus::Analyzing { .. } => ("Analyzing", ladder::foreground_alpha(0.7)),
        ItemStatus::Ready => ("Ready", ladder::muted_foreground().into()),
        ItemStatus::Failed(_) => ("Failed", ladder::danger().into()),
    };
    let fraction = match status {
        ItemStatus::Analyzing { settled, total, .. } if *total > 0 => {
            Some(*settled as f32 / *total as f32)
        }
        _ => None,
    };
    div()
        .flex_none()
        .w(px(STATUS_WIDTH))
        .flex()
        .flex_col()
        .items_end()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(px(11.0))
                .text_color(color)
                .when(*status == ItemStatus::Ready, |el| {
                    el.child(Icon::new(IconName::Check).size(px(11.0)))
                })
                .child(word.to_string())
                .agent_node(Role::Text, format!("Import status: {label}")),
        )
        .children(fraction.map(|fraction| {
            div()
                .w_full()
                .h(px(STATUS_BAR_HEIGHT))
                .rounded_full()
                .bg(glass::ink(0.08))
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction.clamp(0.0, 1.0)))
                        .rounded_full()
                        .bg(glass::ink(0.55)),
                )
        }))
}

/// The sidebar's row for an import: there while one runs, and after one
/// that failed until its page has been opened. A click opens that page.
pub(crate) fn sidebar(shell: &Luma, app: &Entity<Luma>) -> Option<AnyElement> {
    let activity = shell.track_import.as_ref()?;
    let failed = !activity.active && (activity.failed() > 0 || activity.error.is_some());
    if !activity.active && (!failed || activity.seen) {
        return None;
    }
    let label = activity.summary();
    let open = app.clone();
    let keyed = app.clone();
    Some(
        div()
            .flex_shrink_0()
            .px(px(12.))
            .pb(px(4.))
            .child(
                float::nav_row(RowState::Rest, "track-import-status")
                    .id("track-import-status")
                    .tab_stop(true)
                    .on_key_down(move |event, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            keyed.update(cx, |this, cx| this.show_track_import(cx));
                        }
                    })
                    .on_click(move |_, _, cx| {
                        open.update(cx, |this, cx| this.show_track_import(cx));
                    })
                    .child(
                        Icon::new(IconName::ArrowDown)
                            .size(px(12.))
                            .text_color(if failed {
                                Hsla::from(ladder::danger())
                            } else {
                                glass::ink(0.72)
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if failed {
                                Hsla::from(ladder::danger())
                            } else {
                                glass::ink(0.85)
                            })
                            .child(label.clone()),
                    )
                    .agent_node(Role::Button, label),
            )
            .into_any_element(),
    )
}

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
    ambient: &Ambient<'_>,
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
        source_rows(state, ambient, mode, app)
    };
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(div().flex_1().min_w_0().flex().flex_col().child(list))
        .child(source_rail(state, mode, app))
        .into_any_element()
}

fn source_rows(
    state: &AddTracks,
    ambient: &Ambient<'_>,
    mode: ContentMode,
    app: &Entity<Luma>,
) -> AnyElement {
    let rows = Rc::clone(&state.source_rows);
    let selected = Rc::new(state.selected.clone());
    let active = state.source_active;
    let scroll = state.source_scroll.clone();
    let app = app.clone();
    let art = ambient.art.clone();
    let interactive = mode == ContentMode::Interactive;
    // A press on a cover or a scrub bar is a pointer gesture inside a
    // keyboard-first list: it hands focus back to the search field, or the
    // arrows and space would stop reaching the list the moment you clicked.
    let keyboard = state.source_filter_focus.clone();
    let preview = ambient
        .preview
        .map(|preview| (preview.track_id.clone(), RowPreview::of(preview)));
    float::viewport()
        .child(
            uniform_list("source-tracks", rows.len(), move |range, _, cx| {
                let mut asked = false;
                let items = range
                    .map(|index| {
                        let track = &rows[index];
                        let id = track.id.clone();
                        let checked = selected.contains(&id);
                        let title = source_title(track);
                        let key = format!("add-tracks-source-track-{id}");
                        // Asked for here, at layout, so only rows the list
                        // actually lays out — the visible ones — are read.
                        let cover = art.lookup(track);
                        asked |= cover.asked;
                        let previewing = preview
                            .as_ref()
                            .filter(|(track_id, _)| *track_id == id)
                            .map(|(_, preview)| preview);
                        let subtitle = match previewing {
                            Some(preview) => {
                                Some(scrub_line(preview, &app, &keyboard, interactive))
                            }
                            None => track
                                .artist
                                .clone()
                                .map(|artist| float::row_subtitle(artist).into_any_element()),
                        };
                        let toggle = app.clone();
                        row_shell(RowState::of(checked, index == active), &key, true)
                            .child(preview_art(
                                cover.image,
                                previewing,
                                &key,
                                &id,
                                &title,
                                (&app, &keyboard),
                                interactive,
                            ))
                            .child(float::row_text_with(title.clone(), subtitle))
                            .id(SharedString::from(key.clone()))
                            .when(interactive, |row| {
                                row.on_click(move |_, _, cx| {
                                    toggle.update(cx, |this, cx| this.toggle_source_track(&id, cx));
                                })
                            })
                            .agent_node(Role::Row, title)
                            .agent_disabled(!interactive)
                    })
                    .collect();
                if asked {
                    // Deferred: this runs mid-layout, and serving a cover
                    // starts work on the app — which is not to be re-entered
                    // from inside its own frame.
                    let app = app.clone();
                    cx.defer(move |cx| app.update(cx, |this, cx| this.pump_source_art(cx)));
                }
                items
            })
            .track_scroll(&scroll)
            .size_full()
            .px(px(8.0)),
        )
        .into_any_element()
}

/// The preview as one row draws it — copied out of the shell's state so the
/// list's render closure, which outlives this frame's borrows, can hold it.
#[derive(Clone)]
struct RowPreview {
    playing: bool,
    /// Decoding: there is no duration or overview yet.
    loading: bool,
    failed: Option<String>,
    position: f32,
    duration: f32,
    fraction: f32,
    scrubbing: bool,
    overview: Rc<[f32]>,
}

impl RowPreview {
    fn of(preview: &SourcePreview) -> Self {
        let (duration, overview) = preview.loaded.as_ref().map_or_else(
            || (0.0, Rc::from(Vec::new())),
            |loaded| (loaded.duration, Rc::clone(&loaded.overview)),
        );
        Self {
            playing: preview.playing,
            loading: preview.loaded.is_none() && preview.failed.is_none(),
            failed: preview.failed.clone(),
            position: preview.position,
            duration,
            fraction: preview.fraction(),
            scrubbing: preview.scrubbing,
            overview,
        }
    }
}

/// The scrim a cover dims to under the play glyph.
const ART_SCRIM: f32 = 0.45;
/// The playing row's scrim: lighter, because it stays up while the row
/// plays and should read as a state of the cover, not a cover over it.
const ART_SCRIM_ACTIVE: f32 = 0.32;

/// The row's cover, which is also its play button: on hover it dims under a
/// play glyph, and while the row previews it holds a pause glyph.
#[allow(clippy::too_many_arguments)]
fn preview_art(
    cover: Option<Arc<RenderImage>>,
    preview: Option<&RowPreview>,
    key: &str,
    track_id: &str,
    title: &str,
    (app, keyboard): (&Entity<Luma>, &FocusHandle),
    interactive: bool,
) -> AnyElement {
    let group = SharedString::from(format!("{key}-art"));
    let keyboard = keyboard.clone();
    let playing = preview.is_some_and(|preview| preview.playing);
    let held = preview.is_some_and(|preview| preview.failed.is_none());
    let loading = preview.is_some_and(|preview| preview.loading);
    let label = if playing {
        format!("Pause preview of {title}")
    } else {
        format!("Play preview of {title}")
    };
    let toggle = app.clone();
    let track_id = track_id.to_string();
    let has_cover = cover.is_some();
    let glyph = if playing {
        IconName::Pause
    } else {
        IconName::Play
    };
    let scrim = if held { ART_SCRIM_ACTIVE } else { ART_SCRIM };
    let glyph_alpha = if loading { 0.55 } else { 0.95 };
    let reveal = held || interactive;
    // Cover and overlay are painted, each in a layer of its own. Inside the
    // dialog's layer every primitive shares one draw order, and gpui then
    // draws images after quads and glyphs — so an overlay laid out as a
    // sibling of the image would sit *under* it. A layer per pass is what
    // puts the scrim and the glyph on top.
    let paint = canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            let corners = Corners::all(px(luma_ui::radius::CHIP));
            if let Some(cover) = &cover {
                window.paint_layer(bounds, |window| {
                    window
                        .paint_image(bounds, bounds, corners, Arc::clone(cover), 0, false)
                        .ok();
                });
            }
            if !(held || reveal && bounds.contains(&window.mouse_position())) {
                return;
            }
            window.paint_layer(bounds, |window| {
                window.paint_quad(fill(bounds, glass::scrim(scrim)).corner_radii(corners));
                let edge = px(ART_GLYPH);
                let glyph_bounds = Bounds::new(
                    point(
                        bounds.center().x - edge / 2.0,
                        bounds.center().y - edge / 2.0,
                    ),
                    size(edge, edge),
                );
                window
                    .paint_svg(
                        glyph_bounds,
                        glyph.path(),
                        None,
                        TransformationMatrix::unit(),
                        glass::ink(glyph_alpha),
                        cx,
                    )
                    .ok();
            });
        },
    )
    .size_full();
    div()
        .id(group.clone())
        .relative()
        .flex_none()
        .size(px(ART_SIZE))
        .rounded(px(luma_ui::radius::CHIP))
        .bg(glass::wash(glass::WASH_SUBTLE))
        // No visual change of its own: a hover style is what makes gpui
        // repaint this cover as the pointer enters and leaves, which is when
        // the painted overlay above has to appear and go.
        .hover(|style| style)
        .when(held, |art| {
            art.border_1().border_color(glass::hairline(0.35))
        })
        .child(if has_cover {
            paint
                .agent_node(Role::Text, format!("Cover of {title}"))
                .into_any_element()
        } else {
            paint.into_any_element()
        })
        .when(interactive, |art| {
            art.cursor_pointer().on_click(move |_, window, cx| {
                // The cover plays; the row around it selects. One press is
                // one of the two.
                cx.stop_propagation();
                window.focus(&keyboard, cx);
                let track_id = track_id.clone();
                toggle.update(cx, |this, cx| this.toggle_source_preview(&track_id, cx));
            })
        })
        .agent_node(Role::Button, label)
        .agent_disabled(!interactive)
        .into_any_element()
}

/// The play / pause glyph's edge on a cover.
const ART_GLYPH: f32 = 13.0;

/// The previewing row's second line: elapsed time, the scrub bar, remaining
/// time — or why it is not playing.
fn scrub_line(
    preview: &RowPreview,
    app: &Entity<Luma>,
    keyboard: &FocusHandle,
    interactive: bool,
) -> AnyElement {
    let line = div()
        .h(px(SCRUB_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .text_size(px(10.0))
        .text_color(ladder::muted_foreground());
    if let Some(error) = &preview.failed {
        let message = format!("Can't preview: {error}");
        return line
            .child(
                div()
                    .truncate()
                    .text_color(ladder::danger())
                    .child(message.clone()),
            )
            .agent_node(Role::Text, message)
            .into_any_element();
    }
    if preview.loading {
        return line
            .child("Loading preview…")
            .agent_node(Role::Text, "Loading preview")
            .into_any_element();
    }
    let elapsed = clock(preview.position);
    let remaining = format!("-{}", clock(preview.duration - preview.position));
    let label = format!("Preview position: {elapsed} of {}", clock(preview.duration));
    line.child(time_label(elapsed))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(scrub_bar(
                    preview.clone(),
                    app.clone(),
                    keyboard.clone(),
                    interactive,
                ))
                .agent_node(Role::Slider, label)
                .agent_disabled(!interactive),
        )
        .child(time_label(remaining))
        .into_any_element()
}

/// The scrub line's height: the subtitle's line, so a row does not change
/// height when it starts previewing.
const SCRUB_HEIGHT: f32 = 14.0;

fn time_label(text: String) -> Div {
    div()
        .flex_none()
        .min_w(px(28.0))
        .font_family(luma_ui::fonts::MONO)
        .child(text)
}

/// `m:ss`, rounding down — a playhead at 9.99 s has not reached 0:10.
fn clock(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The waveform overview the playhead walks across; a press or drag seeks.
///
/// A canvas because the drag outlives the element under the pointer: the
/// listeners are window-level and keyed to the bar's painted bounds, so a
/// drag that leaves the bar keeps scrubbing to its nearer end.
fn scrub_bar(
    preview: RowPreview,
    app: Entity<Luma>,
    keyboard: FocusHandle,
    interactive: bool,
) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = f32::from(bounds.size.width).max(1.0);
            let height = f32::from(bounds.size.height);
            let played = glass::ink(0.78);
            let ahead = glass::ink(0.22);
            let peaks = preview.overview.len();
            if peaks == 0 {
                // No overview: a plain track with the played part lit.
                let track = Bounds::new(
                    point(bounds.origin.x, bounds.center().y - px(1.0)),
                    size(bounds.size.width, px(2.0)),
                );
                window.paint_quad(fill(track, ahead));
                let mut done = track;
                done.size.width = px(width * preview.fraction);
                window.paint_quad(fill(done, played));
            } else {
                let step = width / peaks as f32;
                let bar = (step - 1.0).max(1.0);
                for (index, peak) in preview.overview.iter().enumerate() {
                    let x = index as f32 * step;
                    let h = (peak * height).clamp(1.0, height);
                    let quad = Bounds::new(
                        point(
                            bounds.origin.x + px(x),
                            bounds.origin.y + px((height - h) / 2.0),
                        ),
                        size(px(bar), px(h)),
                    );
                    let lit = (x + bar / 2.0) / width <= preview.fraction;
                    window.paint_quad(fill(quad, if lit { played } else { ahead }));
                }
            }
            let head = Bounds::new(
                point(
                    bounds.origin.x + px(width * preview.fraction - 0.5),
                    bounds.origin.y,
                ),
                size(px(1.0), bounds.size.height),
            );
            window.paint_quad(fill(head, ladder::playhead()));
            if !interactive {
                return;
            }
            let duration = preview.duration;
            let seek_at = move |x: Pixels| {
                ((f32::from(x - bounds.origin.x)) / width).clamp(0.0, 1.0) * duration
            };
            let down = app.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !bounds.contains(&event.position)
                {
                    return;
                }
                // The row selects on click; a press on its bar is a seek.
                cx.stop_propagation();
                window.focus(&keyboard, cx);
                let seconds = seek_at(event.position.x);
                down.update(cx, |this, cx| {
                    this.set_source_preview_scrubbing(true, cx);
                    this.seek_source_preview(seconds, cx);
                });
            });
            if !preview.scrubbing {
                return;
            }
            let drag = app.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left)
                {
                    let seconds = seek_at(event.position.x);
                    drag.update(cx, |this, cx| this.seek_source_preview(seconds, cx));
                }
            });
            let up = app.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    up.update(cx, |this, cx| this.set_source_preview_scrubbing(false, cx));
                }
            });
        },
    )
    .size_full()
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
    // The same square whether or not there is art to put in it, so a list
    // where only some tracks have covers keeps one left edge. `tracks::album_art`
    // is the app's one recipe for this — art is a path on disk, read through
    // gpui's image cache, never inlined bytes (CLAUDE.md).
    row_shell(state, fade_key, selectable)
        .child(crate::tracks::album_art(art, ART_SIZE))
        .child(float::row_text(
            title.to_string(),
            subtitle.map(|subtitle| subtitle.to_string().into()),
        ))
}

/// A track row up to its cover: the row, and the check box when the list
/// is multi-select. The caller adds the cover and the words.
fn row_shell(state: RowState, fade_key: &str, selectable: bool) -> Div {
    let row = float::menu_row(state, fade_key.to_string())
        .h(px(ROW_HEIGHT))
        .w_full();
    if !selectable {
        return row;
    }
    let checked = state == RowState::Selected;
    row.child(
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

const AUDIO_EXTENSIONS: [&str; 9] = [
    "mp3", "wav", "flac", "aif", "aiff", "m4a", "aac", "ogg", "opus",
];

/// Files stay as picked; a folder becomes the audio files under it, sorted so
/// the import order is stable.
fn audio_files_in(paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| AUDIO_EXTENSIONS.iter().any(|a| ext.eq_ignore_ascii_case(a)))
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found = Vec::new();
            walk(&path, &mut found);
            found.sort();
            out.extend(found);
        } else {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        ImportItem, ItemAnalysis, ItemStatus, StageOutcome, TrackAnalysisEvent,
        TrackImportActivity, TrackImportPhase, TrackImportProgress,
    };

    const STAGES: [&str; 4] = ["beat_grid", "stems", "roots", "waveform"];

    fn event(
        phase: TrackImportPhase,
        track_id: Option<&str>,
        analysis: Option<TrackAnalysisEvent>,
    ) -> TrackImportProgress {
        TrackImportProgress {
            import_id: "import".into(),
            source: "files".into(),
            phase,
            done: 0,
            total: 2,
            track_id: track_id.map(Into::into),
            current_track: None,
            analysis,
            error: None,
        }
    }

    fn analysis(track: &str, analysis: TrackAnalysisEvent) -> TrackImportProgress {
        event(TrackImportPhase::Analyzing, Some(track), Some(analysis))
    }

    fn queued(track: &str) -> TrackImportProgress {
        analysis(
            track,
            TrackAnalysisEvent::Queued {
                stages: STAGES.iter().map(|stage| stage.to_string()).collect(),
            },
        )
    }

    fn started(track: &str, stage: &str) -> TrackImportProgress {
        analysis(
            track,
            TrackAnalysisEvent::StageStarted {
                stage: stage.into(),
            },
        )
    }

    fn settled(track: &str, stage: &str, outcome: StageOutcome) -> TrackImportProgress {
        analysis(
            track,
            TrackAnalysisEvent::StageSettled {
                stage: stage.into(),
                outcome,
            },
        )
    }

    fn activity() -> TrackImportActivity {
        let item = |id: &str| ImportItem {
            track_id: id.into(),
            title: id.into(),
            art: None,
            analysis: ItemAnalysis::default(),
        };
        TrackImportActivity {
            generation: 1,
            import_id: Some("import".into()),
            progress: None,
            active: true,
            error: None,
            venue_id: None,
            items: vec![item("a"), item("b")],
            seen: false,
        }
    }

    fn status(activity: &TrackImportActivity, index: usize) -> ItemStatus {
        activity.items[index].analysis.status()
    }

    /// The bug this fold was rewritten for: phase one's "copied" event names
    /// the song, and the old fold read it as analysis starting — so the
    /// scheduler's real start then read as the finish.
    #[test]
    fn a_copy_event_is_not_analysis() {
        let mut activity = activity();
        activity.apply(&event(TrackImportPhase::Importing, Some("a"), None));
        assert_eq!(status(&activity, 0), ItemStatus::Queued);
        activity.apply(&queued("a"));
        activity.apply(&started("a", "beat_grid"));
        assert!(matches!(status(&activity, 0), ItemStatus::Analyzing { .. }));
        assert_eq!(activity.summary(), "Analyzing 2 songs · 0 of 2 done");
    }

    /// "Ready" exactly when every queued stage has settled, whatever order
    /// the stages start and settle in — parallel siblings settle in any
    /// order — and never one event sooner.
    #[test]
    fn ready_is_never_shown_while_a_stage_is_pending() {
        // Every interleaving the scheduler can produce for four stages is a
        // shuffle of their start and settle events with each start before its
        // settle; a seeded walk visits enough of them to cover the orders.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..500 {
            let mut activity = activity();
            activity.apply(&queued("a"));
            let mut unstarted: Vec<&str> = STAGES.to_vec();
            let mut running: Vec<&str> = Vec::new();
            let mut settled_count = 0;
            while settled_count < STAGES.len() {
                let start = !unstarted.is_empty() && (running.is_empty() || next() % 2 == 0);
                if start {
                    let stage = unstarted.remove(next() as usize % unstarted.len());
                    activity.apply(&started("a", stage));
                    running.push(stage);
                } else {
                    let stage = running.remove(next() as usize % running.len());
                    activity.apply(&settled("a", stage, StageOutcome::Done));
                    settled_count += 1;
                }
                let now = status(&activity, 0);
                if settled_count < STAGES.len() {
                    assert!(
                        matches!(now, ItemStatus::Analyzing { .. }),
                        "{settled_count} of {} settled reads {now:?}",
                        STAGES.len()
                    );
                    assert!(!activity.summary().starts_with("Imported"));
                } else {
                    assert_eq!(now, ItemStatus::Ready);
                }
            }
        }
    }

    #[test]
    fn a_failed_stage_fails_the_song_once_the_rest_settle() {
        let mut activity = activity();
        activity.apply(&queued("a"));
        activity.apply(&started("a", "stems"));
        activity.apply(&settled("a", "stems", StageOutcome::Failed("boom".into())));
        // Siblings still running: the song is not finished in either sense.
        assert!(matches!(status(&activity, 0), ItemStatus::Analyzing { .. }));
        for stage in ["beat_grid", "roots", "waveform"] {
            activity.apply(&settled("a", stage, StageOutcome::Skipped));
        }
        assert_eq!(
            status(&activity, 0),
            ItemStatus::Failed("stems: boom".into())
        );
        // A stage settles once: a repeat cannot change the song's story.
        activity.apply(&settled("a", "stems", StageOutcome::Done));
        assert_eq!(
            status(&activity, 0),
            ItemStatus::Failed("stems: boom".into())
        );
    }

    #[test]
    fn completion_settles_only_what_the_rows_can_account_for() {
        let mut activity = activity();
        // `a` was queued and stopped part-way; `b` was never queued, which is
        // what a song already in the library looks like.
        activity.apply(&queued("a"));
        activity.apply(&started("a", "beat_grid"));
        activity.apply(&event(TrackImportPhase::Complete, None, None));
        activity.active = false;
        assert_eq!(
            status(&activity, 0),
            ItemStatus::Failed("Analysis stopped before it finished".into())
        );
        assert_eq!(status(&activity, 1), ItemStatus::Ready);
        assert_eq!(activity.summary(), "1 of 2 failed to import");
    }

    #[test]
    fn the_header_finishes_with_the_last_row() {
        let mut activity = activity();
        for track in ["a", "b"] {
            activity.apply(&queued(track));
        }
        for stage in STAGES {
            activity.apply(&settled("a", stage, StageOutcome::Done));
        }
        assert_eq!(activity.summary(), "Analyzing 2 songs · 1 of 2 done");
        for stage in STAGES {
            activity.apply(&settled("b", stage, StageOutcome::Done));
        }
        // Before the terminal event arrives: the rows are the truth.
        assert!(activity.active);
        assert_eq!(activity.summary(), "Imported 2 songs");
    }
}
