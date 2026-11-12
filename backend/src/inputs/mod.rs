//! Inputs bound to live controls: MIDI and OSC, from any device, and the
//! lights that answer them.
//!
//! A venue has any number of [`Controller`]s, synced with it: each is the
//! MIDI port it matches and a [`Profile`] (where its pads and faders are,
//! how it shows a light). A [`Binding`] of a controller maps one input
//! address (a MIDI CC or note, an OSC address and argument) to a control of
//! the open venue, with a mode that says what the hardware sends: a fader's
//! position, a pad's press and release, or a latching key's state. A
//! binding hears its address on its controller's port; a controller with no
//! port hears every input. How a button plays (toggle or flash, its
//! envelope) is the button's own, in the controls bank, so a pad on screen
//! and one on a controller always agree.
//!
//! Every MIDI input port is opened as it appears, with its output beside
//! it; OSC is heard on UDP [`OSC_ADDRESS`]. Anything that can send OSC (a
//! phone, TouchOSC, a sensor glove) is an input without anything built for
//! it. Each controller's lights show the surface's looks
//! ([`crate::controls::Look`]), drawn through its profile ([`lights`]).

mod lights;
mod midi;
mod osc;
pub mod profile;
mod store;

pub use profile::Profile;

use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{oneshot, watch};

use crate::models::perform::{LayoutPage, PerformPattern};
use crate::sync::triggers::Attribution;

/// Where OSC is heard: every interface, so devices on the network reach it.
pub const OSC_ADDRESS: &str = "0.0.0.0:8000";
/// How long learning waits for an input.
const LEARN_FOR: Duration = Duration::from_secs(20);

/// The synced tables the hub reads its venue from.
pub const TABLES: &[&str] = &["perform_controllers", "control_bindings"];

/// Moves when sync writes [`TABLES`]: every hub reads its venue again.
static SYNCED: LazyLock<watch::Sender<()>> = LazyLock::new(|| watch::channel(()).0);

/// Rows of [`TABLES`] arrived from another device.
pub fn synced() {
    SYNCED.send_replace(());
}

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
    /// The controller it belongs to.
    pub controller_id: String,
    pub control: String,
    pub address: Address,
    pub mode: Mode,
    /// Reverse the input: a fader's top is 0.
    pub invert: bool,
}

/// A device: the MIDI port it is, and its profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Controller {
    pub id: String,
    pub name: String,
    /// The MIDI input port it matches, by name; empty until one is heard,
    /// and then its bindings hear every input.
    pub port: String,
    pub profile: Profile,
}

impl Controller {
    /// Whether it hears an input from `port` (`None`: OSC).
    fn hears(&self, port: Option<&str>) -> bool {
        self.port.is_empty() || port == Some(self.port.as_str())
    }
}

/// A controller's name, trimmed; refused when empty.
fn named(name: &str) -> Result<String, String> {
    match name.trim() {
        "" => Err("a controller needs a name, such as \"Launchpad\"".into()),
        name => Ok(name.to_owned()),
    }
}

/// Where on a controller a light goes: an address, or a place on its grid.
#[derive(Clone, Debug, PartialEq)]
pub enum Spot {
    Address(Address),
    /// Column and row from the top left, as its profile places them.
    Place(u16, u16),
}

/// A controller as the Perform screen polls it. Its profile stays behind:
/// [`InputHub::controllers`] reads that.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerState {
    pub id: String,
    pub name: String,
    pub port: String,
    /// Whether its port is open now.
    pub connected: bool,
    /// How many pads and faders its profile places.
    pub controls: usize,
}

/// One input arriving: its address, its value 0–1, and the MIDI port it
/// came on (`None` for OSC).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Event {
    pub address: Address,
    pub value: f64,
    pub port: Option<String>,
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
    /// The open venue's controllers, oldest first.
    pub controllers: Vec<ControllerState>,
    /// Every controller's bindings.
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
    reply: oneshot::Sender<Event>,
}

#[derive(Default)]
struct State {
    venue_id: Option<String>,
    controllers: Arc<[Controller]>,
    bindings: Arc<[Binding]>,
    learn: Option<Learn>,
    last: Option<(Event, Instant)>,
    midi_ports: Vec<String>,
    osc: String,
    /// Controllers whose lights the next frame draws from nothing.
    cleared: Vec<String>,
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
    /// The banks an input plays.
    controls: Arc<crate::controls::ControlBanks>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("inputs poisoned")
    }

    /// Apply one input: to learning when it waits, else to every binding of
    /// its address on a controller that hears its port.
    fn receive(&self, event: Event) {
        let mut state = self.lock();
        state.last = Some((event.clone(), Instant::now()));
        if let Some(learn) = state.learn.take() {
            // A pad sends its release as a second event; bind on the press.
            if event.value > 0.0 || matches!(event.address, Address::Osc { .. }) {
                let _ = learn.reply.send(event);
            } else {
                state.learn = Some(learn);
            }
            return;
        }
        let Some(venue_id) = state.venue_id.clone() else {
            return;
        };
        let controls = self.controls.venue(&venue_id);
        let at = Instant::now();
        // A key is down at any value above 0: a pad's note carries how hard
        // it was hit, not whether it is held, and only its release is 0.
        let down = event.value > 0.0;
        let hears = |binding: &Binding| {
            state
                .controllers
                .iter()
                .find(|controller| controller.id == binding.controller_id)
                .is_some_and(|controller| controller.hears(event.port.as_deref()))
        };
        for binding in state
            .bindings
            .iter()
            .filter(|b| b.address == event.address && hears(b))
        {
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
    heard: oneshot::Receiver<Event>,
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
    /// Reads the venue again when sync writes its controllers or bindings.
    follow: OnceLock<tokio::task::AbortHandle>,
}

impl Drop for InputHub {
    fn drop(&mut self) {
        if let Some(follow) = self.follow.get() {
            follow.abort();
        }
    }
}

struct Listeners {
    _midi: midi::Listener,
    _osc: Option<osc::Listener>,
    lights: tokio::task::JoinHandle<()>,
}

impl Drop for Listeners {
    fn drop(&mut self) {
        self.lights.abort();
    }
}

impl InputHub {
    pub(crate) fn new(
        pool: SqlitePool,
        controls: Arc<crate::controls::ControlBanks>,
        devices: bool,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::idle(devices)),
                controls,
            }),
            pool,
            devices,
            listeners: tokio::sync::Mutex::new(None),
            follow: OnceLock::new(),
        }
    }

    /// Bind inputs to `venue_id`'s controls, starting the listeners the
    /// first time; opened again, read its controllers again. Bindings kept
    /// here from before controllers synced move into one first.
    pub(crate) async fn open(&self, venue_id: &str) -> Result<(), String> {
        if let Err(error) = store::adopt(&self.pool, venue_id).await {
            log::warn!("[inputs] the bindings kept here did not move to a controller: {error}");
        }
        let (controllers, bindings) = store::read(&self.pool, venue_id).await?;
        {
            let mut state = self.shared.lock();
            state.venue_id = Some(venue_id.to_owned());
            state.controllers = controllers.into();
            state.bindings = bindings.into();
        }
        self.follow.get_or_init(|| {
            let (shared, pool) = (self.shared.clone(), self.pool.clone());
            let mut synced = SYNCED.subscribe();
            tokio::spawn(async move {
                while synced.changed().await.is_ok() {
                    let venue = shared.lock().venue_id.clone();
                    if let Some(venue) = venue {
                        if let Err(error) = reload(&shared, &pool, &venue).await {
                            log::warn!("[inputs] synced controllers were not read: {error}");
                        }
                    }
                }
            })
            .abort_handle()
        });
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
                lights: tokio::spawn(lights::run(shared.clone())),
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
            controllers: state
                .controllers
                .iter()
                .map(|controller| ControllerState {
                    id: controller.id.clone(),
                    name: controller.name.clone(),
                    port: controller.port.clone(),
                    connected: state.midi_ports.contains(&controller.port),
                    controls: controller.profile.controls.len(),
                })
                .collect(),
            bindings: state.bindings.to_vec(),
        }
    }

    /// The open venue's controllers, each with its profile, oldest first.
    pub(crate) fn controllers(&self) -> Vec<Controller> {
        self.shared.lock().controllers.to_vec()
    }

    /// The open venue, or why there is none.
    fn venue(&self) -> Result<String, String> {
        self.shared
            .lock()
            .venue_id
            .clone()
            .ok_or_else(|| "Open a venue's inputs first".to_owned())
    }

    /// Controller `id` of the open venue.
    fn controller(&self, id: &str) -> Result<Controller, String> {
        self.shared
            .lock()
            .controllers
            .iter()
            .find(|controller| controller.id == id)
            .cloned()
            .ok_or_else(|| "No such controller".to_owned())
    }

    /// Bind the next input heard to `control` of the open venue, on
    /// `controller` (`None`: the first, or a new one, named after the port
    /// heard); an input it already has is bound again, as `mode`. A newer
    /// learn replaces this one, which then fails.
    pub(crate) async fn learn(
        &self,
        control: &str,
        mode: Mode,
        controller: Option<&str>,
        attribution: Attribution<'_>,
    ) -> Result<Binding, String> {
        let venue_id = self.venue()?;
        let controller = match controller {
            Some(id) => Some(self.controller(id)?),
            None => self.shared.lock().controllers.first().cloned(),
        };
        let (reply, heard) = oneshot::channel();
        self.shared.lock().learn = Some(Learn {
            control: control.to_owned(),
            reply,
        });
        let mut waiting = Waiting {
            shared: &self.shared,
            heard,
        };
        let event = match tokio::time::timeout(LEARN_FOR, &mut waiting.heard).await {
            Ok(Ok(event)) => event,
            Ok(Err(_)) => return Err("Learning was replaced or cancelled".into()),
            Err(_) => {
                return Err(
                    "Nothing was heard. Move a fader or press a pad, then try again".into(),
                );
            }
        };
        let binding = store::learned(
            &self.pool,
            &venue_id,
            attribution,
            controller.as_ref(),
            event.port.as_deref(),
            control,
            &event.address,
            mode,
        )
        .await?;
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
        attribution: Attribution<'_>,
    ) -> Result<(), String> {
        store::update_binding(&self.pool, venue_id, attribution, id, mode, invert).await?;
        self.reload(venue_id).await
    }

    pub(crate) async fn unbind(
        &self,
        venue_id: &str,
        id: &str,
        attribution: Attribution<'_>,
    ) -> Result<(), String> {
        store::unbind(&self.pool, venue_id, attribution, id).await?;
        self.reload(venue_id).await
    }

    /// Add a controller to the open venue, matching MIDI port `port`
    /// (empty: none yet).
    pub(crate) async fn add_controller(
        &self,
        name: &str,
        port: &str,
        attribution: Attribution<'_>,
    ) -> Result<Controller, String> {
        let venue_id = self.venue()?;
        let controller =
            store::add_controller(&self.pool, &venue_id, attribution, &named(name)?, port).await?;
        self.reload(&venue_id).await?;
        Ok(controller)
    }

    /// Change controller `id` of the open venue as `change` says, and
    /// store it.
    pub(crate) async fn change_controller(
        &self,
        id: &str,
        attribution: Attribution<'_>,
        change: impl FnOnce(&mut Controller) -> Result<(), String>,
    ) -> Result<Controller, String> {
        let venue_id = self.venue()?;
        let mut controller = self.controller(id)?;
        change(&mut controller)?;
        controller.name = named(&controller.name)?;
        controller.profile.check()?;
        store::save_controller(&self.pool, &venue_id, attribution, &controller).await?;
        self.reload(&venue_id).await?;
        Ok(controller)
    }

    /// Remove controller `id` of the open venue, and its bindings.
    pub(crate) async fn remove_controller(
        &self,
        id: &str,
        attribution: Attribution<'_>,
    ) -> Result<(), String> {
        let venue_id = self.venue()?;
        store::remove_controller(&self.pool, &venue_id, attribution, id).await?;
        self.reload(&venue_id).await
    }

    /// Put `page` on controller `id`'s grid, by its profile: each cell's
    /// controls on the controls at its places, in place of what the
    /// controller had bound there. Answers the bindings made.
    pub(crate) async fn map_page(
        &self,
        id: &str,
        page: &LayoutPage,
        patterns: &[PerformPattern],
        attribution: Attribution<'_>,
    ) -> Result<Vec<Binding>, String> {
        let venue_id = self.venue()?;
        let controller = self.controller(id)?;
        if controller.profile.controls.is_empty() {
            return Err(format!(
                "{} has no profile yet: it does not say where its pads are",
                controller.name
            ));
        }
        let bound = controller.profile.map(page, patterns);
        let addresses: Vec<Address> = controller
            .profile
            .controls
            .iter()
            .map(|place| place.address.clone())
            .collect();
        let bindings =
            store::remap(&self.pool, &venue_id, attribution, id, &addresses, &bound).await?;
        self.reload(&venue_id).await?;
        Ok(bindings)
    }

    /// Show `light` on the control at `spot` of controller `id`, until the
    /// look of a control bound there changes. Fails unless its port is open.
    pub(crate) fn light(
        &self,
        id: &str,
        spot: &Spot,
        light: crate::controls::Light,
    ) -> Result<(), String> {
        let controller = self.controller(id)?;
        let address = match spot {
            Spot::Address(address) => address,
            Spot::Place(column, row) => controller.profile.at(*column, *row).ok_or_else(|| {
                format!(
                    "{}'s profile has no control at ({column}, {row})",
                    controller.name
                )
            })?,
        };
        let message = controller
            .profile
            .show(address, light, true)
            .ok_or("An OSC address has no light")?;
        self.send(&controller, &message)
    }

    /// Turn every control controller `id`'s profile places dark, then show
    /// the set's lights again.
    pub(crate) fn clear_lights(&self, id: &str) -> Result<(), String> {
        let controller = self.controller(id)?;
        for place in &controller.profile.controls {
            if let Some(dark) = controller.profile.show(&place.address, lights::DARK, true) {
                self.send(&controller, &dark)?;
            }
        }
        self.shared.lock().cleared.push(controller.id);
        Ok(())
    }

    fn send(&self, controller: &Controller, message: &[u8]) -> Result<(), String> {
        if midi::send(&controller.port, message) {
            Ok(())
        } else {
            Err(format!("{} is not connected", controller.name))
        }
    }

    async fn reload(&self, venue_id: &str) -> Result<(), String> {
        reload(&self.shared, &self.pool, venue_id).await
    }

    #[cfg(test)]
    pub(crate) fn receive(&self, event: Event) {
        self.shared.receive(event);
    }
}

/// Read `venue_id`'s controllers and bindings again, if it is still open.
async fn reload(shared: &Shared, pool: &SqlitePool, venue_id: &str) -> Result<(), String> {
    let (controllers, bindings) = store::read(pool, venue_id).await?;
    let mut state = shared.lock();
    if state.venue_id.as_deref() == Some(venue_id) {
        state.controllers = controllers.into();
        state.bindings = bindings.into();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
