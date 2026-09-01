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
    // Three states, not two: a tree that could not be read and a venue with no
    // sets in it are different sentences, and the second one is advice.
    let nodes: &[GroupTreeNode] = match (&state.error, state.data.as_ref()) {
        (Some(error), _) => {
            return luma_ui::plate(
                format!("The group tree could not be read: {error}"),
                ladder::danger(),
            )
        }
        (None, None) => {
            return luma_ui::plate("Reading the group tree…", ladder::muted_foreground())
        }
        (None, Some(data)) if data.groups.is_empty() => {
            return luma_ui::plate(
                "No groups yet — a group is derived once a fixture is placed.",
                ladder::muted_foreground(),
            )
        }
        (None, Some(data)) => &data.groups,
    };
    div()
        .id("patch-groups")
        .max_h(px(360.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .children(nodes.iter().map(|node| row(nodes, node)))
        .into_any_element()
}

fn row(nodes: &[GroupTreeNode], node: &GroupTreeNode) -> impl IntoElement {
    let depth = depth_of(nodes, node);
    let count = node.fixtures.len();
    div()
        .pl(px(depth as f32 * 12.0))
        .py(px(3.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(12.5))
                .text_color(ladder::foreground())
                .child(node.label.clone()),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.45))
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
