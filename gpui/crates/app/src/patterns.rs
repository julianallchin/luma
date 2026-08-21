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

use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};

use luma_lib::models::patterns::PatternSummary;

use crate::{Luma, Screen};

/// The screen's whole state: what the query returned, and whether it has.
pub struct Patterns {
    rows: Rc<[PatternSummary]>,
    /// Written in the same assignment as [`Self::rows`] and [`Self::error`],
    /// so "still loading" and "nothing to show" cannot be confused.
    loaded: bool,
    error: Option<String>,
}

impl Luma {
    /// Navigate to the pattern list and read it.
    pub(crate) fn show_patterns(&mut self, cx: &mut Context<Self>) {
        let pending = self.library.patterns();
        self.screen = Screen::Patterns(Patterns {
            rows: Rc::from(Vec::new()),
            loaded: false,
            error: None,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Screen::Patterns(state) = &mut this.screen {
                    state.loaded = true;
                    match result {
                        Ok(rows) => state.rows = rows.into(),
                        Err(message) => state.error = Some(message),
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
        let Screen::Patterns(state) = &self.screen else {
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

pub fn patterns(state: &Patterns, app: &Entity<Luma>) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        .child(header())
        .child(match &state.error {
            Some(message) => plate(
                format!("Failed to load patterns: {message}"),
                ladder::danger().into(),
            ),
            None if !state.loaded => plate(
                "Loading patterns…".to_string(),
                ladder::muted_foreground().into(),
            ),
            None if state.rows.is_empty() => {
                plate("No patterns".to_string(), ladder::muted_foreground().into())
            }
            None => body(state, app).into_any_element(),
        })
}

fn toolbar(state: &Patterns, app: &Entity<Luma>) -> Div {
    let back = app.clone();
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
            luma_ui::luma_button("Back", false)
                .id("back")
                .on_click(move |_, _, cx| back.update(cx, |this, cx| this.back(cx)))
                .agent_node(Role::Button, "Back"),
        )
        .child(
            div()
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(ladder::muted_foreground())
                .child(label.clone())
                .agent_node(Role::Text, label),
        )
}

fn plate(message: String, color: Hsla) -> AnyElement {
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
        .child(div().flex_1().child("NAME"))
        .child(div().w(px(CATEGORY_WIDTH)).child("CATEGORY"))
        .child(div().w(px(AUTHOR_WIDTH)).child("AUTHOR"))
}

fn body(state: &Patterns, app: &Entity<Luma>) -> Div {
    let rows = Rc::clone(&state.rows);
    let app = app.clone();
    div().flex_1().overflow_hidden().child(
        uniform_list("patterns", rows.len(), move |range, _, _| {
            range
                .map(|index| pattern_row(index, &rows[index], &app))
                .collect()
        })
        .size_full(),
    )
}

fn pattern_row(index: usize, pattern: &PatternSummary, app: &Entity<Luma>) -> AnyElement {
    let stripe = if index.is_multiple_of(2) {
        ladder::background()
    } else {
        ladder::stripe()
    };
    let app = app.clone();
    let id = pattern.id.clone();
    row_shell()
        .id(SharedString::from(pattern.id.clone()))
        .h(px(ROW_HEIGHT))
        .bg(stripe)
        .hover(|s| s.bg(ladder::hover()))
        .text_size(px(12.))
        .text_color(ladder::foreground_90())
        .on_click(move |_, _, cx| {
            let id = id.clone();
            app.update(cx, |this, cx| {
                if let Some(pattern) = this.find_pattern(&id) {
                    this.open_pattern(pattern, cx);
                }
            });
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
