//! The song list: one venue's library, as a full page over Arrange, laid out
//! as Finder's column view.
//!
//! The title bar's song title opens it (and ⌘P). Three columns, left to
//! right: the places — "All songs", then the venue's folders, see [`folders`];
//! the songs in the selected place; and the selected song's scores, see
//! [`scores`]. Selecting in one column fills the next. Choosing a score opens
//! it and closes the page. An ownership filter and a search work over the
//! venue's songs; a search lists its matches from every place, under All
//! songs.
//!
//! The selection is the page's, not the frame's: it outlives the page closing
//! and opening again over the same venue, so the page reopens on the folder and
//! song it was left on.
//!
//! # Filtering is the view's job, not the query's
//!
//! `list_tracks_enriched(venue_id)` returns the *whole visible library* and
//! decorates each row with that venue's clip count; the venue id scopes the
//! decoration, not the result set. The list keeps only the rows with
//! `is_in_venue`, the durable score-existence signal. Clip counts are
//! presentation metadata and never decide membership, so a newly added track
//! is visible before its first annotation is authored.
//!
//! Album art comes from `album_art_path` — a path on disk, never inlined bytes.
//! The native host reads the file with `img(path)` and GPUI's image cache
//! handles the decode and the lazy load.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::*;
use luma_ui::float;
use luma_ui::glass;
use luma_ui::ladder;
use luma_ui::motion::FadeHover as _;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::pane;

use luma_lib::models::folders::Folder;
use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;

use crate::agent::ChatStatus;
use crate::workspace::Target;
use crate::Luma;

pub(crate) mod folders;
pub(crate) mod scores;

/// Which tracks the ownership filter admits. Mutually exclusive, and `Mine` is
/// the default.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Mine,
    All,
}

impl Ownership {
    const ALL: [Ownership; 2] = [Ownership::Mine, Ownership::All];

    fn label(self) -> &'static str {
        match self {
            Ownership::Mine => "Mine",
            Ownership::All => "All",
        }
    }

    /// A track with no `uid` is in the guest namespace, which every host reads
    /// as its own.
    fn admits(self, track: &TrackBrowserRow, user: Option<&str>) -> bool {
        match self {
            Ownership::All => true,
            Ownership::Mine => match &track.uid {
                None => true,
                Some(uid) => Some(uid.as_str()) == user,
            },
        }
    }
}

/// What the first column has selected: every song, or one folder's.
///
/// Folders do not nest, so this is the whole of the first column's state and
/// there are always exactly three columns.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    All,
    Folder(String),
}

/// The columns, left to right. The one the keyboard is in wears the stronger
/// selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Column {
    Places,
    Songs,
    Scores,
}

/// The page's whole state: the venue it is showing, everything the seam
/// returned for it, the filters over that, and the selection path through
/// the three columns.
///
/// [`Self::shown`] is indices into [`Self::rows`] rather than a second copy
/// of them, recomputed on every state change instead of on every draw: a draw
/// happens per frame and per hover, and re-filtering a full library there is
/// the difference between a scroll that keeps up and one that does not.
pub struct Tracks {
    /// The venue the rows were decorated for. The editor needs it too — a
    /// track's score is per-venue — so it is kept beside the name it is shown
    /// under rather than re-derived from the screen that opened this one.
    venue_id: String,
    venue_name: String,
    /// The venue is someone else's, shared with this account.
    venue_shared: bool,
    /// The venue left this library while it was open: its owner removed this
    /// account, or it was deleted. A fact about the venue, said in the venue
    /// menu — see [`Luma::check_open_venue`].
    pub(crate) venue_gone: bool,
    load_generation: u64,
    rows: Rc<[TrackBrowserRow]>,
    /// The venue's folders, by name, each with the songs it holds. Re-read
    /// whenever the rows are — see [`Luma::with_tracks_for_venue`].
    folders: Rc<[Folder]>,
    /// The first column's selection.
    place: Place,
    /// The second column's selection, by track id.
    track: Option<String>,
    /// The second column: the songs the filters admit in the place.
    shown: Rc<[usize]>,
    /// How many songs the filters admit in each folder, in [`Self::folders`]'
    /// order, and in all.
    counts: Rc<[usize]>,
    all_count: usize,
    /// The third column: the selected song's scores.
    pub(crate) scores: Option<scores::Scores>,
    /// The column the keyboard is in.
    column: Column,
    /// The first two columns' widths; the scores take the rest.
    widths: [f32; 2],
    songs_scroll: UniformListScrollHandle,
    /// The menu a right-click raised, or none. One slot: two menus open at
    /// once is the bug it rules out.
    menu: Option<folders::Menu>,
    /// The folder whose name is being typed, or none.
    rename: Option<folders::Rename>,
    /// Whether the venue's query has come back. Written in the same
    /// assignment as [`Self::rows`] and [`Self::load_failed`], so "still
    /// loading" and "nothing to show" can never be confused for one another.
    loaded: bool,
    /// The venue's songs could not be read. The cause is in the log.
    load_failed: bool,
    ownership: Ownership,
    /// The typed query, mirrored out of [`Self::search`] on every edit. The
    /// field is the editor; this is what `refilter` reads.
    query: String,
    search: Entity<luma_ui::text_input::TextInput>,
    _search_subscription: Subscription,
    /// The signed-in principal, snapshotted when the venue was selected. The
    /// ownership filter reads it.
    user: Option<String>,
    search_focus: FocusHandle,
    /// A score chosen from another venue's menu, to open here once this
    /// venue's catalogue has landed — it needs its track's row. Taken in
    /// place of the score saved for this venue.
    pub(crate) arrival: Option<(String, crate::track_editor::Score)>,
}

impl Tracks {
    /// The venue this page was opened for.
    pub(crate) fn venue_name(&self) -> &str {
        &self.venue_name
    }

    /// What became of the open venue, if it has left this library.
    pub(crate) fn venue_gone_state(&self) -> Option<&'static str> {
        self.venue_gone.then_some(if self.venue_shared {
            "No longer shared with you"
        } else {
            "Deleted"
        })
    }

    /// The venue a score opens in.
    pub(crate) fn venue_id(&self) -> &str {
        &self.venue_id
    }

    pub(crate) fn load_generation(&self) -> u64 {
        self.load_generation
    }

    /// The loaded row a click carries. Looked up by id rather than by index
    /// because the click was registered against a *filtered* list, and the
    /// filters can change before it lands.
    pub(crate) fn find(&self, track_id: &str) -> Option<TrackBrowserRow> {
        self.rows.iter().find(|row| row.id == track_id).cloned()
    }

    /// Every track in the venue, whatever the filters say.
    pub(crate) fn venue_rows(&self) -> impl Iterator<Item = &TrackBrowserRow> {
        self.rows.iter().filter(|row| row.is_in_venue)
    }

    /// Adopt a freshly venue-decorated library read after an add-track action.
    pub(crate) fn replace_rows(&mut self, rows: Vec<TrackBrowserRow>) {
        self.rows = rows.into();
        self.loaded = true;
        self.load_failed = false;
        self.refilter();
    }

    /// Whether a query is up. A search lists its matches from every place.
    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The place the second column lists: the selected one, or All songs
    /// while a search is up.
    fn shown_place(&self) -> &Place {
        const ALL: &Place = &Place::All;
        if self.searching() {
            ALL
        } else {
            &self.place
        }
    }

    /// The songs the filters admit, in the order the query returned them.
    fn filter(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.is_in_venue
                    && self.ownership.admits(row, self.user.as_deref())
                    && matches(row, &query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Recompute the second column and the counts beside every place. A
    /// selected folder that has gone selects All songs, and a selected song
    /// the column no longer lists is deselected, its scores with it — the
    /// path never runs through something that is not on screen.
    fn refilter(&mut self) {
        if let Place::Folder(id) = &self.place {
            if self.loaded && !self.folders.iter().any(|folder| folder.id == *id) {
                self.place = Place::All;
            }
        }
        let admitted = self.filter();
        fn held(folder: &Folder) -> HashSet<&str> {
            folder.track_ids.iter().map(String::as_str).collect()
        }
        self.counts = self
            .folders
            .iter()
            .map(|folder| {
                let held = held(folder);
                admitted
                    .iter()
                    .filter(|row| held.contains(self.rows[**row].id.as_str()))
                    .count()
            })
            .collect();
        self.all_count = admitted.len();
        let place = match self.shown_place() {
            Place::All => None,
            Place::Folder(id) => self.folders.iter().find(|folder| folder.id == *id),
        };
        self.shown = match place {
            None => admitted.into(),
            Some(folder) => {
                let held = held(folder);
                admitted
                    .into_iter()
                    .filter(|row| held.contains(self.rows[*row].id.as_str()))
                    .collect()
            }
        };
        if self.track.is_some() && self.selected_song().is_none() {
            self.track = None;
            self.scores = None;
            if self.column == Column::Scores {
                self.column = Column::Songs;
            }
        }
    }

    /// Where the selected song is in the second column.
    fn selected_song(&self) -> Option<usize> {
        let track = self.track.as_deref()?;
        self.shown
            .iter()
            .position(|row| self.rows[*row].id == track)
    }

    /// The first column top to bottom: All songs, then each folder.
    fn places(&self) -> Vec<Place> {
        std::iter::once(Place::All)
            .chain(
                self.folders
                    .iter()
                    .map(|folder| Place::Folder(folder.id.clone())),
            )
            .collect()
    }

    /// How a row in `column` that is on the selection path is lit: the
    /// stronger fill in the column the keyboard is in, the weaker in the
    /// others.
    fn path_state(&self, column: Column, on_path: bool) -> Lit {
        match (on_path, self.column == column) {
            (false, _) => Lit::Rest,
            (true, true) => Lit::Focused,
            (true, false) => Lit::Path,
        }
    }
}

/// `title`, `artist` or `album` contains `query`, which is already lowercased
/// and trimmed. An empty query matches everything.
pub(crate) fn matches(track: &TrackBrowserRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    [&track.title, &track.artist, &track.album]
        .into_iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(query))
}

// -- navigation and filter edits ----------------------------------------------
//
// These hang off `Luma` because opening a venue is a `Library` call plus a
// screen transition, and `Luma` owns both.

impl Luma {
    /// Open a venue: the window swaps to it. Its songs load, the picker
    /// closes, and its modes come back as they were — as it was left, or as
    /// it was saved at the last quit. The venue being left keeps its state,
    /// parked.
    pub(crate) fn open_venue(&mut self, venue: Venue, cx: &mut Context<Self>) {
        self.venue_selection_generation = self.venue_selection_generation.wrapping_add(1);
        let generation = self.venue_selection_generation;
        let venue_id = venue.id.clone();
        self.close_overlay(cx);
        let pending = self.library.tracks(&venue.id);
        let remember = self
            .library
            .set_session_item(crate::welcome::LAST_VENUE, &venue.id);
        let search = cx.new(|cx| {
            let mut field = luma_ui::text_input::TextInput::search(PLACEHOLDER, cx);
            field.set_text_size(HEADER_TEXT, cx);
            field
        });
        let search_focus = search.read(cx).focus_handle(cx);
        let search_subscription = cx.subscribe(&search, |luma, field, event, cx| {
            if event == &luma_ui::text_input::Event::Edited {
                let query = field.read(cx).text().to_string();
                luma.track_search_changed(query, cx);
            } else {
                cx.notify();
            }
        });
        // Column widths are the person's, not the venue's: they carry over.
        let widths = self
            .songs
            .as_ref()
            .map_or(DEFAULT_WIDTHS, |songs| songs.widths);
        self.songs = Some(Tracks {
            venue_shared: venue.is_member(),
            venue_gone: false,
            venue_id: venue.id,
            venue_name: venue.name,
            load_generation: generation,
            rows: Rc::from(Vec::new()),
            folders: Rc::from(Vec::new()),
            place: Place::All,
            track: None,
            shown: Rc::from(Vec::new()),
            counts: Rc::from(Vec::new()),
            all_count: 0,
            scores: None,
            column: Column::Places,
            widths,
            songs_scroll: UniformListScrollHandle::new(),
            menu: None,
            rename: None,
            loaded: false,
            load_failed: false,
            ownership: Ownership::Mine,
            query: String::new(),
            search,
            _search_subscription: search_subscription,
            user: self.library.user_id(),
            search_focus,
            arrival: None,
        });
        self.song_list = false;
        self.song_list_close = None;
        // The arriving venue's state goes on screen now, so a body opened
        // before the next draw opens into it.
        self.sync_venue_modes(cx);
        self.restore_venue_modes(&venue_id, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.with_tracks_for_venue(&venue_id, generation, cx, |state| {
                    state.loaded = true;
                    match result {
                        Ok(rows) => {
                            state.rows = rows.into();
                            state.refilter();
                        }
                        Err(error) => {
                            log::warn!("Could not load the songs: {error}");
                            state.load_failed = true;
                        }
                    }
                });
                // A saved score needs its track's row, so it reopens once the
                // catalogue is here — unless another venue's menu sent the
                // window here for a score of its own choosing.
                let arrival = this
                    .songs
                    .as_mut()
                    .filter(|songs| {
                        songs.venue_id == venue_id && songs.load_generation == generation
                    })
                    .and_then(|songs| songs.arrival.take());
                if arrival.is_some() {
                    this.saved_venues.forget_score(&venue_id);
                }
                this.restore_venue_score(&venue_id, cx);
                if let Some((track, score)) = arrival {
                    this.open_score(&track, score, None, cx);
                }
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |_, _| {
            let _ = remember.await;
        })
        .detach();
    }

    fn show_ownership(&mut self, ownership: Ownership, cx: &mut Context<Self>) {
        self.with_tracks(cx, |state| {
            state.ownership = ownership;
            state.refilter();
        });
    }

    /// Mirror an edit of the search field. Filtering is immediate, with no
    /// debounce: the work is one pass over a `Vec` and a virtualized list that
    /// redraws a screenful either way.
    fn track_search_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.with_tracks(cx, |state| {
            state.query = query;
            state.refilter();
        });
    }

    /// Escape inside the search field clears it rather than reaching the
    /// shell. The field's own keymap leaves escape unbound (it is navigation,
    /// not text), so it arrives here — and a query is the nearest thing to
    /// dismiss when one is up.
    fn track_search_escape(&mut self, cx: &mut Context<Self>) {
        let field = self
            .songs
            .as_ref()
            .filter(|state| !state.query.is_empty())
            .map(|state| state.search.clone());
        if let Some(field) = field {
            field.update(cx, |field, cx| field.set_text("", cx));
        }
    }

    /// Run a synchronous edit against the open venue's songs.
    fn with_tracks(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Tracks)) {
        if let Some(state) = &mut self.songs {
            edit(state);
            cx.notify();
        }
    }

    /// Admit an asynchronous venue read only while that venue is still open.
    /// Both initial loads and post-membership refreshes use this rule.
    pub(crate) fn with_tracks_for_venue(
        &mut self,
        venue_id: &str,
        generation: u64,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Tracks),
    ) {
        let Some(state) = &mut self.songs else {
            return;
        };
        if state.venue_id() != venue_id || state.load_generation != generation {
            return;
        }
        edit(state);
        // A loaded catalogue is the only statement this shell ever gets about
        // which tracks exist: there is no delete gesture, so a track that has
        // stopped appearing here has gone, and a score remembered under it can
        // never be reached again. Read before `notify` so the list and the
        // open score agree within one frame.
        let surviving: Option<Vec<String>> = state
            .loaded
            .then(|| state.rows.iter().map(|row| row.id.clone()).collect());
        cx.notify();
        // Which songs a folder holds is a question about the same rows, so the
        // folders are re-read with them: every load of the catalogue comes
        // through here.
        self.reload_folders(cx);
        if let Some(surviving) = surviving {
            self.prune_tracks(venue_id, &surviving, cx);
        }
    }

    /// The open score's track.
    pub(crate) fn front_track(&self) -> Option<&str> {
        match self.workspace.of_mode(crate::workspace::Mode::Arrange)? {
            crate::workspace::Slot {
                target: Target::Score { track, .. },
                ..
            } => Some(track),
            _ => None,
        }
    }

    /// Select `place` in the first column. Another place opens with nothing
    /// selected in it, as a folder does in Finder. A search lists every
    /// place's songs, so choosing a place ends it.
    pub(crate) fn select_place(&mut self, place: Place, cx: &mut Context<Self>) {
        let Some(state) = &mut self.songs else {
            return;
        };
        state.menu = None;
        if *state.shown_place() != place {
            state.track = None;
            state.scores = None;
        }
        state.place = place;
        state.column = Column::Places;
        let field = state.searching().then(|| state.search.clone());
        state.refilter();
        cx.notify();
        if let Some(field) = field {
            field.update(cx, |field, cx| field.set_text("", cx));
        }
    }

    /// Select `track_id` in the second column, and list its scores in the
    /// third. Selecting the song already selected keeps its listing.
    pub(crate) fn select_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(state) = &mut self.songs else {
            return;
        };
        state.menu = None;
        state.column = Column::Songs;
        if let Some(index) = state
            .shown
            .iter()
            .position(|row| state.rows[*row].id == track_id)
        {
            state
                .songs_scroll
                .scroll_to_item(index, ScrollStrategy::Top);
        }
        if state.track.as_deref() == Some(track_id) && state.scores.is_some() {
            if let Some(scores) = &mut state.scores {
                scores.cursor = None;
            }
            cx.notify();
            return;
        }
        let Some(track) = state.find(track_id) else {
            return;
        };
        state.track = Some(track_id.to_string());
        state.scores = Some(scores::Scores::new(track));
        cx.notify();
        self.load_track_scores(track_id, cx);
    }

    /// A key the song list's columns answer: the arrows walk the selection,
    /// Enter opens the score under the cursor. Reports whether it was one.
    fn song_list_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(state) = &mut self.songs else {
            return false;
        };
        let step = match key {
            "up" => -1,
            "down" => 1,
            _ => 0,
        };
        match (state.column, key) {
            (Column::Places, "up" | "down") => {
                let places = state.places();
                let at = places
                    .iter()
                    .position(|place| place == state.shown_place())
                    .unwrap_or(0);
                let next = (at as isize + step).clamp(0, places.len() as isize - 1) as usize;
                self.select_place(places[next].clone(), cx);
            }
            (Column::Places, "right" | "enter") => {
                let first = state.shown.first().map(|row| state.rows[*row].id.clone());
                if let Some(track) = state.track.clone().or(first) {
                    self.select_track(&track, cx);
                }
            }
            (Column::Songs, "up" | "down") => {
                if state.shown.is_empty() {
                    return true;
                }
                let next = match state.selected_song() {
                    None => 0,
                    Some(at) => {
                        (at as isize + step).clamp(0, state.shown.len() as isize - 1) as usize
                    }
                };
                let track = state.rows[state.shown[next]].id.clone();
                self.select_track(&track, cx);
            }
            (Column::Songs, "left") => {
                state.track = None;
                state.scores = None;
                state.column = Column::Places;
                cx.notify();
            }
            (Column::Songs, "right" | "enter") => {
                let venue = state.venue_id.clone();
                if let Some(scores) = state.scores.as_mut().filter(|scores| scores.loaded) {
                    if !scores.items(&venue).is_empty() {
                        scores.cursor = Some(scores.cursor.unwrap_or(0));
                        state.column = Column::Scores;
                        cx.notify();
                    }
                }
            }
            (Column::Scores, "up" | "down") => {
                let venue = state.venue_id.clone();
                if let Some(scores) = &mut state.scores {
                    let last = scores.items(&venue).len().saturating_sub(1) as isize;
                    let at = scores.cursor.map_or(-1, |at| at as isize);
                    scores.cursor = Some((at + step).clamp(0, last) as usize);
                    cx.notify();
                }
            }
            (Column::Scores, "left") => {
                if let Some(scores) = &mut state.scores {
                    scores.cursor = None;
                }
                state.column = Column::Songs;
                cx.notify();
            }
            (Column::Scores, "enter") => {
                let venue = state.venue_id.clone();
                let chosen = state.scores.as_ref().and_then(|scores| {
                    let item = *scores.items(&venue).get(scores.cursor?)?;
                    Some((scores.track.id.clone(), item))
                });
                if let Some((track, item)) = chosen {
                    self.choose_score_item(&track, item, cx);
                }
            }
            (Column::Scores, "right") => {}
            _ => return false,
        }
        true
    }

    /// Give the keyboard to the columns: a press on a row there is a press in
    /// a keyboard-driven view, and the arrows should answer it next.
    fn focus_columns(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    /// Down from the search field: the keyboard goes to the first match.
    fn search_to_songs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = &self.songs else {
            return;
        };
        let Some(first) = state.shown.first().map(|row| state.rows[*row].id.clone()) else {
            return;
        };
        self.focus_columns(window, cx);
        self.select_track(&first, cx);
    }

    /// Follow the pointer while one of the column seams is dragged: `column`
    /// is the column whose right edge it is, and `x` the pointer's distance
    /// from the page's left edge.
    fn drag_song_column(&mut self, column: usize, x: f32, room: f32, cx: &mut Context<Self>) {
        let Some(state) = &mut self.songs else {
            return;
        };
        let lead: f32 = state.widths[..column].iter().map(|w| w + HAIRLINE).sum();
        let others: f32 = state.widths.iter().sum::<f32>() - state.widths[column];
        let ceiling = (room - others - 2. * HAIRLINE - SCORES_MIN).max(COLUMN_MIN);
        state.widths[column] = (x - lead).clamp(COLUMN_MIN, ceiling).round();
        cx.notify();
    }
}

/// The song list's keys, for the region that holds the keyboard while the
/// page is up. Only while the columns themselves have it: a field typing a
/// name or a query keeps its arrows and its Enter.
pub(crate) fn on_key(app: &Entity<Luma>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) {
    let app = app.clone();
    move |event, window, cx| {
        if !app.read(cx).focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        let key = event.keystroke.key.clone();
        if app.update(cx, |this, cx| this.song_list_key(&key, cx)) {
            cx.stop_propagation();
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// The page's inset from its edges, and the gap between a head's controls.
const PAD_X: f32 = 16.;
const GAP: f32 = 12.;
/// The page head's height: the title, the search, the filter and Add songs.
const HEAD_HEIGHT: f32 = 52.;
/// A song row: art over its padding.
/// Every row in every column, places, songs and scores alike: one height,
/// so a row in one column lines up with its neighbours in the next. A song's
/// art is this square.
pub(crate) const ROW: f32 = 36.;
/// How far a row's words sit in from the column's edge.
pub(crate) const ROW_INSET: f32 = 12.;
/// The coverage dot's box.
const DOT: f32 = 6.;
/// The search field's width in the head.
const SEARCH_WIDTH: f32 = 260.;
/// The places' and the songs' widths before either is dragged, the narrowest
/// either can be dragged to, and what the scores always keep.
const DEFAULT_WIDTHS: [f32; 2] = [200., 340.];
const COLUMN_MIN: f32 = 140.;
const SCORES_MIN: f32 = 260.;
/// The rule between two columns.
const HAIRLINE: f32 = 1.;

/// The song list page: the head, then the three columns.
///
/// `statuses` are the open bodies' status dots, drawn on the scores that open
/// them and on the songs and folders that hold those.
pub(crate) fn page(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, ChatStatus>,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut Context<Luma>,
) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .text_color(glass::ink(0.95))
        .child(header(state, app, window))
        .child(match state.load_failed {
            true => luma_ui::plate(
                "Could not load the songs".to_string(),
                ladder::muted_foreground(),
            ),
            false if !state.loaded => {
                luma_ui::plate("Loading tracks…".to_string(), ladder::muted_foreground())
            }
            false => columns(shell, state, statuses, app, cx).into_any_element(),
        })
        // Siblings of the columns, not children of a row: a column clips,
        // and a menu inside it would be cut off at its edge.
        .children(folders::menu(state, app))
        .children(scores::menu(state, app))
        .agent_node(Role::Card, "Song list")
}

/// The page's head: its title and count, the search, the ownership filter,
/// and the way to add songs.
fn header(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    let add = app.clone();
    let count = state.venue_rows().count();
    div()
        .h(px(HEAD_HEIGHT))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .border_b_1()
        .border_color(glass::panel_seam())
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Songs"),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(glass::ink(glass::TEXT_TERTIARY))
                .child(format!("{count} in {}", state.venue_name)),
        )
        .child(div().flex_1())
        .child(search(state, app, window))
        .child(ownership_filter(state, app))
        .child(
            luma_ui::button("Add songs", luma_ui::Enabled::Yes)
                .id("add-track")
                .on_click(move |_, _, cx| add.update(cx, |this, cx| this.show_add_tracks(cx)))
                .agent_node(Role::Button, "Add track"),
        )
}

/// The header's one text size.
const HEADER_TEXT: f32 = 13.;

/// The search field: a [`float::field`] that takes keystrokes.
fn search(state: &Tracks, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    let empty = state.query.is_empty();
    let text = if empty { PLACEHOLDER } else { &state.query };
    let focus = state.search_focus.clone();
    let typed = app.clone();
    float::field()
        .id("search")
        .w(px(SEARCH_WIDTH))
        .flex_none()
        .h(px(28.))
        .text_size(px(HEADER_TEXT))
        .gap(px(6.))
        // Keys the field leaves unbound bubble through here: Escape clears
        // the query (see `Luma::track_search_escape`), and Down goes to the
        // first match.
        .on_key_down(
            move |event, window, cx| match event.keystroke.key.as_str() {
                "escape" => typed.update(cx, |this, cx| this.track_search_escape(cx)),
                "down" => {
                    cx.stop_propagation();
                    typed.update(cx, |this, cx| this.search_to_songs(window, cx));
                }
                _ => {}
            },
        )
        .child(
            gpui_component::Icon::new(luma_ui::icons::IconName::Search)
                .size(px(14.))
                .text_color(glass::ink(glass::TEXT_TERTIARY)),
        )
        .child(div().flex_1().min_w(px(0.)).child(state.search.clone()))
        // The semantic label is the field's VALUE once there is one: a driver
        // asserting on this is asking what it says, not what it would say if
        // empty.
        .agent_node(Role::Input, text.to_string())
        .agent_focused(focus.is_focused(window))
}

/// What the search says while it is empty.
const PLACEHOLDER: &str = "Search tracks";

/// The `mine` / `all` axis: always exactly one pressed.
fn ownership_filter(state: &Tracks, app: &Entity<Luma>) -> Div {
    float::segmented().children(Ownership::ALL.into_iter().map(|ownership| {
        let app = app.clone();
        float::segment(
            ownership.label(),
            ownership == state.ownership,
            ownership.label(),
        )
        .id(ownership.label())
        .on_click(move |_, _, cx| app.update(cx, |this, cx| this.show_ownership(ownership, cx)))
        .agent_node(Role::Toggle, ownership.label())
    }))
}

/// The three columns, hairlines between them, and the grips that drag the
/// first two's widths.
fn columns(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, ChatStatus>,
    app: &Entity<Luma>,
    cx: &mut Context<Luma>,
) -> Div {
    let [places, songs] = state.widths;
    let (first, second) = (app.clone(), app.clone());
    let column = |width: f32| div().w(px(width)).flex_none().h_full().min_h_0();
    let rule = || {
        div()
            .w(px(HAIRLINE))
            .flex_none()
            .h_full()
            .bg(glass::panel_seam())
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .relative()
        .on_drag_move::<PlacesResize>(move |event, _, cx| {
            let x = f32::from(event.event.position.x - event.bounds.left());
            let room = f32::from(event.bounds.size.width);
            first.update(cx, |this, cx| this.drag_song_column(0, x, room, cx));
        })
        .on_drag_move::<SongsResize>(move |event, _, cx| {
            let x = f32::from(event.event.position.x - event.bounds.left());
            let room = f32::from(event.bounds.size.width);
            second.update(cx, |this, cx| this.drag_song_column(1, x, room, cx));
        })
        .child(column(places).child(folders::column(state, statuses, app)))
        .child(rule())
        .child(column(songs).child(songs_column(shell, state, statuses, app)))
        .child(rule())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(scores::column(shell, state, statuses, app)),
        )
        .child(
            pane::resize_handle(
                "places-seam",
                pane::Seam::Vertical,
                places + HAIRLINE / 2.,
                || PlacesResize,
                |app: &mut Luma, _| reset_width(app, 0),
                glass::glass_hover(),
                cx,
            )
            .agent_node(Role::Slider, "Folders column width"),
        )
        .child(
            pane::resize_handle(
                "songs-seam",
                pane::Seam::Vertical,
                places + HAIRLINE + songs + HAIRLINE / 2.,
                || SongsResize,
                |app: &mut Luma, _| reset_width(app, 1),
                glass::glass_hover(),
                cx,
            )
            .agent_node(Role::Slider, "Songs column width"),
        )
}

/// The seam right of the places, under the pointer — routed by type like the
/// shell's own seams.
struct PlacesResize;
/// The seam right of the songs.
struct SongsResize;

/// A double-click on a seam: that column back to its default width.
fn reset_width(app: &mut Luma, column: usize) {
    if let Some(state) = &mut app.songs {
        state.widths[column] = DEFAULT_WIDTHS[column];
    }
}

/// The second column: the songs in the selected place. `uniform_list`
/// virtualizes them, so a library of thousands costs one screenful.
fn songs_column(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, ChatStatus>,
    app: &Entity<Luma>,
) -> AnyElement {
    if state.shown.is_empty() {
        let empty = if state.rows.is_empty() {
            "No tracks imported"
        } else if state.searching() {
            "No matching tracks"
        } else if matches!(state.place, Place::Folder(_)) {
            "No songs in this folder"
        } else {
            "No matching tracks"
        };
        return float::empty_row(empty)
            .agent_node(Role::Text, empty)
            .into_any_element();
    }
    let rows = Rc::clone(&state.rows);
    let shown = Rc::clone(&state.shown);
    let selected = state.track.clone();
    let path = state.path_state(Column::Songs, true);
    let open = shell.front_track().map(str::to_string);
    // Each song's dot: the strongest of its scores' tabs in this venue.
    let mut dots: HashMap<String, ChatStatus> = HashMap::new();
    for (target, status) in statuses {
        if let Target::Score { venue, track, .. } = target {
            if *venue == state.venue_id {
                let dot = dots.entry(track.clone()).or_insert(*status);
                *dot = dot.max(*status);
            }
        }
    }
    let app = app.clone();
    uniform_list("songs", shown.len(), move |range, _, _| {
        range
            .map(|index| {
                let track = &rows[shown[index]];
                let chosen = selected.as_deref() == Some(track.id.as_str());
                song_row(
                    track,
                    index,
                    if chosen { path } else { Lit::Rest },
                    open.as_deref() == Some(track.id.as_str()),
                    dots.get(&track.id).copied(),
                    &app,
                )
            })
            .collect()
    })
    .track_scroll(&state.songs_scroll)
    .size_full()
    .into_any_element()
}

/// One song: its art, title, artist and tempo, its length and how many scores
/// this venue holds for it, and its coverage dot. A press selects it; a
/// right-click is the folder menu.
fn song_row(
    track: &TrackBrowserRow,
    index: usize,
    state: Lit,
    open: bool,
    status: Option<ChatStatus>,
    app: &Entity<Luma>,
) -> AnyElement {
    let name = track_name(track);
    let pressed = app.clone();
    let raised = app.clone();
    let track_id = track.id.clone();
    let menu_track = track.id.clone();
    let key = SharedString::from(format!("song-{}", track.id));
    let artist = track
        .artist
        .clone()
        .unwrap_or_else(|| "Unknown artist".into());
    let subtitle = sub_line(&artist, track.bpm);
    let length = track.duration_seconds.map_or_else(String::new, length);
    column_row(state, index, key)
        .pr(px(ROW_INSET))
        .on_click(move |_, window, cx| {
            let track_id = track_id.clone();
            pressed.update(cx, |this, cx| {
                this.focus_columns(window, cx);
                this.select_track(&track_id, cx);
            });
        })
        .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            let menu = folders::Menu::Song {
                at: event.position,
                track_id: menu_track.clone(),
            };
            raised.update(cx, |this, cx| this.open_song_menu(menu, cx));
        })
        .child(album_art(track.album_art_path.as_deref(), ROW).rounded_none())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(px(12.5))
                        .line_height(px(16.))
                        .child(name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.5))
                        .line_height(px(14.))
                        .text_color(glass::ink(glass::TEXT_SECONDARY))
                        .child(subtitle.clone())
                        .agent_node(Role::Text, subtitle),
                ),
        )
        .children(status.map(|status| crate::agent::status_dot(status, &name)))
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_end()
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .font_family(luma_ui::fonts::MONO)
                        .text_color(glass::ink(if open { 0.95 } else { glass::TEXT_SECONDARY }))
                        .child(length),
                )
                .child(score_count(track)),
        )
        .child(
            div()
                .flex_none()
                .size(px(DOT))
                .children(coverage_dot(track)),
        )
        .agent_node(Role::Row, name)
        .into_any_element()
}

/// How lit a column row is: at rest, on the selection path in a column
/// without the keyboard, or on it in the column with the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Lit {
    Rest,
    Path,
    Focused,
}

/// One row of any column, [`ROW`] tall and the column's full width. Lit by a
/// plain fill, edge to edge: no rounding, no inset, no ring. A row at rest
/// takes the hover fill, which is lighter than either path fill.
///
/// `row` is the row's place in its run — the column's rows from its top or
/// from the heading above them — and every second one is striped, Finder's
/// way, with a fill under hover's.
pub(crate) fn column_row(lit: Lit, row: usize, id: impl Into<SharedString>) -> Stateful<Div> {
    let rest = if row % 2 == 1 {
        glass::wash(glass::WASH_STRIPE)
    } else {
        glass::wash(0.)
    };
    let el = div()
        .id(id.into())
        .w_full()
        .h(px(ROW))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(px(12.5))
        .cursor_pointer();
    match lit {
        Lit::Focused => el.bg(glass::wash(glass::WASH_EMPHASIS)),
        Lit::Path => el.bg(glass::wash(glass::WASH_PRESSED)),
        Lit::Rest => el
            .bg(rest)
            .hover(|style| style.bg(glass::wash(glass::WASH_SUBTLE)))
            .fade_hover(),
    }
}

/// The line under a song's title: `artist · 128 BPM`, the tempo's fraction
/// shown only when it has one.
fn sub_line(artist: &str, bpm: Option<f64>) -> String {
    match bpm {
        Some(bpm) if bpm.fract().abs() < 0.05 => format!("{artist} · {bpm:.0} BPM"),
        Some(bpm) => format!("{artist} · {bpm:.1} BPM"),
        None => artist.to_string(),
    }
}

/// A length as the list prints it: `4:12`.
fn length(seconds: f64) -> String {
    let total = seconds.max(0.).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// The row's cover thumbnail: a neutral plate, with the art painted over it
/// when the track has any.
///
/// The plate is always there and the image sits *inside* it, which is what
/// makes the two states one rect: a track with no art, a path that no longer
/// resolves, and a decode still in flight all show the same square, and none
/// of them can reflow the row.
pub(crate) fn album_art(path: Option<&str>, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(luma_ui::radius::CHIP))
        .overflow_hidden()
        .bg(glass::wash(glass::WASH_SUBTLE))
        .children(
            path.filter(|path| !path.is_empty())
                .map(|path| img(PathBuf::from(path)).size(px(size))),
        )
}

/// The title, or `file_path`'s basename as the last resort.
pub(crate) fn track_name(track: &TrackBrowserRow) -> String {
    if let Some(title) = track.title.as_ref().filter(|t| !t.is_empty()) {
        return title.clone();
    }
    std::path::Path::new(&track.file_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| track.file_path.clone())
}

/// How many scores this venue holds for the track. Zero says so quietly
/// rather than printing a `0`. This is the only place the count is spelled —
/// the automation label is this element's, so a script and a pair of eyes
/// cannot be told different numbers.
fn score_count(track: &TrackBrowserRow) -> impl IntoElement {
    let count = track.venue_score_count;
    let (text, alpha) = match count {
        n if n <= 0 => ("No scores".to_string(), glass::TEXT_TERTIARY),
        1 => ("1 score".to_string(), glass::TEXT_TERTIARY),
        n => (format!("{n} scores"), glass::TEXT_TERTIARY),
    };
    div()
        .text_size(px(10.5))
        .line_height(px(14.))
        .text_color(glass::ink(alpha))
        .child(text)
        // Track-scoped, because a script finds nodes across the whole tree
        // and a bare count would match every row at once.
        .agent_node(
            Role::Text,
            format!("{} venue scores: {}", track_name(track), count.max(0)),
        )
}

/// How much of this track the venue's annotations cover, as one dot: red for
/// none, amber for partial, green from 70% up. A track with no duration has
/// nothing to be a fraction of, so it gets no dot at all.
fn coverage_dot(track: &TrackBrowserRow) -> Option<Div> {
    let duration = track.duration_seconds.filter(|seconds| *seconds > 0.)?;
    let covered = (track.venue_annotation_coverage_seconds / duration).clamp(0., 1.);
    let color = if covered == 0. {
        ladder::status_bad()
    } else if covered >= 0.7 {
        ladder::status_ok()
    } else {
        ladder::status_warn()
    };
    Some(div().size(px(DOT)).rounded_full().bg(color))
}
