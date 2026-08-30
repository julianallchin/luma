//! The inventory table: one row per fixture, and the four facts about it a
//! human is allowed to change here.
//!
//! Label, universe, address and mode. Not position — there is no cell for one,
//! which is the point (gauntlet AF8): the columns *are* the closed list of what
//! the patch page writes.
//!
//! A refused address does not move its row. The edited cell reverts to the
//! stored value the instant the allocator says no, and the sentence it said sits
//! under the row until the next edit on it — so "what did I type" and "what is
//! stored" are never the same pixel telling two stories.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Div, Entity, Point, SharedString};

use luma_lib::models::fixtures::PatchedFixture;
use luma_ui::ladder;
use luma_ui::menu::ContextMenu;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, glass, motion};

use super::{Column, Patch};
use crate::Luma;

/// Column widths. Fixed for the numbers so a table of forty rows reads as a
/// column of numbers rather than forty independently-sized cells; flexible for
/// the two that hold names.
const W_MODEL: f32 = 168.0;
const W_MODE: f32 = 116.0;
const W_UNIVERSE: f32 = 64.0;
const W_ADDRESS: f32 = 64.0;
const W_RANGE: f32 = 90.0;
const W_PLACED: f32 = 76.0;
const ROW_HEIGHT: f32 = 38.0;

/// The narrowest the nine columns fit in: the fixed widths, both name columns
/// at their minimum, the gaps between them and the page's own inset. Below
/// this the table scrolls sideways rather than squeezing — a column of
/// addresses that has been compressed into an ellipsis is a column that lies.
const MIN_WIDTH: f32 = W_MODEL
    + W_MODE
    + W_UNIVERSE
    + W_ADDRESS
    + W_RANGE
    + W_PLACED
    + 2.0 * NAME_MIN_WIDTH
    + 7.0 * COLUMN_GAP
    + 2.0 * ROW_INSET;
const NAME_MIN_WIDTH: f32 = 120.0;
const COLUMN_GAP: f32 = 10.0;
const ROW_INSET: f32 = 20.0;

pub(super) fn table(state: &Patch, app: &Entity<Luma>, window: &gpui::Window) -> AnyElement {
    let body: AnyElement = match (&state.error, state.data.as_ref()) {
        (Some(error), _) => luma_ui::plate(
            format!("Failed to load the patch: {error}"),
            ladder::danger(),
        )
        .into_any_element(),
        (None, None) => {
            luma_ui::plate("Reading the patch…".to_string(), ladder::muted_foreground())
                .into_any_element()
        }
        (None, Some(data)) if data.fixtures.is_empty() => luma_ui::plate(
            "Nothing is patched in this venue yet.".to_string(),
            ladder::muted_foreground(),
        )
        .into_any_element(),
        (None, Some(_)) => rows(state, app, window),
    };
    div()
        .id("patch-table")
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .flex()
        .flex_col()
        .child(head())
        .child(body)
        .into_any_element()
}

/// The column names, quiet and in sentence case. Frameless: one hairline under
/// the row and nothing around the cells, per the comet table spec — no header
/// fill, no outer box, no radius.
fn head() -> impl IntoElement {
    div()
        .flex_shrink_0()
        .h(px(30.0))
        .px(px(ROW_INSET))
        .min_w(px(MIN_WIDTH))
        .flex()
        .items_center()
        .gap(px(COLUMN_GAP))
        .border_b_1()
        .border_color(glass::hairline(HAIRLINE))
        .child(cell_flex().child(float::label("Fixture")))
        .child(cell(W_MODEL).child(float::label("Model")))
        .child(cell(W_MODE).child(float::label("Mode")))
        .child(cell(W_UNIVERSE).child(float::label("Universe")))
        .child(cell(W_ADDRESS).child(float::label("Address")))
        .child(cell(W_RANGE).child(float::label("Channels")))
        .child(cell_flex().child(float::label("Group")))
        .child(cell(W_PLACED).child(float::label("Placed")))
}

fn cell(width: f32) -> Div {
    div().flex_shrink_0().w(px(width)).overflow_hidden()
}

fn cell_flex() -> Div {
    div().flex_1().min_w(px(NAME_MIN_WIDTH)).overflow_hidden()
}

/// The one hairline weight on this page, per the comet table spec: the rule
/// between rows and the rule under the head are the same line, and a table
/// drawn with two weights reads as two tables.
const HAIRLINE: f32 = 0.10;

fn rows(state: &Patch, app: &Entity<Luma>, window: &gpui::Window) -> AnyElement {
    div()
        .id("patch-rows")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .children(
            state
                .rows()
                .iter()
                .map(|row| fixture_row(state, row, app, window)),
        )
        .into_any_element()
}

fn fixture_row(
    state: &Patch,
    row: &PatchedFixture,
    app: &Entity<Luma>,
    window: &gpui::Window,
) -> AnyElement {
    let name: SharedString = row
        .label
        .clone()
        .unwrap_or_else(|| row.model.clone())
        .into();
    let selected = state.selected.contains(&row.id);
    let placed = state.is_placed(&row.id);
    let last = row.address + row.num_channels - 1;
    // The selection vocabulary is snake_case because an expression is typed; a
    // table is read. Underscores become spaces for the eye only — the name a
    // score would name is unchanged.
    let group = state
        .group_path(&row.id)
        .map_or_else(|| "—".to_string(), |path| path.replace('_', " "));
    let editing = state.editing.as_ref().filter(|edit| edit.fixture == row.id);

    let picked = app.clone();
    let menued = app.clone();
    let venue_pick = state.venue_id.clone();
    let venue_menu = state.venue_id.clone();
    let id_pick = row.id.clone();
    let id_menu = row.id.clone();

    let fade_key = SharedString::from(format!("patch-row-{}", row.id));
    let mut line = div()
        .id(fade_key.clone())
        .h(px(ROW_HEIGHT))
        .px(px(ROW_INSET))
        .min_w(px(MIN_WIDTH))
        .flex()
        .items_center()
        .gap(px(COLUMN_GAP))
        .border_b_1()
        .border_color(glass::hairline(HAIRLINE))
        // Selection and hover share one fill; nothing else lifts a row, and
        // there is no zebra — the rule between rows is what keeps a wide table
        // readable, per the comet table spec.
        .bg(if selected {
            glass::card_selected_bg()
        } else {
            motion::hover_blend(&fade_key, glass::wash(0.0), glass::glass_hover())
        })
        .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
            let extend = event.modifiers.secondary() || event.modifiers.shift;
            let venue = venue_pick.clone();
            let id = id_pick.clone();
            picked.update(cx, |this, cx| this.pick_patch_row(venue, id, extend, cx));
        })
        .on_mouse_down(gpui::MouseButton::Right, move |event, _, cx| {
            let venue = venue_menu.clone();
            let id = id_menu.clone();
            let at = event.position;
            menued.update(cx, |this, cx| this.open_patch_menu(venue, id, at, cx));
        });
    // The globally-keyed hover, not `.hover()`: a row that scrolls back into
    // view is a remount, and a local hover would snap rather than fade.
    line.interactivity()
        .on_hover(motion::hover_listener(fade_key));

    line = line
        .child(label_cell(state, row, &name, editing, app, window))
        .child(
            cell(W_MODEL)
                .text_size(px(12.0))
                .text_color(ladder::foreground_alpha(0.62))
                .truncate()
                .child(format!("{} {}", row.manufacturer, row.model)),
        )
        .child(mode_cell(state, row, &name, app))
        .child(number_cell(
            state,
            row,
            &name,
            Column::Universe,
            row.universe,
            W_UNIVERSE,
            editing,
            app,
            window,
        ))
        .child(number_cell(
            state,
            row,
            &name,
            Column::Address,
            row.address,
            W_ADDRESS,
            editing,
            app,
            window,
        ))
        .child(
            reading(W_RANGE, format!("{}–{last}", row.address))
                .agent_node(Role::Text, format!("{name} range = {}–{last}", row.address)),
        )
        .child(
            cell_flex()
                .text_size(px(12.0))
                .text_color(ladder::foreground_alpha(0.62))
                .truncate()
                .child(group.clone())
                .agent_node(Role::Text, format!("{name} group = {group}")),
        )
        .child(
            // Only the exception is written. Every fixture in a finished rig is
            // placed, and a column repeating the word down forty rows says
            // nothing while hiding the one row that does not. "Unplaced" is
            // the word everywhere on this page — the tray is a stage-page
            // idea and naming it here would be a second name for one state.
            cell(W_PLACED)
                .text_size(px(12.0))
                .text_color(ladder::status_warn())
                .child(if placed { "" } else { "Unplaced" })
                .agent_node(
                    Role::Text,
                    format!(
                        "{name} placement = {}",
                        if placed { "placed" } else { "unplaced" }
                    ),
                ),
        );

    let line = line
        .agent_node(Role::Row, name.clone())
        .agent_focused(selected);

    // The refusal rides under the row it is about, so the address it names and
    // the address on screen are one glance apart.
    match state.refusal.as_ref().filter(|r| r.fixture == row.id) {
        None => line.into_any_element(),
        Some(refusal) => div()
            .flex()
            .flex_col()
            .child(line)
            .child(
                div()
                    .px(px(20.0))
                    .pb(px(8.0))
                    .text_size(px(11.5))
                    .text_color(ladder::danger())
                    .child(refusal.message.clone())
                    .agent_node(Role::Text, refusal.message.clone()),
            )
            .into_any_element(),
    }
}

/// A numeric cell. Mono, because a column of addresses is a column of numbers
/// and they have to line up; everything else on this page is sans.
fn reading(width: f32, text: String) -> Div {
    cell(width)
        .font_family(luma_ui::fonts::MONO)
        .text_size(px(12.0))
        .text_color(ladder::foreground_alpha(0.85))
        .child(text)
}

fn label_cell(
    state: &Patch,
    row: &PatchedFixture,
    name: &SharedString,
    editing: Option<&super::Editing>,
    app: &Entity<Luma>,
    _window: &gpui::Window,
) -> AnyElement {
    if let Some(field) = editing
        .filter(|edit| edit.column == Column::Label)
        .and_then(|edit| edit.label.as_ref())
    {
        // `enter` is unbound in a search-mode field on purpose, so it arrives
        // here as a plain key event and gets its meaning from the cell: commit
        // and put the caret away. Blur is the other commit point, and the
        // commit is idempotent so hitting both is one write.
        let committed = app.clone();
        let venue = state.venue_id.clone();
        let id = row.id.clone();
        return cell_flex()
            .on_key_down(move |event, _, cx| {
                if event.keystroke.key != "enter" {
                    return;
                }
                let venue = venue.clone();
                let id = id.clone();
                committed.update(cx, |this, cx| this.commit_patch_label(venue, id, cx));
                cx.stop_propagation();
            })
            .child(float::field().w_full().child(field.clone()))
            .agent_node(Role::Input, format!("{name} label = {name}"))
            .into_any_element();
    }
    let opened = app.clone();
    let venue = state.venue_id.clone();
    let id = row.id.clone();
    cell_flex()
        .id(SharedString::from(format!("patch-label-{}", row.id)))
        .text_size(px(13.0))
        .text_color(ladder::foreground())
        .truncate()
        .child(name.clone())
        .when(row.address_pinned, |cell| cell.text_color(ladder::accent()))
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
            let venue = venue.clone();
            let id = id.clone();
            opened.update(cx, |this, cx| {
                this.edit_patch_cell(venue, id, Column::Label, window, cx);
            });
            cx.stop_propagation();
        })
        .agent_node(Role::Text, format!("{name} label = {name}"))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn number_cell(
    state: &Patch,
    row: &PatchedFixture,
    name: &SharedString,
    column: Column,
    value: i64,
    width: f32,
    editing: Option<&super::Editing>,
    app: &Entity<Luma>,
    _window: &gpui::Window,
) -> AnyElement {
    if let Some(field) = editing
        .filter(|edit| edit.column == column)
        .and_then(|edit| edit.number.as_ref())
    {
        return cell(width + 8.0).child(field.clone()).into_any_element();
    }
    let opened = app.clone();
    let venue = state.venue_id.clone();
    let id = row.id.clone();
    let word = match column {
        Column::Universe => "universe",
        _ => "address",
    };
    reading(width, value.to_string())
        .id(SharedString::from(format!("patch-{word}-{}", row.id)))
        .when(row.address_pinned && column == Column::Address, |cell| {
            cell.text_color(ladder::accent())
        })
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
            let venue = venue.clone();
            let id = id.clone();
            opened.update(cx, |this, cx| {
                this.edit_patch_cell(venue, id, column, window, cx);
            });
            cx.stop_propagation();
        })
        .agent_node(Role::Text, format!("{name} {word} = {value}"))
        .into_any_element()
}

fn mode_cell(
    state: &Patch,
    row: &PatchedFixture,
    name: &SharedString,
    app: &Entity<Luma>,
) -> AnyElement {
    let opened = app.clone();
    let venue = state.venue_id.clone();
    let id = row.id.clone();
    let mode = row.mode_name.clone();
    cell(W_MODE)
        .id(SharedString::from(format!("patch-mode-{}", row.id)))
        .text_size(px(12.0))
        .text_color(ladder::foreground_alpha(0.85))
        .truncate()
        .child(mode.clone())
        .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
            let venue = venue.clone();
            let id = id.clone();
            let at = event.position;
            opened.update(cx, |this, cx| this.open_patch_mode_menu(venue, id, at, cx));
            cx.stop_propagation();
        })
        .agent_node(Role::Select, format!("{name} mode = {mode}"))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------

/// The row menu: duplicate, pin, unpatch. It acts on the *selection*, which the
/// right-click already made contain this row.
pub(super) fn row_menu(
    state: &Patch,
    fixture: &str,
    at: Point<gpui::Pixels>,
    app: &Entity<Luma>,
) -> AnyElement {
    let count = state.selected.len().max(1);
    let pinned = state.row(fixture).is_some_and(|row| row.address_pinned);
    let venue = state.venue_id.clone();

    let dup_app = app.clone();
    let dup_venue = venue.clone();
    let pin_app = app.clone();
    let pin_venue = venue.clone();
    let pin_ids: Vec<String> = state
        .rows()
        .iter()
        .filter(|row| state.selected.contains(&row.id))
        .map(|row| row.id.clone())
        .collect();
    let cut_app = app.clone();
    let cut_venue = venue.clone();
    let closed = app.clone();
    let closed_venue = venue;

    ContextMenu::new("patch-row-menu", at)
        .item(
            if count == 1 {
                "Duplicate".to_string()
            } else {
                format!("Duplicate {count} fixtures")
            },
            move |_, cx| {
                let venue = dup_venue.clone();
                dup_app.update(cx, |this, cx| this.duplicate_patch_rows(venue, cx));
            },
        )
        .item(
            if pinned {
                "Unpin address"
            } else {
                "Pin address"
            },
            move |_, cx| {
                for id in &pin_ids {
                    let venue = pin_venue.clone();
                    let id = id.clone();
                    pin_app.update(cx, |this, cx| this.set_patch_pin(venue, id, !pinned, cx));
                }
            },
        )
        .separator()
        .destructive(
            if count == 1 {
                "Unpatch".to_string()
            } else {
                format!("Unpatch {count} fixtures")
            },
            move |_, cx| {
                let venue = cut_venue.clone();
                cut_app.update(cx, |this, cx| this.unpatch_selection(venue, cx));
            },
        )
        .render(move |_, cx| {
            let venue = closed_venue.clone();
            closed.update(cx, |this, cx| this.close_patch_menus(venue, cx));
        })
}

/// The mode menu: every mode the definition offers, with its width.
pub(super) fn mode_menu(
    state: &Patch,
    fixture: &str,
    at: Point<gpui::Pixels>,
    app: &Entity<Luma>,
) -> AnyElement {
    let Some(row) = state.row(fixture) else {
        return div().into_any_element();
    };
    let venue = state.venue_id.clone();
    let mut menu = ContextMenu::new("patch-mode-menu", at);
    for (mode, channels) in state.modes(row) {
        let picked = app.clone();
        let venue = venue.clone();
        let id = row.id.clone();
        let chosen = mode.clone();
        menu = menu.item(format!("{mode} · {channels} ch"), move |_, cx| {
            let venue = venue.clone();
            let id = id.clone();
            let chosen = chosen.clone();
            picked.update(cx, |this, cx| {
                // Asked without permission to move: a mode whose width no
                // longer fits comes back refused, and the refusal is the
                // question put to the operator.
                this.set_patch_mode(venue, id, chosen, false, cx);
            });
        });
    }
    let closed = app.clone();
    let closed_venue = state.venue_id.clone();
    menu.render(move |_, cx| {
        let venue = closed_venue.clone();
        closed.update(cx, |this, cx| this.close_patch_menus(venue, cx));
    })
}
