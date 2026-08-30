//! The derived group tree, read-only.
//!
//! Groups are derived from where fixtures hang, which is the stage page's
//! business — so this panel *shows* the tree and does not edit it. Editing
//! verbs exist (`rename_group_node` and friends) and belong on the surface that
//! moves the structure the names come from; a rename here would be an override
//! minted against a picture the operator cannot see.

use gpui::prelude::*;
use gpui::{div, px, AnyElement};

use luma_lib::models::groups::GroupTreeNode;
use luma_ui::ladder;
use luma_ui::node::{Instrument as _, Role};

use super::Patch;

pub(super) fn groups(state: &Patch) -> AnyElement {
    let nodes: &[GroupTreeNode] = state.data.as_ref().map_or(&[], |data| &data.groups);
    super::section(
        "GROUPS",
        true,
        div()
            .id("patch-groups")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(12.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .when(nodes.is_empty(), |body| {
                body.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::muted_foreground())
                        .child("No groups yet — a group is derived once a fixture is placed.")
                        .agent_node(
                            Role::Text,
                            "No groups yet — a group is derived once a fixture is placed.",
                        ),
                )
            })
            .children(nodes.iter().map(|node| row(nodes, node)))
            .into_any_element(),
    )
}

fn row(nodes: &[GroupTreeNode], node: &GroupTreeNode) -> impl IntoElement {
    let depth = depth_of(nodes, node);
    let count = node.fixtures.len();
    div()
        .pl(px(depth as f32 * 12.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.0))
                .text_color(ladder::foreground_90())
                .child(node.label.clone()),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(10.5))
                .text_color(ladder::muted_foreground())
                .child(count.to_string()),
        )
        .agent_node(Role::Row, format!("{} = {count}", node.name))
}

/// How deep a node sits. Walked rather than stored because the tree arrives
/// flat and parents-first; a depth column would be a second answer to a
/// question `parent_id` already answers.
fn depth_of(nodes: &[GroupTreeNode], node: &GroupTreeNode) -> usize {
    let mut depth = 0;
    let mut parent = node.parent_id.clone();
    while let Some(id) = parent {
        depth += 1;
        parent = nodes
            .iter()
            .find(|candidate| candidate.id == id)
            .and_then(|candidate| candidate.parent_id.clone());
        if depth > nodes.len() {
            break;
        }
    }
    depth
}
