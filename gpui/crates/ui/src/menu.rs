//! The context menu: a float opened by a right-click, at the pointer.
//!
//! # Why this is a builder and not a `Div`
//!
//! Every other control in this crate is a free function returning a `Div` the
//! caller composes, because none of them hold anything. A context menu holds
//! *a list* — and a list whose rows each carry an action, a tone and an
//! enablement. Handing a caller the pieces to assemble that itself is how the
//! two menus in this app ended up with two spellings of a destructive row; the
//! builder is the one spelling.
//!
//! Open state stays the caller's, as it is for every menu here (see
//! [`crate::arg::select`]'s note): the screen already knows which row was
//! right-clicked, and a second store inside the widget is how two menus end up
//! open at once. What a caller owns is "where, and about what"; what this owns
//! is everything from there to the pixels.
//!
//! # Escape
//!
//! Not bound here. An open menu is the innermost thing on screen, and the app
//! has exactly one rung-by-rung answer to "close what is over me"
//! (`Luma::dismiss_overlay`); a menu that bound its own `escape` would have to
//! out-scope that ladder and would then be the only Escape in the app that
//! does not mean what the others mean. The caller adds a rung.
//!
//! The pointer half *is* here, because it is not a rung but a property of
//! floating: [`crate::float::Dismiss`] closes the menu on any press outside
//! it, and swallows that press.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, ElementId, Point, SharedString, Window};

use crate::float::{self, Dismiss, Dismissal, RowState};
use crate::ladder;
use crate::node::{Instrument as _, Role};

/// What a row *is*, as far as colour is concerned.
///
/// Two tones and no more: a menu row either does the ordinary thing or it
/// destroys something, and every other distinction a row could want to draw
/// (unavailable, checked) is already a state rather than a tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    /// Deletes something. Only the *word* is [`ladder::danger`]; the row keeps
    /// the one hover wash every floating row has. A second hover fill would be
    /// a second hover mechanism, and the tone is already legible in the label
    /// the moment the pointer is nowhere near it — which is when it matters.
    Destructive,
}

/// One line of the menu.
enum Entry {
    Item {
        label: SharedString,
        tone: Tone,
        act: Dismissal,
    },
    /// A hairline between two groups of items.
    Separator,
}

/// A menu hanging at a window-space point.
///
/// ```text
/// float::ContextMenu::new("score-menu", at)
///     .destructive("Delete score", move |_, cx| { … })
///     .render(move |_, cx| { … })
/// ```
#[must_use]
pub struct ContextMenu {
    id: SharedString,
    at: Point<gpui::Pixels>,
    entries: Vec<Entry>,
}

impl ContextMenu {
    /// A menu at `at` — the window-space point the press landed on, which is
    /// what a right-click hands you.
    ///
    /// `id` must be unique app-wide and stable across frames: it keys the
    /// entrance animation and prefixes every row's element id.
    pub fn new(id: impl Into<SharedString>, at: Point<gpui::Pixels>) -> Self {
        Self {
            id: id.into(),
            at,
            entries: Vec::new(),
        }
    }

    /// An ordinary row.
    pub fn item(
        self,
        label: impl Into<SharedString>,
        act: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.push(label, Tone::Normal, act)
    }

    /// A row that destroys something — see [`Tone::Destructive`].
    pub fn destructive(
        self,
        label: impl Into<SharedString>,
        act: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.push(label, Tone::Destructive, act)
    }

    /// A hairline between groups. Leading, trailing and doubled separators are
    /// dropped at render, so a caller may emit one per group unconditionally.
    pub fn separator(mut self) -> Self {
        self.entries.push(Entry::Separator);
        self
    }

    fn push(
        mut self,
        label: impl Into<SharedString>,
        tone: Tone,
        act: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.entries.push(Entry::Item {
            label: label.into(),
            tone,
            act: Box::new(act),
        });
        self
    }

    /// Hang it. `dismiss` is what closing means — run on Escape's rung too, so
    /// the two doors out lead to the same place.
    pub fn render(self, dismiss: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
        let Self { id, at, entries } = self;
        let mut card = float::popover_card().min_w(px(MIN_WIDTH));
        let mut pending_separator = false;
        let mut drawn = 0usize;
        for entry in entries {
            match entry {
                // Held rather than drawn: a separator is only real once
                // something follows it.
                Entry::Separator => pending_separator = drawn > 0,
                Entry::Item { label, tone, act } => {
                    if std::mem::take(&mut pending_separator) {
                        card = card.child(float::divider().my(px(2.0)));
                    }
                    drawn += 1;
                    card = card.child(row(&id, label, tone, act));
                }
            }
        }
        float::anchored_at(
            id,
            at,
            Dismiss::on_press_out(dismiss),
            card.agent_node(Role::Card, MENU_LABEL).into_any_element(),
        )
    }
}

/// The label the whole card carries in the automation tree. One spelling, so a
/// script asserts a menu is up the same way wherever it was opened.
pub const MENU_LABEL: &str = "Context menu";

/// Wide enough that a two-word verb is not a sliver, and narrow enough that a
/// menu of one row does not read as a panel. The same absolute-minimum
/// treatment every other float in the app gets.
const MIN_WIDTH: f32 = 176.0;

fn row(menu: &SharedString, label: SharedString, tone: Tone, act: Dismissal) -> AnyElement {
    let key = SharedString::from(format!("{menu}:{label}"));
    let mut row = float::menu_row(RowState::Rest, key.clone())
        .id(ElementId::Name(key))
        .child(div().flex_1().min_w(px(0.)).child(label.clone()));
    if tone == Tone::Destructive {
        row = row.text_color(ladder::danger());
    }
    row.on_click(move |_, window, cx| act(window, cx))
        .agent_node(Role::Button, label)
        .into_any_element()
}
