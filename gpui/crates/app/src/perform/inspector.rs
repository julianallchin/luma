//! Setup's inspector, beside the grid: the selected cell, or the page when
//! no cell is selected.
//!
//! For a pattern it holds the name, the group it lights, the way into its
//! editor, where its pad (a button node of its graph) is said to live.
//! A fader cell says what it moves. A list holds its values (a color edited
//! in the color picker, a number by dragging, or the color below), its
//! fade, whether it steps on by itself, and what it sets. A group cell holds
//! its name and group. Every cell lists what of it can be mapped, each with
//! a Learn button and the controller inputs bound to it.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, Hsla, SharedString};
use luma_lib::models::perform::{
    Cell, CellContent, ControlTarget, LayoutPage, ListValue, PerformPattern, Step, MOST_VALUES,
};
use luma_patterns::clip_graph::LiveInputKind;
use luma_patterns::press::TimeUnit;
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{glass, ladder, radius, Enabled};

use super::surface::Edit;
use super::{binding_chip, draft_field, group_label, Named, Perform};
use crate::workspace::Target;
use crate::Luma;
use luma_lib::dispatch::handlers::inputs::Mode;

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
        CellContent::Select => "Select",
        CellContent::Clear => "Clear",
        CellContent::Fader { .. } => "Fader",
        CellContent::List { .. } => "List",
        CellContent::Group { .. } => "Group",
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
                        group_picker(state, cell, &pattern.selection.expression, target, app),
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
                        "Row {} plays one at a time: where this pattern comes on, the \
                         row's others go off.",
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
        (CellContent::Fader { targets, .. }, _) => {
            sections.push(
                column()
                    .child(head)
                    .child(name(
                        state,
                        Named::Label(cell.id.clone()),
                        state.title(cell),
                        target,
                        app,
                    ))
                    .into_any_element(),
            );
            sections.push(moves(state, cell, targets, target, app).into_any_element());
        }
        (
            CellContent::List {
                values,
                fade,
                unit,
                targets,
                step,
                ..
            },
            _,
        ) => {
            let colors = cell.content.colors();
            let what = match (colors, targets.is_empty()) {
                (true, true) => {
                    "Tapping a color sets the base color of the groups chosen, or of \
                     every light. After Select it sets the chosen effects' own color, \
                     and Below puts them back on the color under them."
                }
                (true, false) => "Tapping a color fades every live color it sets to it.",
                (false, _) => "Tapping a number sets everything it moves to it.",
            };
            sections.push(
                column()
                    .child(head)
                    .child(name(
                        state,
                        Named::Label(cell.id.clone()),
                        state.title(cell),
                        target,
                        app,
                    ))
                    .child(note(what))
                    .into_any_element(),
            );
            sections.push(
                value_list(state, cell, values, targets.is_empty(), target, app).into_any_element(),
            );
            if colors {
                sections.push(fading(cell, *fade, *unit, target, app).into_any_element());
            }
            sections.push(stepping(cell, *step, target, app).into_any_element());
            sections.push(moves(state, cell, targets, target, app).into_any_element());
        }
        (CellContent::Group { group, .. }, _) => sections.push(
            column()
                .child(head)
                .child(name(
                    state,
                    Named::Label(cell.id.clone()),
                    state.title(cell),
                    target,
                    app,
                ))
                .child(float::inline_field_row(
                    "Group",
                    group_picker(state, cell, group, target, app),
                ))
                .child(note(
                    "Tap it to choose its group: what you do next acts there. Hold it \
                     and press something to act on this group alone.",
                ))
                .into_any_element(),
        ),
        (CellContent::Select, _) => sections.push(
            column()
                .child(head)
                .child(note(
                    "Hold it, or tap it, then press effects: they are chosen instead of \
                     played. The next color tapped on a list with no targets is theirs.",
                ))
                .into_any_element(),
        ),
        (CellContent::Clear, _) => sections.push(
            column()
                .child(head)
                .child(note(
                    "Tap it to choose nothing. Hold it to turn every effect off.",
                ))
                .into_any_element(),
        ),
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

/// The group a pattern lights or a group cell holds, `current`, and the
/// menu to pick another.
fn group_picker(
    state: &Perform,
    cell: &Cell,
    current: &str,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let current = current.to_owned();
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
            let (app, target, id, option) =
                (app.clone(), target.clone(), cell.id.clone(), option.clone());
            luma_ui::luma_select_item(label, RowState::of(*option == current, false))
                .id(SharedString::from(format!("perform-group-{option}")))
                .on_click(move |_, _, cx| {
                    let option = option.clone();
                    app.update(cx, |this, cx| {
                        this.set_group(&target, &id, option, cx);
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

/// Everything a fader cell can move (each pattern's level and faders, and
/// the venue's own controls), or with `colors`, everything a color cell can
/// set: each pattern's live colors.
fn target_options(state: &Perform, colors: bool) -> Vec<(ControlTarget, String)> {
    let mut options = Vec::new();
    for pattern in state.patterns.iter().flatten() {
        if !colors {
            options.push((
                ControlTarget::Level {
                    pattern_id: pattern.id.clone(),
                },
                format!("{} · Level", pattern.name),
            ));
        }
        let wanted = if colors {
            LiveInputKind::Color
        } else {
            LiveInputKind::Fader
        };
        for input in pattern.graph.live_inputs() {
            if input.kind == wanted {
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
    if colors {
        return options;
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

/// A fader or list's targets: a checklist of what it can move.
fn moves(
    state: &Perform,
    cell: &Cell,
    targets: &[ControlTarget],
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let colors = cell.content.colors();
    let options = target_options(state, colors);
    let empty = options.is_empty();
    let rows = options
        .into_iter()
        .enumerate()
        .map(|(index, (option, text))| {
            let checked = targets.contains(&option);
            let mut content = cell.content.clone();
            if let Some(next) = content.targets_mut() {
                if checked {
                    next.retain(|one| *one != option);
                } else {
                    next.push(option);
                }
            }
            let edit = Edit::SetContent(cell.id.clone(), content);
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
    let (label, nothing) = if colors {
        (
            "Sets",
            "Nothing to set yet: give a pattern a live color in its editor.",
        )
    } else {
        ("Moves", "Nothing to move yet: add a pattern first.")
    };
    column()
        .child(float::label(label))
        .children(empty.then(|| note(nothing)))
        .child(div().flex().flex_col().gap(px(2.)).children(rows))
}

/// What of a list an edit here changes.
struct Listed<'a> {
    values: &'a mut Vec<ListValue>,
    fade: &'a mut f64,
    unit: &'a mut TimeUnit,
    step: &'a mut Option<Step>,
}

/// List `cell` as `change` leaves it.
fn relisted(cell: &Cell, change: impl FnOnce(Listed<'_>)) -> Edit {
    let mut content = cell.content.clone();
    if let CellContent::List {
        values,
        fade,
        unit,
        step,
        ..
    } = &mut content
    {
        change(Listed {
            values,
            fade,
            unit,
            step,
        });
    }
    Edit::SetContent(cell.id.clone(), content)
}

/// A list's values: each a chip that opens it, a way to add one (and to a
/// color list with no targets, the color below), and for the one open its
/// color picker or its number, its place and removing it.
fn value_list(
    state: &Perform,
    cell: &Cell,
    values: &[ListValue],
    unlinked: bool,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let open = state
        .value
        .as_ref()
        .filter(|editor| editor.cell == cell.id && editor.index < values.len());
    let pick = |index: Option<usize>| {
        let (app, target, id) = (app.clone(), target.clone(), cell.id.clone());
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| this.open_value(&target, &id, index, cx));
        }
    };
    let chips = values.iter().enumerate().map(|(index, value)| {
        let chosen = open.is_some_and(|editor| editor.index == index);
        let chip = div()
            .id(SharedString::from(format!("perform-value-edit-{index}")))
            .h(px(28.))
            .min_w(px(28.))
            .px(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(radius::CONTROL))
            .text_size(px(10.5))
            .text_color(ladder::foreground_alpha(0.85))
            .border_2()
            .border_color(if chosen {
                ladder::foreground().into()
            } else {
                glass::hairline(0.12)
            })
            .cursor_pointer();
        let chip = match value {
            ListValue::Color(rgb) => chip.bg(super::paint(*rgb)),
            ListValue::Number(number) => chip
                .bg(glass::ink(0.06))
                .child(format!("{:.0}%", number * 100.)),
            ListValue::Below => chip.bg(glass::ink(0.06)).child("Below"),
        };
        chip.on_click(pick((!chosen).then_some(index)))
            .agent_node(Role::Toggle, format!("Edit value {}", index + 1))
            .agent_focused(chosen)
    });
    let full = values.len() >= MOST_VALUES;
    let add = |label: &'static str, key: &'static str, value: ListValue| {
        let (app, target, cell) = (app.clone(), target.clone(), cell.clone());
        let at = values.len();
        luma_ui::button(label, if full { Enabled::No } else { Enabled::Yes })
            .id(key)
            .when(!full, |button| {
                button.on_click(move |_, _, cx| {
                    let edit = relisted(&cell, |list| list.values.push(value));
                    app.update(cx, |this, cx| {
                        this.edit_surface(&target, edit, cx);
                        this.open_value(&target, &cell.id, Some(at), cx);
                    });
                })
            })
            .agent_node(Role::Button, label)
    };
    let colors = values.first().is_none_or(|value| value.is_color());
    let copy = values
        .iter()
        .rev()
        .find(|value| **value != ListValue::Below)
        .copied()
        .unwrap_or(ListValue::Color([1.; 3]));
    let below = (colors && unlinked && !values.contains(&ListValue::Below))
        .then(|| add("Add below", "perform-value-add-below", ListValue::Below));
    let editing = open.map(|editor| {
        let index = editor.index;
        let last = values.len() - 1;
        let shift = |label: &'static str, to: Option<usize>| {
            let (app, target, cell) = (app.clone(), target.clone(), cell.clone());
            let key = format!("perform-value-{}", label.to_lowercase().replace(' ', "-"));
            luma_ui::button(
                label,
                if to.is_some() {
                    Enabled::Yes
                } else {
                    Enabled::No
                },
            )
            .id(SharedString::from(key))
            .when_some(to, |button, to| {
                button.on_click(move |_, _, cx| {
                    let edit = relisted(&cell, |list| list.values.swap(index, to));
                    app.update(cx, |this, cx| {
                        this.edit_surface(&target, edit, cx);
                        this.open_value(&target, &cell.id, Some(to), cx);
                    });
                })
            })
            .agent_node(Role::Button, format!("{label} value {}", index + 1))
        };
        let remove = {
            let (app, target, cell) = (app.clone(), target.clone(), cell.clone());
            let can = last > 0;
            luma_ui::button("Remove", if can { Enabled::Yes } else { Enabled::No })
                .id("perform-value-remove")
                .text_color(Hsla::from(ladder::danger()))
                .when(can, |button| {
                    button.on_click(move |_, _, cx| {
                        let edit = relisted(&cell, |list| {
                            list.values.remove(index);
                        });
                        app.update(cx, |this, cx| {
                            this.open_value(&target, &cell.id, None, cx);
                            this.edit_surface(&target, edit, cx);
                        });
                    })
                })
                .agent_node(Role::Button, format!("Remove value {}", index + 1))
        };
        let number = match values[index] {
            ListValue::Number(number) => {
                let (app, target, cell) = (app.clone(), target.clone(), cell.clone());
                Some(
                    float::scrub("Value", number, 0., 1., 0.01, 120., move |value, _, cx| {
                        let edit =
                            relisted(&cell, |list| list.values[index] = ListValue::Number(value));
                        app.update(cx, |this, cx| this.edit_surface(&target, edit, cx));
                    })
                    .agent_node(Role::Slider, "Value"),
                )
            }
            _ => None,
        };
        column()
            .children(editor.picker.as_ref().map(|(picker, _)| picker.clone()))
            .children(number.map(|number| float::inline_field_row("Number", number)))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(shift("Earlier", index.checked_sub(1)))
                    .child(shift("Later", (index < last).then_some(index + 1)))
                    .child(remove),
            )
    });
    column()
        .child(float::label("Values"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .children(chips)
                .child(add("Add", "perform-value-add", copy))
                .children(below),
        )
        .children(editing)
}

/// A color list's fade: its length, and whether it counts beats or seconds.
fn fading(
    cell: &Cell,
    fade: f64,
    unit: TimeUnit,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let length = {
        let (app, target, cell) = (app.clone(), target.clone(), cell.clone());
        float::scrub("Fade", fade, 0., 16., 0.25, 120., move |value, _, cx| {
            let edit = relisted(&cell, |list| *list.fade = value);
            app.update(cx, |this, cx| this.edit_surface(&target, edit, cx));
        })
        .agent_node(Role::Slider, "Fade")
    };
    let units = [(TimeUnit::Beats, "Beats"), (TimeUnit::Seconds, "Seconds")]
        .into_iter()
        .fold(float::segmented(), |track, (choice, label)| {
            let chosen = unit == choice;
            let key = format!("perform-fade-{label}");
            track.child(
                float::segment(label, chosen, key.clone())
                    .id(SharedString::from(key))
                    .on_click(on_edit(
                        target,
                        app,
                        relisted(cell, |list| *list.unit = choice),
                    ))
                    .agent_node(Role::Toggle, label)
                    .agent_focused(chosen),
            )
        });
    column()
        .child(float::label("Fade"))
        .child(float::inline_field_row("Length", length))
        .child(float::inline_field_row("In", units))
}

/// Whether a list steps on by itself, every how many bars, and whether it
/// loops back to its first value.
fn stepping(
    cell: &Cell,
    step: Option<Step>,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let set = |next: Option<Step>| on_edit(target, app, relisted(cell, |list| *list.step = next));
    let choices = [
        ("Off", None),
        (
            "Every few bars",
            Some(step.unwrap_or(Step {
                bars: 4,
                loops: true,
            })),
        ),
    ];
    let switch = choices
        .into_iter()
        .fold(float::segmented(), |track, (label, choice)| {
            let chosen = step.is_some() == choice.is_some();
            let key = format!("perform-step-{label}");
            track.child(
                float::segment(label, chosen, key.clone())
                    .id(SharedString::from(key))
                    .on_click(set(choice))
                    .agent_node(Role::Toggle, format!("Step: {label}"))
                    .agent_focused(chosen),
            )
        });
    let details = step.map(|step| {
        let bars = {
            let (app, target, cell) = (app.clone(), target.clone(), cell.clone());
            float::scrub(
                "Bars",
                f64::from(step.bars),
                1.,
                32.,
                1.,
                120.,
                move |value, _, cx| {
                    let next = Step {
                        bars: value as u16,
                        ..step
                    };
                    let edit = relisted(&cell, |list| *list.step = Some(next));
                    app.update(cx, |this, cx| this.edit_surface(&target, edit, cx));
                },
            )
            .agent_node(Role::Slider, "Bars")
        };
        let looping = div()
            .id("perform-step-loop")
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(6.))
            .py(px(4.))
            .rounded(px(radius::ROW))
            .cursor_pointer()
            .hover(|style| style.bg(glass::glass_hover()))
            .child(float::checkbox(step.loops))
            .child(
                div()
                    .text_size(px(12.5))
                    .child("Back to the first after the last"),
            )
            .on_click(set(Some(Step {
                loops: !step.loops,
                ..step
            })))
            .agent_node(Role::Checkbox, "Loop")
            .agent_focused(step.loops);
        column()
            .child(float::inline_field_row("Every", bars))
            .child(looping)
    });
    column()
        .child(float::label("Steps"))
        .child(switch)
        .children(details)
        .child(note(
            "It moves on to its next value on the set's bars: the deck's, or \
             4 beats of the tapped tempo.",
        ))
}

/// What of the cell can be mapped, each with Learn and its bindings.
fn mapping(state: &Perform, cell: &Cell, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    // A live color is set by a list, not by a controller.
    let parts: Vec<(super::Part, Mode)> = state
        .parts(cell)
        .into_iter()
        .filter_map(|part| {
            let mode = part.kind.mode()?;
            Some((part, mode))
        })
        .collect();
    let listening = parts
        .iter()
        .any(|(part, _)| state.learning.as_deref() == Some(part.control.as_str()));
    let mapped = parts
        .iter()
        .any(|(part, _)| state.bindings(&part.control).next().is_some());
    let accent = Hsla::from(ladder::accent());
    let rows = parts.iter().map(|(part, mode)| {
        let waiting = state.learning.as_deref() == Some(part.control.as_str());
        let key = format!("perform-learn-{}", part.control);
        let (learn_app, learn_target, control, mode) =
            (app.clone(), target.clone(), part.control.clone(), *mode);
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
