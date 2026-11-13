//! Physical output: one frame per tick, handed to every transport. DMX
//! here; laser projectors, which a DAC plays, in [`laser`].
//!
//! The output loop renders the light state into 512-slot universes once and
//! publishes the result as the *latest* frame. Each driver — Art-Net, USB
//! dongles — runs on its own thread and pulls the newest frame at its own
//! pace, so a dongle stuck in a USB write cannot hold up the render tick or
//! the network. [`DmxRoute`]s say which device port each universe goes to; a
//! universe with no route is sent nowhere.

mod artnet;
mod ether_dream;
mod laser;
mod usb;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use crate::fixtures::engine;
use crate::laser::LaserRig;
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::lasers::{LaserDevice, LaserOutput, LaserRoute};
use crate::models::patch::{DmxRoute, OutputDevice};
use crate::models::universe::UniverseState;
use crate::settings::AppSettings;

/// One tick's output: every patched universe, 512 slots each, slot 0 being
/// DMX channel 1.
#[derive(Debug, Default)]
pub(crate) struct DmxFrame {
    pub universes: HashMap<i64, [u8; 512]>,
}

/// The newest frame and its sequence number. A slot, not a queue: a driver
/// that falls behind skips to the latest frame rather than replaying old ones.
pub(crate) struct FrameSlot {
    latest: Mutex<(u64, Arc<DmxFrame>)>,
    published: Condvar,
}

impl FrameSlot {
    fn new() -> Self {
        Self {
            latest: Mutex::new((0, Arc::new(DmxFrame::default()))),
            published: Condvar::new(),
        }
    }

    fn publish(&self, frame: DmxFrame) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        *latest = (latest.0 + 1, Arc::new(frame));
        self.published.notify_all();
    }

    /// The newest frame, whatever its age.
    pub fn latest(&self) -> (u64, Arc<DmxFrame>) {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        (latest.0, latest.1.clone())
    }

    /// A frame newer than `seen`, waiting up to `timeout` for one.
    pub fn newer_than(&self, seen: u64, timeout: Duration) -> Option<(u64, Arc<DmxFrame>)> {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        let (latest, _) = self
            .published
            .wait_timeout_while(latest, timeout, |latest| latest.0 <= seen)
            .unwrap_or_else(|e| e.into_inner());
        (latest.0 > seen).then(|| (latest.0, latest.1.clone()))
    }
}

/// What every driver reads: the frame, the routes, and whether to run.
pub(crate) struct Shared {
    pub frames: FrameSlot,
    pub routes: RwLock<Arc<Vec<DmxRoute>>>,
    /// The output setting. A driver holds no socket and no device while off.
    pub enabled: AtomicBool,
    /// The address the Art-Net socket binds; `0.0.0.0` for every interface.
    pub artnet_interface: RwLock<String>,
    pub shutdown: AtomicBool,
}

impl Shared {
    pub fn routes(&self) -> Arc<Vec<DmxRoute>> {
        self.routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn running(&self) -> bool {
        !self.shutdown.load(Ordering::Relaxed)
    }
}

/// One transport. Built with the [`Shared`] state, it starts its own threads
/// and owns its wire: discovery, hotplug, pacing, and blackout when it stops.
/// Dropping it stops and joins them.
pub(crate) trait OutputDriver: Send + Sync {
    /// Every device this driver can see now, bound or not.
    fn devices(&self) -> Vec<OutputDevice>;
}

/// What turns light state into DMX: the patch, its fixture definitions, and
/// the previous frame, which `generate_dmx` reads for continuity.
struct Render {
    patched_fixtures: Vec<PatchedFixture>,
    fixture_definitions: HashMap<String, FixtureDefinition>,
    last_universe_buffers: HashMap<i64, [u8; 512]>,
    fixtures_root: PathBuf,
}

/// Physical output, installed only by the desktop host.
pub struct OutputHub {
    render: Mutex<Render>,
    max_dimmer: AtomicU8,
    shared: Arc<Shared>,
    drivers: Vec<Box<dyn OutputDriver>>,
    lasers: Arc<laser::Shared>,
    laser_solve: Mutex<laser::Solve>,
    laser_drivers: Vec<Box<dyn laser::LaserDriver>>,
}

impl OutputHub {
    /// Read the settings and routes, and start every driver.
    ///
    /// # Errors
    /// If the settings or routes cannot be read.
    pub async fn new(pool: &sqlx::SqlitePool, fixtures_root: PathBuf) -> Result<Self, String> {
        let settings = crate::settings::load_settings(pool).await?;
        let routes = crate::database::local::outputs::list(pool).await?;
        let shared = Arc::new(Shared {
            frames: FrameSlot::new(),
            routes: RwLock::new(Arc::new(routes)),
            enabled: AtomicBool::new(settings.dmx_output_enabled),
            artnet_interface: RwLock::new(settings.artnet_interface.clone()),
            shutdown: AtomicBool::new(false),
        });
        let drivers: Vec<Box<dyn OutputDriver>> = vec![
            Box::new(artnet::ArtNetDriver::start(shared.clone())),
            Box::new(usb::UsbDriver::start(shared.clone())),
        ];
        let lasers = Arc::new(laser::Shared::new(
            crate::database::local::outputs::laser_routes(pool).await?,
        ));
        let laser_drivers: Vec<Box<dyn laser::LaserDriver>> =
            vec![Box::new(ether_dream::EtherDream::start(lasers.clone()))];
        Ok(Self {
            render: Mutex::new(Render {
                patched_fixtures: Vec::new(),
                fixture_definitions: HashMap::new(),
                last_universe_buffers: HashMap::new(),
                fixtures_root,
            }),
            max_dimmer: AtomicU8::new(settings.max_dimmer),
            shared,
            drivers,
            lasers,
            laser_solve: Mutex::default(),
            laser_drivers,
        })
    }

    /// Take the settings the drivers care about. Turning output off is seen by
    /// every driver on its next loop, which then lets go of its socket or device.
    fn apply_settings(&self, settings: &AppSettings) {
        self.max_dimmer
            .store(settings.max_dimmer, Ordering::Relaxed);
        *self
            .shared
            .artnet_interface
            .write()
            .unwrap_or_else(|e| e.into_inner()) = settings.artnet_interface.clone();
        self.shared
            .enabled
            .store(settings.dmx_output_enabled, Ordering::Relaxed);
    }

    /// Replace the route table. Called after every bind or unbind, so no driver
    /// reads the database on a frame.
    pub fn set_routes(&self, routes: Vec<DmxRoute>) {
        *self
            .shared
            .routes
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(routes);
    }

    pub fn update_patch(&self, fixtures: Vec<PatchedFixture>) {
        let mut render = self.render.lock().unwrap_or_else(|e| e.into_inner());
        render.patched_fixtures = fixtures;

        let missing: Vec<String> = render
            .patched_fixtures
            .iter()
            .map(|f| f.fixture_path.clone())
            .filter(|p| !render.fixture_definitions.contains_key(p))
            .collect();
        for path in missing {
            if let Ok(def) = crate::fixtures::definition(&render.fixtures_root, &path) {
                render.fixture_definitions.insert(path, (*def).clone());
            }
        }
    }

    /// Render `state` for every transport and hand it to the drivers. Cheap
    /// to call from any thread: it never touches a socket or a device.
    pub fn publish(&self, state: &UniverseState) {
        let master = f32::from(self.max_dimmer.load(Ordering::Relaxed)) / 100.0;
        self.publish_dmx(state, master);
        let solve = self.laser_solve.lock().unwrap_or_else(|e| e.into_inner());
        laser::publish(&self.lasers, &solve, state, f64::from(master));
    }

    fn publish_dmx(&self, state: &UniverseState, master: f32) {
        if !self.shared.enabled.load(Ordering::Relaxed) {
            return;
        }
        let mut render = self.render.lock().unwrap_or_else(|e| e.into_inner());
        // The strobe gate runs on the process's strobe clock, the one the
        // live view draws strobes on, so the screen flashes in phase with
        // the rig.
        let universes = engine::generate_dmx(
            state,
            &render.patched_fixtures,
            &render.fixture_definitions,
            Some(&render.last_universe_buffers),
            master,
            luma_scene::strobe::clock(),
        );
        if universes.is_empty() {
            return;
        }
        render.last_universe_buffers = universes.clone();
        drop(render);
        self.shared.frames.publish(DmxFrame { universes });
    }

    /// Every device any driver can see, in driver order.
    pub fn devices(&self) -> Vec<OutputDevice> {
        self.drivers.iter().flat_map(|d| d.devices()).collect()
    }

    /// The rig whose scans are solved each tick.
    pub fn update_lasers(&self, rig: LaserRig) {
        self.laser_solve
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rig = rig;
    }

    /// Replace the table of which DAC plays each projector.
    pub fn set_laser_routes(&self, routes: Vec<LaserRoute>) {
        self.lasers.set_routes(routes);
    }

    /// Turn laser output on or off. It is off at every launch and is never
    /// stored: nothing reaches a DAC until someone here turns it on.
    pub fn set_laser_output(&self, on: bool) {
        self.lasers.set_on(on);
    }

    /// Show `projector`'s test pattern in place of what it would play, or
    /// with `None` show none.
    pub fn set_laser_test(&self, projector: Option<String>) {
        self.laser_solve
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .testing = projector;
    }

    /// Whether a test pattern is showing, which needs frames though nothing
    /// plays.
    pub fn testing_lasers(&self) -> bool {
        self.lasers.on.load(Ordering::Relaxed)
            && self
                .laser_solve
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .testing
                .is_some()
    }

    pub fn laser_output(&self) -> LaserOutput {
        LaserOutput {
            on: self.lasers.on.load(Ordering::Relaxed),
            testing: self
                .laser_solve
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .testing
                .clone(),
            links: self.lasers.links(),
        }
    }

    /// Every DAC any laser driver can see, in driver order.
    pub fn laser_devices(&self) -> Vec<LaserDevice> {
        self.laser_drivers.iter().flat_map(|d| d.devices()).collect()
    }
}

impl Drop for OutputHub {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        // Wake anything waiting on a frame so it sees the flag now.
        self.shared.frames.published.notify_all();
        // Dropping the drivers joins their threads, each blacking out first.
        self.drivers.clear();
        self.lasers.shutdown.store(true, Ordering::Relaxed);
        self.laser_drivers.clear();
    }
}

/// Re-read the persisted settings into `hub`.
pub async fn reload_settings(hub: &OutputHub, pool: &sqlx::SqlitePool) -> Result<(), String> {
    let settings = crate::settings::load_settings(pool).await?;
    hub.apply_settings(&settings);
    Ok(())
}
