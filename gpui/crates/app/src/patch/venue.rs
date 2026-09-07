//! Compact venue controls beneath the live stage.
use super::{Patch, Section};
use crate::{shell::Body, Luma};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, SharedString, Window};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, glass, ladder};

impl Luma {
    pub(crate) fn highlight_venue_lights(
        &mut self,
        selected: std::collections::HashSet<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(stage) = self.visualizer_mut() {
            stage.replace_selection(
                selected
                    .iter()
                    .cloned()
                    .map(luma_render::frame::EditorObject::Fixture),
            );
            if let Some(build) = stage.build.as_mut() {
                build.select(if selected.len() == 1 {
                    selected.iter().next().cloned()
                } else {
                    None
                });
            }
        }
        cx.notify();
    }

    pub(crate) fn venue_section(&mut self, section: Section, cx: &mut gpui::Context<Self>) {
        self.stage_escape(cx);
        let venue = if let Some(Body::Patch(state)) = self.workspace.active_body_mut() {
            state.section = section;
            state.panel = None;
            state.group_error = None;
            if let Some(editor) = &mut state.group_editor {
                editor.parent_open = false;
            }
            Some(state.venue_id.clone())
        } else {
            None
        };
        if let Some(venue) = venue {
            let selected = self
                .patch_mut(&venue)
                .map(|state| state.selected.clone())
                .unwrap_or_default();
            self.highlight_venue_lights(selected, cx);
            self.reload_patch(venue, cx);
        }
        cx.notify();
    }
}

pub(super) fn render(
    state: &Patch,
    app: &Entity<Luma>,
    view: Option<&crate::stage::StageView>,
    selection: Option<AnyElement>,
    window: &Window,
) -> AnyElement {
    let count = state.rows().len();
    let groups = state.data.as_ref().map_or(0, |d| d.groups.len());
    let mut nav = div().flex().gap(px(4.0)).w_full();
    for (section, label, detail) in [
        (
            Section::Stage,
            "Stage",
            view.map_or(0, |v| v.elements.len()).to_string(),
        ),
        (Section::Fixtures, "Fixtures", count.to_string()),
        (Section::Groups, "Groups", groups.to_string()),
    ] {
        let click = app.clone();
        nav = nav.child(
            div()
                .id(SharedString::from(format!("venue-section-{label}")))
                .flex_1()
                .min_w_0()
                .px(px(10.0))
                .py(px(9.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .bg(if state.section == section {
                    glass::card_selected_bg()
                } else {
                    glass::wash(0.0)
                })
                .hover(|d| d.bg(glass::glass_hover()))
                .on_click(move |_, _, cx| {
                    click.update(cx, |this, cx| this.venue_section(section, cx))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().flex_1().text_size(px(12.5)).child(label))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(ladder::muted_foreground())
                                .child(detail),
                        ),
                )
                .agent_node(Role::Toggle, label)
                .agent_focused(state.section == section),
        );
    }
    let top = div()
        .flex_none()
        .px(px(16.0))
        .pt(px(8.0))
        .pb(px(8.0))
        .flex()
        .flex_col()
        .child(nav);
    let content = match state.section {
        Section::Fixtures => lights(state, app),
        Section::Groups => super::groups::groups(state, app),
        Section::Details => super::details(state, app, window),
        Section::Stage => div()
            .id("venue-layout")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(crate::stage::stage_page(&state.stage, app, view, window))
            .children(selection)
            .children(view.map(|view| elements(view, app)))
            .children(view.map(|view| unplaced(view, app)))
            .into_any_element(),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(top)
        .child(content)
        .agent_node(Role::Card, format!("{} Venue", state.venue_name))
        .into_any_element()
}

fn lights(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let add = app.clone();
    let venue = state.venue_id.clone();
    let assign = app.clone();
    let details = app.clone();
    let mut body = div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .p(px(16.0))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .child(
                    float::btn("Add fixtures", "venue-add-lights")
                        .id("venue-add-lights")
                        .on_click(move |_, window, cx| {
                            add.update(cx, |this, cx| {
                                this.open_add_fixtures(venue.clone(), window, cx)
                            })
                        })
                        .agent_node(Role::Button, "Add fixtures"),
                )
                .when(!state.selected.is_empty(), |d| {
                    d.child(
                        float::btn(
                            format!("Group {} selected", state.selected.len()),
                            "venue-group-selected",
                        )
                        .id("venue-group-selected")
                        .on_click(move |_, window, cx| {
                            assign.update(cx, |this, cx| {
                                this.edit_venue_group(None, true, window, cx)
                            })
                        })
                        .agent_node(Role::Button, "Group selected lights"),
                    )
                })
                .child(
                    float::btn("Patch details", "venue-details")
                        .id("venue-details")
                        .on_click(move |_, _, cx| {
                            details.update(cx, |this, cx| this.venue_section(Section::Details, cx))
                        })
                        .agent_node(Role::Button, "Patch details"),
                ),
        );
    if let Some(error) = &state.error {
        body = body.child(luma_ui::plate(error.clone(), ladder::danger()));
    }
    if state.data.is_none() && state.error.is_none() {
        body = body.child(float::empty_row("Loading fixtures…"));
    }
    let rows = state.rows().iter().map(|row| {
        let selected = state.selected.contains(&row.id);
        let click = app.clone();
        let venue = state.venue_id.clone();
        let id = row.id.clone();
        let name = row.label.clone().unwrap_or_else(|| row.model.clone());
        div()
            .id(SharedString::from(format!("venue-light-{}", row.id)))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .py(px(7.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .bg(if selected {
                glass::card_selected_bg()
            } else {
                glass::wash(0.0)
            })
            .hover(|d| d.bg(glass::glass_hover()))
            .on_click(move |_, _, cx| {
                click.update(cx, |this, cx| {
                    this.pick_patch_row(venue.clone(), id.clone(), true, cx)
                })
            })
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(if selected {
                        ladder::accent().into()
                    } else {
                        ladder::foreground_alpha(0.35)
                    })
                    .child(if selected { "●" } else { "○" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(div().text_size(px(12.5)).truncate().child(name.clone()))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .truncate()
                            .text_color(ladder::foreground_alpha(0.45))
                            .child(format!("{} · {}", row.manufacturer, row.model)),
                    ),
            )
            .children((!state.is_placed(&row.id)).then(|| {
                div()
                    .text_size(px(10.5))
                    .text_color(ladder::foreground_alpha(0.55))
                    .child("Unplaced")
            }))
            .agent_node(Role::Row, name)
            .agent_focused(selected)
    });
    body.child(
        div()
            .id("venue-lights-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(rows),
    )
    .into_any_element()
}

fn elements(view: &crate::stage::StageView, app: &Entity<Luma>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(float::label("Elements"))
        .when(view.elements.is_empty(), |d| {
            d.child(float::empty_row("No stage elements"))
        })
        .children(view.elements.iter().map(|(id, label, parent)| {
            let click = app.clone();
            let id = id.clone();
            let selected = view.selected.as_ref() == Some(&id);
            div()
                .id(SharedString::from(format!("venue-element-{id}")))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(7.0))
                .rounded(px(4.0))
                .bg(if selected {
                    glass::card_selected_bg()
                } else {
                    glass::wash(0.0)
                })
                .hover(|d| d.bg(glass::glass_hover()))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    click.update(cx, |this, cx| this.stage_select_element(id.clone(), cx))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .child(label.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::muted_foreground())
                        .child(parent.clone()),
                )
                .agent_node(Role::Row, format!("Element {label}"))
                .agent_focused(selected)
        }))
        .into_any_element()
}

fn unplaced(view: &crate::stage::StageView, app: &Entity<Luma>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .when(!view.unplaced.is_empty(), |d| {
            d.child(float::label("Unplaced fixtures"))
        })
        .children(view.unplaced.iter().map(|(node, label)| {
            let app = app.clone();
            let node = node.clone();
            let label = label.clone();
            let shown = label.clone();
            float::btn(format!("Place {shown}"), format!("place-{node}"))
                .id(SharedString::from(format!("place-{node}")))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.stage_take(
                            crate::stage::hand::Holding::Unplaced {
                                node: node.clone(),
                                label: label.clone(),
                            },
                            cx,
                        )
                    })
                })
                .agent_node(Role::Button, format!("Place {shown}"))
        }))
        .into_any_element()
}
