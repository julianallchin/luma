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
        return super::section(
            "OUTPUTS",
            false,
            div().px(px(12.0)).pb(px(12.0)).into_any_element(),
        );
    };
    super::section(
        "OUTPUTS",
        false,
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(12.0))
            .pb(px(12.0))
            .children(
                data.universes
                    .iter()
                    .map(|universe| row(state, i64::from(*universe), app)),
            )
            .when(data.universes.is_empty(), |body| {
                body.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(ladder::muted_foreground())
                        .child("No universe is patched yet.")
                        .agent_node(Role::Text, "No universe is patched yet."),
                )
            })
            .children(discovery(state))
            .into_any_element(),
    )
}

fn row(state: &Patch, universe: i64, app: &Entity<Luma>) -> AnyElement {
    let bound = state
        .data
        .as_ref()
        .and_then(|data| data.outputs.iter().find(|out| out.universe == universe));
    let reading: SharedString = match bound {
        Some(out) => format!(
            "Universe {universe} → {}:{} port {}",
            out.node_ip, out.node_port, out.port_address
        )
        .into(),
        None => format!("Universe {universe} → unbound").into(),
    };
    let action = app.clone();
    let venue = state.venue_id.clone();
    let bound_here = bound.is_some();
    let menu_open = state.bind_menu == Some(universe);

    let mut line = div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(luma_ui::fonts::MONO)
                        .text_size(px(11.0))
                        .text_color(if bound_here {
                            ladder::foreground_90()
                        } else {
                            ladder::status_warn()
                        })
                        .child(reading.clone()),
                )
                .child(
                    luma_ui::luma_button(
                        if bound_here { "Unbind" } else { "Bind" },
                        luma_ui::Enabled::Yes,
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
        .when(!bound_here, |line| {
            let warning = format!(
                "Universe {universe} has no node: Art-Net falls back to \
                 net/subnet arithmetic, which aliases it onto {}",
                universe & 0xF
            );
            line.child(
                div()
                    .text_size(px(10.5))
                    .text_color(ladder::status_warn())
                    .child(warning.clone())
                    .agent_node(Role::Text, warning),
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
            .text_size(px(10.5))
            .text_color(ladder::muted_foreground())
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

/// Why the network is empty, when it is. A host with no Art-Net at all and a
/// network with no nodes on it are different answers, and this says which.
fn discovery(state: &Patch) -> Option<AnyElement> {
    let message: SharedString = match (&state.discovery_error, state.nodes.len()) {
        (Some(error), _) => format!("Art-Net discovery unavailable: {error}").into(),
        (None, 0) => "No Art-Net node has answered a poll yet.".into(),
        (None, count) => format!("{count} Art-Net nodes discovered").into(),
    };
    Some(
        div()
            .pt(px(4.0))
            .text_size(px(10.5))
            .text_color(ladder::muted_foreground())
            .child(message.clone())
            .agent_node(Role::Text, message)
            .into_any_element(),
    )
}
