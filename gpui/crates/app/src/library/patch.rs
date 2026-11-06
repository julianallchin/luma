//! The patch page: a venue's DMX paperwork (addresses, modes, auto-patch)
//! and the output devices its universes are routed to.

use std::collections::HashMap;
use std::future::Future;

use luma_lib::dispatch::handlers;
use luma_lib::models::fixtures::{FixtureDefinition, PatchedFixture};
use luma_lib::models::groups::GroupTreeNode;
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
            let (missing, missing_error) =
                match handlers::group_references::missing_venue_groups(&services, venue_id.clone())
                    .await
                    .map_err(refused("missing_venue_groups"))
                {
                    Ok(missing) => (missing, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
            let universes = handlers::fixtures::universes_in_use(&services, venue_id)
                .await
                .map_err(refused("universes_in_use"))?;
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
            Ok(Patch {
                fixtures,
                definitions,
                groups,
                missing,
                missing_error,
                universes,
                routes,
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
    pub missing: Vec<luma_lib::models::groups::MissingGroup>,
    pub missing_error: Option<String>,
    /// Every universe the venue patches into, ascending.
    pub universes: Vec<u16>,
    /// Where each universe goes. Not per-venue: an output is a fact about
    /// this machine's wiring.
    pub routes: Vec<DmxRoute>,
}
