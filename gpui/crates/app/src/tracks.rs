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

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};

use luma_lib::models::tracks::TrackBrowserRow;
use luma_lib::models::venues::Venue;

use crate::Luma;

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
    query: String,
    /// The signed-in principal, snapshotted when the venue was selected. The
    /// ownership filter reads it.
    user: Option<String>,
    /// Exact return target for the venue dialog. Pointer activation focuses
    /// this handle before opening the overlay, so Escape restores the control
    /// itself rather than the sidebar region generically.
    venue_focus: FocusHandle,
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

    pub(crate) fn load_generation(&self) -> u64 {
        self.load_generation
    }

    /// The loaded row a click carries. Looked up by id rather than by index
    /// because the click was registered against a *filtered* list, and the
    /// filters can change before it lands.
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
        let leaving: Vec<crate::shell::Body> = self
            .workspace
            .close_where(|target| target.venue().is_some_and(|owner| owner != venue.id));
        for body in leaving {
            self.teardown(body, cx);
        }
        self.overlay = None;
        let pending = self.library.tracks(&venue.id);
        let remember = self
            .library
            .set_session_item(crate::welcome::LAST_VENUE, &venue.id);
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
            user: self.library.user_id().map(str::to_string),
            venue_focus: cx.focus_handle().tab_stop(true),
            search_focus: cx.focus_handle(),
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
        if let Some(state) = &mut self.sidebar {
            if state.venue_id() == venue_id && state.load_generation == generation {
                edit(state);
                cx.notify();
            }
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
/// The album-art thumbnail's box, and the row's second lead. Square, so the
/// placeholder and a loaded cover occupy the same rect and a row cannot change
/// shape when its art arrives.
const ART: f32 = 32.;
/// The art's corner radius — chrome tier, so comet's small-control radius.
const ART_RADIUS: f32 = 4.;

/// The sidebar: the venue's name (the way back to the picker), the search,
/// the filters, and the venue's tracks as a row list.
pub fn sidebar(state: &Tracks, app: &Entity<Luma>, window: &Window) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        // Glass tier: the sidebar sits transparent on the shell's frost —
        // depth comes from the content cards beside it, not from a fill.
        .text_color(luma_ui::glass::ink(0.85))
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
            None => body(state, app).into_any_element(),
        })
}

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
                .rounded(px(6.))
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
                .rounded(px(6.0))
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
    const PLACEHOLDER: &str = "Search tracks…";
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
        .rounded(px(6.))
        .bg(luma_ui::glass::wash(0.04))
        .border_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .text_size(px(12.))
        .text_color(if empty {
            luma_ui::glass::ink(0.35)
        } else {
            luma_ui::glass::ink(0.85)
        })
        .child(SharedString::from(text.to_string()))
        .track_focus(&focus)
        // While this field has the keyboard, a key a person could be typing
        // is text and not a shortcut — see `keymap`. Escape is the reason
        // this is not academic: it clears the query here and dismisses an
        // overlay everywhere else.
        .key_context(crate::keymap::context::TEXT_INPUT)
        // A press focuses it: `track_focus` registers that itself, and it also
        // stops the shell root underneath from taking the focus back.
        .on_key_down(move |event, _, cx| {
            let keystroke = event.keystroke.clone();
            typed.update(cx, |this, cx| this.search_key(&keystroke, cx));
        })
        .agent_node(Role::Input, text.to_string())
        .agent_focused(focus.is_focused(window))
}

/// One glass filter pill: quiet ink that washes when pressed. The sidebar's
/// own control, not `luma_toggle` — the ladder's slabs belong inside the
/// content cards, and this is the frame.
fn filter_pill(id: &'static str, label: &'static str, on: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h(px(20.))
        .px(px(8.))
        .rounded(px(6.))
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
fn body(state: &Tracks, app: &Entity<Luma>) -> Div {
    let rows = Rc::clone(&state.rows);
    let shown = Rc::clone(&state.shown);
    let app = app.clone();
    div().flex_1().overflow_hidden().child(
        uniform_list("tracks", shown.len(), move |range, _, _| {
            range
                .map(|index| track_row(index, &rows[shown[index]], &app))
                .collect()
        })
        .size_full(),
    )
}

fn track_row(_index: usize, track: &TrackBrowserRow, app: &Entity<Luma>) -> AnyElement {
    let name = track_name(track);
    let opened = app.clone();
    let track_id = track.id.clone();
    let artist = track
        .artist
        .clone()
        .unwrap_or_else(|| "Unknown artist".into());
    let sub = match track.bpm {
        Some(bpm) => format!("{artist} · {bpm:.1}"),
        None => artist,
    };
    let membership = format!("{name} venue scores: {}", track.venue_score_count);
    div()
        .id(SharedString::from(track.id.clone()))
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .overflow_hidden()
        .on_click(move |_, _, cx| {
            let track_id = track_id.clone();
            opened.update(cx, |this, cx| this.open_track(&track_id, cx));
        })
        .rounded(px(6.))
        .hover(|row| row.bg(luma_ui::glass::wash(0.06)))
        .child(
            div()
                .flex_shrink_0()
                .w(px(DOT))
                .children(coverage_dot(track)),
        )
        .child(album_art(track))
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
                        .text_color(luma_ui::glass::ink(0.88))
                        .child(name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(luma_ui::glass::ink(0.45))
                        .child(sub)
                        .agent_node(Role::Text, membership),
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
fn album_art(track: &TrackBrowserRow) -> Div {
    div()
        .flex_shrink_0()
        .size(px(ART))
        .rounded(px(ART_RADIUS))
        .overflow_hidden()
        .bg(luma_ui::glass::wash(0.06))
        .children(
            track
                .album_art_path
                .as_ref()
                .filter(|path| !path.is_empty())
                .map(|path| img(PathBuf::from(path)).size(px(ART))),
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
