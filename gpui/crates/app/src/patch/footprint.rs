//! The patch as channels: each universe a grid of 512 square cells across the
//! page, the fixtures runs of cells on it.
//!
//! The grid is read like lines of text and takes the page's whole width, so a
//! single-channel fixture is still a cell that can be seen and picked up. How
//! many cells make a line is the page's zoom ([`super::Patch::grid_columns`]):
//! fewer across is larger cells and a taller grid.
//!
//! Collisions are `universe_occupancy`'s, verbatim — they and out-of-range are
//! facts the allocator computed, not conditions redrawn here. A grid that
//! decided for itself what overlapped would be the second occupancy rule in
//! the codebase. A fixture's run is only its own address and width.
//!
//! Dragging a run re-addresses it: the press remembers *which channel* of the
//! fixture it landed on, so a run picked up by its middle lands where the
//! pointer is, and a run carried onto another universe's grid lands there.
//! The drop calls `set_fixture_address` like any other typed address and shows
//! the same refusal, because it is the same door.

use gpui::prelude::*;
use gpui::{
    canvas, div, fill, point, px, relative, size, AnyElement, Bounds, Entity, Hsla, Pixels,
    SharedString,
};

use luma_lib::models::fixtures::PatchedFixture;
use luma_lib::models::patch::UniverseCell;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, ladder};

use super::Patch;
use crate::Luma;

/// Channels in a universe: the cells of one grid.
const CELLS: i64 = 512;
/// The space between two cells, in the grid's own colour.
const CELL_GAP: f32 = 1.0;
/// The column the rows' first addresses are written in.
const GUTTER: f32 = 30.0;

pub(super) fn universes(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let mut body = div()
        .id("venue-universes")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(18.0))
        .px(px(12.0))
        .pb(px(12.0));
    match (state.load_failed, state.data.as_ref()) {
        (true, _) => {
            body = body.child(luma_ui::plate(
                "Could not read the patch".to_string(),
                ladder::muted_foreground(),
            ));
        }
        (false, None) => body = body.child(float::empty_row("Reading the patch…")),
        (false, Some(data)) => {
            if data.universes.is_empty() {
                body = body.child(
                    float::empty_row("No universe is patched yet.")
                        .agent_node(Role::Text, "No universe is patched yet."),
                );
            }
            for universe in &data.universes {
                let cells = data.occupancy.get(universe).map_or(&[][..], Vec::as_slice);
                body = body.child(section(state, *universe, cells, app));
            }
        }
    }
    body.agent_node(Role::Card, "Patch").into_any_element()
}

/// One universe: its name, its grid, and what is wrong on it.
fn section(state: &Patch, universe: u16, cells: &[UniverseCell], app: &Entity<Luma>) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(12.5))
                .text_color(ladder::foreground())
                .child(format!("Universe {universe}")),
        )
        .child(grid(state, universe, cells, app))
        .children(collision_notes(universe, cells))
        .children(
            state
                .strip_refusal
                .as_ref()
                .filter(|(refused, _)| *refused == universe)
                .map(|(_, message)| luma_ui::notice::line(message.clone())),
        )
        .into_any_element()
}

/// The channel under a pointer at `at`, on a grid `columns` across drawn in
/// `bounds`.
fn channel_at(bounds: Bounds<Pixels>, at: gpui::Point<Pixels>, columns: i64) -> u16 {
    let rows = CELLS / columns;
    let width = f32::from(bounds.size.width).max(1.0);
    let height = f32::from(bounds.size.height).max(1.0);
    let along = (f32::from(at.x) - f32::from(bounds.origin.x)) / width;
    let down = (f32::from(at.y) - f32::from(bounds.origin.y)) / height;
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let (row, column) = (
        ((down * rows as f32).floor() as i64).clamp(0, rows - 1),
        ((along * columns as f32).floor() as i64).clamp(0, columns - 1),
    );
    u16::try_from(row * columns + column + 1).unwrap_or(1)
}

/// A run of channels as the piece of it on each row: the row, the column it
/// starts at and how many cells it covers there.
fn pieces(start: i64, channels: i64, columns: i64) -> Vec<(i64, i64, i64)> {
    let last = start + channels - 1;
    (0..CELLS / columns)
        .filter_map(|row| {
            let first = row * columns + 1;
            let from = start.max(first);
            let to = last.min(first + columns - 1);
            (from <= to).then(|| (row, from - first, to - from + 1))
        })
        .collect()
}

/// What each of the 512 cells is filled with.
fn fills(state: &Patch, universe: u16, cells: &[UniverseCell]) -> Vec<Hsla> {
    let mut fills: Vec<Hsla> = vec![ladder::trim().into(); CELLS as usize];
    let mut paint = |start: i64, channels: i64, colour: Hsla| {
        for address in start..start + channels {
            if let Some(cell) = usize::try_from(address - 1)
                .ok()
                .and_then(|index| fills.get_mut(index))
            {
                *cell = colour;
            }
        }
    };
    let lifted = state.drag.as_ref().map(|drag| drag.fixture.as_str());
    for (index, row) in state
        .rows()
        .iter()
        .filter(|row| row.universe == i64::from(universe))
        .enumerate()
    {
        let colour: Hsla = if lifted == Some(row.id.as_str()) {
            // Where it was, while it is in the hand.
            ladder::foreground_alpha(0.12)
        } else if state.selected.contains(&row.id) {
            ladder::foreground_90().into()
        } else if index % 2 == 0 {
            ladder::hover().into()
        } else {
            // Two shades down the address order, so two fixtures that touch
            // are told apart without a name on every cell.
            ladder::foreground_alpha(0.26)
        };
        paint(row.address, row.num_channels, colour);
    }
    for (start, end) in collisions(cells) {
        paint(
            i64::from(start),
            i64::from(end - start + 1),
            ladder::danger().into(),
        );
    }
    // Where the dragged run would land, so the ghost is under the pointer
    // before anything is written.
    if let Some(drag) = state.drag.as_ref().filter(|drag| drag.universe == universe) {
        if let Some(row) = state.row(&drag.fixture) {
            let start = i64::from(drag.over) - i64::from(drag.grabbed_channel);
            paint(start, row.num_channels, ladder::accent().into());
        }
    }
    fills
}

fn grid(state: &Patch, universe: u16, cells: &[UniverseCell], app: &Entity<Luma>) -> AnyElement {
    let measured = state.strips.clone();
    let fills = fills(state, universe, cells);
    let columns = state.grid_columns();
    let rows = CELLS / columns;
    // The cells are painted, not laid out: 512 squares a universe are a
    // picture, and the runs over them are what a pointer and a script meet.
    let painted = canvas(
        move |bounds, _, _| {
            measured.borrow_mut().insert(universe, bounds);
        },
        move |bounds, (), window, _| {
            #[allow(clippy::cast_precision_loss)]
            let (cell_w, cell_h) = (
                f32::from(bounds.size.width) / columns as f32,
                f32::from(bounds.size.height) / rows as f32,
            );
            for (index, colour) in fills.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let (column, row) = (
                    (index as i64 % columns) as f32,
                    (index as i64 / columns) as f32,
                );
                let cell = Bounds::new(
                    bounds.origin + point(px(column * cell_w), px(row * cell_h)),
                    size(px(cell_w - CELL_GAP), px(cell_h - CELL_GAP)),
                );
                window.paint_quad(fill(cell, *colour).corner_radii(px(2.0)));
            }
        },
    )
    .absolute()
    .inset_0();

    let moved = app.clone();
    let move_venue = state.venue_id.clone();
    let move_strips = state.strips.clone();
    let released = app.clone();
    let up_venue = state.venue_id.clone();
    let out_released = app.clone();
    let out_venue = state.venue_id.clone();

    #[allow(clippy::cast_precision_loss)]
    let mut cells_area = div()
        .id(SharedString::from(format!("universe-grid-{universe}")))
        .relative()
        .flex_1()
        .min_w_0()
        // Square cells: the grid is as tall as its width makes it.
        .aspect_ratio(columns as f32 / rows as f32)
        .child(painted);
    for row in state
        .rows()
        .iter()
        .filter(|row| row.universe == i64::from(universe))
    {
        cells_area = cells_area.children(run(state, universe, row, app));
    }
    let cells_area = cells_area
        .on_mouse_move(move |event, _, cx| {
            let Some(bounds) = move_strips.borrow().get(&universe).copied() else {
                return;
            };
            let address = channel_at(bounds, event.position, columns);
            let venue = move_venue.clone();
            moved.update(cx, |this, cx| {
                this.drag_strip_to(venue, universe, address, cx);
            });
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
            let venue = up_venue.clone();
            released.update(cx, |this, cx| this.drop_strip_block(venue, cx));
        })
        // A press that leaves the grids before it lifts is still a release.
        // Without this the run stays stuck to the pointer with nothing
        // holding it, which is the one state a drag must not be able to end in.
        .on_mouse_up_out(gpui::MouseButton::Left, move |_, _, cx| {
            let venue = out_venue.clone();
            out_released.update(cx, |this, cx| this.drop_strip_block(venue, cx));
        })
        .agent_node(Role::Card, format!("Universe {universe} channels"));

    // Each row's first address, beside it.
    let gutter = div()
        .flex_none()
        .w(px(GUTTER))
        .flex()
        .flex_col()
        .font_family(luma_ui::fonts::MONO)
        .text_size(px(10.0))
        .text_color(ladder::foreground_alpha(0.4))
        .children((0..rows).map(|row| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .child((row * columns + 1).to_string())
        }));
    div()
        .w_full()
        .flex()
        .child(gutter)
        .child(cells_area)
        .into_any_element()
}

/// One fixture over its cells: a piece per row it crosses, the first of them
/// carrying its name. The cells under it are what is drawn; this is what is
/// read, pressed and found.
fn run(state: &Patch, universe: u16, row: &PatchedFixture, app: &Entity<Luma>) -> Vec<AnyElement> {
    let name: SharedString = row
        .label
        .clone()
        .unwrap_or_else(|| row.model.clone())
        .into();
    let selected = state.selected.contains(&row.id);
    let last = row.address + row.num_channels - 1;
    let columns = state.grid_columns();
    let rows = CELLS / columns;

    pieces(row.address, row.num_channels, columns)
        .into_iter()
        .enumerate()
        .map(|(index, (line, column, width))| {
            let grabbed = app.clone();
            let venue = state.venue_id.clone();
            let id = row.id.clone();
            let strips = state.strips.clone();
            #[allow(clippy::cast_precision_loss)]
            let part = div()
                .id(SharedString::from(format!("grid-run-{}-{line}", row.id)))
                .absolute()
                .top(relative(line as f32 / rows as f32))
                .h(relative(1.0 / rows as f32))
                .left(relative(column as f32 / columns as f32))
                .w(relative(width as f32 / columns as f32))
                .overflow_hidden()
                .pl(px(4.0))
                .pr(px(CELL_GAP))
                .pb(px(CELL_GAP))
                .flex()
                .items_center()
                .gap(px(3.0))
                .text_size(px(10.5))
                .text_color(if selected {
                    ladder::background()
                } else {
                    ladder::foreground()
                })
                .cursor_grab()
                .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
                    let Some(bounds) = strips.borrow().get(&universe).copied() else {
                        return;
                    };
                    let address = channel_at(bounds, event.position, columns);
                    let venue = venue.clone();
                    let id = id.clone();
                    grabbed.update(cx, |this, cx| {
                        this.grab_strip_block(venue, id, universe, address, cx);
                    });
                });
            if index > 0 {
                return part.into_any_element();
            }
            // A hand-set address carries a dot before its name, so it reads on
            // the grid as it does in the table: Auto patch would move it.
            part.when(row.address_pinned, |part| {
                part.child(
                    div()
                        .flex_none()
                        .size(px(5.0))
                        .rounded_full()
                        .bg(ladder::accent()),
                )
            })
            .child(div().truncate().child(name.clone()))
            .agent_node(
                Role::Button,
                format!("{name} at {universe}:{}–{last}", row.address),
            )
            .agent_focused(selected)
            .into_any_element()
        })
        .collect()
}

/// Each contiguous run of colliding channels, first and last address.
fn collisions(cells: &[UniverseCell]) -> Vec<(u16, u16)> {
    let mut runs: Vec<(u16, u16)> = Vec::new();
    let mut colliding: Vec<u16> = cells
        .iter()
        .filter(|cell| cell.collision)
        .map(|cell| cell.address)
        .collect();
    colliding.sort_unstable();
    for address in colliding {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 >= address => *end = address,
            _ => runs.push((address, address)),
        }
    }
    runs
}

/// One line per contiguous run of colliding channels.
///
/// Per run rather than per cell: 512 automation nodes would drown the tree, and
/// "addresses 9 to 16 are claimed twice" is the sentence an operator needs —
/// eight identical ones are not.
///
/// `universe:address`, because `1.5` reads as a number and `1:5` reads as an
/// address — and `1.5–12` reads as neither.
fn collision_notes(universe: u16, cells: &[UniverseCell]) -> Vec<AnyElement> {
    collisions(cells)
        .into_iter()
        .map(|(start, end)| {
            let text = if start == end {
                format!("Collision at {universe}:{start}")
            } else {
                format!("Collision at {universe}:{start}–{end}")
            };
            luma_ui::notice::line(text).into_any_element()
        })
        .collect()
}
