//! Where each universe comes out: a table, not arithmetic.
//!
//! A route names one device port — an Art-Net node's own announced port
//! address, a USB dongle's output — so universes 1 and 17 resolve to different
//! rows by construction rather than by a wider mask. A universe may go to
//! several ports, and a universe with none is sent nowhere, which the row says
//! out loud.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, SharedString};

use luma_lib::models::patch::{DeviceKind, DmxRoute, OutputDevice, OutputPort};
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
        .child(found(state))
        .into_any_element()
}

/// One route as a person reads it: the device by name, then the port in the
/// device's own terms. A device the drivers cannot see right now keeps the
/// name it was bound under and says it is not seen.
fn route_label(route: &DmxRoute, devices: &[OutputDevice]) -> String {
    let live = devices
        .iter()
        .find(|d| d.kind == route.device_kind && d.id == route.device_id);
    let name = live
        .map(|d| d.name.clone())
        .or_else(|| route.device_name.clone())
        .unwrap_or_else(|| route.device_id.clone());
    let port = live
        .and_then(|d| d.ports.iter().find(|p| p.port == route.port))
        .map(|p| p.label.clone())
        .unwrap_or_else(|| match route.device_kind {
            DeviceKind::ArtNet if live.is_some_and(|d| d.any_port) => {
                format!("universe {}", route.port)
            }
            DeviceKind::ArtNet => format!(
                "{}.{}.{}",
                route.port >> 8,
                (route.port >> 4) & 0xF,
                route.port & 0xF
            ),
            DeviceKind::Usb => format!("output {}", route.port + 1),
        });
    match live {
        Some(_) => format!("{name} · {port}"),
        None => format!("{name} · {port} · not seen"),
    }
}

fn row(state: &Patch, universe: i64, app: &Entity<Luma>) -> AnyElement {
    let routes: Vec<&DmxRoute> = state
        .data
        .as_ref()
        .map(|data| {
            data.routes
                .iter()
                .filter(|r| r.universe == universe)
                .collect()
        })
        .unwrap_or_default();
    let labels: Vec<String> = routes
        .iter()
        .map(|route| route_label(route, &state.devices))
        .collect();
    let reading: SharedString = if labels.is_empty() {
        format!("Universe {universe} → Not bound").into()
    } else {
        format!("Universe {universe} → {}", labels.join(", ")).into()
    };
    let menu_open = state.bind_menu == Some(universe);
    let opener = app.clone();
    let venue = state.venue_id.clone();

    let mut line = div().flex().flex_col().gap(px(3.0)).child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.5))
                    .text_color(ladder::foreground())
                    .child(format!("Universe {universe}")),
            )
            .child(
                luma_ui::float::btn("Add output", format!("output-{universe}"))
                    .id(SharedString::from(format!("output-{universe}")))
                    .on_click(move |_, _, cx| {
                        let venue = venue.clone();
                        opener.update(cx, |this, cx| this.open_bind_menu(venue, universe, cx));
                    })
                    .agent_node(Role::Button, format!("Add output to universe {universe}")),
            ),
    );

    if routes.is_empty() {
        line = line.child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::status_warn())
                .child("Not sent anywhere")
                .agent_node(
                    Role::Text,
                    format!("Universe {universe} is not sent anywhere"),
                ),
        );
    }
    for (route, label) in routes.into_iter().zip(labels) {
        line = line.child(destination(state, route, label, app));
    }
    if menu_open {
        line = line.child(device_list(state, universe, app));
    }
    line.agent_node(Role::Row, reading).into_any_element()
}

/// One place a universe goes, with the gesture that stops it.
fn destination(state: &Patch, route: &DmxRoute, label: String, app: &Entity<Luma>) -> AnyElement {
    let universe = route.universe;
    let key = format!(
        "unbind-{universe}-{}-{}-{}",
        route.device_kind.as_str(),
        route.device_id,
        route.port
    );
    let action = app.clone();
    let venue = state.venue_id.clone();
    let chosen = route.clone();
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.55))
                .child(label.clone()),
        )
        .child(
            luma_ui::float::btn("Unbind", key.clone())
                .id(SharedString::from(key))
                .on_click(move |_, _, cx| {
                    let venue = venue.clone();
                    let chosen = chosen.clone();
                    action.update(cx, |this, cx| this.unbind_universe(venue, chosen, cx));
                })
                .agent_node(
                    Role::Button,
                    format!("Unbind universe {universe} from {label}"),
                ),
        )
        .into_any_element()
}

/// Every device port the universe could go to and does not already.
fn device_list(state: &Patch, universe: i64, app: &Entity<Luma>) -> AnyElement {
    let bound = |device: &OutputDevice, port: i64| {
        state.data.as_ref().is_some_and(|data| {
            data.routes.iter().any(|r| {
                r.universe == universe
                    && r.device_kind == device.kind
                    && r.device_id == device.id
                    && r.port == port
            })
        })
    };
    let mut choices: Vec<AnyElement> = Vec::new();
    for device in &state.devices {
        if let Some(problem) = &device.problem {
            let message = format!("{}: {problem}", device.name);
            choices.push(
                div()
                    .text_size(px(11.0))
                    .text_color(ladder::status_warn())
                    .child(message.clone())
                    .agent_node(Role::Text, message)
                    .into_any_element(),
            );
            continue;
        }
        // Broadcast takes any port address; the universe's own number is the
        // one a node listening for it expects.
        let ports = if device.any_port {
            vec![OutputPort {
                port: universe,
                label: format!("universe {universe}"),
            }]
        } else {
            device.ports.clone()
        };
        for port in ports {
            if bound(device, port.port) {
                continue;
            }
            choices.push(choice(state, universe, device, port, app));
        }
    }
    if choices.is_empty() {
        let message = state
            .devices_error
            .clone()
            .unwrap_or_else(|| "No other device port to send it to.".into());
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
        .children(choices)
        .into_any_element()
}

fn choice(
    state: &Patch,
    universe: i64,
    device: &OutputDevice,
    port: OutputPort,
    app: &Entity<Luma>,
) -> AnyElement {
    let key = format!(
        "bind-{universe}-{}-{}-{}",
        device.kind.as_str(),
        device.id,
        port.port
    );
    let label = format!("{} · {} · {}", device.name, device.address, port.label);
    let picked = app.clone();
    let venue = state.venue_id.clone();
    let device = device.clone();
    div()
        .id(SharedString::from(key.clone()))
        .text_size(px(11.0))
        .text_color(ladder::foreground_90())
        .bg(luma_ui::motion::hover_blend(
            &key,
            luma_ui::glass::wash(0.),
            ladder::hover().into(),
        ))
        .on_hover(luma_ui::motion::hover_listener(key.clone()))
        .child(label.clone())
        .on_click(move |_, _, cx| {
            let venue = venue.clone();
            let device = device.clone();
            let port = port.clone();
            picked.update(cx, |this, cx| {
                this.bind_universe(venue, universe, device, port, cx);
            });
        })
        .agent_node(Role::Button, label)
        .into_any_element()
}

/// What the drivers can see, or why they see nothing. A host with no output
/// running, output turned off, and a room with nothing plugged in are three
/// different answers, and this says which.
fn found(state: &Patch) -> AnyElement {
    // Broadcast is listed whenever Art-Net is running, so it is not a find.
    let real = state.devices.iter().filter(|d| !d.any_port).count();
    let message: SharedString = match (&state.devices_error, state.devices.len(), real) {
        // The seam's own sentence, unprefixed: it already says the host has
        // no output, and a second clause in front of it would say it twice.
        (Some(error), _, _) => error.clone(),
        (None, 0, _) => "DMX output is off. Turn it on in Settings → DMX Output.".into(),
        (None, _, 0) => "No DMX device found yet.".into(),
        (None, _, count) => format!("{} found", crate::patch::plural(count, "DMX device")).into(),
    };
    div()
        .pt(px(2.0))
        .text_size(px(11.0))
        .text_color(ladder::foreground_alpha(0.45))
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
}
