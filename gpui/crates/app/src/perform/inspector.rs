//! Setup's inspector, beside the grid: the selected cell, or the page when
//! no cell is selected.
//!
//! For a pattern it holds the name, the group it lights, the way into its
//! editor, where its pad (a button node of its graph) is said to live.
//! A fader cell says what it moves. Every cell lists what of it can be
//! mapped, each with a Learn button and the controller inputs bound to it.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, Hsla, SharedString};
use luma_lib::models::perform::{Cell, CellContent, ControlTarget, LayoutPage, PerformPattern};
use luma_patterns::clip_graph::LiveInputKind;
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{glass, ladder, radius, Enabled};

use super::surface::Edit;
use super::{binding_chip, draft_field, group_label, Named, Perform};
use crate::tabs::Target;
use crate::Luma;

const WIDTH: f32 = 316.;

pub(super) fn panel(state: &Perform, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let sections = match state.selected_cell() {
        Some(cell) => cell_sections(state, cell, target, app),
        None => state
            .page()
            .map(|page| page_sections(state, page, target, app))
            .unwrap_or_default(),
    };
    div()
        .id("perform-inspector")
        .w(px(WIDTH))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .p(px(16.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .border_l_1()
        .border_color(glass::hairline(0.07))
        .children(sections.into_iter().enumerate().map(|(index, section)| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .when(index > 0, |section| {
                    section
                        .pt(px(14.))
                        .border_t_1()
                        .border_color(glass::hairline(0.07))
                })
                .child(section)
        }))
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

/// A sentence of help, in small muted type.
fn note(text: impl Into<SharedString>) -> impl IntoElement {
    let text = text.into();
    div()
        .text_size(px(11.5))
        .text_color(ladder::foreground_alpha(0.5))
        .child(text.clone())
        .agent_node(Role::Text, text)
}

fn column() -> gpui::Div {
    div().flex().flex_col().gap(px(8.))
}

/// A name in large type; a click opens it for typing.
fn name(
    state: &Perform,
    of: Named,
    text: String,
    target: &Target,
    app: &Entity<Luma>,
) -> AnyElement {
    if let Some(draft) = state.draft.as_ref().filter(|draft| draft.of == of) {
        return draft_field(draft, "Name", gpui::relative(1.).into()).into_any_element();
    }
    let (app, target) = (app.clone(), target.clone());
    div()
        .id("perform-name")
        .mx(px(-8.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(radius::CONTROL))
        .truncate()
        .text_size(px(17.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(ladder::foreground())
        .cursor_text()
        .hover(|style| style.bg(glass::ink(0.06)))
        .child(text.clone())
        .on_click(move |_, window, cx| {
            let of = of.clone();
            app.update(cx, |this, cx| this.start_draft(&target, of, window, cx));
        })
        .agent_node(Role::Button, format!("Rename {text}"))
        .into_any_element()
}

/// With no cell selected: the page's name and size, and removing it.
fn page_sections(
    state: &Perform,
    page: &LayoutPage,
    target: &Target,
    app: &Entity<Luma>,
) -> Vec<AnyElement> {
    let size = |label: &'static str, value: u16, resize: fn(u16, &LayoutPage) -> Edit| {
        let (app, target, page) = (app.clone(), target.clone(), page.clone());
        float::inline_field_row(
            label,
            float::scrub(
                label,
                f64::from(value),
                1.,
                f64::from(super::surface::MOST),
                1.,
                120.,
                move |value, _, cx| {
                    let edit = resize(value as u16, &page);
                    app.update(cx, |this, cx| this.edit_surface(&target, edit, cx));
                },
            ),
        )
    };
    let rename = {
        let (app, target, id) = (app.clone(), target.clone(), page.id.clone());
        float::btn("Rename", "perform-page-rename")
            .id("perform-page-rename")
            .on_click(move |_, window, cx| {
                let id = id.clone();
                app.update(cx, |this, cx| {
                    this.start_draft(&target, Named::Page(id), window, cx)
                });
            })
            .agent_node(Role::Button, "Rename page")
    };
    let holds_patterns = page.cells.iter().any(|cell| cell.pattern_id().is_some());
    let only = state
        .layout
        .as_ref()
        .is_none_or(|layout| layout.pages.len() < 2);
    let removable = !holds_patterns && !only;
    let remove = luma_ui::button(
        "Remove page",
        if removable { Enabled::Yes } else { Enabled::No },
    )
    .id("perform-page-remove")
    .text_color(Hsla::from(ladder::danger()))
    .when(removable, |button| {
        button.on_click(on_edit(target, app, Edit::RemovePage))
    })
    .agent_node(Role::Button, "Remove page");
    vec![
        column()
            .child(float::label("Page"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(17.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(page.name.clone()),
                    )
                    .child(rename),
            )
            .child(size("Columns", page.columns, |columns, page| {
                Edit::ResizePage {
                    columns,
                    rows: page.rows,
                }
            }))
            .child(size("Rows", page.rows, |rows, page| Edit::ResizePage {
                columns: page.columns,
                rows,
            }))
            .into_any_element(),
        column()
            .child(note(
                "Click a \u{201c}+\u{201d} to put a pattern in that place. Turn on the \
                 switch at a row's left and its patterns play one at a time.",
            ))
            .into_any_element(),
        column()
            .child(div().flex().child(remove))
            .children((!removable).then(|| {
                note(if only {
                    "A set keeps at least one page."
                } else {
                    "Move or delete this page's patterns to remove it."
                })
            }))
            .into_any_element(),
    ]
}

/// What a cell is, as the inspector heads it.
fn kind(cell: &Cell) -> &'static str {
    match cell.content {
        CellContent::Pattern { .. } => "Pattern",
        CellContent::Master => "Master",
        CellContent::Tap => "Tap",
        CellContent::Fader { .. } => "Fader",
    }
}

fn cell_sections(
    state: &Perform,
    cell: &Cell,
    target: &Target,
    app: &Entity<Luma>,
) -> Vec<AnyElement> {
    let head = div()
        .flex()
        .items_center()
        .justify_between()
        .child(float::label(format!(
            "{} · {} × {}",
            kind(cell),
            cell.width,
            cell.height
        )))
        .child(div().size(px(10.)).rounded_full().bg(state.tint(cell)));
    let mut sections = Vec::new();
    let pattern = cell.pattern_id().and_then(|id| state.pattern(id));
    match (&cell.content, pattern) {
        (CellContent::Pattern { .. }, Some(pattern)) => {
            sections.push(
                column()
                    .child(head)
                    .child(name(
                        state,
                        Named::Pattern(pattern.id.clone()),
                        pattern.name.clone(),
                        target,
                        app,
                    ))
                    .child(float::inline_field_row(
                        "Group",
                        group_picker(state, pattern, target, app),
                    ))
                    .child(edit_pattern(pattern, target, app))
                    .child(note(match pattern.graph.pad() {
                        Some(pad) => format!(
                            "Its pad is the \u{201c}{}\u{201d} button in its graph: edit \
                             the pattern to change its fades and toggle or flash.",
                            pad.name
                        ),
                        None => "It has no pad: no button in its graph feeds its alpha, so \
                                 it plays whenever the set is open."
                            .to_owned(),
                    }))
                    .into_any_element(),
            );
            if state
                .page()
                .is_some_and(|page| page.one_at_a_time.contains(&cell.row))
            {
                sections.push(
                    note(format!(
                        "Row {} plays one at a time: turning this pattern on turns the \
                         row's others off.",
                        cell.row + 1
                    ))
                    .into_any_element(),
                );
            }
        }
        (CellContent::Pattern { .. }, None) => sections.push(
            column()
                .child(head)
                .child(note("This cell's pattern was deleted."))
                .into_any_element(),
        ),
        (CellContent::Fader { label, targets }, _) => {
            sections.push(
                column()
                    .child(head)
                    .child(name(
                        state,
                        Named::Fader(cell.id.clone()),
                        state.title(cell),
                        target,
                        app,
                    ))
                    .into_any_element(),
            );
            sections.push(moves(state, cell, label, targets, target, app).into_any_element());
        }
        (CellContent::Master, _) => sections.push(
            column()
                .child(head)
                .child(note("Scales every pattern of the set at once."))
                .into_any_element(),
        ),
        (CellContent::Tap, _) => sections.push(
            column()
                .child(head)
                .child(note(
                    "Tap it on the beat to set the tempo, when no DJ is followed.",
                ))
                .into_any_element(),
        ),
    }
    sections.push(mapping(state, cell, target, app).into_any_element());
    sections.push(footer(cell, pattern, target, app).into_any_element());
    sections
}

/// The group a pattern lights, and the menu to pick another.
fn group_picker(
    state: &Perform,
    pattern: &PerformPattern,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let current = pattern.selection.expression.clone();
    let mut options: Vec<String> = std::iter::once("all".to_owned())
        .chain(state.groups.iter().cloned())
        .collect();
    // An expression written in the editor is one of the choices too.
    if !options.contains(&current) {
        options.push(current.clone());
    }
    let labels: Vec<String> = options.iter().map(|option| group_label(option)).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let shown = group_label(&current);
    let open = {
        let (app, target) = (app.clone(), target.clone());
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| this.toggle_group_menu(&target, cx));
        }
    };
    let menu = state.group_menu.then(|| {
        let close = {
            let (app, target) = (app.clone(), target.clone());
            move |_: &mut gpui::Window, cx: &mut gpui::App| {
                app.update(cx, |this, cx| this.toggle_group_menu(&target, cx));
            }
        };
        let rows = options.iter().zip(&labels).map(|(option, label)| {
            let (app, target, id, option) = (
                app.clone(),
                target.clone(),
                pattern.id.clone(),
                option.clone(),
            );
            luma_ui::luma_select_item(label, RowState::of(*option == current, false))
                .id(SharedString::from(format!("perform-group-{option}")))
                .on_click(move |_, _, cx| {
                    let option = option.clone();
                    app.update(cx, |this, cx| {
                        this.set_pattern_group(&target, &id, option, cx);
                        this.toggle_group_menu(&target, cx);
                    });
                })
                .agent_node(Role::Button, label.clone())
        });
        float::anchored_below(
            "perform-group-menu",
            luma_ui::CONTROL_HEIGHT,
            Dismiss::on_press_out(close),
            float::popover_card()
                .min_w(px(180.))
                .children(rows)
                .into_any_element(),
        )
    });
    float::picker_chip(&shown, &refs)
        .id("perform-group")
        .on_click(open)
        .children(menu)
        .agent_node(Role::Button, format!("Group: {shown}"))
}

fn edit_pattern(pattern: &PerformPattern, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let (app, target, id) = (app.clone(), target.clone(), pattern.id.clone());
    float::btn_primary("Edit pattern…")
        .id("perform-edit-pattern")
        .justify_center()
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| this.edit_perform_pattern(&target, &id, cx));
        })
        .agent_node(Role::Button, "Edit pattern")
}

/// Everything a fader cell can move: each pattern's level and faders, and
/// the venue's own controls.
fn target_options(state: &Perform) -> Vec<(ControlTarget, String)> {
    let mut options = Vec::new();
    for pattern in state.patterns.iter().flatten() {
        options.push((
            ControlTarget::Level {
                pattern_id: pattern.id.clone(),
            },
            format!("{} · Level", pattern.name),
        ));
        for input in pattern.graph.live_inputs() {
            if input.kind == LiveInputKind::Fader {
                options.push((
                    ControlTarget::Node {
                        pattern_id: pattern.id.clone(),
                        node: input.node,
                    },
                    format!("{} · {}", pattern.name, input.name),
                ));
            }
        }
    }
    for control in &state.controls {
        if !luma_lib::controls::perform::owns(&control.name) {
            options.push((
                ControlTarget::Named {
                    name: control.name.clone(),
                },
                group_label(&control.name),
            ));
        }
    }
    options
}

/// A fader cell's targets: a checklist of what it can move.
fn moves(
    state: &Perform,
    cell: &Cell,
    label: &str,
    targets: &[ControlTarget],
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let options = target_options(state);
    let empty = options.is_empty();
    let rows = options
        .into_iter()
        .enumerate()
        .map(|(index, (option, text))| {
            let checked = targets.contains(&option);
            let mut next: Vec<ControlTarget> = targets.to_vec();
            if checked {
                next.retain(|one| *one != option);
            } else {
                next.push(option);
            }
            let edit = Edit::SetContent(
                cell.id.clone(),
                CellContent::Fader {
                    label: label.to_owned(),
                    targets: next,
                },
            );
            div()
                .id(SharedString::from(format!("perform-moves-{index}")))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(6.))
                .py(px(4.))
                .rounded(px(radius::ROW))
                .cursor_pointer()
                .hover(|style| style.bg(glass::glass_hover()))
                .child(float::checkbox(checked))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .child(text.clone()),
                )
                .on_click(on_edit(target, app, edit))
                .agent_node(Role::Checkbox, text)
                .agent_focused(checked)
        });
    column()
        .child(float::label("Moves"))
        .children(empty.then(|| note("Nothing to move yet: add a pattern first.")))
        .child(div().flex().flex_col().gap(px(2.)).children(rows))
}

/// What of the cell can be mapped, each with Learn and its bindings.
fn mapping(state: &Perform, cell: &Cell, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let parts = state.parts(cell);
    let listening = parts
        .iter()
        .any(|part| state.learning.as_deref() == Some(part.control.as_str()));
    let mapped = parts
        .iter()
        .any(|part| state.bindings(&part.control).next().is_some());
    let accent = Hsla::from(ladder::accent());
    let rows = parts.iter().map(|part| {
        let waiting = state.learning.as_deref() == Some(part.control.as_str());
        let key = format!("perform-learn-{}", part.control);
        let (learn_app, learn_target, control, mode) = (
            app.clone(),
            target.clone(),
            part.control.clone(),
            part.kind.mode(),
        );
        let learn = if waiting {
            float::btn_primary("Cancel").id(SharedString::from(key))
        } else {
            float::btn("Learn", key.clone()).id(SharedString::from(key))
        }
        .on_click(move |_, _, cx| {
            learn_app.update(cx, |this, cx| this.learn(&learn_target, &control, mode, cx));
        })
        .agent_node(
            Role::Button,
            if waiting {
                "Cancel learning".to_owned()
            } else {
                format!("Learn {}", part.label)
            },
        );
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(ladder::foreground_alpha(0.85))
                            .child(part.label.clone()),
                    )
                    .child(learn),
            )
            .children(
                state
                    .bindings(&part.control)
                    .map(|binding| binding_chip(binding, target, app)),
            )
    });
    let listen = listening.then(|| {
        let text = "Press a pad or move a fader on your controller…";
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(radius::PANEL))
            .bg(accent.opacity(0.12))
            .text_size(px(12.))
            .text_color(accent)
            .child(div().flex_none().size(px(9.)).rounded_full().bg(accent))
            .child(text)
            .agent_node(Role::Text, text)
    });
    column()
        .child(float::label("Mapping"))
        .children(listen)
        .children(rows)
        .children(
            (!mapped && !listening)
                .then(|| note("Not mapped. Press Learn, then touch your controller.")),
        )
}

/// Duplicate and Delete for a pattern; Remove for any other cell.
fn footer(
    cell: &Cell,
    pattern: Option<&PerformPattern>,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let row = div().flex().gap(px(8.));
    match pattern {
        Some(pattern) => {
            let duplicate = {
                let (app, target, id) = (app.clone(), target.clone(), cell.id.clone());
                luma_ui::button("Duplicate", Enabled::Yes)
                    .id("perform-duplicate")
                    .flex_1()
                    .on_click(move |_, _, cx| {
                        app.update(cx, |this, cx| this.duplicate_cell(&target, &id, cx));
                    })
                    .agent_node(Role::Button, "Duplicate")
            };
            let delete = {
                let (app, target, id) = (app.clone(), target.clone(), pattern.id.clone());
                luma_ui::button("Delete", Enabled::Yes)
                    .id("perform-delete")
                    .flex_1()
                    .text_color(Hsla::from(ladder::danger()))
                    .on_click(move |_, _, cx| {
                        app.update(cx, |this, cx| this.ask_delete_pattern(&target, &id, cx));
                    })
                    .agent_node(Role::Button, "Delete pattern")
            };
            row.child(duplicate).child(delete)
        }
        None => row.child(
            luma_ui::button("Remove from page", Enabled::Yes)
                .id("perform-remove-cell")
                .flex_1()
                .on_click(on_edit(target, app, Edit::Remove(cell.id.clone())))
                .agent_node(Role::Button, "Remove from page"),
        ),
    }
}
