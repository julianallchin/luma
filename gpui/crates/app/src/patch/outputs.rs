//! Where each universe comes out: a table, not arithmetic.
//!
//! `(net << 8) | (subnet << 4) | (universe & 0xF)` aliases universe 17 onto
//! universe 1 and cannot name a second node at all. A row names one node and
//! carries that node's **own** announced port address, so 1 and 17 resolve to
//! different rows by construction rather than by a wider mask.
//!
//! A universe with no row is drawn as exactly that — unbound, falling back to
//! the arithmetic — because a silent fallback is how the aliasing went
//! unnoticed for as long as it did.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, SharedString};

use luma_ui::ladder;
use luma_ui::node::{Instrument as _, Role};

use super::Patch;
use crate::Luma;

pub(super) fn outputs(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let Some(data) = state.data.as_ref() else {
        return div().into_any_element();
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .children(
            data.universes
                .iter()
                .map(|universe| row(state, i64::from(*universe), app)),
        )
        .when(data.universes.is_empty(), |body| {
            body.child(
                div()
                    .text_size(px(12.0))
                    .text_color(ladder::foreground_alpha(0.55))
                    .child("No universe is patched yet.")
                    .agent_node(Role::Text, "No universe is patched yet."),
            )
        })
        .children(discovery(state))
        .into_any_element()
}

fn row(state: &Patch, universe: i64, app: &Entity<Luma>) -> AnyElement {
    let bound = state
        .data
        .as_ref()
        .and_then(|data| data.outputs.iter().find(|out| out.universe == universe));
    let destination: SharedString = match bound {
        Some(out) => match out.node_name.as_deref() {
            Some(name) => format!("{name} · {} · port {}", out.node_ip, out.port_address).into(),
            None => format!("{} · port {}", out.node_ip, out.port_address).into(),
        },
        None => "Not bound".into(),
    };
    let reading: SharedString = format!("Universe {universe} → {destination}").into();
    let action = app.clone();
    let venue = state.venue_id.clone();
    let bound_here = bound.is_some();
    let menu_open = state.bind_menu == Some(universe);

    let mut line = div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(ladder::foreground())
                                .child(format!("Universe {universe}")),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.5))
                                .text_color(if bound_here {
                                    ladder::foreground_alpha(0.55)
                                } else {
                                    ladder::status_warn().into()
                                })
                                .child(destination),
                        ),
                )
                .child(
                    luma_ui::float::btn(
                        if bound_here { "Unbind" } else { "Bind" },
                        format!("output-{universe}"),
                    )
                    .id(SharedString::from(format!("output-{universe}")))
                    .on_click(move |_, _, cx| {
                        let venue = venue.clone();
                        action.update(cx, |this, cx| {
                            if bound_here {
                                this.unbind_universe(venue, universe, cx);
                            } else {
                                this.open_bind_menu(venue, universe, cx);
                            }
                        });
                    })
                    .agent_node(
                        Role::Button,
                        format!(
                            "{} universe {universe}",
                            if bound_here { "Unbind" } else { "Bind" }
                        ),
                    ),
                ),
        )
        // Named, not silent: an unbound universe still sends, through the
        // arithmetic this table replaced, and that arithmetic puts 17 and 1 on
        // the same wire. Universes 0–15 are their own alias, though — saying
        // "aliases it onto 1" under universe 1 is a warning about nothing.
        .when(!bound_here && universe & 0xF != universe, |line| {
            let warning = format!(
                "Falls back to net/subnet arithmetic, which aliases it onto {}",
                universe & 0xF
            );
            line.child(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::foreground_alpha(0.45))
                    .child(warning.clone())
                    .agent_node(
                        Role::Text,
                        format!("Universe {universe} has no node: {warning}"),
                    ),
            )
        });

    if menu_open {
        line = line.child(node_list(state, universe, app));
    }
    line.agent_node(Role::Row, reading).into_any_element()
}

/// The discovered nodes, as the choices for one universe.
fn node_list(state: &Patch, universe: i64, app: &Entity<Luma>) -> AnyElement {
    if state.nodes.is_empty() {
        let message = state
            .discovery_error
            .clone()
            .unwrap_or_else(|| "No Art-Net node has answered a poll yet.".into());
        return div()
            .pl(px(8.0))
            .text_size(px(11.5))
            .text_color(ladder::foreground_alpha(0.45))
            .child(message.clone())
            .agent_node(Role::Text, message)
            .into_any_element();
    }
    div()
        .pl(px(8.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .children(state.nodes.iter().map(|node| {
            let picked = app.clone();
            let venue = state.venue_id.clone();
            let chosen = node.clone();
            let label = format!(
                "{} · {} · port {}",
                node.ip, node.long_name, node.port_address
            );
            div()
                .id(SharedString::from(format!("bind-{universe}-{}", node.ip)))
                .text_size(px(11.0))
                .text_color(ladder::foreground_90())
                .hover(|row| row.bg(ladder::hover()))
                .child(label.clone())
                .on_click(move |_, _, cx| {
                    let venue = venue.clone();
                    let chosen = chosen.clone();
                    picked.update(cx, |this, cx| {
                        this.bind_universe(venue, universe, chosen, cx);
                    });
                })
                .agent_node(Role::Button, label)
        }))
        .into_any_element()
}

/// Why the network is empty, when it is. A host with no Art-Net running at all
/// and a network with no nodes on it are different answers, and this says
/// which — the first in the seam's words, the second in this page's.
fn discovery(state: &Patch) -> Option<AnyElement> {
    let message: SharedString = match (&state.discovery_error, state.nodes.len()) {
        // The seam's own sentence, unprefixed: it already says the host has no
        // Art-Net, and a second clause in front of it would say it twice.
        (Some(error), _) => error.clone(),
        (None, 0) => "No Art-Net node has answered a poll yet.".into(),
        (None, count) => {
            format!("{} discovered", crate::patch::plural(count, "Art-Net node")).into()
        }
    };
    Some(
        div()
            .pt(px(2.0))
            .text_size(px(11.0))
            .text_color(ladder::foreground_alpha(0.45))
            .child(message.clone())
            .agent_node(Role::Text, message)
            .into_any_element(),
    )
}
