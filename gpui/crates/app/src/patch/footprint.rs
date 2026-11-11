//! The patch as channels: each universe a strip of 512 across the page, the
//! fixtures blocks on it, and where the universe comes out beside its name.
//!
//! The strip is four lanes of 128, read like lines of text. One lane of 512
//! leaves a single-channel fixture two pixels wide; four keep the page's whole
//! width and make it something that can be read and picked up.
//!
//! Collisions are `universe_occupancy`'s, verbatim — they and out-of-range are
//! facts the allocator computed, not conditions redrawn here. A strip that
//! decided for itself what overlapped would be the second occupancy rule in
//! the codebase. A block is only its fixture's own address and width.
//!
//! Dragging a block re-addresses it: the press remembers *which channel* of the
//! fixture it landed on, so a block picked up by its middle lands where the
//! pointer is, and a block carried onto another universe's strip lands there.
//! The drop calls `set_fixture_address` like any other typed address and shows
//! the same refusal, because it is the same door.

use gpui::prelude::*;
use gpui::{canvas, div, px, relative, AnyElement, Bounds, Entity, Pixels, SharedString};

use luma_lib::models::fixtures::PatchedFixture;
use luma_lib::models::patch::UniverseCell;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, glass, ladder};

use super::Patch;
use crate::Luma;

/// Channels to a lane, and lanes to a universe.
const LANE_CHANNELS: i64 = 128;
const LANES: i64 = 4;
const LANE_HEIGHT: f32 = 26.0;
const LANE_GAP: f32 = 2.0;
/// The column the lanes' first addresses are written in.
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
            body = body.child(super::outputs::found(state));
        }
    }
    body.agent_node(Role::Card, "Patch").into_any_element()
}

/// One universe: where it comes out, its strip, and what is wrong on it.
fn section(state: &Patch, universe: u16, cells: &[UniverseCell], app: &Entity<Luma>) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(super::outputs::row(state, i64::from(universe), app))
        .child(strip(state, universe, cells, app))
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

/// The channel under a pointer at `at`, on lanes drawn in `bounds`.
fn channel_at(bounds: Bounds<Pixels>, at: gpui::Point<Pixels>) -> u16 {
    let width = f32::from(bounds.size.width).max(1.0);
    let height = f32::from(bounds.size.height).max(1.0);
    let along = (f32::from(at.x) - f32::from(bounds.origin.x)) / width;
    let down = (f32::from(at.y) - f32::from(bounds.origin.y)) / height;
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let (lane, channel) = (
        ((down * LANES as f32).floor() as i64).clamp(0, LANES - 1),
        ((along * LANE_CHANNELS as f32).floor() as i64).clamp(0, LANE_CHANNELS - 1),
    );
    u16::try_from(lane * LANE_CHANNELS + channel + 1).unwrap_or(1)
}

/// A run of channels as the piece of it on each lane: the lane, and where the
/// piece starts and how wide it is as fractions of the lane.
fn pieces(start: i64, channels: i64) -> Vec<(i64, f32, f32)> {
    let last = start + channels - 1;
    (0..LANES)
        .filter_map(|lane| {
            let first = lane * LANE_CHANNELS + 1;
            let from = start.max(first);
            let to = last.min(first + LANE_CHANNELS - 1);
            #[allow(clippy::cast_precision_loss)]
            (from <= to).then(|| {
                (
                    lane,
                    (from - first) as f32 / LANE_CHANNELS as f32,
                    (to - from + 1) as f32 / LANE_CHANNELS as f32,
                )
            })
        })
        .collect()
}

/// A piece's box on its lane.
fn piece(lane: i64, left: f32, width: f32) -> gpui::Div {
    #[allow(clippy::cast_precision_loss)]
    let top = lane as f32 * (LANE_HEIGHT + LANE_GAP);
    div()
        .absolute()
        .top(px(top))
        .h(px(LANE_HEIGHT))
        .left(relative(left))
        .w(relative(width))
}

fn strip(state: &Patch, universe: u16, cells: &[UniverseCell], app: &Entity<Luma>) -> AnyElement {
    let measured = state.strips.clone();
    let measure = canvas(
        move |bounds, _, _| {
            measured.borrow_mut().insert(universe, bounds);
        },
        |_, (), _, _| {},
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
    let height = LANES as f32 * LANE_HEIGHT + (LANES - 1) as f32 * LANE_GAP;
    let mut lanes = div()
        .id(SharedString::from(format!("universe-strip-{universe}")))
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(height))
        .child(measure);
    for lane in 0..LANES {
        lanes = lanes.child(piece(lane, 0.0, 1.0).rounded(px(4.0)).bg(ladder::trim()));
        // A rule every 16 channels: enough to read an address off the lane
        // without a grid of 512 cells.
        for eighth in 1..8u8 {
            lanes = lanes.child(
                piece(lane, f32::from(eighth) / 8.0, 0.0)
                    .w(px(1.0))
                    .bg(glass::hairline(0.08)),
            );
        }
    }
    for row in state
        .rows()
        .iter()
        .filter(|row| row.universe == i64::from(universe))
    {
        lanes = lanes.children(block(state, universe, row, app));
    }
    for (start, end) in collisions(cells) {
        for (lane, left, width) in pieces(i64::from(start), i64::from(end - start + 1)) {
            lanes = lanes.child(piece(lane, left, width).bg(ladder::danger()).opacity(0.75));
        }
    }
    // Where the dragged block would land, so the ghost is under the pointer
    // before anything is written.
    if let Some(drag) = state.drag.as_ref().filter(|drag| drag.universe == universe) {
        if let Some(row) = state.row(&drag.fixture) {
            let start = i64::from(drag.over) - i64::from(drag.grabbed_channel);
            for (lane, left, width) in pieces(start, row.num_channels) {
                lanes = lanes.child(
                    piece(lane, left, width)
                        .rounded(px(4.0))
                        .bg(ladder::accent())
                        .opacity(0.7),
                );
            }
        }
    }
    let lanes = lanes
        .on_mouse_move(move |event, _, cx| {
            let Some(bounds) = move_strips.borrow().get(&universe).copied() else {
                return;
            };
            let address = channel_at(bounds, event.position);
            let venue = move_venue.clone();
            moved.update(cx, |this, cx| {
                this.drag_strip_to(venue, universe, address, cx);
            });
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
            let venue = up_venue.clone();
            released.update(cx, |this, cx| this.drop_strip_block(venue, cx));
        })
        // A press that leaves the strips before it lifts is still a release.
        // Without this the block stays stuck to the pointer with nothing
        // holding it, which is the one state a drag must not be able to end in.
        .on_mouse_up_out(gpui::MouseButton::Left, move |_, _, cx| {
            let venue = out_venue.clone();
            out_released.update(cx, |this, cx| this.drop_strip_block(venue, cx));
        })
        .agent_node(Role::Card, format!("Universe {universe} channels"));

    // Each lane's first address, beside it.
    let gutter = div()
        .flex_none()
        .w(px(GUTTER))
        .h(px(height))
        .flex()
        .flex_col()
        .gap(px(LANE_GAP))
        .font_family(luma_ui::fonts::MONO)
        .text_size(px(10.0))
        .text_color(ladder::foreground_alpha(0.4))
        .children((0..LANES).map(|lane| {
            div()
                .h(px(LANE_HEIGHT))
                .flex()
                .items_center()
                .child((lane * LANE_CHANNELS + 1).to_string())
        }));
    div()
        .w_full()
        .flex()
        .child(gutter)
        .child(lanes)
        .into_any_element()
}

/// One fixture on its universe's strip, as wide as its channels: one piece
/// per lane it crosses, the first of them carrying its name.
fn block(
    state: &Patch,
    universe: u16,
    row: &PatchedFixture,
    app: &Entity<Luma>,
) -> Vec<AnyElement> {
    let name: SharedString = row
        .label
        .clone()
        .unwrap_or_else(|| row.model.clone())
        .into();
    let selected = state.selected.contains(&row.id);
    let lifted = state
        .drag
        .as_ref()
        .is_some_and(|drag| drag.fixture == row.id);
    let last = row.address + row.num_channels - 1;

    pieces(row.address, row.num_channels)
        .into_iter()
        .enumerate()
        .map(|(index, (lane, left, width))| {
            let grabbed = app.clone();
            let venue = state.venue_id.clone();
            let id = row.id.clone();
            let strips = state.strips.clone();
            let part = piece(lane, left, width)
                .id(SharedString::from(format!("strip-block-{}-{lane}", row.id)))
                .rounded(px(4.0))
                .overflow_hidden()
                .px(px(4.0))
                .flex()
                .items_center()
                // The gap between two blocks that touch is the lane showing
                // through this edge.
                .border_1()
                .border_color(if row.address_pinned {
                    ladder::accent().into()
                } else {
                    gpui::Hsla::from(ladder::trim())
                })
                .bg(if selected {
                    ladder::foreground_90()
                } else {
                    ladder::hover()
                })
                .text_size(px(10.5))
                .text_color(if selected {
                    ladder::background()
                } else {
                    ladder::foreground()
                })
                .when(lifted, |block| block.opacity(0.4))
                .cursor_grab()
                .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
                    let Some(bounds) = strips.borrow().get(&universe).copied() else {
                        return;
                    };
                    let address = channel_at(bounds, event.position);
                    let venue = venue.clone();
                    let id = id.clone();
                    grabbed.update(cx, |this, cx| {
                        this.grab_strip_block(venue, id, universe, address, cx);
                    });
                });
            if index > 0 {
                return part.into_any_element();
            }
            part.child(div().truncate().child(name.clone()))
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
