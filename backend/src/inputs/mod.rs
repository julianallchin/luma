//! Inputs bound to live controls: MIDI and OSC, from any device.
//!
//! A [`Binding`] maps one input address (a MIDI CC or note, an OSC address
//! and argument) to a control of the open venue, with a mode that says what
//! the hardware sends: a fader's position, a pad's press and release, or a
//! latching key's state. How a button plays (toggle or flash, its envelope)
//! is the button's own, in the controls bank, so a pad on screen and one on
//! a controller always agree. Bindings are this machine's, like the devices
//! plugged into it, and are kept per venue in the local-only
//! `control_bindings` table.
//!
//! Every MIDI input port is opened as it appears; OSC is heard on UDP
//! [`OSC_ADDRESS`]. Anything that can send OSC (a phone, TouchOSC, a sensor
//! glove) is an input without anything built for it.

mod midi;
mod osc;
mod store;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::oneshot;

/// Where OSC is heard: every interface, so devices on the network reach it.
pub const OSC_ADDRESS: &str = "0.0.0.0:8000";
/// How long learning waits for an input.
const LEARN_FOR: Duration = Duration::from_secs(20);

/// One input that can move a control.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Address {
    MidiCc {
        channel: u8,
        number: u8,
    },
    MidiNote {
        channel: u8,
        number: u8,
    },
    /// Argument `arg` of messages to `address`: an XY pad or a glove sends
    /// several numbers to one address.
    Osc {
        address: String,
        #[serde(default)]
        arg: u8,
    },
}

/// What the hardware sends, and so how its messages reach the control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A fader or knob: the control follows its position.
    #[default]
    Fader,
    /// A pad that sends a press and a release: each is a press of the
    /// control's button, which plays by its own mode (a toggle flips on the
    /// press, a flash holds until the release). A control no button
    /// declares is 1 while held.
    Momentary,
    /// A key that latches by itself: each message is its new state, on or
    /// off. The control's button is gated to that state whatever its mode,
    /// with its own envelope; any other control is set to 1 or 0.
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: String,
    pub control: String,
    pub address: Address,
    pub mode: Mode,
    /// Reverse the input: a fader's top is 0.
    pub invert: bool,
}

/// One input arriving: its address and its value 0–1.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Event {
    pub address: Address,
    pub value: f64,
}

/// What the inputs are doing, for the Perform screen.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputState {
    pub venue_id: Option<String>,
    /// The MIDI input ports open now.
    pub midi_ports: Vec<String>,
    /// Where OSC is heard, or why it is not.
    pub osc: String,
    /// The control learning waits to bind, if any.
    pub learning: Option<String>,
    /// The last input heard and how long ago, so a person can see their
    /// device reaches Luma.
    pub last: Option<LastInput>,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastInput {
    pub address: Address,
    pub value: f64,
    pub ms_ago: u64,
}

struct Learn {
    control: String,
    reply: oneshot::Sender<Address>,
}

#[derive(Default)]
struct State {
    venue_id: Option<String>,
    bindings: Vec<Binding>,
    learn: Option<Learn>,
    last: Option<(Event, Instant)>,
    midi_ports: Vec<String>,
    osc: String,
}

impl State {
    /// Nothing open yet. A host without devices says so where OSC would.
    fn idle(devices: bool) -> Self {
        Self {
            osc: if devices {
                "Not open yet"
            } else {
                "Off in this host"
            }
            .into(),
            ..Self::default()
        }
    }
}

struct Shared {
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("inputs poisoned")
    }

    /// Apply one input: to learning when it waits, else to every binding of
    /// its address.
    fn receive(&self, event: Event) {
        let mut state = self.lock();
        state.last = Some((event.clone(), Instant::now()));
        if let Some(learn) = state.learn.take() {
            // A pad sends its release as a second event; bind on the press.
            if event.value > 0.0 || matches!(event.address, Address::Osc { .. }) {
                let _ = learn.reply.send(event.address);
            } else {
                state.learn = Some(learn);
            }
            return;
        }
        let Some(venue_id) = state.venue_id.clone() else {
            return;
        };
        let controls = crate::controls::venue(&venue_id);
        let at = Instant::now();
        // A key is down at any value above 0: a pad's note carries how hard
        // it was hit, not whether it is held, and only its release is 0.
        let down = event.value > 0.0;
        for binding in state.bindings.iter().filter(|b| b.address == event.address) {
            match binding.mode {
                Mode::Fader if binding.invert => controls.set(&binding.control, 1.0 - event.value),
                Mode::Fader => controls.set(&binding.control, event.value),
                Mode::Momentary => controls.press(&binding.control, down != binding.invert, at),
                Mode::Toggle => controls.set(
                    &binding.control,
                    f64::from(u8::from(down != binding.invert)),
                ),
            }
        }
    }
}

/// A learn being waited for. However the wait ends (heard, timed out, or
/// dropped by a caller that stopped waiting), the learn is over; a newer one
/// that replaced it is not.
struct Waiting<'a> {
    shared: &'a Shared,
    heard: oneshot::Receiver<Address>,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.heard.close();
        let mut state = self.shared.lock();
        if state
            .learn
            .as_ref()
            .is_some_and(|learn| learn.reply.is_closed())
        {
            state.learn = None;
        }
    }
}

/// The MIDI and OSC listeners and the open venue's bindings.
pub struct InputHub {
    shared: Arc<Shared>,
    pool: SqlitePool,
    /// Whether to open devices and sockets at all. Only the desktop host
    /// does; a harness binds and applies, and hears nothing.
    devices: bool,
    listeners: tokio::sync::Mutex<Option<Listeners>>,
}

struct Listeners {
    _midi: midi::Listener,
    _osc: Option<osc::Listener>,
}

impl InputHub {
    pub(crate) fn new(pool: SqlitePool, devices: bool) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::idle(devices)),
            }),
            pool,
            devices,
            listeners: tokio::sync::Mutex::new(None),
        }
    }

    /// Bind inputs to `venue_id`'s controls, starting the listeners the
    /// first time.
    pub(crate) async fn open(&self, venue_id: &str) -> Result<(), String> {
        let bindings = store::list(&self.pool, venue_id).await?;
        {
            let mut state = self.shared.lock();
            state.venue_id = Some(venue_id.to_owned());
            state.bindings = bindings;
        }
        let mut listeners = self.listeners.lock().await;
        if self.devices && listeners.is_none() {
            let shared = self.shared.clone();
            let midi = midi::listen(
                {
                    let shared = shared.clone();
                    move |event| shared.receive(event)
                },
                {
                    let shared = shared.clone();
                    move |ports| shared.lock().midi_ports = ports
                },
            );
            let osc = {
                let shared = shared.clone();
                osc::listen(OSC_ADDRESS, move |event| shared.receive(event)).await
            };
            let (osc, status) = match osc {
                Ok(listener) => {
                    let status = format!("Listening on {}", listener.address);
                    (Some(listener), status)
                }
                Err(error) => (None, error),
            };
            shared.lock().osc = status;
            *listeners = Some(Listeners {
                _midi: midi,
                _osc: osc,
            });
        }
        Ok(())
    }

    /// Stop listening and forget the venue: another person signed in.
    pub(crate) async fn close(&self) {
        *self.listeners.lock().await = None;
        *self.shared.lock() = State::idle(self.devices);
    }

    pub(crate) fn state(&self) -> InputState {
        let state = self.shared.lock();
        InputState {
            venue_id: state.venue_id.clone(),
            midi_ports: state.midi_ports.clone(),
            osc: state.osc.clone(),
            learning: state.learn.as_ref().map(|learn| learn.control.clone()),
            last: state.last.as_ref().map(|(event, at)| LastInput {
                address: event.address.clone(),
                value: event.value,
                ms_ago: at.elapsed().as_millis() as u64,
            }),
            bindings: state.bindings.clone(),
        }
    }

    /// Bind the next input heard to `control` of the open venue; an input
    /// it already has is bound again, as `mode`. A newer learn replaces this
    /// one, which then fails.
    pub(crate) async fn learn(&self, control: &str, mode: Mode) -> Result<Binding, String> {
        let (reply, heard) = oneshot::channel();
        let venue_id = {
            let mut state = self.shared.lock();
            let venue_id = state
                .venue_id
                .clone()
                .ok_or("Open a venue's inputs first")?;
            state.learn = Some(Learn {
                control: control.to_owned(),
                reply,
            });
            venue_id
        };
        let mut waiting = Waiting {
            shared: &self.shared,
            heard,
        };
        let address = match tokio::time::timeout(LEARN_FOR, &mut waiting.heard).await {
            Ok(Ok(address)) => address,
            Ok(Err(_)) => return Err("Learning was replaced or cancelled".into()),
            Err(_) => {
                return Err(
                    "Nothing was heard. Move a fader or press a pad, then try again".into(),
                );
            }
        };
        let mut binding = Binding {
            id: uuid::Uuid::new_v4().to_string(),
            control: control.to_owned(),
            address,
            mode,
            invert: false,
        };
        binding.id = store::upsert(&self.pool, &venue_id, &binding).await?;
        self.reload(&venue_id).await?;
        Ok(binding)
    }

    /// Stop waiting for an input to learn.
    pub(crate) fn cancel_learn(&self) {
        self.shared.lock().learn = None;
    }

    pub(crate) async fn update(
        &self,
        venue_id: &str,
        id: &str,
        mode: Mode,
        invert: bool,
    ) -> Result<(), String> {
        store::update(&self.pool, venue_id, id, mode, invert).await?;
        self.reload(venue_id).await
    }

    pub(crate) async fn unbind(&self, venue_id: &str, id: &str) -> Result<(), String> {
        store::delete(&self.pool, venue_id, id).await?;
        self.reload(venue_id).await
    }

    async fn reload(&self, venue_id: &str) -> Result<(), String> {
        let bindings = store::list(&self.pool, venue_id).await?;
        let mut state = self.shared.lock();
        if state.venue_id.as_deref() == Some(venue_id) {
            state.bindings = bindings;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn receive(&self, event: Event) {
        self.shared.receive(event);
    }
}

#[cfg(test)]
mod tests;
