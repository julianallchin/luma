//! One universe as 512 cells.
//!
//! The cells are `universe_occupancy`'s, verbatim — collision and out-of-range
//! are both facts the allocator computed, not conditions redrawn here. A
//! footprint strip that decided for itself what overlapped would be the second
//! occupancy rule in the codebase.
//!
//! Dragging a block re-addresses it: the press remembers *which channel* of the
//! fixture it landed on, so a block picked up by its middle lands where the
//! pointer is. The drop calls `set_fixture_address` like any other typed
//! address and shows the same refusal, because it is the same door.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, SharedString};

use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use super::Patch;
use crate::Luma;

/// 32 across, 16 down — the shape a DMX universe is always drawn in.
const COLUMNS: u16 = 32;
const CELL: f32 = 8.0;
const GAP: f32 = 1.0;

pub(super) fn footprint(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    super::section(
        "FOOTPRINT",
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(12.0))
            .pb(px(12.0))
            .child(selector(state, app))
            .child(grid(state, app))
            .children(collision_notes(state))
            .children(state.strip_refusal.as_ref().map(|message| {
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::danger())
                    .child(message.clone())
                    .agent_node(Role::Text, message.clone())
            }))
            .into_any_element(),
    )
    .flex_shrink_0()
    .into_any_element()
}

/// A chip per universe in use. A row rather than a dropdown: a venue runs one
/// universe per truss, so the whole list is three or four chips wide and
/// hiding it behind a trigger would cost a click to read a fact.
fn selector(state: &Patch, app: &Entity<Luma>) -> impl IntoElement {
    let universes = state
        .data
        .as_ref()
        .map(|data| data.universes.clone())
        .unwrap_or_default();
    let universes = if universes.is_empty() {
        vec![state.strip_universe]
    } else {
        universes
    };
    div()
        .flex()
        .flex_wrap()
        .gap(px(4.0))
        .children(universes.into_iter().map(|universe| {
            let shown = universe == state.strip_universe;
            let picked = app.clone();
            let venue = state.venue_id.clone();
            luma_ui::luma_toggle(&format!("U{universe}"), shown)
                .id(SharedString::from(format!("patch-universe-{universe}")))
                .on_click(move |_, _, cx| {
                    let venue = venue.clone();
                    picked.update(cx, |this, cx| this.show_universe(venue, universe, cx));
                })
                .agent_node(Role::Toggle, format!("Universe {universe}"))
                .agent_focused(shown)
        }))
}

fn grid(state: &Patch, app: &Entity<Luma>) -> impl IntoElement {
    let dragged = state.drag.as_ref();
    let released = app.clone();
    let venue_up = state.venue_id.clone();
    div()
        .id("patch-footprint")
        .flex()
        .flex_wrap()
        .w(px(f32::from(COLUMNS) * (CELL + GAP)))
        .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
            let venue = venue_up.clone();
            released.update(cx, |this, cx| this.drop_strip_block(venue, cx));
        })
        .children((1..=512u16).map(|address| {
            let cell = state.cells.iter().find(|cell| cell.address == address);
            let occupied = cell.and_then(|cell| cell.fixture_id.as_ref());
            let collided = cell.is_some_and(|cell| cell.collision);
            let selected = occupied.is_some_and(|id| state.selected.contains(id));
            // Where the dragged block would land, so the ghost is under the
            // pointer before anything is written.
            let ghosted = dragged.is_some_and(|drag| {
                state.row(&drag.fixture).is_some_and(|row| {
                    let start = i32::from(drag.over) - i32::from(drag.grabbed_channel);
                    let offset = i32::from(address) - start;
                    (0..i32::try_from(row.num_channels).unwrap_or(0)).contains(&offset)
                })
            });
            let pressed = app.clone();
            let entered = app.clone();
            let venue_down = state.venue_id.clone();
            let venue_move = state.venue_id.clone();
            div()
                .id(SharedString::from(format!("footprint-{address}")))
                .w(px(CELL))
                .h(px(CELL))
                .m(px(GAP / 2.0))
                .bg(if collided {
                    ladder::danger()
                } else if ghosted {
                    ladder::accent()
                } else if selected {
                    ladder::foreground_90()
                } else if occupied.is_some() {
                    ladder::apex()
                } else {
                    ladder::trim()
                })
                .when(cell.is_some_and(|cell| cell.pinned), |cell| {
                    cell.border_1().border_color(ladder::accent())
                })
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    let venue = venue_down.clone();
                    pressed.update(cx, |this, cx| this.grab_strip_cell(venue, address, cx));
                })
                .on_mouse_move(move |_, _, cx| {
                    let venue = venue_move.clone();
                    entered.update(cx, |this, cx| this.drag_strip_to(venue, address, cx));
                })
        }))
}

/// One line per contiguous run of colliding channels.
///
/// Per run rather than per cell: 512 automation nodes would drown the tree, and
/// "addresses 9 to 16 are claimed twice" is the sentence an operator needs —
/// eight identical ones are not.
fn collision_notes(state: &Patch) -> Vec<AnyElement> {
    let mut notes = Vec::new();
    let mut run: Option<(u16, u16)> = None;
    for address in 1..=512u16 {
        let collided = state
            .cells
            .iter()
            .any(|cell| cell.address == address && cell.collision);
        match (collided, run) {
            (true, None) => run = Some((address, address)),
            (true, Some((start, _))) => run = Some((start, address)),
            (false, Some((start, end))) => {
                notes.push(collision_note(state.strip_universe, start, end));
                run = None;
            }
            (false, None) => {}
        }
    }
    if let Some((start, end)) = run {
        notes.push(collision_note(state.strip_universe, start, end));
    }
    notes
}

fn collision_note(universe: u16, start: u16, end: u16) -> AnyElement {
    let text = if start == end {
        format!("Collision at {universe}.{start}")
    } else {
        format!("Collision at {universe}.{start}–{end}")
    };
    div()
        .text_size(px(11.0))
        .text_color(ladder::danger())
        .child(text.clone())
        .agent_node(Role::Text, text)
        .into_any_element()
}
