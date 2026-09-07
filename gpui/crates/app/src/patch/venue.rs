//! Compact venue controls beneath the live stage.
use super::Patch;
use crate::{shell::Body, Luma};
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, Window};
use luma_ui::node::{Instrument as _, Role};
use luma_ui::{float, ladder};

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

pub(super) fn render(state: &Patch, app: &Entity<Luma>, window: &Window) -> AnyElement {
    let add = app.clone();
    let venue = state.venue_id.clone();
    let details = app.clone();
    let body = div()
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
    let lights = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .child(format!("Fixtures · {}", state.rows().len())),
                )
                .child(
                    float::btn("Add", "venue-add-fixtures")
                        .id("venue-add-fixtures")
                        .on_click(move |_, window, cx| {
                            add.update(cx, |this, cx| {
                                this.open_add_fixtures(venue.clone(), window, cx)
                            })
                        })
                        .agent_node(Role::Button, "Add fixtures"),
                )
                .child(
                    float::btn("Patch", "venue-details")
                        .id("venue-details")
                        .on_click(move |_, _, cx| {
                            details.update(cx, |this, cx| this.show_patch_details(true, cx))
                        })
                        .agent_node(Role::Button, "Patch details"),
                ),
        );
    let lights = lights.child(super::table::compact(state, app, window));
    body.child(
        div()
            .size_full()
            .flex()
            .child(lights.agent_node(Role::Card, "Venue fixtures"))
            .child(super::groups::panel(state, app)),
    )
    .children(
        state
            .mode_menu
            .as_ref()
            .map(|(id, at)| super::table::mode_menu(state, id, *at, app)),
    )
    .agent_node(Role::Card, format!("{} Venue", state.venue_name))
    .into_any_element()
}
