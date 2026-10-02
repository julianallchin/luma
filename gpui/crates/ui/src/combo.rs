//! A combo box: a filter field over a list that goes levels deep.
//!
//! The card is a navigator, the same object as the add-track palette: a
//! header band with the filter, the list, and a footer legend. The field
//! holds focus and takes the text keys; the navigation keys bubble out of it
//! to the card ([`Nav`]). ↵ on a row reports it ([`Event::Pick`]); the owner
//! decides what a pick means — open something and close the card, or
//! [`Combo::push`] a deeper level.
//!
//! # Only the list changes level
//!
//! The header and the footer stay where they are, and the field in the
//! header is the one field for every level. So focus never moves while a
//! level slides in, and the card keeps one size whatever a level holds:
//! loading, empty, or a long filtered list. A level keeps its own query,
//! cursor and scroll, so walking back finds the list as it was left.
//!
//! The slide between levels is [`morph::layers`], the dialogs' route morph
//! without the dialog surface: the host (a popover) brings its own.

use std::ops::Range;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    div, px, uniform_list, AnyElement, Context, Div, Entity, EventEmitter, Focusable as _,
    KeyDownEvent, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window,
};
use gpui_component::Icon;

use crate::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, MorphTransition};
use crate::float::{self, Nav, Picker, RowState};
use crate::icons::IconName;
use crate::node::{AgentNode as _, Instrument as _, Role};
use crate::text_input::{self, TextInput};
use crate::{ladder, motion, radius};

/// What a combo box lists. One type for every level, so a level is a list of
/// rows and nothing about its depth.
pub trait Row: Clone + 'static {
    /// Unique among the rows of the card and stable across frames: the row's
    /// element id and its hover key.
    fn key(&self) -> SharedString;
    /// The words a reader and a driver know the row by.
    fn label(&self) -> SharedString;
    /// Whether the row survives `query`, which is trimmed, lowercased and
    /// never empty.
    fn matches(&self, query: &str) -> bool;
    /// What the row shows, inside the plate the card draws for it.
    fn content(&self) -> AnyElement;
}

/// What the card tells its owner.
pub enum Event<T> {
    /// ↵, → or a click on this row.
    Pick(T),
    /// Escape, or the `esc` cap.
    Dismiss,
}

/// The height of every row. `uniform_list` virtualizes on one row height,
/// so this is a layout fact; it fits a 32px lead with the row's padding.
pub const ROW_HEIGHT: f32 = 44.0;
/// The card's width.
const WIDTH: f32 = 380.0;
/// Rows a level shows without scrolling.
const VISIBLE_ROWS: usize = 7;
/// The list's own top and bottom gutter — [`float::viewport`]'s.
const LIST_GUTTER: f32 = 6.0;
/// The body's height: the visible rows and the list's gutters. Fixed, so the
/// card does not change size with what a level holds.
const BODY_HEIGHT: f32 = VISIBLE_ROWS as f32 * ROW_HEIGHT + 2.0 * LIST_GUTTER;

/// One level: its rows, its query, its cursor and its scroll.
pub struct Level<T> {
    /// Who the level is about. [`Combo::fill`] matches on it, so a listing
    /// that lands after the reader went elsewhere fills nothing.
    key: SharedString,
    placeholder: SharedString,
    /// What the list says when the level has no rows at all.
    empty: SharedString,
    listing: Listing,
    picker: Picker<T>,
    scroll: UniformListScrollHandle,
}

enum Listing {
    Loading,
    Ready,
    Failed(SharedString),
}

impl<T: Row> Level<T> {
    /// A level whose rows are on their way, until [`Level::with_rows`] or
    /// [`Combo::fill`] gives them.
    #[must_use]
    pub fn new(
        key: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        empty: impl Into<SharedString>,
    ) -> Self {
        Self {
            key: key.into(),
            placeholder: placeholder.into(),
            empty: empty.into(),
            listing: Listing::Loading,
            picker: Picker::new(|row: &T, query: &str| row.matches(&query.to_lowercase())),
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// The level with its rows.
    #[must_use]
    pub fn with_rows(mut self, rows: Vec<T>) -> Self {
        self.picker.set_rows(rows);
        self.listing = Listing::Ready;
        self
    }
}

/// The card's state. An entity, because it owns a text field and the
/// field's subscription, and handles its own keys.
pub struct Combo<T: Row> {
    label: SharedString,
    /// Every level from the root down. The ones deeper than [`Self::depth`]
    /// are kept until the next push, so a level sliding out still has its
    /// rows to paint.
    levels: Vec<Level<T>>,
    depth: usize,
    /// Keyed by depth.
    morph: MorphDialog<usize>,
    filter: Entity<TextInput>,
    focus_pending: bool,
    _filter: Subscription,
}

impl<T: Row> EventEmitter<Event<T>> for Combo<T> {}

impl<T: Row> Combo<T> {
    /// A card on `root`. `label` names the card for readers and drivers.
    pub fn new(label: impl Into<SharedString>, root: Level<T>, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| TextInput::search(root.placeholder.clone(), cx));
        let subscription = cx.subscribe(&filter, |this, field, event, cx| {
            if event == &text_input::Event::Edited {
                let query = field.read(cx).text().to_string();
                this.filter_changed(query, cx);
            } else {
                cx.notify();
            }
        });
        Self {
            label: label.into(),
            levels: vec![root],
            depth: 0,
            morph: MorphDialog::new(route(0, MorphTransition::Forward), body_size()),
            filter,
            focus_pending: true,
            _filter: subscription,
        }
    }

    /// Go one level deeper, onto `level`, with an empty query.
    pub fn push(&mut self, level: Level<T>, cx: &mut Context<Self>) {
        self.levels.truncate(self.depth + 1);
        self.levels.push(level);
        self.depth += 1;
        self.arrive(MorphTransition::Forward, cx);
    }

    /// Give the level keyed `key` its rows, or the reason it has none. A
    /// level the reader has left is not filled.
    pub fn fill(&mut self, key: &str, rows: Result<Vec<T>, String>, cx: &mut Context<Self>) {
        let level = &mut self.levels[self.depth];
        if level.key != key {
            return;
        }
        match rows {
            Ok(rows) => {
                level.picker.set_rows(rows);
                level.listing = Listing::Ready;
            }
            Err(error) => level.listing = Listing::Failed(error.into()),
        }
        cx.notify();
    }

    /// One level up. At the root there is nowhere to go.
    fn back(&mut self, cx: &mut Context<Self>) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        self.arrive(MorphTransition::Back, cx);
    }

    /// Slide to the level at [`Self::depth`] and put its query and
    /// placeholder in the field.
    fn arrive(&mut self, transition: MorphTransition, cx: &mut Context<Self>) {
        self.morph.request(
            route(self.depth, transition),
            Instant::now(),
            motion::reduced_motion(cx),
        );
        let level = &self.levels[self.depth];
        let (query, placeholder) = (level.picker.query().to_string(), level.placeholder.clone());
        self.filter.update(cx, |field, cx| {
            field.set_placeholder(placeholder, cx);
            field.set_text(query, cx);
        });
        cx.notify();
    }

    /// Mirror an edit of the field into the level. The cursor goes back to
    /// the top, because the row under it belonged to the previous result.
    fn filter_changed(&mut self, query: String, cx: &mut Context<Self>) {
        let level = &mut self.levels[self.depth];
        if level.picker.query() == query {
            return;
        }
        level.picker.set_query(query);
        level.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(nav) = Nav::of(&event.keystroke, self.filter.read(cx).is_empty()) else {
            return;
        };
        cx.stop_propagation();
        match nav {
            Nav::Dismiss => cx.emit(Event::Dismiss),
            Nav::Back => self.back(cx),
            Nav::Step(delta) => {
                let level = &mut self.levels[self.depth];
                if let Some(at) = level.picker.step(delta) {
                    level.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            Nav::Open => {
                if let Some(row) = self.levels[self.depth].picker.current().cloned() {
                    cx.emit(Event::Pick(row));
                }
            }
            // A combo box only opens; there is no set to commit.
            Nav::Submit => {}
        }
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        // The bands take the popover's corner, not the dialog's.
        let mut band = float::header_band().rounded_t(px(radius::CARD));
        if self.depth > 0 {
            band = band.child(
                float::key_cap_pressable(float::key_cap())
                    .id("combo-back")
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx)))
                    .child(Icon::new(IconName::ArrowLeft).size(px(12.5)))
                    .agent_node(Role::Button, "Back"),
            );
        }
        let placeholder = self.levels[self.depth].placeholder.clone();
        let focused = self.filter.read(cx).focus_handle(cx).is_focused(window);
        band.child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(14.0))
                .child(self.filter.clone())
                .agent_node(Role::Input, placeholder)
                .agent_focused(focused),
        )
        .child(
            float::key_cap_pressable(float::key_cap())
                .id("combo-close")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Dismiss)))
                .child("esc")
                .agent_node(Role::Button, "Close"),
        )
    }

    /// The level at `depth`, as one layer of the slide.
    fn body(&self, depth: usize, mode: ContentMode, cx: &mut Context<Self>) -> AnyElement {
        let level = &self.levels[depth];
        let list = float::viewport();
        let message = match &level.listing {
            Listing::Loading => {
                return list
                    .child(
                        float::list().child(
                            float::skeleton_rows(6, cx.entity_id(), cx)
                                .agent_node(Role::Text, "Loading…"),
                        ),
                    )
                    .into_any_element();
            }
            Listing::Failed(error) => {
                return list
                    .child(float::list().child(
                        float::error_row(error.clone()).agent_node(Role::Text, error.clone()),
                    ))
                    .into_any_element();
            }
            Listing::Ready if !level.picker.is_empty() => None,
            Listing::Ready if level.picker.query().trim().is_empty() => Some(level.empty.clone()),
            Listing::Ready => Some(SharedString::from("Nothing matches")),
        };
        if let Some(message) = message {
            return list
                .child(
                    float::list()
                        .child(float::empty_row(message.clone()).agent_node(Role::Text, message)),
                )
                .into_any_element();
        }
        if mode == ContentMode::PaintOnly {
            return list.child(self.still_rows(level)).into_any_element();
        }
        list.child(
            uniform_list(
                "combo-rows",
                level.picker.shown().len(),
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    range
                        .filter_map(|index| this.row(depth, index, cx))
                        .collect()
                }),
            )
            .track_scroll(&level.scroll)
            .size_full()
            .px(px(8.0)),
        )
        .into_any_element()
    }

    /// One live row: hover lifts it, a click picks it.
    fn row(&self, depth: usize, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let level = &self.levels[depth];
        let row = level.picker.get(index)?.clone();
        let state = RowState::of(false, index == level.picker.cursor());
        let (key, label) = (row.key(), row.label());
        Some(
            plate(&row, state)
                .id(key)
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(Event::Pick(row.clone()))))
                .agent_node(Role::Row, label)
                .into_any_element(),
        )
    }

    /// A level that is sliding: the rows it showed where it was scrolled to,
    /// painted only. A paint-only layer holds no listeners and no scroll.
    fn still_rows(&self, level: &Level<T>) -> Div {
        let scrolled = -f32::from(level.scroll.0.borrow().base_handle.offset().y);
        let first = (scrolled / ROW_HEIGHT).floor().max(0.0) as usize;
        let rows = (first..first + VISIBLE_ROWS + 1).filter_map(|index| {
            let row = level.picker.get(index)?;
            let state = RowState::of(false, index == level.picker.cursor());
            Some(plate(row, state).agent_node(Role::Row, row.label()))
        });
        div().size_full().overflow_hidden().px(px(8.0)).child(
            div()
                .relative()
                .top(px(first as f32 * ROW_HEIGHT - scrolled))
                .children(rows),
        )
    }
}

impl<T: Row> gpui::Render for Combo<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if self.morph.tick(now, motion::reduced_motion(cx)) {
            window.request_animation_frame();
        }
        if std::mem::take(&mut self.focus_pending) {
            // Deferred: the field's element does not exist until this frame.
            let focus = self.filter.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        let sample = self.morph.sample(now);
        // Each layer is a column the level's viewport fills, so the list
        // gets the body's whole height to virtualize over.
        let layers = morph::layers(&sample, |&depth, mode| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.body(depth, mode, cx))
                .into_any_element()
        });
        div()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .text_color(ladder::foreground())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.key(event, cx)))
            .child(self.header(window, cx))
            .child(
                div()
                    .relative()
                    .w(px(WIDTH))
                    .h(px(BODY_HEIGHT))
                    .overflow_hidden()
                    .children(layers),
            )
            .child(float::nav_legend("Open").rounded_b(px(radius::CARD)))
            .agent_node(Role::Card, self.label.clone())
    }
}

/// A row's plate: the house list row at the card's one row height.
fn plate<T: Row>(row: &T, state: RowState) -> Div {
    float::menu_row(state, row.key())
        .h(px(ROW_HEIGHT))
        .w_full()
        .child(row.content())
}

fn body_size() -> MorphSize {
    MorphSize::new(WIDTH, BODY_HEIGHT)
}

fn route(depth: usize, transition: MorphTransition) -> morph::RouteDescriptor<usize> {
    morph::RouteDescriptor::exact(depth, WIDTH, BODY_HEIGHT).with_transition(transition)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Song(&'static str);

    impl Row for Song {
        fn key(&self) -> SharedString {
            self.0.into()
        }
        fn label(&self) -> SharedString {
            self.0.into()
        }
        fn matches(&self, query: &str) -> bool {
            self.0.to_lowercase().contains(query)
        }
        fn content(&self) -> AnyElement {
            div().into_any_element()
        }
    }

    /// The match a level filters with is case-blind: rows match on what they
    /// say, not on how the query was typed.
    #[test]
    fn a_level_filters_case_blind() {
        let mut level = Level::new("root", "Search…", "Nothing here")
            .with_rows(vec![Song("Aurora"), Song("Strobe")]);
        level.picker.set_query("AUR");
        assert_eq!(
            level.picker.shown().cloned().collect::<Vec<_>>(),
            [Song("Aurora")]
        );
    }
}
