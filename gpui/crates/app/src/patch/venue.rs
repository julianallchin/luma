//! The venue page beneath the live stage: fixtures on the left, groups on the
//! right, one selection between them and the picture above.
use super::Patch;
use crate::Luma;
use gpui::prelude::*;
use gpui::{div, AnyElement, Entity};
use luma_ui::ladder;
use luma_ui::node::{Instrument as _, Role};

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
}

pub(super) fn render(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let fixtures = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(super::band(state, app))
        .child(super::table::table(state, app))
        .agent_node(Role::Card, "Venue fixtures");
    div()
        .size_full()
        .relative()
        .flex()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(fixtures)
        .child(super::groups::panel(state, app))
        .children(
            state
                .menu
                .as_ref()
                .map(|(fixture, at)| super::table::row_menu(state, fixture, *at, app)),
        )
        .children(
            state
                .mode_menu
                .as_ref()
                .map(|(fixture, at)| super::table::mode_menu(state, fixture, *at, app)),
        )
        .agent_node(Role::Card, format!("{} Venue", state.venue_name))
        .into_any_element()
}
