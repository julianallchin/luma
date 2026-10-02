//! The track browser: one venue's library, as the shell's sidebar.
//!
//! A row list at sidebar width, in comet's session-row anatomy: a status lead,
//! the title, and `artist · bpm` muted under it. An ownership filter and a
//! search work over the venue's rows. There are no added-by or preprocessing
//! columns; a wider tracks surface can add them if one is ever wanted.
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
//! # Folders
//!
//! Under the venue's own row the list shows the venue's folders, each opening
//! onto the songs it holds, and then every song under "All songs". A song in a
//! folder is the same song as under All songs: it opens the same scores. See
//! [`folders`].
//!
//! Album art comes from `album_art_path` — a path on disk, never inlined bytes
//! (see CLAUDE.md on why bulk responses carry paths). The native host reads
//! the file with `img(path)` and
//! GPUI's image cache handles the decode and the lazy load.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, RowState};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{ladder, motion};

use luma_lib::models::folders::Folder;
use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;

use crate::agent::TabStatus;
use crate::tabs::Target;
use crate::Luma;

pub(crate) mod folders;
pub(crate) mod scores;

/// Which of the sidebar's two levels the column is showing.
///
/// A *level*, not a screen: both are the same column showing the same
/// person's library at two depths, and the way between them is one gesture
/// with one reverse. Keeping them in one enum is what makes "the sidebar is
/// somewhere" a single fact — the filters, the search and the scroll offset
/// all belong to [`Level::Tracks`] and travel with it.
pub(crate) enum Level {
    Tracks,
    /// Boxed for the reason [`crate::Luma::sign_in`] is: the deep level
    /// carries a whole track row and a listing, and the shallow one is the
    /// state the sidebar is in for most of a session.
    Scores(Box<scores::Scores>),
}

/// Which way a level change is travelling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Into the scores: the tracks leave left, the scores arrive from the right.
    In,
    /// Back out again — the exact reverse.
    Out,
}

/// A level change in flight.
///
/// Driven by hand off the wall clock rather than by `with_animation`, for the
/// reason [`luma_ui::pane::PaneWidth`] states: gpui keys an animation
/// element's start time by its element-id path, so any remount above it
/// replays the tween from zero — and a virtualized row list remounts
/// constantly.
pub(crate) struct Push {
    direction: Direction,
    /// Where the flying track row starts (or ends, popping): the top of its
    /// list row, in pixels from the top of the pushing region.
    ///
    /// Snapshotted at the gesture rather than derived per frame, because the
    /// list it was measured in is sliding away underneath the flight.
    row_top: f32,
    started: Instant,
}

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

/// One line of the list. Every kind is [`ROW_HEIGHT`] tall: the list is a
/// `uniform_list`, which sizes every item like its first.
#[derive(Clone, Copy)]
enum Entry {
    /// A folder; `folder` indexes [`Tracks::folders`], and `songs` is how
    /// many of its songs the filters admit.
    Folder {
        folder: usize,
        songs: usize,
        open: bool,
    },
    /// The way to make another folder.
    NewFolder,
    /// The heading over every song, with how many the filters admit.
    AllSongs { songs: usize },
    /// What All songs says when it lists nothing.
    NoSongs,
    /// A song; `row` indexes [`Tracks::rows`]. `folder` is the folder it is
    /// listed under, or none under All songs.
    Song { row: usize, folder: Option<usize> },
}

/// The screen's whole state: the venue it is showing, everything the seam
/// returned for it, and the three filters over that.
///
/// [`Self::entries`] is indices into [`Self::rows`] and [`Self::folders`]
/// rather than a second copy of them, recomputed on every state change instead
/// of on every draw: a draw happens per frame and per hover, and re-filtering
/// a full library there is the difference between a scroll that keeps up and
/// one that does not.
pub struct Tracks {
    /// The venue the rows were decorated for. The editor needs it too — a
    /// track's score is per-venue — so it is kept beside the name it is shown
    /// under rather than re-derived from the screen that opened this one.
    venue_id: String,
    venue_name: String,
    load_generation: u64,
    rows: Rc<[TrackBrowserRow]>,
    /// The venue's folders, by name, each with the songs it holds. Re-read
    /// whenever the rows are — see [`Luma::with_tracks_for_venue`].
    folders: Rc<[Folder]>,
    /// The open folders, by id.
    expanded: HashSet<String>,
    /// What the list draws, top to bottom.
    entries: Rc<[Entry]>,
    /// The entry the scores level was entered from. A song can be listed twice
    /// — in a folder and under All songs — so its track alone does not say
    /// which row the shared element left.
    origin: Option<usize>,
    /// The menu a right-click raised, or none. One slot: two menus open at
    /// once is the bug it rules out.
    menu: Option<folders::Menu>,
    /// The folder whose name is being typed, or none.
    rename: Option<folders::Rename>,
    /// Why the last folder change failed, until the next one lands.
    folder_error: Option<String>,
    /// Whether the venue's query has come back. Written in the same
    /// assignment as [`Self::rows`] and [`Self::error`], so "still loading"
    /// and "nothing to show" can never be confused for one another.
    loaded: bool,
    error: Option<String>,
    ownership: Ownership,
    /// The typed query, mirrored out of [`Self::search`] on every edit. The
    /// field is the editor; this is what `refilter` reads.
    query: String,
    search: Entity<luma_ui::text_input::TextInput>,
    _search_subscription: Subscription,
    /// The signed-in principal, snapshotted when the venue was selected. The
    /// ownership filter reads it.
    user: Option<String>,
    /// Exact return target for the venue dialog. Pointer activation focuses
    /// this handle before opening the overlay, so Escape restores the control
    /// itself rather than the sidebar region generically.
    venue_focus: FocusHandle,
    search_focus: FocusHandle,
    /// Which level the column is on, and the change taking it there.
    pub(crate) level: Level,
    push: Option<Push>,
    /// The row list's scroll offset, which the shared element's start position
    /// is measured against — a row's `y` is its index times [`ROW_HEIGHT`]
    /// plus this.
    list_scroll: UniformListScrollHandle,
    /// The pushing region's box and the list viewport's, as they painted last.
    /// A click carries a window position but not the boxes it landed in, so
    /// the two that the flight's arithmetic needs are probed — see
    /// [`luma_ui::arg::bounds_probe`].
    region: Rc<Cell<Option<Bounds<Pixels>>>>,
    list_box: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Tracks {
    /// The venue this screen was opened for. The window title is the only
    /// reader outside this module.
    pub(crate) fn venue_name(&self) -> &str {
        &self.venue_name
    }

    /// The venue a row click opens the editor in.
    pub(crate) fn venue_id(&self) -> &str {
        &self.venue_id
    }

    pub(crate) fn load_generation(&self) -> u64 {
        self.load_generation
    }

    /// The loaded row a click carries. Looked up by id rather than by index
    /// because the click was registered against a *filtered* list, and the
    /// filters can change before it lands.
    /// First row whose title contains `needle`, for the launch-time
    /// reproduction driver. Substring rather than exact because the titles this
    /// is aimed at are long enough that nobody types them correctly.
    pub(crate) fn find_titled(&self, needle: &str) -> Option<TrackBrowserRow> {
        self.rows
            .iter()
            .find(|row| {
                row.title
                    .as_deref()
                    .is_some_and(|title| title.to_lowercase().contains(&needle.to_lowercase()))
            })
            .cloned()
    }

    pub(crate) fn find(&self, track_id: &str) -> Option<TrackBrowserRow> {
        self.rows.iter().find(|row| row.id == track_id).cloned()
    }

    /// Every track in the venue, whatever the sidebar's filters say — the
    /// rows the `+`'s combo box lists.
    pub(crate) fn venue_rows(&self) -> impl Iterator<Item = &TrackBrowserRow> {
        self.rows.iter().filter(|row| row.is_in_venue)
    }

    /// Adopt a freshly venue-decorated library read after an add-track action.
    pub(crate) fn replace_rows(&mut self, rows: Vec<TrackBrowserRow>) {
        self.rows = rows.into();
        self.loaded = true;
        self.error = None;
        self.refilter();
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

    /// Rebuild [`Self::entries`]: each folder, followed by its songs when it
    /// is open; the way to make another; then every song. A folder lists the
    /// songs the filters admit, in the same order as All songs.
    fn refilter(&mut self) {
        let shown = self.filter();
        let mut entries = Vec::with_capacity(self.folders.len() + shown.len() + 3);
        for (index, folder) in self.folders.iter().enumerate() {
            let held: HashSet<&str> = folder.track_ids.iter().map(String::as_str).collect();
            let songs: Vec<usize> = shown
                .iter()
                .copied()
                .filter(|row| held.contains(self.rows[*row].id.as_str()))
                .collect();
            let open = self.expanded.contains(&folder.id);
            entries.push(Entry::Folder {
                folder: index,
                songs: songs.len(),
                open,
            });
            if open {
                entries.extend(songs.into_iter().map(|row| Entry::Song {
                    row,
                    folder: Some(index),
                }));
            }
        }
        entries.push(Entry::NewFolder);
        entries.push(Entry::AllSongs { songs: shown.len() });
        if shown.is_empty() {
            entries.push(Entry::NoSongs);
        }
        entries.extend(
            shown
                .into_iter()
                .map(|row| Entry::Song { row, folder: None }),
        );
        self.entries = entries.into();
    }

    /// The track of the song at `index`, if that entry is a song.
    fn song_at(&self, index: usize) -> Option<&TrackBrowserRow> {
        match self.entries.get(index)? {
            Entry::Song { row, .. } => Some(&self.rows[*row]),
            _ => None,
        }
    }

    /// The first entry that lists `track_id`.
    fn first_song(&self, track_id: &str) -> Option<usize> {
        (0..self.entries.len()).find(|index| {
            self.song_at(*index)
                .is_some_and(|track| track.id == track_id)
        })
    }

    /// Push to `level`, with the shared element starting at `row_top`.
    ///
    /// Under reduced motion the column simply *is* on the new level: no
    /// flight, no ghost, nothing to wait for — the same rule
    /// [`luma_ui::pane::PaneWidth::retarget`] applies to a sliding region.
    fn enter(&mut self, level: scores::Scores, row_top: f32, cx: &App) {
        self.level = Level::Scores(Box::new(level));
        self.push = (!motion::reduced_motion(cx)).then(|| Push {
            direction: Direction::In,
            row_top,
            started: Instant::now(),
        });
    }

    /// Pop back to the track list. The flight is the entrance's exact reverse,
    /// so it reads the row's original position back out of the push it is
    /// undoing rather than measuring a list that is not on screen.
    fn leave(&mut self, cx: &App) {
        if !matches!(self.level, Level::Scores(_)) {
            return;
        }
        if motion::reduced_motion(cx) {
            self.level = Level::Tracks;
            self.push = None;
            return;
        }
        let row_top = self.push.as_ref().map_or_else(
            || self.row_top(self.flying_index().unwrap_or(0)),
            |push| push.row_top,
        );
        self.push = Some(Push {
            direction: Direction::Out,
            row_top,
            started: Instant::now(),
        });
    }

    /// How far the column is towards the scores level: 0 is the track list, 1
    /// is the scores, and anything between is a push in flight.
    fn progress(&self) -> f32 {
        let Some(push) = &self.push else {
            return match self.level {
                Level::Tracks => 0.,
                Level::Scores(_) => 1.,
            };
        };
        let eased = motion::exit_progress(&motion::PUSH, push.started);
        match push.direction {
            Direction::In => eased,
            Direction::Out => 1. - eased,
        }
    }

    /// Whether the level change has arrived, so the frame after it can drop
    /// the bookkeeping and stop asking for frames.
    fn push_settled(&self) -> bool {
        self.push
            .as_ref()
            .is_some_and(|push| motion::exit_progress(&motion::PUSH, push.started) >= 1.)
    }

    /// The track the shared element is carrying, while one is in flight.
    fn flying(&self) -> Option<&TrackBrowserRow> {
        let Level::Scores(level) = &self.level else {
            return None;
        };
        self.push.as_ref().map(|_| &level.track)
    }

    /// Where the flying row's list position is, in pixels from the top of the
    /// pushing region. `index` is a position in [`Self::entries`], which is
    /// what the list draws.
    fn row_top(&self, index: usize) -> f32 {
        let (Some(region), Some(list)) = (self.region.get(), self.list_box.get()) else {
            return 0.;
        };
        let offset = f32::from(self.list_scroll.0.borrow().base_handle.offset().y);
        f32::from(list.origin.y - region.origin.y) + index as f32 * ROW_HEIGHT + offset
    }

    /// The scores level's track, as an index into the entries currently
    /// shown: the row it was entered from while that row still lists it, else
    /// the first that does. The position a pop flies back to.
    fn flying_index(&self) -> Option<usize> {
        let Level::Scores(level) = &self.level else {
            return None;
        };
        self.origin
            .filter(|index| {
                self.song_at(*index)
                    .is_some_and(|track| track.id == level.track.id)
            })
            .or_else(|| self.first_song(&level.track.id))
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
// screen transition, and `Luma` owns both. They live here so the router stays
// a router.

impl Luma {
    /// Select a venue: the sidebar fills with its tracks, the picker overlay
    /// closes, and the venue's own tab set comes on screen — the one it had
    /// when it was left, or the one saved at the last quit. The venue being
    /// left keeps its set, parked: a venue is a project.
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
        self.sidebar = Some(Tracks {
            venue_id: venue.id,
            venue_name: venue.name,
            load_generation: generation,
            rows: Rc::from(Vec::new()),
            folders: Rc::from(Vec::new()),
            expanded: HashSet::new(),
            entries: Rc::from(Vec::new()),
            origin: None,
            menu: None,
            rename: None,
            folder_error: None,
            loaded: false,
            error: None,
            ownership: Ownership::Mine,
            query: String::new(),
            search,
            _search_subscription: search_subscription,
            user: self.library.user_id(),
            venue_focus: cx.focus_handle().tab_stop(true),
            search_focus,
            level: Level::Tracks,
            push: None,
            list_scroll: UniformListScrollHandle::new(),
            region: Rc::new(Cell::new(None)),
            list_box: Rc::new(Cell::new(None)),
        });
        // The arriving venue's set goes on screen now, so a tab opened before
        // the next draw opens into it.
        self.sync_venue_tabs(cx);
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
                        Err(error) => state.error = Some(error.to_string()),
                    }
                });
                // A saved score tab needs its track's row, so the venue's
                // saved tabs reopen once the catalogue is here.
                this.restore_tabs(&venue_id, cx);
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |_, _| {
            let _ = remember.await;
        })
        .detach();
    }

    /// Enter the scores level for the song at `index` of what the list is
    /// showing. The index, not the id, because the flight starts at the row's
    /// *place* — and a song in a folder is on screen twice.
    fn push_scores(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.sidebar else {
            return;
        };
        let Some(track) = browser.song_at(index).map(|track| track.id.clone()) else {
            return;
        };
        let row_top = browser.row_top(index);
        browser.origin = Some(index);
        browser.menu = None;
        self.show_scores(&track, row_top, window, cx);
    }

    /// `→` in the sidebar: into the front tab's track's scores.
    ///
    /// The front tab's track rather than a focused row, because the list is
    /// virtualized and its rows carry no focus handles — the front tab is the
    /// one notion of "the row this column is currently about" that survives a
    /// scroll.
    pub(crate) fn enter_selected_scores(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        if matches!(browser.level, Level::Scores(_)) {
            return;
        }
        let Some(picked) = self.front_track() else {
            return;
        };
        let Some(index) = browser.first_song(picked) else {
            return;
        };
        self.push_scores(index, window, cx);
    }

    /// Pop back to the track list. Escape reaches this through
    /// [`Luma::dismiss_overlay`] — one dismissal ladder, innermost first.
    pub(crate) fn leave_scores(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(browser) = &mut self.sidebar else {
            return false;
        };
        if !matches!(browser.level, Level::Scores(_)) {
            return false;
        }
        browser.leave(cx);
        cx.notify();
        true
    }

    /// Retire a finished level change and keep an unfinished one drawing.
    ///
    /// Called once per frame from the shell, before the sidebar is rendered
    /// immutably — the same place and for the same reason the sidebar's own
    /// width tween is stepped there.
    pub(crate) fn tick_sidebar_push(&mut self, window: &mut Window) {
        let Some(browser) = &mut self.sidebar else {
            return;
        };
        if browser.push.is_none() {
            return;
        }
        if browser.push_settled() {
            if browser
                .push
                .as_ref()
                .is_some_and(|push| push.direction == Direction::Out)
            {
                browser.level = Level::Tracks;
            }
            browser.push = None;
        } else {
            window.request_animation_frame();
        }
    }

    fn show_ownership(&mut self, ownership: Ownership, cx: &mut Context<Self>) {
        self.with_tracks(cx, |state| {
            state.ownership = ownership;
            state.refilter();
        });
    }

    /// Mirror an edit of the sidebar filter. Filtering is immediate, with no
    /// debounce: the work is one pass over a `Vec` and a `uniform_list` that
    /// redraws a screenful either way.
    fn track_search_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.with_tracks(cx, |state| {
            state.query = query;
            state.refilter();
        });
    }

    /// Escape inside the sidebar filter clears it rather than reaching the
    /// shell. The field's own keymap leaves escape unbound (it is navigation,
    /// not text), so it arrives here — and a query is the nearest thing to
    /// dismiss when one is up.
    fn track_search_escape(&mut self, cx: &mut Context<Self>) {
        let field = self
            .sidebar
            .as_ref()
            .filter(|state| !state.query.is_empty())
            .map(|state| state.search.clone());
        if let Some(field) = field {
            field.update(cx, |field, cx| field.set_text("", cx));
        }
    }

    /// Run a synchronous edit against the selected venue's browser.
    fn with_tracks(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Tracks)) {
        if let Some(state) = &mut self.sidebar {
            edit(state);
            cx.notify();
        }
    }

    /// Admit an asynchronous venue read only while that venue still owns the
    /// sidebar. Both initial loads and post-membership refreshes use this rule.
    pub(crate) fn with_tracks_for_venue(
        &mut self,
        venue_id: &str,
        generation: u64,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Tracks),
    ) {
        let Some(state) = &mut self.sidebar else {
            return;
        };
        if state.venue_id() != venue_id || state.load_generation != generation {
            return;
        }
        edit(state);
        // A loaded catalogue is the only statement this shell ever gets about
        // which tracks exist: there is no delete gesture, so a track that has
        // stopped appearing here has gone, and the tabs remembered under it can
        // never be reached again. Read before `notify` so the strip and the
        // sidebar agree within one frame.
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
}

impl Luma {
    /// The front tab's track, when the front tab is a score.
    pub(crate) fn front_track(&self) -> Option<&str> {
        match self.workspace.active()? {
            Target::Score { track, .. } => Some(track),
            Target::Venue { .. } => None,
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// Comet's session-row anatomy at sidebar width: two text lines plus the lead.
const ROW_HEIGHT: f32 = 44.;
const GAP: f32 = 8.;
const PAD_X: f32 = 12.;
/// The coverage dot's box.
const DOT: f32 = 6.;
/// The trailing slot a track row reserves for its chevron.
const CHEVRON_SLOT: f32 = 20.;
/// The album-art thumbnail's box, and the row's second lead. Square, so the
/// placeholder and a loaded cover occupy the same rect and a row cannot change
/// shape when its art arrives.
const ART: f32 = 32.;

/// The sidebar: the two levels, the push between them, and the account at the
/// foot.
///
/// The sidebar is a launcher (`docs/specs/venue-tabs.md` rule 7): its rows
/// open tabs or bring them to the front, and the row of the front tab wears
/// the selection ring. `statuses` are the live tabs' status dots, drawn on
/// the rows that open those tabs.
///
/// Takes the whole shell rather than the browser alone, because the column's
/// two ends are about different things: the levels are the venue's, the foot
/// is the person's — and the foot is *outside* the pushing region for exactly
/// that reason.
///
/// # The push
///
/// Both levels are laid out at the column's full width and offset along one
/// axis, so neither reflows while it travels. The track row the gesture named
/// is drawn *once*, over both, flying from its place in the list to the head
/// of the arriving level — which is why the two share a horizontal inset and
/// a row height: the shared element then travels in `y` alone, and there is no
/// box interpolation to get wrong.
pub(crate) fn sidebar(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, TabStatus>,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let t = state.progress();
    let travel = crate::shell::SIDEBAR_WIDTH;
    div()
        .size_full()
        .flex()
        .flex_col()
        // Glass tier: the sidebar sits transparent on the shell's frost —
        // depth comes from the content cards beside it, not from a fill.
        .text_color(luma_ui::glass::ink(0.85))
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .relative()
                .overflow_hidden()
                .child(luma_ui::arg::bounds_into(&state.region))
                // Each level is mounted only while it is on screen. A level
                // parked off the edge at zero opacity is not merely wasted
                // layout: it is still in the accessibility tree and still a
                // tab stop, so the column would answer for two subjects at
                // once.
                .children((t < 1.).then(|| {
                    sliding(-travel * t, 1. - t)
                        .child(tracks_level(shell, state, statuses, app, window))
                }))
                .children(match &state.level {
                    Level::Scores(level) => {
                        Some(sliding(travel * (1. - t), t).child(scores::level(
                            shell,
                            state,
                            level,
                            statuses,
                            app,
                            window,
                            state.push.is_some(),
                        )))
                    }
                    Level::Tracks => None,
                })
                .children(flight(state, t))
                // A column mid-flight answers no pointer — the same rule a
                // leaving dialog follows, and for the same reason: the thing
                // under the cursor is not where it appears to be.
                .when(state.push.is_some(), |el| {
                    el.child(div().absolute().inset_0().occlude())
                }),
        )
        .child(crate::sync_status::sidebar(shell, app))
        .child(account_foot(shell, app, window))
}

/// One level, at its offset along the push. Laid out at the column's full
/// width whatever the offset, so a travelling level never re-wraps.
fn sliding(x: f32, opacity: f32) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(x))
        .w(px(crate::shell::SIDEBAR_WIDTH))
        .flex()
        .flex_col()
        .when(opacity < 1., |el| el.opacity(opacity.max(0.)))
}

/// The shared element: the track row itself, over both levels, on its way
/// between its place in the list and the head of the scores.
fn flight(state: &Tracks, t: f32) -> Option<Div> {
    let track = state.flying()?;
    let push = state.push.as_ref()?;
    Some(
        div()
            .absolute()
            .left(px(PAD_X))
            .right(px(PAD_X))
            .top(px(motion::lerp(push.row_top, scores::BACK_ROW_HEIGHT, t)))
            .child(track_face(track, true)),
    )
}

/// The track list itself: the venue's name (the way back to the picker), the
/// search, the filters, the venue's own row, and the songs.
fn tracks_level(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, TabStatus>,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let venue = Target::Venue {
        venue: state.venue_id.clone(),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(header(state, app, window))
        .child(venue_row(
            shell.workspace.active() == Some(&venue),
            statuses.get(&venue).copied(),
            app,
        ))
        .children(
            state
                .folder_error
                .clone()
                .map(|error| float::error_row(error).mx(px(PAD_X))),
        )
        .child(match &state.error {
            Some(message) => luma_ui::plate(
                format!("Failed to load tracks: {message}"),
                ladder::danger(),
            ),
            None if !state.loaded => {
                luma_ui::plate("Loading tracks…".to_string(), ladder::muted_foreground())
            }
            None => body(state, statuses, shell.front_track(), app).into_any_element(),
        })
        // A sibling of the list, not a child of a row: the list clips, and a
        // menu inside it would be cut off at the column's edge.
        .children(folders::menu(state, app))
}

/// What the foot says when nobody is signed in — the guest namespace, which is
/// a working library and not a failure. One spelling, shared with the settings
/// screen's account row.
pub(crate) const GUEST_ACCOUNT: &str = "Working locally";

/// The account, at the foot of the sidebar: who this library belongs to, and
/// the door to the two things that can be done about it.
///
/// It is here rather than in a corner of the window because it is the one
/// control that is *about the person* rather than about the venue — and
/// because a corner control shares its band with the tab strip, which means a
/// narrow window has to choose between them. The sidebar's own column always
/// has a bottom edge.
fn account_foot(shell: &Luma, app: &Entity<Luma>, window: &Window) -> Div {
    let label = shell
        .library
        .account()
        .map_or_else(|| GUEST_ACCOUNT.to_string(), |a| a.label().to_string());
    let toggle = app.clone();
    let keyed = app.clone();
    let focus = shell.account_focus.clone();
    div()
        .flex()
        .flex_shrink_0()
        .flex_col()
        // No fill of its own. The sidebar's tone is painted once, by the
        // region (`glass::tone_column`); a second wash down here would read as
        // a plane bolted to the bottom of the column rather than the end of
        // it. The hairline is the whole separation.
        .border_t_1()
        .border_color(luma_ui::glass::hairline(0.07))
        .px(px(PAD_X))
        .py(px(6.))
        .child(
            float::nav_row(RowState::Rest, "account-foot")
                .id("account")
                .track_focus(&shell.account_focus)
                .tab_stop(true)
                .on_key_down(move |event, _, cx| {
                    if event.keystroke.key == "enter" {
                        keyed.update(cx, |this, cx| this.toggle_account_menu(cx));
                    }
                })
                .on_click(move |_, window, cx| {
                    window.focus(&focus, cx);
                    toggle.update(cx, |this, cx| this.toggle_account_menu(cx));
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(luma_ui::glass::ink(0.72))
                        .child(label.clone())
                        .agent_node(Role::Text, label),
                )
                .child(
                    gpui_component::Icon::new(luma_ui::icons::IconName::ChevronUp)
                        .size(px(11.))
                        .text_color(luma_ui::glass::ink(0.45)),
                )
                .children(account_menu(shell, app))
                // Named for what it is, not for whose it is: the address
                // beside it is a value that changes with the session, and a
                // control addressed by its value cannot be found before the
                // session is known.
                .agent_node(Role::Button, "Account")
                .agent_focused(shell.account_focus.is_focused(window)),
        )
        .children(account_failure(shell))
}

/// What a sign-out that could not land says, under the row it was pressed on.
///
/// The settings screen shows the same string, but a person who never opened
/// settings pressed this gesture from the foot — and a gesture whose only
/// report lives on a screen they are not on is a gesture that silently does
/// nothing. Both doors, one message; the error is on [`crate::Luma`] rather
/// than on either screen for exactly this reason.
fn account_failure(shell: &Luma) -> Option<AnyElement> {
    let error = shell.account_action.error.clone()?;
    Some(
        div()
            .px(px(float::ROW_INSET))
            .pt(px(4.))
            .text_size(px(11.))
            .text_color(ladder::danger())
            .child(error.clone())
            .agent_node(Role::Text, error)
            .into_any_element(),
    )
}

/// The menu the foot opens, above it — the two things there are to do about an
/// account. An actions menu, not a value picker: each row goes somewhere.
///
/// A child of the trigger, so [`float::anchored_above`] has an edge to hang
/// from, and a [`luma_ui::dialog::Popup`] so it leaves with the same motion
/// every other menu in the app does.
fn account_menu(shell: &Luma, app: &Entity<Luma>) -> Option<AnyElement> {
    shell.account_menu.get()?;
    let closing = shell.account_menu.closing_since();
    let identity = if shell.account_action.signing_out {
        "Signing out…"
    } else if shell.library.user_id().is_some() {
        "Sign out"
    } else {
        "Sign in"
    };
    let pressable = closing.is_none() && !shell.account_action.signing_out;
    let dismiss = app.clone();
    let settings = app.clone();
    let switch = app.clone();
    let card = float::popover_card()
        .w(px(ACCOUNT_MENU_WIDTH))
        .child(
            float::menu_row(RowState::Rest, "account-settings")
                .id("account-settings")
                .when(closing.is_none(), |row| {
                    row.on_click(move |_, _, cx| {
                        settings.update(cx, |this, cx| {
                            this.close_account_menu(cx);
                            this.open_settings(cx);
                        });
                    })
                })
                .child(div().flex_1().min_w_0().child("Settings"))
                // The chord is the other door to this row, and saying so here
                // is the only place a person meets it.
                .child(float::key_cap().child("⌘,"))
                .agent_node(Role::Row, "Settings"),
        )
        .child(
            float::menu_row(RowState::Rest, "account-identity")
                .id("account-identity")
                .when(pressable, |row| {
                    row.on_click(move |_, _, cx| {
                        switch.update(cx, |this, cx| {
                            this.close_account_menu(cx);
                            this.switch_identity(cx);
                        });
                    })
                })
                .child(div().flex_1().min_w_0().child(identity))
                .agent_node(Role::Row, identity)
                .agent_disabled(!pressable),
        );
    Some(match closing {
        Some(since) => float::anchored_above_closing(
            "account-menu",
            float::NAV_ROW_HEIGHT,
            card.into_any_element(),
            luma_ui::motion::exit_progress(&luma_ui::motion::MENU_OUT, since),
        ),
        None => float::anchored_above(
            "account-menu",
            float::NAV_ROW_HEIGHT,
            float::Dismiss::on_press_out(move |_, cx| {
                dismiss.update(cx, |this, cx| this.close_account_menu(cx));
            }),
            card.into_any_element(),
        ),
    })
}

/// Wide enough for the longer of the two rows plus its key cap, and no wider:
/// a menu of two words that spanned the sidebar would read as a panel.
const ACCOUNT_MENU_WIDTH: f32 = 200.0;

/// The sidebar's header: the venue select, the search and the add-track
/// button, and the ownership filter.
///
/// Every control here shares one height, one text size and the field's
/// material, so the rows read as one block.
fn header(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    let add = app.clone();
    div()
        .flex()
        .flex_shrink_0()
        .flex_col()
        .gap(px(HEADER_GAP))
        .px(px(PAD_X))
        .pt(px(HEADER_GAP))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(HEADER_GAP))
                .child(venue_select(state, app, window)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(HEADER_GAP))
                .child(search(state, app, window))
                .child(
                    square(luma_ui::icons::IconName::Plus, "add-track")
                        .id("add-track")
                        .on_click(move |_, _, cx| {
                            add.update(cx, |this, cx| this.show_add_tracks(cx))
                        })
                        .agent_node(Role::Button, "Add track"),
                ),
        )
        .child(ownership_filter(state, app))
}

/// The height every header control shares: a sidebar nav row's.
const HEADER_CONTROL: f32 = float::NAV_ROW_HEIGHT;

/// The header's one text size, the same as the sidebar's rows and menus.
const HEADER_TEXT: f32 = 13.;

/// The gap between header rows, and between a field and its button.
const HEADER_GAP: f32 = 6.;

/// The icon size of a header control.
const HEADER_ICON: f32 = 14.;

/// The quiet field material every header control wears.
fn header_field() -> Div {
    float::field()
        .h(px(HEADER_CONTROL))
        .text_size(px(HEADER_TEXT))
}

/// A square header button in the field's material.
fn square(icon: luma_ui::icons::IconName, fade_key: &str) -> Div {
    let fade_key = SharedString::from(fade_key.to_string());
    let mut button = header_field()
        .w(px(HEADER_CONTROL))
        .px(px(0.))
        .justify_center()
        .cursor_pointer()
        .text_color(luma_ui::glass::ink(0.7))
        .child(gpui_component::Icon::new(icon).size(px(HEADER_ICON)))
        .bg(luma_ui::motion::hover_blend(
            &fade_key,
            luma_ui::glass::ink(0.03),
            luma_ui::glass::glass_hover(),
        ));
    button
        .interactivity()
        .on_hover(luma_ui::motion::hover_listener(fade_key));
    button
}

/// The venue select. It reopens the venue picker — the picker overlay is the
/// one venue-choosing mechanism, so the field is a door to it rather than a
/// second selector that could disagree with it.
fn venue_select(state: &Tracks, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    let picker = app.clone();
    let venue_focus = state.venue_focus.clone();
    header_field()
        .id("venue")
        .flex_1()
        .min_w(px(0.))
        .gap(px(GAP))
        .cursor_pointer()
        .track_focus(&state.venue_focus)
        .tab_stop(true)
        .on_click(move |_, window, cx| {
            window.focus(&venue_focus, cx);
            picker.update(cx, |this, cx| this.show_venues(cx));
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .child(state.venue_name.clone()),
        )
        .child(
            gpui_component::Icon::new(luma_ui::icons::IconName::ChevronDown)
                .size(px(HEADER_ICON))
                .text_color(luma_ui::glass::ink(0.45)),
        )
        .agent_node(Role::Button, state.venue_name.clone())
        .agent_focused(state.venue_focus.is_focused(window))
}

/// The venue's own row: opens the venue tab, or brings it to the front. It
/// wears the ring while that tab is in front, and the tab's status dot.
fn venue_row(front: bool, status: Option<TabStatus>, app: &Entity<Luma>) -> Div {
    let open = app.clone();
    div()
        .flex_shrink_0()
        .px(px(PAD_X))
        .pt(px(HEADER_GAP))
        .child(
            float::nav_row(RowState::of(front, false), "venue-row")
                .id("venue-row")
                .on_click(move |_, _, cx| open.update(cx, |this, cx| this.open_venue_tab(None, cx)))
                .child(
                    gpui_component::Icon::new(luma_ui::icons::IconName::Stage)
                        .size(px(HEADER_ICON))
                        .text_color(luma_ui::glass::ink(if front { 0.95 } else { 0.7 })),
                )
                .child(div().flex_1().min_w(px(0.)).truncate().child(VENUE_ROW))
                .children(status.map(|status| crate::agent::status_dot(status, VENUE_ROW)))
                .agent_node(Role::Row, VENUE_ROW),
        )
}

/// What the venue's row says. One spelling: the row and its status dot are
/// both named by it.
const VENUE_ROW: &str = "Venue setup";

/// The search field: a [`float::field`] that takes keystrokes.
///
/// It edits a `String` on the sidebar's state rather than hosting a real text
/// editor — no caret, no selection, no IME — because the browser needs a
/// filter, and every one of those is a control `luma-ui` would have to own for
/// the whole app rather than one this list invents.
fn search(state: &Tracks, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    let empty = state.query.is_empty();
    let text = if empty { PLACEHOLDER } else { &state.query };
    let focus = state.search_focus.clone();
    let typed = app.clone();
    header_field()
        .id("search")
        .flex_1()
        .min_w(px(0.))
        .gap(px(6.))
        // Keys the field leaves unbound bubble through here — see
        // `Luma::track_search_escape`.
        .on_key_down(move |event, _, cx| {
            if event.keystroke.key == "escape" {
                typed.update(cx, |this, cx| this.track_search_escape(cx));
            }
        })
        .child(
            gpui_component::Icon::new(luma_ui::icons::IconName::Search)
                .size(px(HEADER_ICON))
                .text_color(luma_ui::glass::ink(0.45)),
        )
        .child(div().flex_1().min_w(px(0.)).child(state.search.clone()))
        // The semantic label is the field's VALUE once there is one: a driver
        // asserting on this is asking what it says, not what it would say if
        // empty.
        .agent_node(Role::Input, text.to_string())
        .agent_focused(focus.is_focused(window))
}

/// What the sidebar filter says while it is empty. One spelling: the field is
/// constructed where the venue opens and rendered far below it.
const PLACEHOLDER: &str = "Search tracks";

/// The `mine` / `all` axis: always exactly one pressed. Full width, so the two
/// cells split the row evenly.
fn ownership_filter(state: &Tracks, app: &Entity<Luma>) -> Div {
    float::segmented()
        .w_full()
        .h(px(HEADER_CONTROL))
        .children(Ownership::ALL.into_iter().map(|ownership| {
            let app = app.clone();
            float::segment(
                ownership.label(),
                ownership == state.ownership,
                ownership.label(),
            )
            // A soft fill, without the selection ring a list row carries.
            .shadow(Vec::new())
            .text_size(px(HEADER_TEXT))
            .id(ownership.label())
            .on_click(move |_, _, cx| app.update(cx, |this, cx| this.show_ownership(ownership, cx)))
            .agent_node(Role::Toggle, ownership.label())
        }))
}

/// The scrolling rows. `uniform_list` virtualizes them, so a library of
/// thousands costs one screenful of elements. Everything the closure needs is refcounted, so a redraw
/// copies two pointers rather than the library.
///
/// The viewport's box is probed rather than derived from the chrome above it:
/// the push measures a row's `y` against it, and a constant restating the
/// head's and the filters' heights is a constant that drifts the first time
/// either is retuned.
fn body(
    state: &Tracks,
    statuses: &HashMap<Target, TabStatus>,
    selected: Option<&str>,
    app: &Entity<Luma>,
) -> Div {
    let rows = Rc::clone(&state.rows);
    let entries = Rc::clone(&state.entries);
    let folders = Rc::clone(&state.folders);
    let dots = folders::statuses(state, statuses);
    let renaming = state
        .rename
        .as_ref()
        .map(|rename| (rename.folder_id.clone(), rename.field.clone()));
    let app = app.clone();
    let selected = selected.map(str::to_string);
    // The entry the shared element is carrying, while one is in flight.
    let flying = state.flying().and_then(|_| state.flying_index());
    let no_songs = if state.rows.is_empty() {
        "No tracks imported"
    } else {
        "No matching tracks"
    };
    div()
        .flex_1()
        .min_h(px(0.))
        .relative()
        .overflow_hidden()
        .child(luma_ui::arg::bounds_into(&state.list_box))
        .child(
            uniform_list("tracks", entries.len(), move |range, _, _| {
                range
                    .map(|index| match entries[index] {
                        Entry::Folder {
                            folder,
                            songs,
                            open,
                        } => {
                            let folder_row = &folders[folder];
                            let field = renaming
                                .as_ref()
                                .filter(|(id, _)| *id == folder_row.id)
                                .map(|(_, field)| field);
                            folders::folder_row(folder_row, songs, open, dots[folder], field, &app)
                        }
                        Entry::NewFolder => folders::new_folder_row(&app),
                        Entry::AllSongs { songs } => all_songs(songs),
                        Entry::NoSongs => slot(
                            div()
                                .px(px(float::ROW_INSET))
                                .text_size(px(12.))
                                .text_color(ladder::muted_foreground())
                                .child(no_songs)
                                .agent_node(Role::Text, no_songs),
                        )
                        .into_any_element(),
                        Entry::Song { row, folder } => {
                            let track = &rows[row];
                            track_row(
                                track,
                                index,
                                folder.map(|folder| folders[folder].id.as_str()),
                                selected.as_deref() == Some(track.id.as_str()),
                                flying == Some(index),
                                &app,
                            )
                        }
                    })
                    .collect()
            })
            .track_scroll(&state.list_scroll)
            .size_full(),
        )
}

/// One track as the column draws it wherever it appears: in the list, at the
/// head of its scores, and in flight between the two.
///
/// Shared so the push has something to *be*. Two spellings of this row would
/// make the shared element a lookalike rather than the same object, and the
/// flight would read as a cross-fade between two similar things.
fn track_face(track: &TrackBrowserRow, lit: bool) -> Div {
    let artist = track
        .artist
        .clone()
        .unwrap_or_else(|| "Unknown artist".into());
    let sub = match track.bpm {
        Some(bpm) => format!("{artist} · {bpm:.1}"),
        None => artist,
    };
    div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(float::ROW_INSET))
        .overflow_hidden()
        // The lead column: how much of the track this venue has annotated,
        // and — when there is more than nothing — how many scores say so. The
        // count is the second level's subject stated at sidebar width, so a
        // track two people have scored is legible before it is opened.
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(3.))
                .children(coverage_dot(track))
                .children(score_count(track)),
        )
        .child(album_art(track.album_art_path.as_deref(), ART))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .gap(px(2.))
                // Both lines truncate rather than wrap. The row's height is
                // declared, not measured — `uniform_list` gives every row
                // exactly [`ROW_HEIGHT`] — so a title allowed to take a second
                // line does not make its row taller, it pushes the artist line
                // out of the bottom of it.
                .child(
                    div()
                        .truncate()
                        .text_size(px(12.))
                        // The front tab's row carries full ink; the rest sit
                        // back as a quiet list. This is the only weight
                        // difference — the ring already says which row is in
                        // front, and a second louder signal would be shouting.
                        .text_color(luma_ui::glass::ink(if lit { 0.95 } else { 0.72 }))
                        .child(track_name(track)),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(luma_ui::glass::ink(0.45))
                        .child(sub.clone())
                        .agent_node(Role::Text, sub),
                ),
        )
        // The trailing slot the list row's chevron sits in, reserved on every
        // face so the head and the list row are the *same box* — the shared
        // element then travels in `y` alone. Empty at the head, which is the
        // price of that and cheaper than interpolating a width.
        .child(div().flex_shrink_0().w(px(CHEVRON_SLOT)))
}

/// One list row: the track, the gesture on it, and the selection ring.
///
/// The row is a door to the track's *documents*, not to a timeline: pressing
/// it pushes to the scores level, where choosing which score the editor opens
/// is a deliberate act rather than a guess at which one you meant. The chevron
/// is the hint that the row goes somewhere, and is drawn — not pressed. One
/// row, one target.
fn track_row(
    track: &TrackBrowserRow,
    index: usize,
    folder: Option<&str>,
    picked: bool,
    flying: bool,
    app: &Entity<Luma>,
) -> AnyElement {
    let name = track_name(track);
    let deeper = app.clone();
    let raised = app.clone();
    let track_id = track.id.clone();
    // A song listed in a folder is the same track twice on screen, so its
    // element is keyed by the folder too.
    let key = match folder {
        Some(folder) => format!("{folder}/{}", track.id),
        None => track.id.clone(),
    };
    let row = div()
        .id(SharedString::from(key.clone()))
        .w_full()
        .h(px(ROW_HEIGHT))
        .relative()
        .flex()
        .items_center()
        .on_click(move |_, window, cx| {
            deeper.update(cx, |this, cx| this.push_scores(index, window, cx));
        })
        .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            let menu = folders::Menu::Song {
                at: event.position,
                track_id: track_id.clone(),
            };
            raised.update(cx, |this, cx| this.open_sidebar_menu(menu, cx));
        })
        // Comet's selection recipe, from the one place it is written down:
        // hover and selection share the *fill*, and only the picked row also
        // carries the inset ring. Two fills would make a hovered row and the
        // picked row compete for one reading, and the moment the pointer rests
        // on the picked row they would have to resolve into one anyway.
        .rounded(px(luma_ui::radius::ROW))
        .when(picked, |row| {
            row.bg(luma_ui::glass::card_selected_bg())
                .shadow(luma_ui::glass::card_selected_shadows())
        })
        .when(!picked, |row| {
            row.bg(luma_ui::motion::hover_blend(
                &format!("track-row-{key}"),
                luma_ui::glass::wash(0.),
                luma_ui::glass::glass_hover(),
            ))
            .on_hover(luma_ui::motion::hover_listener(format!("track-row-{key}")))
        })
        // The row the shared element is carrying is drawn by the flight, not
        // here — one track, one row on screen.
        .child(track_face(track, picked).when(flying, |face| face.opacity(0.)))
        // Silkscreen, not a control: the whole row is the door, so a second
        // hit target here would be two ways to say one thing — and a chevron
        // that could be pressed *separately* is a promise that it does
        // something else.
        .child(
            div()
                .absolute()
                .right(px(float::ROW_INSET - 2.))
                .size(px(CHEVRON_SLOT))
                .flex()
                .items_center()
                .justify_center()
                .text_color(luma_ui::glass::ink(if picked { 0.6 } else { 0.35 }))
                .child(
                    gpui_component::Icon::new(luma_ui::icons::IconName::ChevronRight).size(px(11.)),
                ),
        )
        .agent_node(Role::Row, name);
    // Inset by the sidebar's gutter, so the fill lines up with the search
    // field above it; a folder's songs sit one step further in.
    slot(row)
        .when(folder.is_some(), |slot| slot.pl(px(PAD_X + FOLDER_INDENT)))
        .into_any_element()
}

/// How far a folder's songs sit in from the folder.
const FOLDER_INDENT: f32 = 14.;

/// One line of the list: [`ROW_HEIGHT`] tall, inset by the sidebar's gutter.
fn slot(content: impl IntoElement) -> Div {
    div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .px(px(PAD_X))
        .child(content)
}

/// The heading over every song, with how many the filters admit, over a
/// hairline that opens them.
fn all_songs(songs: usize) -> AnyElement {
    slot(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(float::ROW_INSET))
                    .child(div().flex_1().child(float::label(ALL_SONGS)))
                    .child(luma_ui::caption(format!("{songs} songs"))),
            )
            .child(float::divider()),
    )
    .items_end()
    .pb(px(6.))
    .into_any_element()
}

/// What the heading over every song says.
const ALL_SONGS: &str = "All songs";

/// The row's cover thumbnail: a neutral plate, with the art painted over it
/// when the track has any.
///
/// The plate is always there and the image sits *inside* it, which is what
/// makes the two states one rect: a track with no art, a path that no longer
/// resolves, and a decode still in flight all show the same square, and none
/// of them can reflow the row. `img` reads the file through gpui's global
/// image cache, so a row scrolled back into view costs a cache hit rather than
/// a decode, so art needs no preloading.
pub(crate) fn album_art(path: Option<&str>, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        // A hair tighter than the row that holds it — a thumbnail nested in a
        // rounded row wants the smaller corner, or the two radii fight.
        .rounded(px(luma_ui::radius::CHIP))
        .overflow_hidden()
        .bg(luma_ui::glass::wash(luma_ui::glass::WASH_SUBTLE))
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

/// How much of this track the venue's annotations cover, as the one dot drawn
/// before the title: red for none, amber for partial, green
/// from 70% up. A track with no duration has nothing to be a fraction of, so
/// it gets no dot at all.
/// How many scores this venue holds for the track, when it holds any.
///
/// Zero draws nothing rather than a `0`: the row's lead is a status column,
/// and a column of zeros reads as data when it is really the absence of any.
/// This is the only place the count is spelled — the automation label is this
/// element's, so a script and a pair of eyes cannot be told different numbers.
fn score_count(track: &TrackBrowserRow) -> Option<impl IntoElement> {
    let count = track.venue_score_count;
    if count <= 0 {
        return None;
    }
    Some(
        div()
            .flex_shrink_0()
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(luma_ui::glass::ink(0.45))
            .child(format!("{count}"))
            // Track-scoped, because a script finds nodes across the whole
            // tree and a bare count would match every row at once.
            .agent_node(
                Role::Text,
                format!("{} venue scores: {count}", track_name(track)),
            ),
    )
}

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
    Some(
        div()
            .flex_shrink_0()
            .w(px(DOT))
            .h(px(DOT))
            .rounded_full()
            .bg(color),
    )
}
