//! The controllers' lights: the surface's looks, drawn through each
//! controller's profile, sent to the MIDI output of its port.
//!
//! A frame draws every bound control of every connected controller and
//! sends only what changed since that control was last drawn, so a light
//! someone set by hand stays until its control's look changes. A control
//! that leaves the surface goes dark. Frames come [`EVERY`] on a task of
//! their own: the frame path and the input threads never wait for one.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Address, Binding, Controller, Shared};
use crate::controls::{Light, Shine, Surface};

/// How often the lights are drawn.
pub(super) const EVERY: Duration = Duration::from_millis(40);
/// How long a blink drawn here is lit, then dark.
const BLINK: Duration = Duration::from_millis(250);

/// What was last drawn on each control of each controller.
#[derive(Default)]
pub(super) struct Lights {
    drawn: HashMap<(String, Address), Vec<u8>>,
    /// The ports open at the last frame: one that comes back is drawn whole.
    open: BTreeSet<String>,
}

impl Lights {
    /// The messages that bring every connected controller to `surface`,
    /// each with the port it goes to: what changed, and dark for what left.
    /// `ports` are the MIDI ports open now; `phase` whether a blink drawn
    /// here is lit.
    pub(super) fn frame(
        &mut self,
        controllers: &[Controller],
        bindings: &[Binding],
        surface: &Surface,
        ports: &[String],
        phase: bool,
    ) -> Vec<(String, Vec<u8>)> {
        let open: BTreeSet<String> = ports.iter().cloned().collect();
        let connected: HashMap<&str, &Controller> = controllers
            .iter()
            .filter(|controller| open.contains(&controller.port))
            .map(|controller| (controller.id.as_str(), controller))
            .collect();
        // A port that came back, or a controller no longer connected, is
        // drawn from nothing.
        let came: BTreeSet<&String> = open.difference(&self.open).collect();
        self.drawn.retain(|(id, _), _| {
            connected
                .get(id.as_str())
                .is_some_and(|controller| !came.contains(&controller.port))
        });
        self.open = open;
        let mut sent = Vec::new();
        let mut seen = HashSet::new();
        for binding in bindings {
            let Some(controller) = connected.get(binding.controller_id.as_str()) else {
                continue;
            };
            let Some(look) = surface.get(&binding.control) else {
                continue;
            };
            let key = (binding.controller_id.clone(), binding.address.clone());
            // One address bound to two controls shows the first.
            if !seen.insert(key.clone()) {
                continue;
            }
            let Some(message) = controller.profile.draw(&binding.address, look, phase) else {
                continue;
            };
            if self.drawn.get(&key) != Some(&message) {
                sent.push((controller.port.clone(), message.clone()));
                self.drawn.insert(key, message);
            }
        }
        let gone: Vec<(String, Address)> = self
            .drawn
            .keys()
            .filter(|key| !seen.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            self.drawn.remove(&key);
            let controller = connected[key.0.as_str()];
            if let Some(dark) = controller.profile.show(&key.1, DARK, phase) {
                sent.push((controller.port.clone(), dark));
            }
        }
        sent
    }
}

/// A light that shows nothing.
const DARK: Light = Light {
    color: [0.; 3],
    bright: false,
    shine: Shine::Steady,
};

/// Draw the open venue's lights on every connected controller, until the
/// task is aborted.
pub(super) async fn run(shared: Arc<Shared>) {
    let mut lights = Lights::default();
    let mut ticks = tokio::time::interval(EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let start = Instant::now();
    loop {
        ticks.tick().await;
        let (venue, controllers, bindings, ports) = {
            let state = shared.lock();
            (
                state.venue_id.clone(),
                state.controllers.clone(),
                state.bindings.clone(),
                state.midi_ports.clone(),
            )
        };
        let now = Instant::now();
        let surface = match &venue {
            Some(venue) => shared.controls.venue(venue).surface_at(now),
            None => Surface::default(),
        };
        let phase = (now - start).as_millis() / BLINK.as_millis() % 2 == 0;
        for (port, message) in lights.frame(&controllers, &bindings, &surface, &ports, phase) {
            super::midi::send(&port, &message);
        }
    }
}

#[cfg(test)]
mod tests;
