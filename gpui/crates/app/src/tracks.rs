//! The song list: one venue's library, as a full page over Arrange.
//!
//! The title bar's song title opens it (and ⌘P). A row is a song: its art,
//! title, artist, BPM, length and how many scores this venue holds for it. A
//! press expands it in place to its scores — see [`scores`] — and choosing a
//! score opens it and closes the page. An ownership filter and a search work
//! over the venue's rows; the venue's folders lead the list.
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
//! At the head of the list the page shows the venue's folders, each opening
//! onto the songs it holds, and then every song under "All songs". A song in a
//! folder is the same song as under All songs: it opens the same scores. See
//! [`folders`].
//!
//! Album art comes from `album_art_path` — a path on disk, never inlined bytes.
//! The native host reads the file with `img(path)` and GPUI's image cache
//! handles the decode and the lazy load.

use luma_ui::motion::FadeHover;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float;
use luma_ui::glass;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};

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

/// One line of the list. The list measures each row as it draws it, so a
/// row's height is written only on the row.
#[derive(Clone, Copy, PartialEq, Eq)]
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
    /// The song at `row`'s scores, expanded under it.
    Scores { row: usize, folder: Option<usize> },
}

/// The page's whole state: the venue it is showing, everything the seam
/// returned for it, the filters over that, and the songs expanded to their
/// scores.
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
    /// The songs expanded to their scores, by track id — see [`scores`].
    pub(crate) scores: HashMap<String, scores::Scores>,
    /// What the list draws, top to bottom.
    entries: Rc<[Entry]>,
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
    search_focus: FocusHandle,
    /// The row list's measured heights and scroll position, one item per
    /// entry.
    list: ListState,
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

    /// The song at `row`, and the expansion under it when it was expanded
    /// from this place in the list — a song listed in a folder and under All
    /// songs is expanded where it was pressed, not twice.
    fn song(&self, row: usize, folder: Option<usize>) -> [Option<Entry>; 2] {
        let song = Entry::Song { row, folder };
        let place = folder.map(|folder| self.folders[folder].id.as_str());
        let expanded = self
            .scores
            .get(&self.rows[row].id)
            .is_some_and(|scores| scores.folder.as_deref() == place);
        [
            Some(song),
            expanded.then_some(Entry::Scores { row, folder }),
        ]
    }

    /// Rebuild [`Self::entries`]: each folder, followed by its songs when it
    /// is open; the way to make another; then every song. A folder lists the
    /// songs the filters admit, in the same order as All songs. An expanded
    /// song carries its scores under it.
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
                for row in songs {
                    entries.extend(self.song(row, Some(index)).into_iter().flatten());
                }
            }
        }
        entries.push(Entry::NewFolder);
        entries.push(Entry::AllSongs { songs: shown.len() });
        if shown.is_empty() {
            entries.push(Entry::NoSongs);
        }
        for row in shown {
            entries.extend(self.song(row, None).into_iter().flatten());
        }
        self.set_entries(entries);
    }

    /// Adopt `entries`, telling the list only which run of rows changed: the
    /// rows around it keep their measured heights, and a folder opening keeps
    /// the scroll where it was.
    fn set_entries(&mut self, entries: Vec<Entry>) {
        let old = &self.entries;
        let same = |(a, b): &(&Entry, &Entry)| a == b;
        let head = old.iter().zip(&entries).take_while(same).count();
        let tail = old[head..]
            .iter()
            .rev()
            .zip(entries[head..].iter().rev())
            .take_while(same)
            .count();
        self.list
            .splice(head..old.len() - tail, entries.len() - head - tail);
        self.entries = entries.into();
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
        self.songs = Some(Tracks {
            venue_id: venue.id,
            venue_name: venue.name,
            load_generation: generation,
            rows: Rc::from(Vec::new()),
            folders: Rc::from(Vec::new()),
            expanded: HashSet::new(),
            scores: HashMap::new(),
            entries: Rc::from(Vec::new()),
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
            search_focus,
            list: ListState::new(0, ListAlignment::Top, px(0.)),
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
                        Err(error) => state.error = Some(error.to_string()),
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
}

// -- rendering ----------------------------------------------------------------

/// A song row: art, two columns of text, the numbers.
const ROW_HEIGHT: f32 = 52.;
/// The page's inset from its edges, and the gap between a row's columns.
const PAD_X: f32 = 16.;
const GAP: f32 = 12.;
/// The page head's height: the title, the search, the filter and Add songs.
const HEAD_HEIGHT: f32 = 52.;
/// The column heads' row.
const COLUMNS_HEIGHT: f32 = 28.;
/// The coverage dot's box.
const DOT: f32 = 6.;
/// The disclosure column at a row's start, and the art beside it.
const DISCLOSURE: f32 = 16.;
const ART: f32 = 36.;
/// The numeric columns' widths: BPM, length, scores.
const NUMBER: f32 = 60.;
const SCORES: f32 = 96.;
/// The artist column.
const ARTIST: f32 = 220.;
/// The search field's width in the head.
const SEARCH_WIDTH: f32 = 260.;

/// The song list page: the head, the column heads, and the list.
///
/// `statuses` are the open bodies' status dots, drawn on the score rows that
/// open them and on closed folders.
pub(crate) fn page(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, ChatStatus>,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .text_color(glass::ink(0.95))
        .child(header(state, app, window))
        .child(columns())
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
            None => body(shell, state, statuses, app).into_any_element(),
        })
        // Siblings of the list, not children of a row: the list clips, and a
        // menu inside it would be cut off at its edge.
        .children(folders::menu(state, app))
        .children(scores::menus(state, app))
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

/// The heads over the list's columns.
fn columns() -> Div {
    let head = |text: &'static str| {
        div()
            .text_size(px(11.))
            .text_color(glass::ink(glass::TEXT_TERTIARY))
            .child(text)
    };
    div()
        .h(px(COLUMNS_HEIGHT))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .border_b_1()
        .border_color(glass::inner_rule())
        .child(div().w(px(DISCLOSURE + DOT + 4.)).flex_none())
        .child(div().w(px(ART)).flex_none())
        .child(head("Title").flex_1().min_w_0())
        .child(head("Artist").w(px(ARTIST)).flex_none())
        .child(head("BPM").w(px(NUMBER)).flex_none())
        .child(head("Length").w(px(NUMBER)).flex_none())
        .child(head("Scores").w(px(SCORES)).flex_none())
}

/// The header's one text size, the same as the rows'.
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
        // Keys the field leaves unbound bubble through here — see
        // `Luma::track_search_escape`.
        .on_key_down(move |event, _, cx| {
            if event.keystroke.key == "escape" {
                typed.update(cx, |this, cx| this.track_search_escape(cx));
            }
        })
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

/// The scrolling rows. `list` virtualizes them, so a library of thousands
/// costs one screenful of elements, and measures each as it draws it, so a
/// song, a folder and an expanded song's scores can be different heights.
fn body(
    shell: &Luma,
    state: &Tracks,
    statuses: &HashMap<Target, ChatStatus>,
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
    // Each expanded song's scores, drawn ahead of the list so its closure
    // copies elements' inputs rather than borrowing the shell.
    let expansions: HashMap<String, Rc<dyn Fn() -> AnyElement>> = state
        .scores
        .keys()
        .map(|track| {
            (
                track.clone(),
                scores::expansion(shell, state, track, statuses, app),
            )
        })
        .collect();
    let app = app.clone();
    let open = shell.front_track().map(str::to_string);
    let no_songs = if state.rows.is_empty() {
        "No tracks imported"
    } else {
        "No matching tracks"
    };
    let expanded: HashSet<String> = state.scores.keys().cloned().collect();
    div().flex_1().min_h(px(0.)).child(
        list(state.list.clone(), move |index, _, _| {
            match entries[index] {
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
                        .h(px(float::NAV_ROW_HEIGHT))
                        .flex()
                        .items_center()
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
                        folder.map(|folder| folders[folder].id.as_str()),
                        open.as_deref() == Some(track.id.as_str()),
                        expanded.contains(&track.id),
                        &app,
                    )
                }
                Entry::Scores { row, .. } => expansions
                    .get(&rows[row].id)
                    .map_or_else(|| div().into_any_element(), |draw| draw()),
            }
        })
        .size_full(),
    )
}

/// One song: the disclosure and the coverage dot, the art, title, artist,
/// BPM, length and its scores in this venue. A press expands it to its
/// scores, or folds them away; a right-click is the folder menu.
fn track_row(
    track: &TrackBrowserRow,
    folder: Option<&str>,
    open: bool,
    expanded: bool,
    app: &Entity<Luma>,
) -> AnyElement {
    let name = track_name(track);
    let pressed = app.clone();
    let raised = app.clone();
    let track_id = track.id.clone();
    let menu_track = track.id.clone();
    let place = folder.map(str::to_string);
    // A song listed in a folder is the same track twice on screen, so its
    // element is keyed by the folder too.
    let key = match folder {
        Some(folder) => format!("{folder}/{}", track.id),
        None => track.id.clone(),
    };
    let text = |value: String, alpha: f32| {
        div()
            .truncate()
            .text_size(px(12.))
            .text_color(glass::ink(alpha))
            .child(value)
    };
    let number = |value: String| {
        div()
            .w(px(NUMBER))
            .flex_none()
            .text_size(px(11.))
            .font_family(luma_ui::fonts::MONO)
            .text_color(glass::ink(glass::TEXT_SECONDARY))
            .child(value)
    };
    let artist = track
        .artist
        .clone()
        .unwrap_or_else(|| "Unknown artist".into());
    let bpm = track
        .bpm
        .map_or_else(String::new, |bpm| format!("{bpm:.1}"));
    let length = track
        .duration_seconds
        .map_or_else(String::new, |seconds| length(seconds));
    div()
        .id(SharedString::from(key))
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .when(folder.is_some(), |row| row.pl(px(PAD_X + FOLDER_INDENT)))
        .border_b_1()
        .border_color(glass::inner_rule())
        .when(open, |row| row.bg(glass::wash(glass::WASH_PRESSED)))
        .when(!open, |row| {
            row.bg(glass::wash(0.))
                .hover(|style| style.bg(glass::wash(glass::WASH_SUBTLE)))
                .fade_hover()
        })
        .on_click(move |_, _, cx| {
            let (track_id, place) = (track_id.clone(), place.clone());
            pressed.update(cx, |this, cx| {
                this.toggle_track_scores(&track_id, place, cx)
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
        .child(
            div()
                .w(px(DISCLOSURE + DOT + 4.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .w(px(DISCLOSURE))
                        .flex()
                        .justify_center()
                        .text_color(glass::ink(glass::TEXT_TERTIARY))
                        .child(
                            gpui_component::Icon::new(if expanded {
                                luma_ui::icons::IconName::ChevronDown
                            } else {
                                luma_ui::icons::IconName::ChevronRight
                            })
                            .size(px(11.)),
                        ),
                )
                .children(coverage_dot(track)),
        )
        .child(album_art(track.album_art_path.as_deref(), ART))
        .child(text(name.clone(), 0.95).flex_1().min_w_0())
        .child(
            text(artist.clone(), glass::TEXT_SECONDARY)
                .w(px(ARTIST))
                .flex_none()
                .agent_node(Role::Text, sub_line(&artist, track.bpm)),
        )
        .child(number(bpm))
        .child(number(length))
        .child(div().w(px(SCORES)).flex_none().child(score_count(track)))
        .agent_node(Role::Row, name)
        .into_any_element()
}

/// What a row said under its title before the list had columns — `artist ·
/// bpm` — kept as the artist cell's name so a script reads one line.
fn sub_line(artist: &str, bpm: Option<f64>) -> String {
    match bpm {
        Some(bpm) => format!("{artist} · {bpm:.1}"),
        None => artist.to_string(),
    }
}

/// A length as the list prints it: `4:12`.
fn length(seconds: f64) -> String {
    let total = seconds.max(0.).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// How far a folder's songs sit in from the folder.
const FOLDER_INDENT: f32 = 14.;

/// One line of the list, inset by the page's gutter. As tall as what it
/// holds.
fn slot(content: impl IntoElement) -> Div {
    div().w_full().flex().px(px(PAD_X)).child(content)
}

/// The heading over every song, with how many the filters admit.
fn all_songs(songs: usize) -> AnyElement {
    slot(
        div()
            .w_full()
            .flex()
            .items_center()
            .px(px(float::ROW_INSET))
            .child(div().flex_1().child(float::label(ALL_SONGS)))
            .child(luma_ui::caption(format!("{songs} songs"))),
    )
    .py(px(8.))
    .border_b_1()
    .border_color(glass::inner_rule())
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
        1 => ("1 score".to_string(), glass::TEXT_SECONDARY),
        n => (format!("{n} scores"), glass::TEXT_SECONDARY),
    };
    div()
        .text_size(px(12.))
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
    Some(
        div()
            .flex_shrink_0()
            .w(px(DOT))
            .h(px(DOT))
            .rounded_full()
            .bg(color),
    )
}
