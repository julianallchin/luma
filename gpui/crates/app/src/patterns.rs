//! The pattern list: every pattern in the library, and the door into the
//! graph editor.
//!
//! The web app has no screen like this — patterns are reached from a track's
//! timeline or the perform page, and `/pattern/:id` is a top-level route with
//! no index. This is the minimum a native host needs to get to the same
//! editor: a name, its category, and its author, in one striped table.
//!
//! Deliberately not scoped to a venue. `list_patterns` takes no arguments,
//! because a pattern belongs to the library rather than to a room, so this
//! screen hangs off the welcome screen and not off a venue.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::Enabled;

use luma_lib::models::patterns::PatternSummary;

use crate::Luma;

/// The screen's whole state: what the query returned, and whether it has.
pub struct Patterns {
    rows: Rc<[PatternSummary]>,
    /// Written in the same assignment as [`Self::rows`] and [`Self::error`],
    /// so "still loading" and "nothing to show" cannot be confused.
    loaded: bool,
    error: Option<String>,
}

impl Luma {
    /// Open the pattern picker overlay and read the list.
    pub(crate) fn show_patterns(&mut self, cx: &mut Context<Self>) {
        let pending = self.library.patterns();
        self.overlay.open(crate::shell::Overlay::Patterns(Patterns {
            rows: Rc::from(Vec::new()),
            loaded: false,
            error: None,
        }));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(crate::shell::Overlay::Patterns(state)) = this.overlay.open_mut() {
                    state.loaded = true;
                    match result {
                        Ok(rows) => state.rows = rows.into(),
                        Err(error) => state.error = Some(error.to_string()),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The loaded pattern a row click carries.
    fn find_pattern(&self, id: &str) -> Option<PatternSummary> {
        let Some(crate::shell::Overlay::Patterns(state)) = self.overlay.get() else {
            return None;
        };
        state.rows.iter().find(|row| row.id == id).cloned()
    }
}

// -- rendering ----------------------------------------------------------------

const ROW_HEIGHT: f32 = 32.;
const CATEGORY_WIDTH: f32 = 140.;
const AUTHOR_WIDTH: f32 = 140.;
const GAP: f32 = 8.;
const PAD_X: f32 = 16.;

/// `track_open` is whether the workspace resolved a track context for the
/// graph doors (`Luma::graph_track_context`). Without one the rows are inert
/// with the stated reason — the overlay stays a full pattern browser, but a
/// row cannot open an editor that could not preview (§6).
pub fn patterns(
    state: &Patterns,
    app: &Entity<Luma>,
    first_focus: &FocusHandle,
    first_focused: bool,
    last_focus: &FocusHandle,
    last_focused: bool,
    track_open: bool,
) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        // The dialog host owns the card surface; route content supplies only
        // its interior so the renderer effect can be upgraded in one place.
        .text_color(ladder::foreground())
        .child(toolbar(state, app, first_focus, first_focused, track_open))
        .child(header())
        .child(match &state.error {
            Some(message) => luma_ui::plate(
                format!("Failed to load patterns: {message}"),
                ladder::danger(),
            ),
            None if !state.loaded => {
                luma_ui::plate("Loading patterns…".to_string(), ladder::muted_foreground())
            }
            None if state.rows.is_empty() => {
                luma_ui::plate("No patterns".to_string(), ladder::muted_foreground())
            }
            None => body(state, app, last_focus, last_focused, track_open).into_any_element(),
        })
}

fn toolbar(
    state: &Patterns,
    app: &Entity<Luma>,
    first_focus: &FocusHandle,
    first_focused: bool,
    track_open: bool,
) -> Div {
    let close = app.clone();
    let label = format!("{} PATTERNS", state.rows.len());
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
            luma_ui::luma_button("Close", Enabled::Yes)
                .id("close")
                .track_focus(first_focus)
                .tab_stop(true)
                .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
                .agent_node(Role::Button, "Close")
                .agent_focused(first_focused),
        )
        .child(luma_ui::silkscreen(label))
        // The inert rows' reason, said once for the list rather than muttered
        // per row (the row itself only dims).
        .when(!track_open, |strip| {
            strip.child(luma_ui::silkscreen(
                crate::graph::NO_TRACK_REASON.to_uppercase(),
            ))
        })
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
        .child(div().flex_1().child("NAME"))
        .child(div().w(px(CATEGORY_WIDTH)).child("CATEGORY"))
        .child(div().w(px(AUTHOR_WIDTH)).child("AUTHOR"))
}

fn body(
    state: &Patterns,
    app: &Entity<Luma>,
    last_focus: &FocusHandle,
    last_focused: bool,
    track_open: bool,
) -> Div {
    let rows = Rc::clone(&state.rows);
    let row_count = rows.len();
    let app = app.clone();
    let last_focus = last_focus.clone();
    div().flex_1().overflow_hidden().child(
        uniform_list("patterns", rows.len(), move |range, _, _| {
            range
                .map(|index| {
                    pattern_row(
                        index,
                        &rows[index],
                        &app,
                        (index + 1 == row_count).then_some((&last_focus, last_focused)),
                        track_open,
                    )
                })
                .collect()
        })
        .size_full(),
    )
}

fn pattern_row(
    index: usize,
    pattern: &PatternSummary,
    app: &Entity<Luma>,
    focus: Option<(&FocusHandle, bool)>,
    track_open: bool,
) -> AnyElement {
    let stripe = if index.is_multiple_of(2) {
        ladder::background()
    } else {
        ladder::stripe()
    };
    let app = app.clone();
    let id = pattern.id.clone();
    row_shell()
        .id(SharedString::from(pattern.id.clone()))
        // Inert rows keep their tab stops: the dialog's focus ring is about
        // reaching things, and a row you can land on but cannot act on reads
        // as disabled instead of missing.
        .tab_index(0)
        .when_some(focus.map(|(handle, _)| handle), |row, handle| {
            row.track_focus(handle).tab_stop(true)
        })
        .h(px(ROW_HEIGHT))
        .bg(stripe)
        .text_size(px(12.))
        .text_color(ladder::foreground_90())
        .map(|row| {
            if track_open {
                row.hover(|s| s.bg(ladder::hover()))
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        app.update(cx, |this, cx| {
                            if let Some(pattern) = this.find_pattern(&id) {
                                this.open_pattern(pattern, cx);
                            }
                        });
                    })
            } else {
                row.opacity(ladder::DISABLED_OPACITY)
            }
        })
        .child(div().flex_1().child(pattern.name.clone()))
        .child(
            div()
                .w(px(CATEGORY_WIDTH))
                .text_color(ladder::muted_foreground())
                .child(pattern.category_name.clone().unwrap_or_else(|| "—".into())),
        )
        .child(
            div()
                .w(px(AUTHOR_WIDTH))
                .text_color(ladder::muted_foreground())
                .child(pattern.author_name.clone().unwrap_or_else(|| "—".into())),
        )
        .agent_node(Role::Row, pattern.name.clone())
        .agent_disabled(!track_open)
        .agent_focused(focus.is_some_and(|(_, focused)| focused))
        .into_any_element()
}

/// The one row geometry both the header and the rows lay out against.
fn row_shell() -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .overflow_hidden()
}
