//! The selection card: the selected piece's height, freedom, angle and span
//! controls, and a row's count and layout when a fixture row is selected.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity};
use luma_ui::float;
use luma_ui::ladder;
use luma_ui::node::{Instrument as _, Role};

use crate::Luma;

use super::build::Build;
use super::hand::{self, Hand};
use super::layer::MAX_RUN_M;
use super::row::{DEFAULT_PITCH_M, MAX_COUNT};

/// Width of a numeric control in the stacked inspector.
const SELECTED_CARD_CONTROL_W: f32 = 174.0;

/// The highest a free placement may be scrubbed to, in metres — a rig's trim,
/// not a building's.
const MAX_TRIM_M: f64 = 8.0;

/// A quiet line of prose in the inspector: what the graph says about the
/// selection, in the graph's own words.
fn note(text: String) -> AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(ladder::foreground_alpha(0.5))
        .child(text.clone())
        .agent_node(Role::Text, text)
        .into_any_element()
}

/// The word an editable freedom goes by in the inspector.
///
/// The graph's parameter keys are `u`, `yaw`, `trim` — the names the solver
/// argues in. A person adjusting a light is sliding it, turning it or lifting
/// it, so the row is titled for the gesture and the key stays in the model.
fn axis_title(key: &str) -> &'static str {
    match key {
        "u" => "Slide",
        "yaw" => "Turn",
        _ => "Trim",
    }
}

/// Object controls belong to the scene, beside the selected object.
pub(crate) fn selection_controls(build: &Build, app: &Entity<Luma>) -> Option<AnyElement> {
    if !matches!(build.hand, Hand::Idle) {
        return None;
    }
    let selected = build.selected_view()?;
    let mut card = div().flex().flex_col().w_full().gap(px(6.0));
    let duplicate = app.clone();
    let remove = app.clone();
    card = card.child(
        div().w_full().child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(12.0))
                .text_color(ladder::foreground())
                .child(selected.label.clone())
                .agent_node(Role::Text, selected.label.clone()),
        ),
    );
    let is_light = build
        .graph
        .node(&selected.node)
        .is_some_and(|node| node.kind == luma_scene::venue::NodeKind::Fixture);
    let actions = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .when(is_light, |actions| {
            let pick_up = app.clone();
            actions.child(
                float::btn("Move", "selection-move")
                    .id("selection-move")
                    .on_click(move |_, _, cx| pick_up.update(cx, |this, cx| this.stage_move(cx)))
                    .agent_node(Role::Button, "Move light"),
            )
        })
        .child(
            float::btn("Duplicate", "selection-duplicate")
                .id("selection-duplicate")
                .on_click(move |_, _, cx| duplicate.update(cx, |this, cx| this.stage_duplicate(cx)))
                .agent_node(Role::Button, "Duplicate element"),
        )
        .child(
            float::btn("Remove", "selection-remove")
                .id("selection-remove")
                .on_click(move |_, _, cx| remove.update(cx, |this, cx| this.stage_delete(cx)))
                .agent_node(Role::Button, "Remove element"),
        );
    // A fixture clicked out of a row selected the row, and the row
    // is what this card edits: count and layout re-run the same
    // `distribute` the configure popover committed, live.
    if build.distribution.len() > 1 && build.distribution.contains(&selected.node) {
        let n = build.distribution.len();
        card = card.child(
            div()
                .text_size(px(11.0))
                .text_color(ladder::foreground_alpha(0.45))
                .child(format!("Row of {n}"))
                .agent_node(Role::Text, format!("Row of {n}")),
        );
        let member = selected.node.clone();
        let set_count = app.clone();
        card = card.child(float::inline_field_row(
            "Count",
            float::scrub(
                "stage-row-count",
                n as f64,
                1.0,
                MAX_COUNT,
                1.0,
                SELECTED_CARD_CONTROL_W,
                move |value, _, cx| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let count = value.round().max(1.0) as usize;
                    if count != n {
                        let member = member.clone();
                        set_count.update(cx, |this, cx| {
                            this.stage_redistribute(member, count, cx);
                        });
                    }
                },
            ),
        ));
        let even_now = matches!(
            build.distribution_layout,
            luma_scene::distribute::Layout::Even
        );
        let mut track = float::segmented().w(px(SELECTED_CARD_CONTROL_W));
        for (name, even) in [("Even", true), ("Spacing", false)] {
            let app = app.clone();
            let member = selected.node.clone();
            track = track.child(
                float::segment(name, even_now == even, name)
                    .id(gpui::ElementId::Name(format!("row-layout-{name}").into()))
                    .on_click(move |_, _, cx| {
                        let member = member.clone();
                        app.update(cx, |this, cx| {
                            if let Some(build) = this.build_mut() {
                                build.distribution_layout = if even {
                                    luma_scene::distribute::Layout::Even
                                } else {
                                    luma_scene::distribute::Layout::Spacing(DEFAULT_PITCH_M)
                                };
                            }
                            this.stage_redistribute(member, n, cx);
                        });
                    })
                    .agent_node(Role::Toggle, format!("Row {name}")),
            );
        }
        card = card.child(float::inline_field_row("Layout", track));
    }
    // Show the mount's solved elevation, including its supporting structure.
    let height = build
        .graph
        .edge(&selected.node)
        .and_then(|edge| build.room.socket_world(&selected.node, &edge.my_socket))
        .map(|pose| pose.y);
    if let Some(height) = height {
        let liftable = build.graph.edge(&selected.node).is_some_and(|edge| {
            build
                .room
                .socket(&edge.parent, &edge.their_socket)
                .is_some_and(|host| {
                    host.socket_type.kind() == luma_scene::sockets::SocketKind::Surface
                        && build
                            .room
                            .pose(&edge.parent)
                            .is_some_and(|pose| pose.transform_vector3(host.normal).y.abs() > 0.001)
                })
        });
        let base = height - selected.trim;
        let (surface_name, support) = build
            .surface_below(&selected.node)
            .map(|(id, height)| {
                let label = if id == build.room.root() {
                    "floor".to_string()
                } else {
                    build.label_of(&id)
                };
                (label, height.max(base))
            })
            .unwrap_or_else(|| ("surface".to_string(), base));
        if liftable {
            let preview = app.clone();
            let app = app.clone();
            card = card.child(float::inline_field_row(
                "Height",
                luma_ui::scrub_number::ScrubNumber::new(
                    format!("stage-height-{}", selected.node),
                    height,
                    support..=(base + MAX_TRIM_M).max(support),
                    0.01,
                    SELECTED_CARD_CONTROL_W,
                    "m",
                    move |height, _, cx| {
                        app.update(cx, |this, cx| {
                            this.stage_set_trim((height - base).max(0.0), cx)
                        });
                    },
                )
                .on_preview(move |height, _, cx| {
                    preview.update(cx, |this, cx| {
                        this.stage_preview_trim((height - base).max(0.0), cx)
                    });
                }),
            ));
            card = card.child(note(if (height - support).abs() < 0.005 {
                format!("Above floor · seated on {surface_name}")
            } else {
                format!(
                    "Above floor · {:.2} m above {surface_name}",
                    height - support
                )
            }));
        } else {
            card = card.child(float::inline_field_row(
                "Height",
                div().child(format!("{height:.2} m")),
            ));
            card = card.child(note("Above floor · follows its mount".to_string()));
        }
    }
    let editable = selected
        .freedom
        .param()
        .map(|key| (key, selected.param, if key == "u" { 1.0 } else { 360.0 }));
    if let Some((key, value, max)) = editable {
        let app = app.clone();
        let node = selected.node.clone();
        // The joint says what a turn lands on: 90° at a bolt
        // circle, 5° at a rail post.
        let step = if key == "yaw" {
            selected.roll_step
        } else {
            0.05
        };
        card = card.child(float::inline_field_row(
            axis_title(key),
            float::scrub(
                format!("stage-{key}"),
                value,
                0.0,
                max,
                step,
                SELECTED_CARD_CONTROL_W,
                move |value, _, cx| {
                    let node = node.clone();
                    app.update(cx, |this, cx| {
                        if key == "yaw" {
                            this.stage_set_param(&node, key, value.to_radians(), cx);
                        } else {
                            this.stage_set_param(&node, key, value, cx);
                        }
                    });
                },
            ),
        ));
    }
    if let Some(angle) = selected.angle {
        let app = app.clone();
        let node = selected.node.clone();
        card = card.child(float::inline_field_row(
            "Angle",
            float::scrub(
                "stage-angle",
                angle,
                0.0,
                180.0,
                5.0,
                SELECTED_CARD_CONTROL_W,
                move |value, _, cx| {
                    let node = node.clone();
                    app.update(cx, |this, cx| {
                        this.stage_set_param(&node, "angle", value, cx);
                    });
                },
            ),
        ));
    }
    {
        // Pan turns about the mount normal, tilt leans off it; `[0, 0]` is
        // straight out of the surface it is clamped to.
        for ((key, title, limit), value) in [("pan", "Pan", 180.0), ("tilt", "Tilt", 90.0)]
            .into_iter()
            .zip(selected.aim)
            .filter_map(|(axis, value)| Some((axis, value?)))
        {
            let app = app.clone();
            let node = selected.node.clone();
            card = card.child(float::inline_field_row(
                title,
                float::scrub(
                    format!("stage-{key}"),
                    value,
                    -limit,
                    limit,
                    5.0,
                    SELECTED_CARD_CONTROL_W,
                    move |value, _, cx| {
                        let node = node.clone();
                        app.update(cx, |this, cx| {
                            this.stage_set_param(&node, key, value.to_radians(), cx);
                        });
                    },
                ),
            ));
        }
    }
    if let Some(span) = selected.span {
        let app = app.clone();
        let node = selected.node.clone();
        card = card.child(float::inline_field_row(
            "Span",
            float::scrub(
                "stage-span",
                span,
                hand::LENGTH_STEP_M,
                MAX_RUN_M,
                hand::LENGTH_STEP_M,
                SELECTED_CARD_CONTROL_W,
                move |value, _, cx| {
                    let node = node.clone();
                    app.update(cx, |this, cx| {
                        this.stage_set_param(&node, "span", value, cx);
                    });
                },
            ),
        ));
    }
    card = card
        .children(selected.relation.clone().map(note))
        .children(selected.constraint.clone().map(note));

    Some(
        card.child(actions.mt(px(6.0)))
            .agent_node(Role::Card, "Selection card")
            .into_any_element(),
    )
}
