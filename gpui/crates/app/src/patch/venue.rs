//! Compact venue controls beneath the live stage.
use super::Patch;
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

    pub(crate) fn show_patch_details(&mut self, open: bool, cx: &mut gpui::Context<Self>) {
        if let Some(Body::Patch(state)) = self.workspace.active_body_mut() {
            state.details_open = open;
            state.panel = None;
            state.mode_menu = None;
            state.menu = None;
        }
        cx.notify();
    }
}

pub(super) fn render(
    state: &Patch,
    app: &Entity<Luma>,
    selection: Option<AnyElement>,
    window: &Window,
) -> AnyElement {
    let add = app.clone();
    let venue = state.venue_id.clone();
    let details = app.clone();
    let mut body = div()
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground());
    // The detail sheet replaces the lower region's pointer plane while the
    // stage remains available. Do not mount a second set of editable cells.
    if state.details_open {
        let close = app.clone();
        return body
            .child(
                div().flex().justify_end().px(px(16.0)).py(px(6.0)).child(
                    float::btn("Close patch details", "close-patch-details")
                        .id("close-patch-details")
                        .on_click(move |_, _, cx| {
                            close.update(cx, |this, cx| this.show_patch_details(false, cx))
                        })
                        .agent_node(Role::Button, "Close patch details"),
                ),
            )
            .child(super::details(state, app, window))
            .agent_node(Role::Card, "Patch details sheet")
            .into_any_element();
    }
    body = body
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .px(px(16.0))
                .py(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .child(format!("Fixtures · {}", state.rows().len())),
                )
                .child(
                    float::btn("Add fixtures", "venue-add-fixtures")
                        .id("venue-add-fixtures")
                        .on_click(move |_, window, cx| {
                            add.update(cx, |this, cx| {
                                this.open_add_fixtures(venue.clone(), window, cx)
                            })
                        })
                        .agent_node(Role::Button, "Add fixtures"),
                )
                .child(
                    float::btn("Patch details", "venue-details")
                        .id("venue-details")
                        .on_click(move |_, _, cx| {
                            details.update(cx, |this, cx| this.show_patch_details(true, cx))
                        })
                        .agent_node(Role::Button, "Patch details"),
                ),
        )
        .child(super::groups::strip(state, app));
    if state.group_editor.is_some() {
        body = body.child(super::groups::editor(state, app));
    } else {
        let fixture = (state.selected.len() == 1)
            .then(|| state.selected.iter().next())
            .flatten()
            .and_then(|id| state.row(id));
        if let Some(fixture) = fixture {
            body = body.child(super::table::inspector(
                state, fixture, selection, app, window,
            ));
        } else if let Some(selection) = selection {
            body = body.child(div().flex_none().px(px(16.0)).pb(px(8.0)).child(selection));
        }
        if let Some(error) = &state.error {
            body = body.child(luma_ui::plate(error.clone(), ladder::danger()));
        }
        if state.data.is_none() && state.error.is_none() {
            body = body.child(float::empty_row("Loading fixtures…"));
        }
        body = body.child(fixtures(state, app));
    }
    body.children(
        state
            .mode_menu
            .as_ref()
            .map(|(id, at)| super::table::mode_menu(state, id, *at, app)),
    )
    .agent_node(Role::Card, format!("{} Venue", state.venue_name))
    .into_any_element()
}

fn fixtures(state: &Patch, app: &Entity<Luma>) -> AnyElement {
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
            .on_mouse_down(gpui::MouseButton::Left, move |event, _, cx| {
                click.update(cx, |this, cx| {
                    this.pick_patch_row(
                        venue.clone(),
                        id.clone(),
                        event.modifiers.shift || event.modifiers.platform,
                        cx,
                    )
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
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
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
