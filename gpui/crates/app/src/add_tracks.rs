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
use luma_lib::models::tracks::TrackBrowserRow;
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float;
use luma_ui::text_input::{self, TextInput};

use crate::{Luma, SourceLibrary, SourcePlaylist, SourceTrack, TrackSource};

mod art;
mod browser;
mod import;
mod navigation;
mod palette;
mod preview;
pub(crate) mod progress;
mod rows;
mod source;

pub(crate) use art::ArtCache;
use browser::track_browser;
pub(crate) use import::TrackImportActivity;
use palette::palette;
pub(crate) use preview::{reap_preview, SourcePreview};
use progress::import_progress;
use source::source_library;

/// The one size every route takes — see the module docs. 680 wide is comet's
/// palette grammar; the height is the two bands plus a body deep enough for
/// eight rows, so a filtered list rarely needs the scrollbar to make sense.
const PALETTE_SIZE: MorphSize = MorphSize::new(680.0, 416.0);

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

/// What picking a library track does.
pub(crate) enum Purpose {
    /// Add it to the venue: the dialog's ordinary job.
    AddToVenue,
    /// Play it on the Perform tab's virtual deck, then close.
    PlayVirtual(crate::workspace::Target),
}

pub(crate) struct AddTracks {
    instance_id: uuid::Uuid,
    venue_id: String,
    purpose: Purpose,
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
    /// [`RowState`](float::RowState); the browser has no selection at all and the source list's
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
    fn new(venue_id: String, route: Route, purpose: Purpose, cx: &mut Context<Luma>) -> Self {
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
            purpose,
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
