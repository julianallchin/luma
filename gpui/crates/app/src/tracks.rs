//! The track browser: one venue's library as a filtered, striped table.
//!
//! Mirrors `src/features/tracks/components/track-browser.tsx` — the same
//! column order and widths, the same 10px uppercase silkscreen headers, the
//! same `bg-card` / `bg-stripe` alternation with a `--hover` lift, and the same
//! three filters over one query's worth of rows.
//!
//! # Filtering is the view's job, not the query's
//!
//! `list_tracks_enriched(venue_id)` returns the *whole visible library* and
//! decorates each row with that venue's clip count; the venue id scopes the
//! decoration, not the result set. So "in venue" means
//! `venue_annotation_count > 0`, exactly as the web browser computes it, and a
//! host that skipped that filter would show the entire library on a venue
//! screen. (That count is clips, not scores: a track added to a setlist with an
//! empty score reads as *not* in the venue until something is annotated on it.
//! The web app has the same gap — its import flow creates the empty score this
//! filter then hides.)
//!
//! Album art comes from `album_art_path` — a path on disk, never inlined bytes
//! (see CLAUDE.md on why bulk responses carry paths). The web side has to route
//! that path through Tauri's asset protocol to get it past the webview; a
//! native host just reads the file, so `img(path)` is the whole story and
//! GPUI's image cache handles the decode and the lazy load.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};

use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;

use crate::{Luma, Screen};

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
    rows: Rc<[TrackBrowserRow]>,
    shown: Rc<[usize]>,
    /// Whether the venue's query has come back. Written in the same
    /// assignment as [`Self::rows`] and [`Self::error`], so "still loading"
    /// and "nothing to show" can never be confused for one another.
    loaded: bool,
    error: Option<String>,
    ownership: Ownership,
    in_venue: bool,
    query: String,
    /// The signed-in principal, snapshotted when the screen opened. The
    /// ownership filter and the added-by column both read it.
    user: Option<String>,
    /// `uid -> display name` for other people's tracks, filled by one lookup
    /// after the rows land. Absent uids read as "shared", as on the web side.
    names: Rc<HashMap<String, String>>,
    search_focus: FocusHandle,
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

    /// The loaded row a click carries. Looked up by id rather than by index
    /// because the click was registered against a *filtered* list, and the
    /// filters can change before it lands.
    pub(crate) fn find(&self, track_id: &str) -> Option<TrackBrowserRow> {
        self.rows.iter().find(|row| row.id == track_id).cloned()
    }

    /// The rows the filters admit, in the order the query returned them.
    fn filter(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.ownership.admits(row, self.user.as_deref())
                    && (!self.in_venue || row.venue_annotation_count > 0)
                    && matches(row, &query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn refilter(&mut self) {
        self.shown = self.filter().into();
    }

    /// Every uid on screen that is somebody else's and has no name yet.
    fn unnamed_uids(&self) -> Vec<String> {
        let mut uids: Vec<String> = self
            .rows
            .iter()
            .filter_map(|row| row.uid.as_deref())
            .filter(|uid| Some(*uid) != self.user.as_deref() && !self.names.contains_key(*uid))
            .map(str::to_string)
            .collect();
        uids.sort_unstable();
        uids.dedup();
        uids
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
    /// Navigate to a venue's tracks. The table renders immediately, empty, and
    /// fills in when the query lands — the venue is already known, so there is
    /// nothing to wait for before drawing the screen.
    pub(crate) fn open_venue(&mut self, venue: Venue, cx: &mut Context<Self>) {
        let pending = self.library.tracks(&venue.id);
        self.screen = Screen::Tracks(Tracks {
            venue_id: venue.id,
            venue_name: venue.name,
            rows: Rc::from(Vec::new()),
            shown: Rc::from(Vec::new()),
            loaded: false,
            error: None,
            ownership: Ownership::Mine,
            in_venue: true,
            query: String::new(),
            user: self.library.user_id().map(str::to_string),
            names: Rc::new(HashMap::new()),
            search_focus: cx.focus_handle(),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.with_tracks(cx, |state| {
                    state.loaded = true;
                    match result {
                        Ok(rows) => {
                            state.rows = rows.into();
                            state.refilter();
                        }
                        Err(message) => state.error = Some(message),
                    }
                });
                this.load_display_names(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Name the other people whose tracks are on screen. Skipped entirely when
    /// every row is this host's own, which is the usual case and the only one
    /// an offline host can answer.
    fn load_display_names(&mut self, cx: &mut Context<Self>) {
        let Screen::Tracks(state) = &self.screen else {
            return;
        };
        let uids = state.unnamed_uids();
        if uids.is_empty() {
            return;
        }
        let pending = self.library.display_names(uids);
        cx.spawn(async move |this, cx| {
            // A directory lookup is a network call; failing it leaves the
            // column reading "shared", which is the same thing it says for a
            // uid the directory does not know.
            if let Ok(names) = pending.await {
                this.update(cx, |this, cx| {
                    this.with_tracks(cx, |state| {
                        let mut merged = (*state.names).clone();
                        merged.extend(names);
                        state.names = Rc::new(merged);
                    })
                })
                .ok();
            }
        })
        .detach();
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

    /// One keystroke into the search field. Filtering is immediate — the web
    /// side debounces because every keystroke there re-renders a React tree
    /// over the same already-loaded rows; here the work is one pass over a
    /// `Vec` and a `uniform_list` that redraws a screenful either way.
    fn search_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        self.with_tracks(cx, |state| {
            match keystroke.key.as_str() {
                "backspace" => {
                    state.query.pop();
                }
                "escape" => state.query.clear(),
                _ => match keystroke.key_char.as_deref() {
                    // A keystroke with no character is a chord or a navigation
                    // key: not text, and not this field's business.
                    Some(text) if !text.contains(['\n', '\t']) => state.query.push_str(text),
                    _ => return,
                },
            }
            state.refilter();
        });
    }

    /// Run `edit` against the track screen's state, if that is still what is
    /// showing. A load that lands after the user navigated away is a no-op.
    fn with_tracks(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Tracks)) {
        if let Screen::Tracks(state) = &mut self.screen {
            edit(state);
            cx.notify();
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// Row height. The web rows are content-sized by their 32px album-art cell.
const ROW_HEIGHT: f32 = 32.;
/// `grid-cols-[28px_56px_1fr_1fr_70px_60px_60px_70px]`, minus the selection
/// checkbox this host does not have.
const ART_WIDTH: f32 = 56.;
const BPM_WIDTH: f32 = 70.;
const TIME_WIDTH: f32 = 60.;
const STATUS_WIDTH: f32 = 60.;
const ADDED_BY_WIDTH: f32 = 70.;
const SEARCH_WIDTH: f32 = 200.;
const GAP: f32 = 8.;
const PAD_X: f32 = 16.;
/// The coverage dot's box, matching the web side's `w-1.5 h-1.5`.
const DOT: f32 = 6.;

/// Render the browser. `app` is the root entity every control writes through.
pub fn tracks(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app, window))
        .child(header())
        .child(match &state.error {
            Some(message) => plate(
                format!("Failed to load tracks: {message}"),
                ladder::danger().into(),
            ),
            None if !state.loaded => plate(
                "Loading tracks…".to_string(),
                ladder::muted_foreground().into(),
            ),
            None if state.shown.is_empty() => plate(
                if state.query.is_empty() {
                    "No tracks imported".to_string()
                } else {
                    "No matching tracks".to_string()
                },
                ladder::muted_foreground().into(),
            ),
            None => body(state, app).into_any_element(),
        })
}

/// The way back, the venue, the search field, and the filters.
fn toolbar(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    let back = app.clone();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(PAD_X))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            luma_ui::luma_button("Back", false)
                .id("back")
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.show_venues(cx)))
                .agent_node(Role::Button, "Back"),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.venue_name.clone())
                .agent_node(Role::Text, state.venue_name.clone()),
        )
        .child(count(state.shown.len()))
        .child(div().flex_1())
        .child(search(state, app, window))
        .child(ownership_filter(state, app))
        .child(in_venue_filter(state, app))
}

/// How many rows the filters admit — the web browser's footer, kept in the
/// toolbar because a native window has no second bar to spare.
fn count(shown: usize) -> impl IntoElement {
    let label = format!("{shown} TRACKS");
    div()
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(ladder::muted_foreground())
        .child(label.clone())
        .agent_node(Role::Text, label)
}

/// The search field: a `luma_input` that takes keystrokes.
///
/// It edits a `String` on the screen's state rather than hosting a real text
/// editor — no caret, no selection, no IME — because the browser needs a
/// filter, and every one of those is a control `luma-ui` would have to own for
/// the whole app rather than one this screen invents.
fn search(state: &Tracks, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    const PLACEHOLDER: &str = "Search tracks…";
    let empty = state.query.is_empty();
    let text = if empty { PLACEHOLDER } else { &state.query };
    let focus = state.search_focus.clone();
    let clicked = focus.clone();
    let typed = app.clone();
    luma_ui::luma_input(text, empty, SEARCH_WIDTH)
        .id("search")
        .track_focus(&focus)
        .on_click(move |_, window, cx| window.focus(&clicked, cx))
        .on_key_down(move |event, _, cx| {
            let keystroke = event.keystroke.clone();
            typed.update(cx, |this, cx| this.search_key(&keystroke, cx));
        })
        .agent_node(Role::Input, text.to_string())
        .agent_focused(focus.is_focused(window))
}

/// `<Toggle>`s for `mine` / `all`: one axis, always exactly one pressed.
fn ownership_filter(state: &Tracks, app: &Entity<Luma>) -> Div {
    div().flex().children(
        Ownership::ALL
            .into_iter()
            .enumerate()
            .map(|(index, ownership)| {
                let app = app.clone();
                luma_ui::luma_toggle_segment(
                    ownership.label(),
                    ownership == state.ownership,
                    index == 0,
                )
                .id(ownership.label())
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| this.show_ownership(ownership, cx))
                })
                .agent_node(Role::Toggle, ownership.label())
            }),
    )
}

/// The other axis, and independent of ownership: whether to show only tracks
/// this venue has annotations on.
fn in_venue_filter(state: &Tracks, app: &Entity<Luma>) -> Div {
    let app = app.clone();
    div().child(
        luma_ui::luma_toggle("In Venue", state.in_venue)
            .id("in-venue")
            .on_click(move |_, _, cx| app.update(cx, |this, cx| this.toggle_in_venue(cx)))
            .agent_node(Role::Toggle, "In Venue"),
    )
}

/// The whole body when there is nothing to list: one centred line that says
/// so, named so a script can read the reason instead of inferring it from an
/// empty node list.
fn plate(message: String, color: gpui::Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .text_color(color)
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
}

fn header() -> Div {
    row_shell()
        .flex_shrink_0()
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ladder::muted_foreground())
        .child(art_cell().child(""))
        .child(flex_cell().child("TITLE"))
        .child(flex_cell().child("ARTIST"))
        .child(numeric_cell(BPM_WIDTH).child("BPM"))
        .child(numeric_cell(TIME_WIDTH).child("TIME"))
        .child(centered_cell(STATUS_WIDTH).child("STATUS"))
        .child(numeric_cell(ADDED_BY_WIDTH).child("ADDED BY"))
}

/// The scrolling rows. `uniform_list` virtualizes them, so a library of
/// thousands costs one screenful of elements — the same reason the web side
/// runs a virtualizer. Everything the closure needs is refcounted, so a redraw
/// copies three pointers rather than the library.
fn body(state: &Tracks, app: &Entity<Luma>) -> Div {
    let rows = Rc::clone(&state.rows);
    let shown = Rc::clone(&state.shown);
    let names = Rc::clone(&state.names);
    let user = state.user.clone();
    let app = app.clone();
    div().flex_1().overflow_hidden().child(
        uniform_list("tracks", shown.len(), move |range, _, _| {
            range
                .map(|index| {
                    track_row(
                        index,
                        &rows[shown[index]],
                        &Chrome {
                            user: user.as_deref(),
                            names: &names,
                            app: &app,
                        },
                    )
                })
                .collect()
        })
        .size_full(),
    )
}

/// What a row needs beyond its own record: who is looking, what the other
/// people are called, and where a click on it goes.
struct Chrome<'a> {
    user: Option<&'a str>,
    names: &'a HashMap<String, String>,
    app: &'a Entity<Luma>,
}

fn track_row(index: usize, track: &TrackBrowserRow, chrome: &Chrome) -> AnyElement {
    let stripe = if index.is_multiple_of(2) {
        ladder::background()
    } else {
        ladder::stripe()
    };
    let name = track_name(track);
    let opened = chrome.app.clone();
    let track_id = track.id.clone();
    row_shell()
        .id(SharedString::from(track.id.clone()))
        .on_click(move |_, _, cx| {
            let track_id = track_id.clone();
            opened.update(cx, |this, cx| this.open_track(&track_id, cx));
        })
        .h(px(ROW_HEIGHT))
        .bg(stripe)
        .hover(|s| s.bg(ladder::hover()))
        .text_size(px(12.))
        .text_color(ladder::foreground_90())
        .child(art(track))
        .child(
            flex_cell()
                .flex()
                .items_center()
                .gap(px(6.))
                .children(coverage_dot(track))
                .child(name.clone()),
        )
        .child(
            flex_cell().child(
                track
                    .artist
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".into()),
            ),
        )
        .child(
            numeric_cell(BPM_WIDTH)
                .font_family("SF Mono")
                .child(match track.bpm {
                    Some(bpm) => format!("{bpm:.1}"),
                    None => "--".into(),
                }),
        )
        .child(
            numeric_cell(TIME_WIDTH)
                .font_family("SF Mono")
                .child(duration(track.duration_seconds)),
        )
        .child(preprocessing_status(track))
        .child(numeric_cell(ADDED_BY_WIDTH).child(added_by(track, chrome)))
        .agent_node(Role::Row, name)
        .into_any_element()
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

/// The seven preprocessing steps, as "done / total".
///
/// The web side draws this as a progress ring with the step names in a
/// tooltip. GPUI has no arc primitive and this host has no hover cards, so the
/// same fact is written out — which is also the only form a driver can read.
fn preprocessing_status(track: &TrackBrowserRow) -> Div {
    let steps = [
        track.has_storage,
        track.has_beats,
        track.has_stems,
        track.has_roots,
        track.has_drum_onsets,
        track.has_bar_classifications,
        track.has_genres,
    ];
    let done = steps.iter().filter(|step| **step).count();
    centered_cell(STATUS_WIDTH)
        .font_family("SF Mono")
        .text_size(px(10.))
        .text_color(if done == steps.len() {
            ladder::status_ok()
        } else {
            ladder::muted_foreground()
        })
        .child(format!("{done}/{}", steps.len()))
}

/// Who imported this track: "you" for your own and for the guest namespace,
/// the person's display name otherwise, "shared" while — or if — that name
/// cannot be looked up.
fn added_by(track: &TrackBrowserRow, chrome: &Chrome) -> String {
    match &track.uid {
        Some(uid) if Some(uid.as_str()) != chrome.user => chrome
            .names
            .get(uid)
            .cloned()
            .unwrap_or_else(|| "shared".into()),
        _ => "you".into(),
    }
}

/// `formatDuration`: `M:SS`, `--:--` when unknown.
fn duration(seconds: Option<f64>) -> String {
    match seconds {
        Some(seconds) if seconds.is_finite() => {
            let total = seconds.max(0.);
            format!("{}:{:02}", (total / 60.) as u64, (total % 60.) as u64)
        }
        _ => "--:--".into(),
    }
}

// -- cell geometry, shared by the header and every row ------------------------

/// One row's box. `w_full` is load-bearing: a `uniform_list` item is laid out
/// against its own content unless it claims the list's width, and without it
/// the `flex_1` cells never expand and the stripe stops where the text does.
fn row_shell() -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .overflow_hidden()
}

/// The art column's box: `h-8 w-14 overflow-hidden`, which is what makes the
/// row 32px tall on the web side too.
fn art_cell() -> Div {
    div()
        .flex_shrink_0()
        .w(px(ART_WIDTH))
        .h(px(ROW_HEIGHT))
        .overflow_hidden()
}

/// One row's art: the image cropped to fill, or the same "no art" plate the
/// web side draws when a track has none.
fn art(track: &TrackBrowserRow) -> Div {
    let cell = art_cell();
    match &track.album_art_path {
        // Explicit pixel bounds, not `size_full`: an `Img` falls back to the
        // decoded image's intrinsic size when its own box isn't definite, and
        // an oversized cell doesn't just overflow — it breaks `uniform_list`,
        // which scrolls on the assumption that every row is the same height.
        Some(path) => cell.child(
            img(PathBuf::from(path))
                .w(px(ART_WIDTH))
                .h(px(ROW_HEIGHT))
                .object_fit(ObjectFit::Cover),
        ),
        None => cell
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000033))
            .text_size(px(7.))
            .text_color(ladder::muted_foreground())
            .child("NO ART"),
    }
}

fn flex_cell() -> Div {
    div().flex_1().min_w(px(0.)).overflow_hidden()
}

fn numeric_cell(width: f32) -> Div {
    div()
        .flex_shrink_0()
        .w(px(width))
        .flex()
        .justify_end()
        .overflow_hidden()
}

fn centered_cell(width: f32) -> Div {
    div()
        .flex_shrink_0()
        .w(px(width))
        .flex()
        .justify_center()
        .overflow_hidden()
}
