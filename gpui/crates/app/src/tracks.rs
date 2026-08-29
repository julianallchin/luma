//! The track browser: one venue's library, as the shell's sidebar.
//!
//! The web's table (`src/features/tracks/components/track-browser.tsx`) became
//! a row list at sidebar width when the shell landed — comet's session-row
//! anatomy: a status lead, the title, `artist · bpm` muted under it. The three
//! filters and the search are the web browser's, unchanged, over one query's
//! worth of rows. The added-by and preprocessing columns died with the table;
//! they return with a wider tracks surface if one is ever wanted.
//!
//! # Filtering is the view's job, not the query's
//!
//! `list_tracks_enriched(venue_id)` returns the *whole visible library* and
//! decorates each row with that venue's clip count; the venue id scopes the
//! decoration, not the result set. "In venue" is the durable score-existence
//! signal returned as `is_in_venue`; clip counts remain presentation metadata
//! and never decide membership. This keeps a newly added track visible before
//! its first annotation is authored.
//!
//! Album art comes from `album_art_path` — a path on disk, never inlined bytes
//! (see CLAUDE.md on why bulk responses carry paths). The web side has to route
//! that path through Tauri's asset protocol to get it past the webview; a
//! native host just reads the file, so `img(path)` is the whole story and
//! GPUI's image cache handles the decode and the lazy load.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, RowState};
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{ladder, motion};

use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;

use crate::Luma;

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
/// the default the web browser opens with.
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
    /// as its own — the same `!t.uid || t.uid === currentUserId` the web side
    /// filters on.
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

/// The screen's whole state: the venue it is showing, everything the seam
/// returned for it, and the three filters over that.
///
/// [`Self::shown`] is indices into [`Self::rows`] rather than a second copy of
/// the rows, recomputed on every state change instead of on every draw: a draw
/// happens per frame and per hover, and re-filtering a full library there is
/// the difference between a scroll that keeps up and one that does not.
pub struct Tracks {
    /// The venue the rows were decorated for. The editor needs it too — a
    /// track's score is per-venue — so it is kept beside the name it is shown
    /// under rather than re-derived from the screen that opened this one.
    venue_id: String,
    venue_name: String,
    load_generation: u64,
    rows: Rc<[TrackBrowserRow]>,
    shown: Rc<[usize]>,
    /// Whether the venue's query has come back. Written in the same
    /// assignment as [`Self::rows`] and [`Self::error`], so "still loading"
    /// and "nothing to show" can never be confused for one another.
    loaded: bool,
    error: Option<String>,
    ownership: Ownership,
    in_venue: bool,
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

    /// Adopt a freshly venue-decorated library read after an add-track action.
    pub(crate) fn replace_rows(&mut self, rows: Vec<TrackBrowserRow>) {
        self.rows = rows.into();
        self.loaded = true;
        self.error = None;
        self.refilter();
    }

    /// The rows the filters admit, in the order the query returned them.
    fn filter(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.ownership.admits(row, self.user.as_deref())
                    && (!self.in_venue || row.is_in_venue)
                    && matches(row, &query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn refilter(&mut self) {
        self.shown = self.filter().into();
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
    /// pushing region. `index` is a position in [`Self::shown`], which is what
    /// the list draws.
    fn row_top(&self, index: usize) -> f32 {
        let (Some(region), Some(list)) = (self.region.get(), self.list_box.get()) else {
            return 0.;
        };
        let offset = f32::from(self.list_scroll.0.borrow().base_handle.offset().y);
        f32::from(list.origin.y - region.origin.y) + index as f32 * ROW_HEIGHT + offset
    }

    /// The scores level's track, as an index into the rows currently shown —
    /// the position a pop with no entrance to reverse would fly back to.
    fn flying_index(&self) -> Option<usize> {
        let Level::Scores(level) = &self.level else {
            return None;
        };
        self.shown
            .iter()
            .position(|row| self.rows[*row].id == level.track.id)
    }
}

/// `title`, `artist` or `album` contains `query`, which is already lowercased
/// and trimmed. An empty query matches everything.
fn matches(track: &TrackBrowserRow, query: &str) -> bool {
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
    /// closes, and any *rig* tabs of another venue close with it — a
    /// visualizer is a view of a venue, and a view of a room you left is a
    /// window into nowhere. Track and graph tabs stay: they are about their
    /// own subjects, and closing somebody's open timeline because they glanced
    /// at another room would be the shell throwing work away.
    pub(crate) fn open_venue(&mut self, venue: Venue, cx: &mut Context<Self>) {
        // A remembered row belongs to the browser that named it. Keeping it
        // across a venue switch would make the new-tab menu advertise an
        // editor subject the current browser cannot honestly open.
        self.selected_track = None;
        self.venue_selection_generation = self.venue_selection_generation.wrapping_add(1);
        let generation = self.venue_selection_generation;
        let venue_id = venue.id.clone();
        // Across every parked set, not just the one on screen: a patch tab for
        // the room being left is just as stale sitting in another track's
        // remembered strip as it is in this one. The track and graph tabs in
        // those sets stay — they are about their own subjects, and the room
        // they were opened beside is not one of them.
        let leaving: Vec<crate::shell::Body> =
            self.parked.close_where(&mut self.workspace, |target| {
                target.venue().is_some_and(|owner| owner != venue.id)
            });
        for body in leaving {
            self.teardown(body, cx);
        }
        self.close_overlay(cx);
        let pending = self.library.tracks(&venue.id);
        let remember = self
            .library
            .set_session_item(crate::welcome::LAST_VENUE, &venue.id);
        let search = cx.new(|cx| luma_ui::text_input::TextInput::search(PLACEHOLDER, cx));
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
            shown: Rc::from(Vec::new()),
            loaded: false,
            error: None,
            ownership: Ownership::Mine,
            in_venue: true,
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
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |_, _| {
            let _ = remember.await;
        })
        .detach();
    }

    /// Enter the scores level for the row at `index` of what the list is
    /// showing. The index, not the id, because the flight starts at the row's
    /// *place* — and two rows of the same track cannot be on screen at once.
    fn push_scores(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let Some(track) = browser
            .shown
            .get(index)
            .map(|row| browser.rows[*row].id.clone())
        else {
            return;
        };
        let row_top = browser.row_top(index);
        // Going into a track's scores *is* picking that track: the strip and
        // the stage are scoped to the pick, and a column reading one track
        // while they read another was the disagreement this avoids. Setting
        // the field is the whole gesture — the scope is synced at draw
        // ([`crate::Luma::sync_workspace_scope`]).
        self.selected_track = Some(track.clone());
        self.show_scores(&track, row_top, window, cx);
    }

    /// `→` in the sidebar: into the picked track's scores.
    ///
    /// The picked track rather than a focused row, because the list is
    /// virtualized and its rows carry no focus handles — the pick is the one
    /// notion of "the row this column is currently about" that survives a
    /// scroll.
    pub(crate) fn enter_selected_scores(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        if matches!(browser.level, Level::Scores(_)) {
            return;
        }
        let Some(picked) = self.selected_track.as_deref() else {
            return;
        };
        let Some(index) = browser
            .shown
            .iter()
            .position(|row| browser.rows[*row].id == picked)
        else {
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

    fn toggle_in_venue(&mut self, cx: &mut Context<Self>) {
        self.with_tracks(cx, |state| {
            state.in_venue = !state.in_venue;
            state.refilter();
        });
    }

    /// Mirror an edit of the sidebar filter. Filtering is immediate — the web
    /// side debounces because every keystroke there re-renders a React tree
    /// over the same already-loaded rows; here the work is one pass over a
    /// `Vec` and a `uniform_list` that redraws a screenful either way.
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
        if let Some(surviving) = surviving {
            self.prune_tab_scopes(venue_id, &surviving, cx);
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// Comet's session-row anatomy at sidebar width: two text lines plus the lead.
const ROW_HEIGHT: f32 = 44.;
const GAP: f32 = 8.;
const PAD_X: f32 = 12.;
/// The coverage dot's box, matching the web side's `w-1.5 h-1.5`.
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
/// Takes the whole shell rather than the browser alone, because the column's
/// two ends are about different things: the levels are the venue's, the foot
/// is the person's — and the foot is *outside* the pushing region for exactly
/// that reason. The picked track comes from there too — it is not the same as
/// the open editor tab: the pick is what the strip and the stage are scoped
/// to, and it survives closing that track's tabs.
///
/// # The push
///
/// Both levels are laid out at the column's full width and offset along one
/// axis, so neither reflows while it travels. The track row the gesture named
/// is drawn *once*, over both, flying from its place in the list to the head
/// of the arriving level — which is why the two share a horizontal inset and
/// a row height: the shared element then travels in `y` alone, and there is no
/// box interpolation to get wrong.
pub fn sidebar(shell: &Luma, state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
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
                    sliding(-travel * t, 1. - t).child(tracks_level(shell, state, app, window))
                }))
                .children(match &state.level {
                    Level::Scores(level) => Some(sliding(travel * (1. - t), t).child(
                        scores::level(shell, state, level, app, window, state.push.is_some()),
                    )),
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
            .left_0()
            .right_0()
            .top(px(motion::lerp(push.row_top, scores::BACK_ROW_HEIGHT, t)))
            .child(track_face(track, true)),
    )
}

/// The track list itself: the venue's name (the way back to the picker), the
/// search, the filters, and the rows.
fn tracks_level(shell: &Luma, state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    let flying = state.flying().map(|track| track.id.as_str());
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(head(state, app, window))
        .child(filters(state, app, window))
        .child(match &state.error {
            Some(message) => luma_ui::plate(
                format!("Failed to load tracks: {message}"),
                ladder::danger(),
            ),
            None if !state.loaded => {
                luma_ui::plate("Loading tracks…".to_string(), ladder::muted_foreground())
            }
            None if state.shown.is_empty() => luma_ui::plate(
                if state.query.is_empty() {
                    "No tracks imported".to_string()
                } else {
                    "No matching tracks".to_string()
                },
                ladder::muted_foreground(),
            ),
            None => body(state, shell.selected_track.as_deref(), flying, app).into_any_element(),
        })
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
        .px(px(PAD_X - float::ROW_INSET))
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
                    gpui_component::Icon::new(gpui_component::IconName::ChevronUp)
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
    Some(float::anchored_above(
        "account-menu",
        float::NAV_ROW_HEIGHT,
        float::Dismiss::on_press_out(move |_, cx| {
            dismiss.update(cx, |this, cx| this.close_account_menu(cx));
        }),
        card.into_any_element(),
    ))
}

/// Wide enough for the longer of the two rows plus its key cap, and no wider:
/// a menu of two words that spanned the sidebar would read as a panel.
const ACCOUNT_MENU_WIDTH: f32 = 200.0;

/// The venue at the head of the sidebar. Pressing it reopens the venue picker
/// — the picker overlay is the one venue-choosing mechanism, so the head is a
/// door to it rather than a second selector that could disagree with it.
///
/// Glass language, hand-set: the sidebar is chrome (spec §9), and a ladder
/// slab up here would be the instrument tier leaking into the frame.
fn head(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    let picker = app.clone();
    let venue_focus = state.venue_focus.clone();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .py(px(8.))
        .child(
            div()
                .id("venue")
                .track_focus(&state.venue_focus)
                .tab_stop(true)
                .h(px(24.))
                .px(px(8.))
                .rounded(px(luma_ui::radius::CONTROL))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(luma_ui::glass::ink(0.85))
                .hover(|button| button.bg(luma_ui::glass::wash(0.06)))
                .on_click(move |_, window, cx| {
                    window.focus(&venue_focus, cx);
                    picker.update(cx, |this, cx| this.show_venues(cx));
                })
                .child(state.venue_name.clone())
                .child(
                    gpui_component::Icon::new(gpui_component::IconName::ChevronDown)
                        .size(px(11.))
                        .text_color(luma_ui::glass::ink(0.45)),
                )
                .agent_node(Role::Button, state.venue_name.clone())
                .agent_focused(state.venue_focus.is_focused(window)),
        )
        .child(div().flex_1())
        .child({
            let add = app.clone();
            div()
                .id("add-track")
                .size(px(24.0))
                .rounded(px(luma_ui::radius::CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .hover(|button| button.bg(luma_ui::glass::wash(0.06)))
                .on_click(move |_, _, cx| add.update(cx, |this, cx| this.show_add_tracks(cx)))
                .child(gpui_component::Icon::new(gpui_component::IconName::Plus).size(px(12.0)))
                .agent_node(Role::Button, "Add track")
        })
        // How many rows the filters admit — the web browser's footer, kept in
        // the head because the sidebar has no second bar to spare.
        .child({
            let count = format!("{} TRACKS", state.shown.len());
            div()
                .text_size(px(10.))
                .text_color(luma_ui::glass::ink(0.35))
                .child(count.clone())
                .agent_node(Role::Text, count)
        })
}

/// The search over the filters, stacked — at sidebar width they do not share
/// a row.
fn filters(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .flex_col()
        .gap(px(6.))
        .px(px(PAD_X))
        .py(px(8.))
        .child(search(state, app, window))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(GAP))
                .child(ownership_filter(state, app))
                .child(in_venue_filter(state, app)),
        )
}

/// The search field: a `luma_input` that takes keystrokes.
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
    div()
        .id("search")
        .w(px(crate::shell::SIDEBAR_WIDTH - 2. * PAD_X))
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(luma_ui::radius::CONTROL))
        .bg(luma_ui::glass::wash(0.04))
        .border_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .text_size(px(12.))
        // Keys the field leaves unbound bubble through here — see
        // `Luma::track_search_escape`.
        .on_key_down(move |event, _, cx| {
            if event.keystroke.key == "escape" {
                typed.update(cx, |this, cx| this.track_search_escape(cx));
            }
        })
        .child(state.search.clone())
        // The semantic label is the field's VALUE once there is one: a driver
        // asserting on this is asking what it says, not what it would say if
        // empty.
        .agent_node(Role::Input, text.to_string())
        .agent_focused(focus.is_focused(window))
}

/// What the sidebar filter says while it is empty. One spelling: the field is
/// constructed where the venue opens and rendered far below it.
const PLACEHOLDER: &str = "Search tracks…";

/// One glass filter pill: quiet ink that washes when pressed. The sidebar's
/// own control, not `luma_toggle` — the ladder's slabs belong inside the
/// content cards, and this is the frame.
fn filter_pill(id: &'static str, label: &'static str, on: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h(px(20.))
        .px(px(8.))
        .rounded(px(luma_ui::radius::CONTROL))
        .flex()
        .items_center()
        .text_size(px(11.))
        .when(on, |pill| {
            pill.bg(luma_ui::glass::wash(0.10))
                .text_color(luma_ui::glass::ink(0.90))
        })
        .when(!on, |pill| {
            pill.text_color(luma_ui::glass::ink(0.45))
                .hover(|pill| pill.bg(luma_ui::glass::wash(0.06)))
        })
        .child(SharedString::from(label))
}

/// The `mine` / `all` axis: always exactly one pressed.
fn ownership_filter(state: &Tracks, app: &Entity<Luma>) -> Div {
    div()
        .flex()
        .gap(px(2.))
        .children(Ownership::ALL.into_iter().map(|ownership| {
            let app = app.clone();
            filter_pill(
                ownership.label(),
                ownership.label(),
                ownership == state.ownership,
            )
            .on_click(move |_, _, cx| app.update(cx, |this, cx| this.show_ownership(ownership, cx)))
            .agent_node(Role::Toggle, ownership.label())
        }))
}

/// The other axis, and independent of ownership: whether to show only tracks
/// this venue has annotations on.
fn in_venue_filter(state: &Tracks, app: &Entity<Luma>) -> Div {
    let app = app.clone();
    div().child(
        filter_pill("in-venue", "In Venue", state.in_venue)
            .on_click(move |_, _, cx| app.update(cx, |this, cx| this.toggle_in_venue(cx)))
            .agent_node(Role::Toggle, "In Venue"),
    )
}

/// The scrolling rows. `uniform_list` virtualizes them, so a library of
/// thousands costs one screenful of elements — the same reason the web side
/// runs a virtualizer. Everything the closure needs is refcounted, so a redraw
/// copies two pointers rather than the library.
///
/// The viewport's box is probed rather than derived from the chrome above it:
/// the push measures a row's `y` against it, and a constant restating the
/// head's and the filters' heights is a constant that drifts the first time
/// either is retuned.
fn body(state: &Tracks, selected: Option<&str>, flying: Option<&str>, app: &Entity<Luma>) -> Div {
    let rows = Rc::clone(&state.rows);
    let shown = Rc::clone(&state.shown);
    let app = app.clone();
    let selected = selected.map(str::to_string);
    let flying = flying.map(str::to_string);
    div()
        .flex_1()
        .min_h(px(0.))
        .relative()
        .overflow_hidden()
        .child(luma_ui::arg::bounds_into(&state.list_box))
        .child(
            uniform_list("tracks", shown.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let row = &rows[shown[index]];
                        let picked = selected.as_deref() == Some(row.id.as_str());
                        let flew = flying.as_deref() == Some(row.id.as_str());
                        track_row(row, index, picked, flew, &app)
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
        .px(px(PAD_X))
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
                        // The picked row is the one the whole workspace is
                        // scoped to, so it carries full ink; the rest sit back
                        // as a quiet list. This is the only weight difference —
                        // the ring already says which row is picked, and a
                        // second louder signal would just be shouting.
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
    picked: bool,
    flying: bool,
    app: &Entity<Luma>,
) -> AnyElement {
    let name = track_name(track);
    let deeper = app.clone();
    div()
        .id(SharedString::from(track.id.clone()))
        .w_full()
        .h(px(ROW_HEIGHT))
        .relative()
        .flex()
        .items_center()
        .on_click(move |_, window, cx| {
            deeper.update(cx, |this, cx| this.push_scores(index, window, cx));
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
            row.hover(|row| row.bg(luma_ui::glass::glass_hover()))
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
                .right(px(PAD_X - 2.))
                .size(px(CHEVRON_SLOT))
                .flex()
                .items_center()
                .justify_center()
                .text_color(luma_ui::glass::ink(if picked { 0.6 } else { 0.35 }))
                .child(
                    gpui_component::Icon::new(gpui_component::IconName::ChevronRight).size(px(11.)),
                ),
        )
        .agent_node(Role::Row, name)
        .into_any_element()
}

/// The row's cover thumbnail: a neutral plate, with the art painted over it
/// when the track has any.
///
/// The plate is always there and the image sits *inside* it, which is what
/// makes the two states one rect: a track with no art, a path that no longer
/// resolves, and a decode still in flight all show the same square, and none
/// of them can reflow the row. `img` reads the file through gpui's global
/// image cache, so a row scrolled back into view costs a cache hit rather than
/// a decode — the reason the web side has to preload art by hand
/// (`track-browser.tsx`) and this one does not.
pub(crate) fn album_art(path: Option<&str>, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        // A hair tighter than the row that holds it — a thumbnail nested in a
        // rounded row wants the smaller corner, or the two radii fight.
        .rounded(px(luma_ui::radius::CHIP))
        .overflow_hidden()
        .bg(luma_ui::glass::wash(0.06))
        .children(
            path.filter(|path| !path.is_empty())
                .map(|path| img(PathBuf::from(path)).size(px(size))),
        )
}

/// The web side falls back through title → filename; `file_path`'s basename is
/// the same last resort.
fn track_name(track: &TrackBrowserRow) -> String {
    if let Some(title) = track.title.as_ref().filter(|t| !t.is_empty()) {
        return title.clone();
    }
    std::path::Path::new(&track.file_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| track.file_path.clone())
}

/// How much of this track the venue's annotations cover, as the one dot the
/// web browser draws before the title: red for none, amber for partial, green
/// from 70% up. A track with no duration has nothing to be a fraction of, so
/// it gets no dot at all.
///
/// The web side has a fourth, blue state for "auto-lit, needs review". That
/// flag lives only in a frontend store, never in the library, so this host
/// cannot know it — and a dot that silently said "uncovered" for it would be
/// worse than one state fewer.
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
            .text_size(px(9.))
            .font_weight(FontWeight::BOLD)
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
