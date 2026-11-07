//! The 3D venue: reading a whole rig, the venue-graph verbs that place and
//! move nodes, and the fixture catalogue they place from.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;

use luma_lib::dispatch::handlers;
use luma_lib::models::distribute::{DistributeLayout, DistributeReport};
use luma_lib::models::fixtures::{FixtureDefinition, FixtureEntry, PatchedFixture};
use luma_lib::models::venue_graph::{PlacementReport, Reach, ResolvedVenue, VenueGraphRows};
use luma_scene::environment::{VenueEnvironment, VenueHaze};

use super::{refused, Library, LibraryError};

impl Library {
    /// Everything the 3D view needs to draw a venue: its patched fixtures, its
    /// venue graph solved, and the definition behind every distinct fixture
    /// path.
    ///
    /// One method rather than three because they are only meaningful together
    /// — a rig with its definitions missing is not a partial rig, it is fixtures
    /// with no geometry — and because the definitions cannot even be *asked*
    /// for until the fixture list has landed. A screen that orchestrated that
    /// would be a screen that knew the second call depends on the first.
    pub fn venue_rig(&self, venue_id: &str) -> impl Future<Output = Result<Rig, LibraryError>> {
        let venue_id = venue_id.to_string();
        self.task("get_patched_fixtures", |services| async move {
            let fixtures = handlers::fixtures::get_patched_fixtures(&services, venue_id.clone())
                .await
                .map_err(refused("get_patched_fixtures"))?;
            let venue_graph = handlers::stage::get_resolved_venue(&services, venue_id.clone())
                .await
                .map_err(refused("get_resolved_venue"))?;
            let rows = handlers::stage::get_venue_graph(&services, venue_id.clone())
                .await
                .map_err(refused("get_venue_graph"))?;
            // The room's own lighting and atmosphere, fetched with its
            // geometry: a rig, the environment it stands in and the haze it
            // stands in are one picture, and a viewport that adopted the
            // pieces before the light would draw a frame of the wrong room.
            // Both off one read — they are two columns of the same row.
            let room = handlers::venues::get_venue(&services, venue_id.clone())
                .await
                .map_err(refused("get_venue"))?;
            let lasers = handlers::lasers::get_laser_rig(&services, venue_id)
                .await
                .map_err(refused("get_laser_rig"))?;
            let mut definitions = HashMap::new();
            for path in fixtures
                .iter()
                .map(|f| f.fixture_path.clone())
                .collect::<std::collections::BTreeSet<_>>()
            {
                // A venue can outlive a fixture bundle: a definition that no
                // longer resolves leaves that fixture undrawn rather than
                // taking the whole rig down with it.
                if let Ok(def) =
                    handlers::fixtures::get_fixture_definition(&services, path.clone()).await
                {
                    definitions.insert(path, def);
                }
            }
            Ok(Rig {
                fixtures,
                venue: venue_graph,
                definitions,
                rows,
                environment: room.environment,
                haze: room.haze,
                lasers,
            })
        })
    }

    // -----------------------------------------------------------------
    // The venue graph — the verbs. The two reads arrive together in
    // `venue_rig`, because a pose and the relation that produced it are one
    // answer.
    //
    // Thin on purpose. Each is one handler in `dispatch::handlers::stage` (or
    // `distribute`), called with its own arguments, and every mutating one hands
    // back the whole solved venue inside its `PlacementReport`: a graph edit
    // moves everything bolted to what it touched, so a caller that re-fetched
    // would be asking for a second solve of a graph it was just given.
    // -----------------------------------------------------------------

    /// Every QLC+ definition matching `query`, for the distribution popup and
    /// the patch page's add dialog.
    ///
    /// The index is built on the way in. `search_fixtures` refuses on an
    /// un-indexed state, and nothing in this binary had ever built one — so
    /// rather than every caller remembering a warmup, the search is the thing
    /// that cannot be asked before there is something to search. Building it is
    /// a directory walk, so it happens once per process and not once per
    /// keystroke.
    pub fn search_fixtures(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<FixtureEntry>, LibraryError>> + use<> {
        let indexed = Arc::clone(&self.fixtures_indexed);
        let query = query.to_owned();
        self.task("search_fixtures", move |services| async move {
            if !indexed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let built = handlers::fixtures::initialize_fixtures(&services).await;
                if built.is_err() {
                    indexed.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                built.map_err(refused("initialize_fixtures"))?;
            }
            handlers::fixtures::search_fixtures(&services, query, offset, limit)
                .await
                .map_err(refused("search_fixtures"))
        })
    }

    /// One QLC+ definition, for its mode list.
    pub fn fixture_definition(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<FixtureDefinition, LibraryError>> + use<> {
        let path = path.to_owned();
        self.call("get_fixture_definition", |services| async move {
            handlers::fixtures::get_fixture_definition(&services, path).await
        })
    }

    /// Place a new node by mating two sockets.
    #[allow(clippy::too_many_arguments)]
    pub fn attach(
        &self,
        venue_id: &str,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        parent_id: &str,
        my_socket: &str,
        their_socket: &str,
        yaw: f64,
        params: BTreeMap<String, f64>,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (venue_id, kind, parent_id) =
            (venue_id.to_owned(), kind.to_owned(), parent_id.to_owned());
        let (catalog_ref, label) = (catalog_ref.map(str::to_owned), label.map(str::to_owned));
        let (my_socket, their_socket) = (my_socket.to_owned(), their_socket.to_owned());
        self.call("attach", move |services| async move {
            handlers::stage::attach(
                &services,
                venue_id,
                kind,
                catalog_ref,
                label,
                parent_id,
                Some(my_socket),
                their_socket,
                Some(yaw),
                Some(params),
            )
            .await
        })
    }

    /// Free placement: seat a new node on a surface at `(u, v, yaw, trim)`.
    #[allow(clippy::too_many_arguments)]
    pub fn place_free(
        &self,
        venue_id: &str,
        kind: &str,
        catalog_ref: Option<&str>,
        label: Option<&str>,
        surface: Option<(&str, &str)>,
        my_socket: &str,
        seat: luma_scene::venue::SurfacePlacement,
        params: BTreeMap<String, f64>,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (surface_node_id, surface_socket) = match surface {
            Some((node, socket)) => (Some(node.to_owned()), Some(socket.to_owned())),
            None => (None, None),
        };
        let (venue_id, kind, my_socket) =
            (venue_id.to_owned(), kind.to_owned(), my_socket.to_owned());
        let (catalog_ref, label) = (catalog_ref.map(str::to_owned), label.map(str::to_owned));
        self.call("place_free", move |services| async move {
            handlers::stage::place_free(
                &services,
                venue_id,
                kind,
                catalog_ref,
                label,
                surface_node_id,
                surface_socket,
                Some(my_socket),
                seat.u,
                seat.v,
                Some(seat.yaw),
                Some(seat.trim),
                // The generator's own vocabulary — a scrubbed span, a hinge's
                // angle — travels with the piece whichever way it is put down.
                // Dropping it here made a free placement silently forget the
                // length the operator had just set, which is the same params
                // `attach` carries.
                Some(params),
            )
            .await
        })
    }

    /// Write down a far end: this socket meets that one. A check, never an
    /// edge.
    pub fn constrain(
        &self,
        venue_id: &str,
        node_id: &str,
        my_socket: &str,
        target_node: &str,
        target_socket: &str,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (venue_id, node_id, my_socket) = (
            venue_id.to_owned(),
            node_id.to_owned(),
            my_socket.to_owned(),
        );
        let (target_node, target_socket) = (target_node.to_owned(), target_socket.to_owned());
        self.call("constrain", |services| async move {
            handlers::stage::constrain(
                &services,
                venue_id,
                node_id,
                my_socket,
                target_node,
                target_socket,
            )
            .await
        })
    }

    /// Merge parameters into a node, and optionally rename it. `yaw` is
    /// spelled as itself and lands on the edge.
    pub fn set_params(
        &self,
        venue_id: &str,
        node_id: &str,
        params: BTreeMap<String, f64>,
        label: Option<&str>,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (venue_id, node_id, label) = (
            venue_id.to_owned(),
            node_id.to_owned(),
            label.map(str::to_owned),
        );
        self.call("set_params", |services| async move {
            handlers::stage::set_params(&services, venue_id, node_id, params, label).await
        })
    }

    /// Run a stick out of an open socket, along the way it faces. `length_m`
    /// of `None` means "to whatever the ray found"; longer than that gap is
    /// refused, and equal to it writes the far-end check as well.
    pub fn extend(
        &self,
        venue_id: &str,
        node_id: &str,
        socket: &str,
        length_m: Option<f64>,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (venue_id, node_id, socket) =
            (venue_id.to_owned(), node_id.to_owned(), socket.to_owned());
        self.call("extend", move |services| async move {
            handlers::stage::extend(&services, venue_id, node_id, socket, length_m).await
        })
    }

    /// What a run out of this socket would meet, and the buildable gap to it.
    ///
    /// Cast once, when the socket is clicked — the answer is a property of the
    /// room, and re-casting it while a length is being typed would let the
    /// number the operator is editing move under them.
    pub fn extend_reach(
        &self,
        venue_id: &str,
        node_id: &str,
        socket: &str,
    ) -> impl Future<Output = Result<Option<Reach>, LibraryError>> + use<> {
        let (venue_id, node_id, socket) =
            (venue_id.to_owned(), node_id.to_owned(), socket.to_owned());
        self.call("extend_reach", |services| async move {
            handlers::stage::extend_reach(&services, venue_id, node_id, socket).await
        })
    }

    /// Copy a subtree onto another socket, optionally mirrored.
    pub fn duplicate(
        &self,
        venue_id: &str,
        node_id: &str,
        parent_id: &str,
        their_socket: &str,
        flip: bool,
    ) -> impl Future<Output = Result<PlacementReport, LibraryError>> + use<> {
        let (venue_id, node_id) = (venue_id.to_owned(), node_id.to_owned());
        let (parent_id, their_socket) = (parent_id.to_owned(), their_socket.to_owned());
        self.call("duplicate", move |services| async move {
            handlers::stage::duplicate(
                &services,
                venue_id,
                node_id,
                parent_id,
                their_socket,
                Some(flip),
            )
            .await
        })
    }

    /// Delete a node and everything hanging off it.
    pub fn delete_subtree(
        &self,
        venue_id: &str,
        node_id: &str,
    ) -> impl Future<Output = Result<ResolvedVenue, LibraryError>> + use<> {
        let (venue_id, node_id) = (venue_id.to_owned(), node_id.to_owned());
        self.call("delete_subtree", |services| async move {
            handlers::stage::delete_subtree(&services, venue_id, node_id).await
        })
    }

    /// Replace the venue's graph and patch with a snapshot of themselves —
    /// the undo stack's one verb. A light the snapshot names comes back with
    /// its patch row; a light it does not name goes, with its patch row.
    pub fn restore_graph(
        &self,
        venue_id: &str,
        rows: &VenueGraphRows,
        patch: &[PatchedFixture],
    ) -> impl Future<Output = Result<ResolvedVenue, LibraryError>> + use<> {
        let (venue_id, rows, patch) = (venue_id.to_owned(), rows.clone(), patch.to_vec());
        self.call("restore_graph", |services| async move {
            handlers::stage::restore_graph(&services, venue_id, rows, patch).await
        })
    }

    /// One command, one transaction: place, name, group and patch a row of
    /// fixtures along a host feature.
    #[allow(clippy::too_many_arguments)]
    pub fn distribute(
        &self,
        venue_id: &str,
        host: Option<(&str, &str)>,
        fixture_path: &str,
        mode_name: &str,
        count: usize,
        layout: DistributeLayout,
        label_prefix: Option<&str>,
    ) -> impl Future<Output = Result<DistributeReport, LibraryError>> + use<> {
        let (host_node_id, host_socket) = match host {
            Some((node, socket)) => (Some(node.to_owned()), Some(socket.to_owned())),
            None => (None, None),
        };
        let (venue_id, fixture_path) = (venue_id.to_owned(), fixture_path.to_owned());
        let (mode_name, label_prefix) = (mode_name.to_owned(), label_prefix.map(str::to_owned));
        self.call("distribute", move |services| async move {
            handlers::distribute::distribute(
                &services,
                venue_id,
                host_node_id,
                host_socket,
                fixture_path,
                mode_name,
                count,
                layout,
                label_prefix,
            )
            .await
        })
    }

    /// Re-lay the row `member_node_id` belongs to at a new count or layout.
    /// The row is derived server-side; a count that does not fit rolls back
    /// to the row that did.
    pub fn redistribute(
        &self,
        venue_id: &str,
        member_node_id: &str,
        count: usize,
        layout: DistributeLayout,
    ) -> impl Future<Output = Result<DistributeReport, LibraryError>> + use<> {
        let (venue_id, member_node_id) = (venue_id.to_owned(), member_node_id.to_owned());
        self.call("redistribute", move |services| async move {
            handlers::distribute::redistribute(&services, venue_id, member_node_id, count, layout)
                .await
        })
    }
}

/// One venue's 3D contents, as [`Library::venue_rig`] resolves them.
///
/// Rebuilt on venue change, never per frame: geometry is what a venue *is*,
/// and only the light state on top of it moves (spec §2.2).
pub struct Rig {
    /// Every patched fixture, in patch order. The **patch**: what exists and
    /// how it is addressed. Where each one *is* comes from `venue`.
    pub fixtures: Vec<PatchedFixture>,
    /// The venue graph, solved: every pose in the room, derived. A node's id
    /// is the thing it stands for, so a fixture node's id is its `fixtures`
    /// row id and a fixture nobody has placed simply has no node here.
    pub venue: ResolvedVenue,
    /// Keyed by `fixture_path`; a path whose bundle no longer resolves is
    /// absent rather than an error.
    pub definitions: HashMap<String, FixtureDefinition>,
    /// What kind of room this is and how far up its one dial is — the venue's
    /// own truth, and the only thing that says how the picture is lit.
    pub environment: VenueEnvironment,
    /// How hazy this room is and what its haze looks like — venue truth beside
    /// [`Self::environment`], off the same venue row, and for the same reason:
    /// the atmosphere is part of the picture, not of whoever is looking at it.
    pub haze: VenueHaze,
    /// The graph as rows — `(parent, my_socket, their_socket, params)`.
    ///
    /// [`Self::venue`] is the same graph *solved*, and a solve throws the
    /// relations away: it answers where a node is, never which socket met
    /// which. The builder needs both — the pose to draw and the socket to
    /// click — so the two arrive together rather than as a second fetch that
    /// could disagree with the first.
    pub rows: VenueGraphRows,
    /// The projectors and zones the visualizer solves each frame's beams
    /// with, as a DAC would (`docs/design/lasers.md`).
    pub lasers: luma_lib::laser::LaserRig,
}

impl Rig {
    /// Whether there is anything at all to draw. The root node is the room
    /// itself, so a venue with only that is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fixtures.is_empty() && self.venue.nodes.len() <= 1
    }
}
