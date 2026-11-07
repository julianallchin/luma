//! The grid: the page tabs, the gutter of rows that play one at a time, and
//! the page's cells, played in Perform and arranged in Setup.
//!
//! The grid fills the room it has: a cell's place is a fraction of the
//! page, so a page of four rows on a tall window gets tall cells. What a
//! pattern cell shows follows its shape. A single place is its pad alone; a
//! taller cell stands its level and its graph's other faders and buttons up
//! under the pad, and a wider one lays them along it. A pattern with no pad
//! shows the same face, which nothing presses: it plays whenever the set is
//! open.

use std::cell::Cell as Shared;

use gpui::prelude::*;
use gpui::{canvas, div, px, relative, AnyElement, Axis, Entity, Hsla, MouseButton, SharedString};
use luma_lib::models::perform::{Cell, CellContent, LayoutPage, PerformLayout};
use luma_patterns::clip_graph::LiveInputKind;
use luma_patterns::press::PressMode;
use luma_ui::float::{self, RowState};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{glass, ladder, radius};

use super::surface::{self, Edit, Rect, View};
use super::{group_label, Named, Part, PartKind, Perform};
use crate::workspace::Target;
use crate::Luma;

/// The air between two cells.
const GAP: f32 = 10.;
/// The gutter's width: Setup's one-at-a-time switches, or Perform's
/// brackets.
const GUTTER_SETUP: f32 = 42.;
const GUTTER_PERFORM: f32 = 14.;
/// A horizontal fader's row.
const ROW: f32 = 22.;

/// The page tabs and the page.
pub(super) fn surface(state: &Perform, target: &Target, app: &Entity<Luma>) -> AnyElement {
    let Some((layout, page)) = state
        .layout
        .as_ref()
        .and_then(|layout| Some((layout, state.surface.page(layout)?)))
    else {
        return div()
            .text_size(px(12.))
            .text_color(ladder::foreground_alpha(0.45))
            .child("Reading the Perform set…")
            .into_any_element();
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(tabs(state, layout, page, target, app))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(gutter(state, page, target, app))
                .child(grid(state, page, target, app)),
        )
        .when(state.moving(), |surface| {
            // A button's envelope is played here, so draw every frame until
            // each settles.
            surface.child(
                canvas(
                    |_, _, _| (),
                    |_, (), window, _| window.request_animation_frame(),
                )
                .absolute()
                .size_0(),
            )
        })
        .into_any_element()
}

fn on_edit(
    target: &Target,
    app: &Entity<Luma>,
    edit: Edit,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    let (app, target) = (app.clone(), target.clone());
    move |_, _, cx| {
        app.update(cx, |this, cx| this.edit_surface(&target, edit.clone(), cx));
    }
}

/// The pages, a "+" for another in Setup, and a line about the page.
fn tabs(
    state: &Perform,
    layout: &PerformLayout,
    page: &LayoutPage,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let setup = state.surface.view == View::Setup;
    let tabs = layout.pages.iter().enumerate().map(|(index, one)| {
        let renaming = state
            .draft
            .as_ref()
            .filter(|draft| draft.of == Named::Page(one.id.clone()));
        if let Some(draft) = renaming {
            return super::draft_field(draft, "Page name", px(140.).into()).into_any_element();
        }
        let chosen = one.id == page.id;
        let key = format!("perform-page-{}", one.id);
        let (app, target, id) = (app.clone(), target.clone(), one.id.clone());
        float::nav_row(RowState::of(chosen, false), key.clone())
            .id(SharedString::from(key))
            .px(px(14.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(ladder::foreground_alpha(if chosen { 1. } else { 0.55 }))
            .child(one.name.clone())
            .on_click(move |event, window, cx| {
                let id = id.clone();
                app.update(cx, |this, cx| {
                    if setup && event.click_count() == 2 {
                        this.start_draft(&target, Named::Page(id), window, cx);
                    } else {
                        this.edit_surface(&target, Edit::Page(index), cx);
                    }
                });
            })
            .agent_node(Role::Toggle, format!("Page {}", one.name))
            .agent_focused(chosen)
            .into_any_element()
    });
    let note = if setup {
        "Double-click a page to rename it · drag a cell to move it, its corner to resize it"
            .to_owned()
    } else {
        let on = page
            .cells
            .iter()
            .filter_map(Cell::pattern_id)
            .filter(|id| state.pad(id).is_some_and(|pad| state.on(&pad)))
            .count();
        format!("{} · {on} on", page.name)
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(4.))
        .children(tabs)
        .when(setup, |row| {
            row.child(
                float::btn("+", "perform-page-add")
                    .id("perform-page-add")
                    .text_size(px(16.))
                    .on_click(on_edit(target, app, Edit::AddPage))
                    .agent_node(Role::Button, "Add page"),
            )
        })
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.45))
                .child(note.clone())
                .agent_node(Role::Text, note),
        )
}

/// Where a cell of `page` covering `rect` sits: a fraction of the grid, its
/// half of the gap all round.
fn place(rect: Rect, page: &LayoutPage) -> gpui::Div {
    let (columns, rows) = (f32::from(page.columns), f32::from(page.rows));
    div()
        .absolute()
        .left(relative(f32::from(rect.column) / columns))
        .top(relative(f32::from(rect.row) / rows))
        .w(relative(f32::from(rect.width) / columns))
        .h(relative(f32::from(rect.height) / rows))
        .p(px(GAP / 2.))
}

/// One mark per row: Setup's switch, or Perform's bracket on a row that
/// plays one at a time.
fn gutter(
    state: &Perform,
    page: &LayoutPage,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let setup = state.surface.view == View::Setup;
    let rows = f32::from(page.rows);
    let marks = (0..page.rows).map(|row| {
        let on = page.one_at_a_time.contains(&row);
        let slot = div()
            .absolute()
            .left_0()
            .w_full()
            .top(relative(f32::from(row) / rows))
            .h(relative(1. / rows))
            .py(px(GAP / 2.));
        let tip = "One at a time: a pattern of this row turns the row's others off";
        if setup {
            let key = format!("perform-one-at-a-time-{row}");
            slot.child(
                div()
                    .id(SharedString::from(key))
                    .pt(px(10.))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .child(float::switch(if on { 1. } else { 0. }))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
                    })
                    .on_click(on_edit(target, app, Edit::OneAtATime(row)))
                    .agent_node(Role::Toggle, format!("One at a time, row {}", row + 1))
                    .agent_focused(on),
            )
        } else {
            slot.when(on, |slot| {
                slot.child(
                    div()
                        .id(SharedString::from(format!("perform-bracket-{row}")))
                        .size_full()
                        .py(px(8.))
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .w(px(6.))
                                .h_full()
                                .border_l_2()
                                .border_t_2()
                                .border_b_2()
                                .rounded_l(px(4.))
                                .border_color(Hsla::from(ladder::accent()).opacity(0.5)),
                        )
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip).build(window, cx)
                        })
                        .agent_node(Role::Text, format!("Row {} plays one at a time", row + 1)),
                )
            })
        }
    });
    div()
        .relative()
        .flex_none()
        .h_full()
        .w(px(if setup { GUTTER_SETUP } else { GUTTER_PERFORM }))
        .children(marks)
}

/// A cell being dragged: which one, and whether by its corner (resize) or
/// its body (move). `grab` is the place within the cell it was picked up
/// by, learned at the drag's first move.
struct CellDrag {
    id: String,
    resize: bool,
    from: Rect,
    grab: Shared<Option<(u16, u16)>>,
}

struct Ghost;

impl Render for Ghost {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The page's cells, and in Setup a "+" in every free place.
fn grid(
    state: &Perform,
    page: &LayoutPage,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let setup = state.surface.view == View::Setup;
    let slots = setup.then(|| {
        (0..page.rows)
            .flat_map(|row| (0..page.columns).map(move |column| (column, row)))
            .filter(|(column, row)| !surface::covered(page, *column, *row))
            .map(|(column, row)| slot(state, page, column, row, target, app))
            .collect::<Vec<_>>()
    });
    let cells = page.cells.iter().map(|cell| {
        let preview = state
            .surface
            .preview
            .as_ref()
            .filter(|(id, _)| *id == cell.id)
            .map(|(_, rect)| *rect);
        let rect = preview.unwrap_or_else(|| Rect::of(cell));
        let body = if setup {
            let fits = preview.is_none_or(|rect| {
                page.misfit(&Cell {
                    column: rect.column,
                    row: rect.row,
                    width: rect.width,
                    height: rect.height,
                    ..cell.clone()
                })
                .is_none()
            });
            setup_cell(state, cell, rect, fits, target, app)
        } else {
            face(state, cell, rect, Some((target, app)))
        };
        place(rect, page).child(body)
    });
    let (columns, rows) = (page.columns, page.rows);
    let (moved_app, moved_target) = (app.clone(), target.clone());
    let (dropped_app, dropped_target) = (app.clone(), target.clone());
    div()
        .id("perform-grid")
        .relative()
        .flex_1()
        .min_w_0()
        .h_full()
        .children(slots.into_iter().flatten())
        .children(cells)
        .when(setup, |grid| {
            grid.on_drag_move(move |event: &gpui::DragMoveEvent<CellDrag>, _, cx| {
                let drag = event.drag(cx);
                let local = event.event.position - event.bounds.origin;
                let step = |extent: gpui::Pixels, count: u16| f32::from(extent) / f32::from(count);
                let at = |offset: gpui::Pixels, extent: gpui::Pixels, count: u16| {
                    ((f32::from(offset) / step(extent, count)).floor().max(0.) as u16)
                        .min(count.saturating_sub(1))
                };
                let column = at(local.x, event.bounds.size.width, columns);
                let row = at(local.y, event.bounds.size.height, rows);
                let from = drag.from;
                let rect = if drag.resize {
                    Rect {
                        width: (column + 1).saturating_sub(from.column).max(1),
                        height: (row + 1).saturating_sub(from.row).max(1),
                        ..from
                    }
                } else {
                    let (grab_column, grab_row) = match drag.grab.get() {
                        Some(grab) => grab,
                        None => {
                            let grab = (
                                column.saturating_sub(from.column).min(from.width - 1),
                                row.saturating_sub(from.row).min(from.height - 1),
                            );
                            drag.grab.set(Some(grab));
                            grab
                        }
                    };
                    Rect {
                        column: column
                            .saturating_sub(grab_column)
                            .min(columns.saturating_sub(from.width)),
                        row: row
                            .saturating_sub(grab_row)
                            .min(rows.saturating_sub(from.height)),
                        ..from
                    }
                };
                let id = drag.id.clone();
                moved_app.update(cx, |this, cx| {
                    let same = this
                        .perform_mut(&moved_target)
                        .and_then(|state| state.surface.preview.as_ref())
                        .is_some_and(|(at, shown)| *at == id && *shown == rect);
                    if !same {
                        this.edit_surface(&moved_target, Edit::Preview(id, rect), cx);
                    }
                });
            })
            .on_drop(move |_: &CellDrag, _, cx| {
                dropped_app.update(cx, |this, cx| {
                    this.edit_surface(&dropped_target, Edit::Drop, cx)
                });
            })
        })
        .agent_node(Role::Card, format!("Page {}", page.name))
}

/// An empty place in Setup: a "+" that opens the menu to fill it.
fn slot(
    state: &Perform,
    page: &LayoutPage,
    column: u16,
    row: u16,
    target: &Target,
    app: &Entity<Luma>,
) -> AnyElement {
    let open = state
        .surface
        .adding
        .as_ref()
        .is_some_and(|adding| (adding.column, adding.row) == (column, row));
    let accent = Hsla::from(ladder::accent());
    place(Rect::at(column, row), page)
        .child(
            div()
                .id(SharedString::from(format!("perform-slot-{column}-{row}")))
                .relative()
                .size_full()
                .rounded(px(radius::CHIP))
                .border_1()
                .border_dashed()
                .border_color(if open { accent } else { glass::hairline(0.12) })
                .when(open, |slot| slot.bg(accent.opacity(0.08)))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(20.))
                .text_color(if open {
                    accent
                } else {
                    ladder::foreground_alpha(0.3)
                })
                .cursor_pointer()
                .hover(|style| style.border_color(accent).text_color(accent))
                .child("+")
                .on_click(on_edit(target, app, Edit::Adding(Some((column, row)))))
                .children(open.then(|| super::add::menu(state, target, app)))
                .agent_node(
                    Role::Button,
                    format!("New cell, row {}, column {}", row + 1, column + 1),
                ),
        )
        .into_any_element()
}

/// A cell in Setup: what it shows, still, with its outline; selected, its
/// grip and the corner that resizes it. A press selects it and a drag moves
/// it.
fn setup_cell(
    state: &Perform,
    cell: &Cell,
    rect: Rect,
    fits: bool,
    target: &Target,
    app: &Entity<Luma>,
) -> AnyElement {
    let selected = state.surface.selected.as_deref() == Some(cell.id.as_str());
    let title = state.title(cell);
    let accent = Hsla::from(ladder::accent());
    let outline = div()
        .absolute()
        .inset_0()
        .rounded(px(radius::CHIP))
        .map(|line| match (fits, selected) {
            (false, _) => line.border_2().border_color(Hsla::from(ladder::danger())),
            (true, true) => line.border_2().border_color(accent),
            (true, false) => line.border_1().border_color(glass::hairline(0.08)),
        });
    let badge = state
        .parts(cell)
        .iter()
        .find_map(|part| state.bindings(&part.control).next())
        .map(|binding| {
            div()
                .absolute()
                .bottom(px(6.))
                // Clear of the resize corner.
                .right(px(14.))
                .px(px(5.))
                .py(px(1.))
                .rounded(px(radius::CHIP))
                .bg(accent)
                .text_size(px(9.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(gpui::white())
                .child(super::address_badge(&binding.address))
        });
    let corner = selected.then(|| {
        div()
            .id(SharedString::from(format!("perform-resize-{}", cell.id)))
            .absolute()
            .right(px(-5.))
            .bottom(px(-5.))
            .size(px(12.))
            .rounded(px(3.))
            .bg(gpui::white())
            .border_2()
            .border_color(accent)
            .cursor_nwse_resize()
            .on_drag(
                CellDrag {
                    id: cell.id.clone(),
                    resize: true,
                    from: rect,
                    grab: Shared::new(None),
                },
                |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| Ghost)
                },
            )
            .agent_node(Role::Button, format!("Resize {title}"))
    });
    let grip = selected.then(|| {
        div()
            .absolute()
            .top(px(4.))
            .left(relative(0.5))
            .ml(px(-10.))
            .w(px(20.))
            .h(px(3.))
            .rounded_full()
            .bg(ladder::foreground_alpha(0.4))
    });
    // The face, its outline and the badge clip to the cell's corners; the
    // grip and the resize corner sit over the edge, outside the clip.
    let body = div()
        .absolute()
        .inset_0()
        .rounded(px(radius::CHIP))
        .overflow_hidden()
        .child(face(state, cell, rect, None))
        .child(outline)
        .children(badge);
    div()
        .id(SharedString::from(format!("perform-cell-{}", cell.id)))
        .relative()
        .size_full()
        .cursor_grab()
        .child(body)
        .children(grip)
        .children(corner)
        .on_click(on_edit(target, app, Edit::Select(Some(cell.id.clone()))))
        .on_drag(
            CellDrag {
                id: cell.id.clone(),
                resize: false,
                from: rect,
                grab: Shared::new(None),
            },
            |_, _, _, cx| cx.new(|_| Ghost),
        )
        .agent_node(Role::Button, format!("Select {title}"))
        .agent_focused(selected)
        .into_any_element()
}

/// What a cell's controls do: played in Perform, or `None`, shown still.
type Live<'a> = Option<(&'a Target, &'a Entity<Luma>)>;

/// What a cell shows. In Perform (`live`) its pads press and its faders
/// move; in Setup it looks the same and does neither.
fn face(state: &Perform, cell: &Cell, rect: Rect, live: Live) -> AnyElement {
    match &cell.content {
        CellContent::Pattern { pattern_id } => pattern_face(state, cell, pattern_id, rect, live),
        CellContent::Master | CellContent::Fader { .. } => fader_face(state, cell, rect, live),
        CellContent::Tap => tap_face(state, cell, live),
    }
}

/// How a pattern cell lays out its controls under its pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arrangement {
    /// The pad alone.
    Pad,
    /// The pad on top, this many controls standing side by side under it.
    Stood(usize),
    /// The pad on top, this many controls lying one over another under it.
    Laid(usize),
}

/// How a cell `width` × `height` places of `count` controls shows them: as
/// many as fit, up when it is taller than wide and along when wider.
pub(super) fn arrange(width: u16, height: u16, count: usize) -> Arrangement {
    if count == 0 || (width, height) == (1, 1) {
        Arrangement::Pad
    } else if height >= width {
        Arrangement::Stood(count.min(2 * usize::from(width)))
    } else {
        Arrangement::Laid(count.min(3 * usize::from(height) - 2))
    }
}

/// The glow around a pattern, as bright as its pad's envelope.
fn glow(tint: Hsla, level: f32) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: tint.opacity(0.45 * level.clamp(0., 1.)),
        offset: gpui::point(px(0.), px(0.)),
        blur_radius: px(26.),
        spread_radius: px(-6.),
        inset: false,
    }]
}

/// The plate every cell is drawn on.
fn plate() -> gpui::Div {
    div()
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .rounded(px(radius::CHIP))
        .overflow_hidden()
        .bg(glass::card_bg())
}

fn pattern_face(
    state: &Perform,
    cell: &Cell,
    pattern_id: &str,
    rect: Rect,
    live: Live,
) -> AnyElement {
    let pattern = state.pattern(pattern_id);
    let title = state.title(cell);
    let tint = state.tint(cell);
    let parts = state.parts(cell);
    let (pad, controls) = match parts.split_first() {
        Some((pad, controls)) if pad.kind == PartKind::Pad => (Some(pad), controls),
        _ => (None, &parts[..]),
    };
    let level = pad.map_or(0., |pad| state.value(&pad.control, 0.) as f32);
    let on = pad.is_some_and(|pad| state.on(&pad.control));
    let subtitle = pattern.map(|pattern| {
        let group = group_label(&pattern.selection.expression);
        match pattern.graph.pad().map(|pad| pad.kind) {
            Some(LiveInputKind::Button(press)) if press.mode == PressMode::Flash => {
                format!("{group} · flash")
            }
            Some(_) => group,
            None => format!("{group} · always on"),
        }
    });
    let arrangement = arrange(rect.width, rect.height, controls.len());
    // GPUI clips to rectangles, not to rounded corners, so the fill rounds
    // itself: its top always meets the card's, its bottom only when the pad
    // is the whole cell.
    let corner = px(radius::CHIP);
    let fill = div()
        .absolute()
        .inset_0()
        .rounded_t(corner)
        .when(arrangement == Arrangement::Pad, |fill| {
            fill.rounded_b(corner)
        })
        .bg(tint.opacity(level.clamp(0., 1.)));
    let pad_view =
        div().relative().w_full().min_h_0().child(fill).child(
            div()
                .relative()
                .size_full()
                .p(px(10.))
                .flex()
                .flex_col()
                .justify_between()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .line_clamp(2)
                                .text_size(px(13.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(ladder::foreground())
                                .child(title.clone()),
                        )
                        .child(div().flex_none().mt(px(3.)).size(px(9.)).rounded_full().bg(
                            if on {
                                ladder::foreground().into()
                            } else {
                                tint
                            },
                        )),
                )
                .children(subtitle.map(|subtitle| {
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(ladder::foreground_alpha(if level > 0.4 {
                            0.85
                        } else {
                            0.55
                        }))
                        .child(subtitle)
                })),
        );
    let pad_view = match (live, pad) {
        (Some((target, app)), Some(pad)) => press(
            pad_view.id(SharedString::from(format!("perform-pad-{}", cell.id))),
            &pad.control,
            PartKind::Pad,
            target,
            app,
        )
        .agent_node(
            Role::Button,
            format!("Pad: {title}{}", if on { " (on)" } else { "" }),
        )
        .into_any_element(),
        _ => pad_view.into_any_element(),
    };
    let card = plate().when(level > 0., |card| card.shadow(glow(tint, level)));
    let card = match arrangement {
        Arrangement::Pad => card.child(div().flex_1().min_h_0().flex().child(pad_view)),
        Arrangement::Stood(shown) => card
            .child(
                div()
                    .flex_none()
                    .h(relative(1. / f32::from(rect.height)))
                    .flex()
                    .child(pad_view),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(6.))
                    .p(px(8.))
                    .border_t_1()
                    .border_color(glass::hairline(0.06))
                    .children(
                        controls[..shown]
                            .iter()
                            .map(|part| stood(state, cell, &title, part, tint, live)),
                    ),
            ),
        Arrangement::Laid(shown) => card
            .child(div().flex_1().min_h_0().flex().child(pad_view))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .px(px(10.))
                    .pt(px(6.))
                    .pb(px(8.))
                    .children(
                        controls[..shown]
                            .iter()
                            .map(|part| laid(state, cell, &title, part, tint, live)),
                    ),
            ),
    };
    card.into_any_element()
}

/// Press and release handlers for a button of a cell: the pointer's
/// release, wherever it lands, lets go of the button it pressed.
fn press(
    element: gpui::Stateful<gpui::Div>,
    control: &str,
    kind: PartKind,
    target: &Target,
    app: &Entity<Luma>,
) -> gpui::Stateful<gpui::Div> {
    let handler = |down: bool| {
        let (app, target, control) = (app.clone(), target.clone(), control.to_owned());
        move |cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                this.press_part(&target, &control, kind, down, cx)
            });
        }
    };
    let (down, up, out) = (handler(true), handler(false), handler(false));
    element
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| down(cx))
        .on_mouse_up(MouseButton::Left, move |_, _, cx| up(cx))
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| out(cx))
}

/// A fader's track: live, or as it looks.
fn track(
    state: &Perform,
    cell: &Cell,
    name: String,
    part: &Part,
    axis: Axis,
    tint: Hsla,
    live: Live,
) -> AnyElement {
    let PartKind::Fader { rest } = part.kind else {
        return div().into_any_element();
    };
    let value = state.value(&part.control, rest) as f32;
    match live {
        Some((target, app)) => {
            let (app, target, control) = (app.clone(), target.clone(), part.control.clone());
            luma_ui::luma_fader(
                // The control's name first, so a script can find the fader.
                SharedString::from(format!("perform-fader-{}-{}", part.control, cell.id)),
                value,
                axis,
                tint,
                move |value, _, cx| {
                    app.update(cx, |this, cx| {
                        this.set_perform_value(&target, &control, value, cx)
                    });
                },
            )
            .size_full()
            .agent_node(Role::Slider, name)
            .into_any_element()
        }
        None => luma_ui::fader_track(value, axis, tint)
            .size_full()
            .into_any_element(),
    }
}

/// A button node of a pattern's graph: a small pad of its own.
fn button(
    state: &Perform,
    cell: &Cell,
    name: String,
    part: &Part,
    tint: Hsla,
    live: Live,
) -> AnyElement {
    let on = state.on(&part.control);
    let plate = div()
        .id(SharedString::from(format!(
            "perform-button-{}-{}",
            part.control, cell.id
        )))
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::CONTROL))
        .bg(if on {
            tint.opacity(0.7)
        } else {
            glass::ink(0.06)
        })
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(ladder::foreground_alpha(0.9))
        .child(div().truncate().px(px(4.)).child(part.label.clone()));
    match live {
        Some((target, app)) => press(plate, &part.control, PartKind::Button, target, app)
            .agent_node(
                Role::Button,
                format!("{name}{}", if on { " (on)" } else { "" }),
            )
            .into_any_element(),
        None => plate.into_any_element(),
    }
}

/// A fader's reading as a percentage.
fn percent(state: &Perform, part: &Part) -> String {
    match part.kind {
        PartKind::Fader { rest } => format!("{:.0}%", state.value(&part.control, rest) * 100.),
        _ => String::new(),
    }
}

/// A control standing under a tall cell's pad: its track, its name and its
/// reading under it.
fn stood(
    state: &Perform,
    cell: &Cell,
    title: &str,
    part: &Part,
    tint: Hsla,
    live: Live,
) -> AnyElement {
    let name = format!("{title} {}", part.label);
    let control = match part.kind {
        PartKind::Fader { .. } => track(state, cell, name, part, Axis::Vertical, tint, live),
        _ => button(state, cell, name, part, tint, live),
    };
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().flex_1().min_h_0().flex().child(control))
        .child(
            div()
                .flex()
                .justify_between()
                .gap(px(4.))
                .text_size(px(10.5))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(ladder::foreground_alpha(0.8))
                        .child(part.label.clone()),
                )
                .child(
                    div()
                        .font_family(luma_ui::fonts::MONO)
                        .text_color(ladder::foreground_alpha(0.5))
                        .child(percent(state, part)),
                ),
        )
        .into_any_element()
}

/// A control lying under a wide cell's pad: its name, its track, its
/// reading.
fn laid(
    state: &Perform,
    cell: &Cell,
    title: &str,
    part: &Part,
    tint: Hsla,
    live: Live,
) -> AnyElement {
    let name = format!("{title} {}", part.label);
    let control = match part.kind {
        PartKind::Fader { .. } => track(state, cell, name, part, Axis::Horizontal, tint, live),
        _ => button(state, cell, name, part, tint, live),
    };
    div()
        .h(px(ROW))
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(10.5))
        .child(
            div()
                .w(px(56.))
                .truncate()
                .text_color(ladder::foreground_alpha(0.8))
                .child(part.label.clone()),
        )
        .child(div().flex_1().h_full().flex().child(control))
        .child(
            div()
                .w(px(34.))
                .text_right()
                .font_family(luma_ui::fonts::MONO)
                .text_color(ladder::foreground_alpha(0.5))
                .child(percent(state, part)),
        )
        .into_any_element()
}

/// The master or a fader of its own: its name over a fader filling the cell,
/// standing when the cell is tall and lying when it is wide.
fn fader_face(state: &Perform, cell: &Cell, rect: Rect, live: Live) -> AnyElement {
    let title = state.title(cell);
    let tint = state.tint(cell);
    let parts = state.parts(cell);
    let Some(part) = parts.first() else {
        return plate().into_any_element();
    };
    let axis = if rect.height >= rect.width {
        Axis::Vertical
    } else {
        Axis::Horizontal
    };
    let control = track(state, cell, title.clone(), part, axis, tint, live);
    plate()
        .p(px(10.))
        .gap(px(6.))
        .items_center()
        .child(
            div()
                .w_full()
                .text_center()
                .truncate()
                .text_size(px(12.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ladder::foreground())
                .child(title),
        )
        .child(
            div()
                .w_full()
                .when(axis == Axis::Vertical, |slot| slot.flex_1().min_h_0())
                .when(axis == Axis::Horizontal, |slot| slot.h(px(ROW + 4.)))
                .flex()
                .child(control),
        )
        .child(
            div()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.55))
                .child(percent(state, part)),
        )
        .into_any_element()
}

/// Tap: pressed on the beat, it sets the tempo; it shows the tempo and the
/// beat of the bar.
fn tap_face(state: &Perform, cell: &Cell, live: Live) -> AnyElement {
    let tempo = state.tempo();
    let held = state.pressed.as_deref() == Some(luma_lib::controls::perform::TAP);
    let lit = tempo
        .and_then(|(_, beat)| beat)
        .map(|beat| beat.rem_euclid(4.).floor() as usize);
    let beats = div().flex().gap(px(5.)).children((0..4).map(|index| {
        div().size(px(8.)).rounded_full().bg(if lit == Some(index) {
            ladder::foreground_alpha(0.95)
        } else {
            glass::hairline(0.18)
        })
    }));
    let readout = match tempo {
        Some((bpm, _)) => format!("{bpm:.1} BPM"),
        None => "No tempo yet".into(),
    };
    let body = div()
        .id(SharedString::from(format!("perform-tap-{}", cell.id)))
        .size_full()
        .p(px(10.))
        .flex()
        .flex_col()
        .justify_between()
        .rounded(px(radius::CHIP))
        .bg(glass::ink(if held { 0.22 } else { 0.08 }))
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ladder::foreground())
                .child("Tap"),
        )
        .child(
            div().flex().flex_col().gap(px(5.)).child(beats).child(
                div()
                    .font_family(luma_ui::fonts::MONO)
                    .text_size(px(11.))
                    .text_color(ladder::foreground_alpha(0.6))
                    .child(readout.clone()),
            ),
        );
    let body = match live {
        Some((target, app)) => press(
            body,
            luma_lib::controls::perform::TAP,
            PartKind::Tap,
            target,
            app,
        )
        .agent_node(Role::Button, format!("Tap pad, {readout}"))
        .into_any_element(),
        None => body.into_any_element(),
    };
    plate().child(body).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_shows_as_many_controls_as_its_shape_holds() {
        assert_eq!(
            arrange(1, 1, 3),
            Arrangement::Pad,
            "a single place is its pad"
        );
        assert_eq!(arrange(2, 2, 0), Arrangement::Pad, "nothing to show");
        assert_eq!(arrange(1, 2, 1), Arrangement::Stood(1));
        assert_eq!(
            arrange(1, 3, 5),
            Arrangement::Stood(2),
            "a column holds two"
        );
        assert_eq!(
            arrange(2, 1, 4),
            Arrangement::Laid(1),
            "one row under the pad"
        );
        assert_eq!(arrange(3, 2, 9), Arrangement::Laid(4));
        // A bigger cell never holds fewer than a smaller one.
        let shown = |arrangement| match arrangement {
            Arrangement::Pad => 0,
            Arrangement::Stood(n) | Arrangement::Laid(n) => n,
        };
        for height in 1..5u16 {
            for width in 1..5u16 {
                let here = shown(arrange(width, height, 99));
                assert!(shown(arrange(width + 1, height, 99)) >= here);
                assert!(shown(arrange(width, height + 1, 99)) >= here);
            }
        }
    }
}
