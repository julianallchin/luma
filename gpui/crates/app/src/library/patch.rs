//! The patch page: a venue's DMX paperwork (addresses, modes, auto-patch),
//! the output devices its universes are routed to, and its laser projectors
//! with the DACs that play them.

use std::collections::HashMap;
use std::future::Future;

use luma_lib::dispatch::handlers;
use luma_lib::laser::{LaserRig, Window};
use luma_lib::models::fixtures::{FixtureDefinition, PatchedFixture};
use luma_lib::models::groups::GroupTreeNode;
use luma_lib::models::lasers::{LaserDevice, LaserOutput, LaserRoute};
use luma_lib::models::patch::{AutoPatchReport, DmxRoute, OutputDevice, OutputPort, UniverseCell};

use super::{refused, Library, LibraryError};

impl Library {
    // -- the patch -----------------------------------------------------------

    /// Everything the patch page reads about a venue, in one pass.
    ///
    /// One method for the same reason [`Self::venue_rig`] is one: these are
    /// only meaningful together — a patch list with no group tree is a table
    /// with a blank column — and a page that orchestrated six calls would
    /// be a page that knew which of them depend on each other.
    pub fn patch_data(&self, venue_id: &str) -> impl Future<Output = Result<Patch, LibraryError>> {
        let venue_id = venue_id.to_owned();
        self.task("get_patched_fixtures", |services| async move {
            let fixtures = handlers::fixtures::get_patched_fixtures(&services, venue_id.clone())
                .await
                .map_err(refused("get_patched_fixtures"))?;
            let groups = handlers::groups::list_group_tree(&services, venue_id.clone())
                .await
                .map_err(refused("list_group_tree"))?;
            let usage =
                match handlers::group_references::venue_group_usage(&services, venue_id.clone())
                    .await
                    .map_err(refused("venue_group_usage"))
                {
                    Ok(usage) => Some(usage),
                    Err(error) => {
                        log::warn!("could not check which groups the scores use: {error}");
                        None
                    }
                };
            let universes = handlers::fixtures::universes_in_use(&services, venue_id.clone())
                .await
                .map_err(refused("universes_in_use"))?;
            let mut occupancy = std::collections::BTreeMap::new();
            for universe in &universes {
                let cells = handlers::fixtures::universe_occupancy(
                    &services,
                    venue_id.clone(),
                    i64::from(*universe),
                )
                .await
                .map_err(refused("universe_occupancy"))?;
                occupancy.insert(*universe, cells);
            }
            let routes = handlers::output::list_routes(&services)
                .await
                .map_err(refused("list_routes"))?;
            let mut definitions = HashMap::new();
            for path in fixtures
                .iter()
                .map(|f| f.fixture_path.clone())
                .collect::<std::collections::BTreeSet<_>>()
            {
                // A venue outliving its bundle leaves that fixture without a
                // mode list rather than taking the page down — same contract
                // as `venue_rig`.
                if let Ok(def) =
                    handlers::fixtures::get_fixture_definition(&services, path.clone()).await
                {
                    definitions.insert(path, def);
                }
            }
            let lasers = Lasers {
                rig: handlers::lasers::get_laser_rig(&services, venue_id.clone())
                    .await
                    .map_err(refused("get_laser_rig"))?,
                routes: handlers::output::list_laser_routes(&services)
                    .await
                    .map_err(refused("list_laser_routes"))?,
                // A host with no physical output sees no DAC and has no
                // switch to show.
                devices: handlers::output::list_laser_devices(&services)
                    .await
                    .unwrap_or_default(),
                output: handlers::output::get_laser_output(&services).await.ok(),
            };
            Ok(Patch {
                fixtures,
                definitions,
                groups,
                usage,
                universes,
                occupancy,
                routes,
                lasers,
            })
        })
    }

    /// One universe as 512 cells — what the footprint strip draws, collisions
    /// and all.
    pub fn universe_occupancy(
        &self,
        venue_id: &str,
        universe: u16,
    ) -> impl Future<Output = Result<Vec<UniverseCell>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("universe_occupancy", move |services| async move {
            handlers::fixtures::universe_occupancy(&services, venue_id, i64::from(universe)).await
        })
    }

    /// Put one fixture at a hand-chosen address, and pin it there. A collision
    /// or a footprint past 512 comes back as [`CommandError::Invalid`](luma_lib::error::CommandError::Invalid) naming
    /// the conflict, with nothing written.
    pub fn set_fixture_address(
        &self,
        venue_id: &str,
        id: &str,
        universe: i64,
        address: i64,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, id) = (venue_id.to_owned(), id.to_owned());
        self.call("set_fixture_address", move |services| async move {
            handlers::fixtures::set_fixture_address(&services, venue_id, id, universe, address)
                .await
        })
    }

    /// Repatch one fixture into another of its definition's modes.
    ///
    /// `allow_move` off asks whether the new width fits where it stands; the
    /// refusal is the question the page puts to the human, and calling again
    /// with it on is the answer.
    pub fn set_fixture_mode(
        &self,
        venue_id: &str,
        id: &str,
        mode_name: &str,
        allow_move: bool,
    ) -> impl Future<Output = Result<PatchedFixture, LibraryError>> + use<> {
        let (venue_id, id, mode_name) = (venue_id.to_owned(), id.to_owned(), mode_name.to_owned());
        self.call("set_fixture_mode", move |services| async move {
            handlers::fixtures::set_fixture_mode(&services, venue_id, id, mode_name, allow_move)
                .await
        })
    }

    /// Hold one address against auto-patch, or release it.
    pub fn set_address_pinned(
        &self,
        venue_id: &str,
        id: &str,
        pinned: bool,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, id) = (venue_id.to_owned(), id.to_owned());
        self.call("set_address_pinned", move |services| async move {
            handlers::fixtures::set_address_pinned(&services, venue_id, id, pinned).await
        })
    }

    pub fn rename_patched_fixture(
        &self,
        venue_id: &str,
        id: &str,
        label: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, id, label) = (venue_id.to_owned(), id.to_owned(), label.to_owned());
        self.call("rename_patched_fixture", |services| async move {
            handlers::fixtures::rename_patched_fixture(&services, venue_id, id, label).await
        })
    }

    /// Unpatch: the paperwork and the node in the room, together.
    pub fn remove_patched_fixture(
        &self,
        venue_id: &str,
        id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, id) = (venue_id.to_owned(), id.to_owned());
        self.call("remove_patched_fixture", |services| async move {
            handlers::fixtures::remove_patched_fixture(&services, venue_id, id).await
        })
    }

    /// Re-derive every address from where the fixtures hang, discarding the
    /// hand-set ones it touches. What the Auto Patch button means.
    pub fn auto_patch(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<AutoPatchReport, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("auto_patch", |services| async move {
            handlers::fixtures::auto_patch(&services, venue_id).await
        })
    }

    // -- outputs --------------------------------------------------------------

    /// Every DMX device the drivers can see now. Fails on a host with no
    /// output at all, which is a state the outputs panel says out loud rather
    /// than drawing as an empty room.
    pub fn output_devices(
        &self,
    ) -> impl Future<Output = Result<Vec<OutputDevice>, LibraryError>> + use<> {
        self.call("list_output_devices", |services| async move {
            handlers::output::list_output_devices(&services).await
        })
    }

    /// Send one universe to one port of a device, alongside wherever it
    /// already goes.
    pub fn bind_route(
        &self,
        universe: i64,
        device: &OutputDevice,
        port: &OutputPort,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (device_kind, device_id, port) = (device.kind, device.id.clone(), port.port);
        let (device_name, address) = (Some(device.name.clone()), Some(device.address.clone()));
        self.call("bind_route", move |services| async move {
            handlers::output::bind_route(
                &services,
                universe,
                device_kind,
                device_id,
                port,
                device_name,
                address,
            )
            .await
        })
    }

    // -- lasers --------------------------------------------------------------

    /// Laser output's switch and each bound projector's DAC, with the DACs
    /// in reach: what changes while the page is open. `None` on a host with
    /// no physical output.
    pub fn laser_output(
        &self,
    ) -> impl Future<Output = Option<(LaserOutput, Vec<LaserDevice>)>> + use<> {
        let pending = self.call("get_laser_output", |services| async move {
            let output = handlers::output::get_laser_output(&services).await?;
            let devices = handlers::output::list_laser_devices(&services).await?;
            Ok((output, devices))
        });
        async move { pending.await.ok() }
    }

    pub fn set_laser_output(
        &self,
        on: bool,
    ) -> impl Future<Output = Result<LaserOutput, LibraryError>> + use<> {
        self.call("set_laser_output", move |services| async move {
            handlers::output::set_laser_output(&services, on).await
        })
    }

    /// Show `projector`'s test pattern, or with `None` none.
    pub fn set_laser_test(
        &self,
        projector: Option<String>,
    ) -> impl Future<Output = Result<LaserOutput, LibraryError>> + use<> {
        self.call("set_laser_test", move |services| async move {
            handlers::output::set_laser_test(&services, projector).await
        })
    }

    /// Play `projector` through `device`.
    pub fn bind_laser(
        &self,
        projector: &str,
        device: &LaserDevice,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (projector, device) = (projector.to_owned(), device.clone());
        self.call("bind_laser_route", move |services| async move {
            handlers::output::bind_laser_route(
                &services,
                projector,
                device.kind,
                device.id,
                Some(device.name),
                Some(device.address),
            )
            .await
        })
    }

    pub fn unbind_laser(
        &self,
        projector: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let projector = projector.to_owned();
        self.call("unbind_laser_route", move |services| async move {
            handlers::output::unbind_laser_route(&services, projector).await
        })
    }

    /// Write a batch of laser edits to a venue, in order: which projectors
    /// have a kind of their own, the changed settings, limits, and the
    /// venue's window.
    pub fn write_lasers(
        &self,
        venue_id: &str,
        edits: LaserEdits,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("set_laser_settings", move |services| async move {
            // A projector's own kind starts from the venue's, so before the
            // settings that then change it.
            for (projector, own) in edits.own {
                handlers::lasers::set_laser_own(&services, venue_id.clone(), projector, own)
                    .await?;
            }
            for (projector, settings) in edits.settings {
                handlers::lasers::set_laser_settings(
                    &services,
                    venue_id.clone(),
                    projector,
                    settings,
                )
                .await?;
            }
            for (projector, points) in edits.limits {
                handlers::lasers::set_laser_limit(&services, venue_id.clone(), projector, points)
                    .await?;
            }
            if let Some(window) = edits.window {
                handlers::lasers::set_laser_window(&services, venue_id.clone(), window).await?;
            }
            Ok(())
        })
    }

    pub fn unbind_route(
        &self,
        route: &DmxRoute,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (universe, device_kind, port) = (route.universe, route.device_kind, route.port);
        let device_id = route.device_id.clone();
        self.call("unbind_route", move |services| async move {
            handlers::output::unbind_route(&services, universe, device_kind, device_id, port).await
        })
    }
}

/// One venue's paperwork, as [`Library::patch_data`] resolves it.
///
/// The patch is what *exists*; where a fixture hangs is the stage page's
/// business and never reaches here, because a pose would invite this page to
/// edit one.
pub struct Patch {
    /// Every patched fixture, in patch order.
    pub fixtures: Vec<PatchedFixture>,
    /// Keyed by `fixture_path`; a path whose bundle no longer resolves is
    /// absent rather than an error.
    pub definitions: HashMap<String, FixtureDefinition>,
    /// Saved fixture collections.
    pub groups: Vec<GroupTreeNode>,
    /// What selects each group, for the groups something does. `None` when
    /// that could not be read; the cause is in the log.
    pub usage: Option<Vec<luma_lib::models::groups::GroupUsage>>,
    /// Every universe the venue patches into, ascending.
    pub universes: Vec<u16>,
    /// Each universe in use as its 512 cells, as the allocator sees them.
    pub occupancy: std::collections::BTreeMap<u16, Vec<UniverseCell>>,
    /// Where each universe goes. Not per-venue: an output is a fact about
    /// this machine's wiring.
    pub routes: Vec<DmxRoute>,
    pub lasers: Lasers,
}

/// A venue's laser projectors and what plays them.
pub struct Lasers {
    /// The projectors, set up, and the venue's window.
    pub rig: LaserRig,
    /// Which DAC plays each projector: this machine's wiring.
    pub routes: Vec<LaserRoute>,
    /// The DACs in reach now.
    pub devices: Vec<LaserDevice>,
    /// Laser output's switch and links; `None` on a host with no physical
    /// output.
    pub output: Option<LaserOutput>,
}

/// Laser edits waiting to be written: the newest value of each thing
/// changed.
#[derive(Default)]
pub struct LaserEdits {
    /// By projector, or `None` for what the venue's projectors share, by
    /// the parameter each setting is stored under.
    pub settings:
        std::collections::BTreeMap<Option<String>, std::collections::BTreeMap<String, f64>>,
    /// By projector: whether it has a kind of its own.
    pub own: std::collections::BTreeMap<String, bool>,
    /// By projector: its limit, or `None` to lift it.
    pub limits: std::collections::BTreeMap<String, Option<Vec<[f64; 2]>>>,
    /// The venue's window, or `None` to fit it to the projectors again.
    pub window: Option<Option<Window>>,
}

impl LaserEdits {
    pub fn is_empty(&self) -> bool {
        !self.changes_graph() && self.limits.is_empty()
    }

    /// Whether any of it is stored in the venue's graph, and so is a step
    /// the stage's undo takes back. A limit is not.
    pub fn changes_graph(&self) -> bool {
        !self.settings.is_empty() || !self.own.is_empty() || self.window.is_some()
    }
}
