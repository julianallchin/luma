//! The fixture table: one row per fixture, the facts about it a person may
//! change here, and the groups it is in.
//!
//! Label, universe, address and mode are the cells that write. Not position —
//! there is no cell for one, which is the point (gauntlet AF8): the columns
//! *are* the closed list of what the patch page writes.
//!
//! A refused address does not move its row. The edited cell reverts to the
//! stored value the instant the allocator says no, and the sentence it said sits
//! under the row until the next edit on it — so "what did I type" and "what is
//! stored" are never the same pixel telling two stories.
//!
//! A click on a row selects it and nothing else: command toggles one row,
//! shift takes the run from the last one clicked. The stage, the footprint and
//! the groups panel read and write the same selection.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Div, Entity, Point, SharedString};
use luma_ui::motion::FadeHover;

use luma_lib::models::fixtures::PatchedFixture;
use luma_ui::ladder;
use luma_ui::menu::ContextMenu;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, glass};

use super::{Column, Patch, Pick};
use crate::Luma;

/// Column widths. Fixed for the numbers so a table of forty rows reads as a
/// column of numbers rather than forty independently-sized cells; flexible for
/// the name and the groups.
const W_MODEL: f32 = 132.0;
const W_MODE: f32 = 96.0;
const W_NUMBER: f32 = 56.0;
const W_RANGE: f32 = 72.0;
const ROW_HEIGHT: f32 = 34.0;

/// The narrowest the seven columns fit in: the fixed widths, both flexible
/// columns at their minimum, the gaps between them and the row's own inset.
/// Below this the table scrolls sideways rather than squeezing — a column of
/// addresses that has been compressed into an ellipsis is a column that lies.
const MIN_WIDTH: f32 = W_MODEL
    + W_MODE
    + 2.0 * W_NUMBER
    + W_RANGE
    + 2.0 * NAME_MIN_WIDTH
    + 6.0 * COLUMN_GAP
    + 2.0 * ROW_INSET;
const NAME_MIN_WIDTH: f32 = 120.0;
/// How many of a fixture's groups its row names before it counts the rest.
const SHOWN_GROUPS: usize = 3;
const COLUMN_GAP: f32 = 10.0;
const ROW_INSET: f32 = 12.0;

pub(super) fn table(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let body: AnyElement = match (state.load_failed, state.data.as_ref()) {
        (true, _) => luma_ui::plate(
            "Could not read the fixtures".to_string(),
            ladder::muted_foreground(),
        )
        .into_any_element(),
        (false, None) => float::empty_row("Loading fixtures…").into_any_element(),
        (false, Some(data)) if data.fixtures.is_empty() => {
            float::empty_row("No fixtures yet.").into_any_element()
        }
        (false, Some(_)) => rows(state, app),
    };
    div()
        .id("venue-fixture-table")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_x_scroll()
        .flex()
        .flex_col()
        .child(head())
        .child(body)
        .agent_node(Role::Card, "Fixture table")
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
        .child(cell(W_NUMBER).child(float::label("Universe")))
        .child(cell(W_NUMBER).child(float::label("Address")))
        .child(cell(W_RANGE).child(float::label("Channels")))
        .child(cell_flex().child(float::label("Groups")))
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

fn rows(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    div()
        .id("venue-fixture-rows")
        .flex_1()
        .min_h_0()
        .min_w(px(MIN_WIDTH))
        .overflow_y_scroll()
        .children(state.rows().iter().map(|row| fixture_row(state, row, app)))
        .into_any_element()
}

fn fixture_row(state: &Patch, row: &PatchedFixture, app: &Entity<Luma>) -> AnyElement {
    let name: SharedString = row
        .label
        .clone()
        .unwrap_or_else(|| row.model.clone())
        .into();
    let selected = state.selected.contains(&row.id);
    let last = row.address + row.num_channels - 1;
    let editing = state.editing.as_ref().filter(|edit| edit.fixture == row.id);

    let picked = app.clone();
    let menued = app.clone();
    let venue_pick = state.venue_id.clone();
    let venue_menu = state.venue_id.clone();
    let id_pick = row.id.clone();
    let id_menu = row.id.clone();

    let line = div()
        .id(SharedString::from(format!("venue-light-{}", row.id)))
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
            glass::wash(0.0)
        })
        .when(!selected, |line| {
            line.hover(|style| style.bg(glass::glass_hover()))
                .fade_hover()
        })
        .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
            let pick = if event.modifiers.shift {
                Pick::Through
            } else if event.modifiers.secondary() {
                Pick::Toggle
            } else {
                Pick::Only
            };
            let venue = venue_pick.clone();
            let id = id_pick.clone();
            picked.update(cx, |this, cx| this.pick_patch_row(venue, id, pick, cx));
        })
        .on_mouse_down(gpui::MouseButton::Right, move |event, _, cx| {
            let venue = venue_menu.clone();
            let id = id_menu.clone();
            let at = event.position;
            menued.update(cx, |this, cx| this.open_patch_menu(venue, id, at, cx));
        })
        .child(label_cell(state, row, &name, editing, app))
        .child(
            cell(W_MODEL)
                .text_size(px(12.0))
                .text_color(ladder::foreground_alpha(0.62))
                .truncate()
                .child(format!("{} {}", row.manufacturer, row.model)),
        )
        .child(mode_cell(state, row, &name, app, W_MODE));
    // A fixture DMX does not drive (a laser projector) has no address to
    // show or set.
    let line = if row.num_channels == 0 {
        line.child(reading(W_NUMBER, "–".into()))
            .child(reading(W_NUMBER, "–".into()))
            .child(
                reading(W_RANGE, "–".into())
                    .agent_node(Role::Text, format!("{name} has no DMX address")),
            )
    } else {
        line.child(number_cell(
            state,
            row,
            &name,
            Column::Universe,
            row.universe,
            W_NUMBER,
            editing,
            app,
        ))
        .child(number_cell(
            state,
            row,
            &name,
            Column::Address,
            row.address,
            W_NUMBER,
            editing,
            app,
        ))
        .child(
            reading(W_RANGE, format!("{}–{last}", row.address))
                .agent_node(Role::Text, format!("{name} range = {}–{last}", row.address)),
        )
    };
    let line = line
        .child(groups_cell(state, row, &name, app))
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
                    .px(px(ROW_INSET))
                    .py(px(6.0))
                    .child(luma_ui::notice::line(refusal.message.clone())),
            )
            .into_any_element(),
    }
}

/// The groups a fixture is in, each one a way to select the rest of it.
fn groups_cell(
    state: &Patch,
    row: &PatchedFixture,
    name: &SharedString,
    app: &Entity<Luma>,
) -> AnyElement {
    let groups = state.groups_of(&row.id);
    // A group's name is snake_case because a selection is typed by name; a
    // table is read. Underscores become spaces for the eye only.
    let names: Vec<String> = groups
        .iter()
        .map(|group| group.name.replace('_', " "))
        .collect();
    let said = if names.is_empty() {
        "—".to_string()
    } else {
        names.join(" · ")
    };
    let mut cell = cell_flex().flex().items_center().gap(px(4.0));
    if groups.is_empty() {
        cell = cell.child(
            div()
                .text_size(px(12.0))
                .text_color(ladder::foreground_alpha(0.35))
                .child("—"),
        );
    }
    // What fits at the column's narrowest. The rest are a count, so a
    // fixture in a dozen groups does not push its row sideways.
    let hidden = groups.len().saturating_sub(SHOWN_GROUPS);
    for (group, shown) in groups.iter().zip(names).take(SHOWN_GROUPS) {
        let picked = app.clone();
        let id = group.id.clone();
        cell = cell.child(
            float::tag(shown)
                .id(SharedString::from(format!(
                    "fixture-group-{}-{}",
                    row.id, group.id
                )))
                .cursor_pointer()
                .hover(|style| style.bg(glass::glass_hover()))
                .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
                    let extend = event.modifiers.secondary() || event.modifiers.shift;
                    picked.update(cx, |this, cx| this.select_venue_group(&id, extend, cx));
                    cx.stop_propagation();
                }),
        );
    }
    if hidden > 0 {
        cell =
            cell.child(float::tag(format!("+{hidden}")).text_color(ladder::foreground_alpha(0.5)));
    }
    cell.agent_node(Role::Text, format!("{name} group = {said}"))
        .into_any_element()
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
    let sole = state.selected.len() == 1 && state.selected.contains(&row.id);
    cell_flex()
        .id(SharedString::from(format!("patch-label-{}", row.id)))
        .text_size(px(13.0))
        .text_color(ladder::foreground())
        .truncate()
        .child(name.clone())
        .when(row.address_pinned, |cell| cell.text_color(ladder::accent()))
        // The first click on a row selects it; a click on the name of the one
        // selected row edits it. A name that opened under every click would
        // make a row impossible to pick up by its widest cell.
        .when(sole, |cell| {
            cell.on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
                if event.modifiers.shift || event.modifiers.secondary() {
                    return;
                }
                let venue = venue.clone();
                let id = id.clone();
                opened.update(cx, |this, cx| {
                    this.edit_patch_cell(venue, id, Column::Label, window, cx);
                });
                cx.stop_propagation();
            })
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

/// The row's mode, marked when Luma cannot light the fixture in it.
fn mode_text(state: &Patch, row: &PatchedFixture) -> String {
    if state.supported(row) {
        row.mode_name.clone()
    } else {
        format!("{} · unsupported", row.mode_name)
    }
}

fn mode_colour(state: &Patch, row: &PatchedFixture) -> gpui::Hsla {
    if state.supported(row) {
        ladder::foreground_alpha(0.85)
    } else {
        ladder::danger().into()
    }
}

fn mode_cell(
    state: &Patch,
    row: &PatchedFixture,
    name: &SharedString,
    app: &Entity<Luma>,
    width: f32,
) -> AnyElement {
    let opened = app.clone();
    let venue = state.venue_id.clone();
    let id = row.id.clone();
    let mode = mode_text(state, row);
    cell(width)
        .id(SharedString::from(format!("patch-mode-{}", row.id)))
        .text_size(px(12.0))
        .text_color(mode_colour(state, row))
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
    for (mode, channels, supported) in state.modes(row) {
        let picked = app.clone();
        let venue = venue.clone();
        let id = row.id.clone();
        let chosen = mode.clone();
        let label = if supported {
            format!("{mode} · {channels} ch")
        } else {
            format!("{mode} · {channels} ch · unsupported")
        };
        menu = menu.item(label, move |_, cx| {
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
