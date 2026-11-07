//! The menu an empty place's "+" opens in Setup: a new pattern, from a
//! preset, lighting the group chosen here; or a group cell on the group
//! chosen, a list of colors or of numbers, a fader of its own, or a
//! utility: the master, tap, Select or Clear.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, SharedString};
use luma_lib::models::perform::{CellContent, ListValue};
use luma_ui::float::{self, Dismiss, RowState};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{glass, ladder, radius};

use super::surface::Edit;
use super::{group_label, Perform};
use crate::workspace::Target;
use crate::Luma;

const WIDTH: f32 = 380.;

/// A new list of `values`, fading over a beat, setting no targets.
fn list(label: &str, values: Vec<ListValue>) -> CellContent {
    CellContent::List {
        label: label.into(),
        values,
        fade: 1.,
        unit: luma_patterns::press::TimeUnit::Beats,
        targets: Vec::new(),
        step: None,
    }
}

/// A new color list: a warm white, red, amber and blue. Until it has
/// targets, it sets the base color.
fn colors() -> CellContent {
    let values = ["#ffd9a8", "#ff2a1a", "#ffa000", "#2050ff"]
        .into_iter()
        .filter_map(luma_patterns::color_space::from_hex)
        .map(ListValue::Color)
        .collect();
    list("Colors", values)
}

/// A new number list: a quarter, a half and all the way, to move what it
/// is given.
fn numbers() -> CellContent {
    let values = [0.25, 0.5, 1.].map(ListValue::Number).to_vec();
    list("Numbers", values)
}

/// The menu, hung off the place it fills. A press outside closes it.
pub(super) fn menu(state: &Perform, target: &Target, app: &Entity<Luma>) -> AnyElement {
    let Some(adding) = state.surface.adding.as_ref() else {
        return div().into_any_element();
    };
    let edit = |edit: Edit| {
        let (app, target) = (app.clone(), target.clone());
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| this.edit_surface(&target, edit.clone(), cx));
        }
    };
    let groups = std::iter::once("all".to_owned())
        .chain(state.groups.iter().cloned())
        .map(|group| {
            let chosen = adding.group == group;
            let label = if group == "all" {
                "All".to_owned()
            } else {
                group_label(&group)
            };
            let key = format!("perform-add-group-{group}");
            float::nav_row(RowState::of(chosen, false), key.clone())
                .id(SharedString::from(key))
                .text_color(ladder::foreground_alpha(if chosen { 1. } else { 0.65 }))
                .child(label.clone())
                .on_click(edit(Edit::AddingGroup(group)))
                .agent_node(Role::Toggle, format!("Group {label}"))
                .agent_focused(chosen)
        });
    let presets = luma_patterns::presets().clips.iter().map(|preset| {
        let key = format!("perform-add-preset-{}", preset.name);
        let (app, target) = (app.clone(), target.clone());
        div()
            .id(SharedString::from(key))
            .w(px((WIDTH - 28. - 18.) / 4.))
            .p(px(6.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(radius::ROW))
            .bg(glass::ink(0.05))
            .cursor_pointer()
            .hover(|style| style.bg(glass::glass_hover()))
            .child(
                div()
                    .h(px(4.))
                    .rounded_full()
                    .bg(ladder::pattern(&preset.name)),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(preset.name.clone()),
            )
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| this.add_perform_pattern(&target, preset, cx));
            })
            .agent_node(Role::Button, preset.name.clone())
    });
    let other = |label: &'static str, content: CellContent| {
        let (app, target) = (app.clone(), target.clone());
        let key = format!("perform-add-{label}");
        float::btn(label, key.clone())
            .id(SharedString::from(key))
            .on_click(move |_, _, cx| {
                let content = content.clone();
                app.update(cx, |this, cx| this.add_cell(&target, content, cx));
            })
            .agent_node(Role::Button, label)
    };
    let browse = {
        let (app, target) = (app.clone(), target.clone());
        float::btn("Browse with previews…", "perform-add-browse")
            .id("perform-add-browse")
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| this.browse_perform_presets(&target, cx));
            })
            .agent_node(Role::Button, "Browse with previews")
    };
    let close = {
        let (app, target) = (app.clone(), target.clone());
        move |_: &mut gpui::Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                this.edit_surface(&target, Edit::Adding(None), cx)
            });
        }
    };
    let card = div()
        .w(px(WIDTH))
        .p(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("New pattern"),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(ladder::foreground_alpha(0.5))
                        .child("It lives in this cell. Change anything later in its editor."),
                ),
        )
        .child(float::label("Group"))
        .child(div().flex().flex_wrap().gap(px(4.)).children(groups))
        .child(
            div()
                .id("perform-add-presets")
                .max_h(px(232.))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(presets),
        )
        .child(div().flex().child(browse))
        .child(div().h(px(1.)).bg(glass::hairline(0.08)))
        .child(float::label("Or put something else here"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .child(other(
                    "Group",
                    CellContent::Group {
                        label: String::new(),
                        group: adding.group.clone(),
                    },
                ))
                .child(other("Colors", colors()))
                .child(other("Numbers", numbers()))
                .child(other(
                    "Fader",
                    CellContent::Fader {
                        label: "Fader".into(),
                        targets: Vec::new(),
                    },
                ))
                .child(other("Master", CellContent::Master))
                .child(other("Tap", CellContent::Tap))
                .child(other("Select", CellContent::Select))
                .child(other("Clear", CellContent::Clear)),
        );
    float::anchored_below(
        "perform-add-menu",
        0.,
        Dismiss::on_press_out(close),
        card.into_any_element(),
    )
}
