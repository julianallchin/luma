//! `distribute` — the command that builds a rig instead of typing one.
//!
//! One call, one transaction: `count` fixtures are patched, named, placed,
//! bolted to a host face and filed into groups. It is the *only* fixture
//! constructor besides the patch page's non-placed add
//! ([`crate::services::fixture_create`]), which is what makes "a fixture at the
//! origin that nobody placed" unrepresentable rather than merely discouraged
//! (gauntlet AF9).
//!
//! # Everything it does is somebody else's rule
//!
//! This module composes; it decides nothing on its own, which is the whole
//! defence against a second allocator, a second naming rule, a second grouping
//! (AF10).
//!
//! | question | whose answer |
//! |---|---|
//! | how long is the face, which way does it run | [`luma_render::face`] |
//! | where along it does each fixture sit | [`luma_scene::distribute`] |
//! | what pose is that | [`luma_scene::venue::place_on`], through the resolver |
//! | which universe and address | [`luma_scene::patch::next_addresses`] |
//! | what is it called | [`crate::services::fixture_create::ModelNumbering`] |
//! | which group does it land in | [`crate::services::group_derivation`] |
//!
//! # Refuse, never squeeze
//!
//! A distribution that does not fit writes **nothing** and says how long the
//! host would have to be ([`FitFailure`]). The number it reports is a length
//! the host can actually be built at, so feeding it back into the run's `span`
//! and re-running the same call succeeds — that is the gauntlet's acceptance
//! test for this surface, and the reason `needed_m` is quantized rather than
//! raw.
//!
//! The refusal is a *report*, not an error: nothing was wrong with the call,
//! the truss is short. Only the things the design doc calls hard errors — a
//! socket that does not exist, a polarity that forbids the joint — come back as
//! [`CommandError`]s, and they come back before any row is written.

use std::collections::BTreeMap;
use std::path::Path;

use luma_render::face::host_face;
use luma_scene::distribute::{offsets, Fit, Layout};
use luma_scene::patch::{next_addresses, Footprint};
use luma_scene::venue::{Edge, EdgeError, Node, NodeKind, ResolvedVenue, FLOOR_SOCKET};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{VenueAccess, Write};
use crate::database::local::venue_graph as venue_graph_db;
use crate::models::fixtures::FixtureDefinition;
use crate::services::fixture_create::{self, Naming, NewFixture};
use crate::services::group_derivation::derive_groups;
use crate::services::{fixtures as fixture_service, groups as group_service};

/// The fallback body width, in metres, for a definition whose QLC+ physical
/// block does not say.
///
/// A moving head is 0.3–0.4 m across and a par is less, so this errs toward
/// admitting a row that a measured definition would refuse — which is the right
/// direction: the human can see eight lights crowded on a truss, and cannot see
/// a refusal that had no measurement behind it.
pub const DEFAULT_FIXTURE_WIDTH_M: f64 = 0.3;

/// What a caller asked for.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// The node whose face hosts the row; `None` is the venue root.
    pub host_node: Option<&'a str>,
    /// The face on it. Defaults to the root's [`FLOOR_SOCKET`].
    pub host_socket: Option<&'a str>,
    pub fixture_path: &'a str,
    pub mode_name: &'a str,
    pub count: usize,
    pub layout: Layout,
    /// The naming term, if the row should not be named after the model.
    pub label_prefix: Option<&'a str>,
}

/// One fixture a distribution created.
#[derive(Debug, Clone)]
pub struct Placed {
    /// The patch row id, which is also its venue-graph node id.
    pub id: String,
    pub label: String,
    pub universe: u16,
    pub address: u16,
    /// Metres along the host face from its middle — ascending across the row,
    /// which is the order the addresses run in.
    pub along_m: f64,
    /// The derived group it landed in, deepest first path. Empty only for a
    /// venue whose derivation found no role for it.
    pub group_path: Vec<String>,
}

/// Why a row will not fit, and what to do about it.
#[derive(Debug, Clone)]
pub struct FitFailure {
    /// How long the host face would have to be — already a length the host can
    /// be built at, so it can be used verbatim.
    pub needed_m: f64,
    /// How long it is now.
    pub available_m: f64,
    /// The node whose length to change: the host itself.
    pub extend_node: String,
}

/// What one distribution did.
#[derive(Debug, Clone)]
pub struct Report {
    /// Whether anything was placed. `false` carries a [`Self::fit`] and no
    /// rows: a distribution is all of its fixtures or none of them.
    pub ok: bool,
    pub host_node: String,
    pub host_socket: String,
    pub fixtures: Vec<Placed>,
    pub fit: Option<FitFailure>,
    /// Whatever the solve had to decide, once the row was placed.
    pub warnings: Vec<String>,
    /// Open structural sockets left in the venue, as the resolver reports them.
    pub dangling: Vec<String>,
    /// Subtrees the solve could not reach — the tray, and anything detached.
    pub unplaced: Vec<String>,
}

/// Patch, name, place and group `count` fixtures along one host face.
///
/// # Errors
/// The design's two hard errors, both raised before any write: a face the host
/// does not have, and a joint its polarity forbids. Plus the database's own,
/// and a definition or mode that cannot be read.
pub async fn distribute(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
    request: Request<'_>,
) -> Result<Report, String> {
    let sockets = crate::venue_graph::sockets(fixtures_root)?;
    let mut graph = crate::venue_graph::graph(access).await?;
    let host_id = match request.host_node {
        Some(id) => id.to_string(),
        None => graph.root().to_string(),
    };
    let host_socket = request.host_socket.unwrap_or(FLOOR_SOCKET).to_string();
    let host = graph
        .node(&host_id)
        .ok_or_else(|| format!("`{host_id}` is not a node in this venue"))?
        .clone();
    let face = host_face(sockets, &host, &host_socket).ok_or_else(|| {
        format!(
            "`{}` has no face `{host_socket}`",
            host.label.clone().unwrap_or(host_id.clone())
        )
    })?;

    // Whether a fixture may hang here at all is asked *before* the row is laid
    // out, so a bolt plate is refused as the wrong joint rather than reported
    // as a face that is too short. Same rule the resolver enforces at
    // `attach` — `SocketType::mates`, not a second reading of it.
    let clamp = luma_render::catalog::fixture_clamp();
    if !clamp.socket_type.mates(face.socket.socket_type) {
        return Err(EdgeError::Polarity {
            held: clamp.socket_type,
            host: face.socket.socket_type,
        }
        .to_string());
    }

    let (definition, channels) = definition_and_width(fixtures_root, &request)?;
    let width = body_width_m(&definition);

    let stations = match offsets(face.feature, request.layout, request.count, width) {
        Ok(stations) => stations,
        Err(Fit::TooLong {
            needed_m,
            available_m,
        }) => {
            return Ok(Report {
                ok: false,
                host_node: host_id,
                host_socket,
                fixtures: Vec::new(),
                fit: Some(FitFailure {
                    needed_m,
                    available_m,
                    extend_node: host.id.clone(),
                }),
                warnings: Vec::new(),
                dangling: Vec::new(),
                unplaced: Vec::new(),
            });
        }
    };

    // Addresses come from the allocator, against the venue as it stands — the
    // rows do not exist yet, so there is nothing for `allocate` to order them
    // by and `next_addresses` is the door built for exactly this caller.
    let solved = luma_scene::venue::resolve(&graph, sockets);
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let run = run_of(&solved, &host_id);
    let footprints = next_addresses(
        &solved,
        &crate::services::patch::inputs(&rows),
        run.as_deref(),
        channels,
        stations.len(),
    );
    if footprints.len() < stations.len() {
        return Err(format!(
            "there is no room in the patch for {} more fixtures of {channels} channels",
            stations.len()
        ));
    }

    let mut numbering = fixture_create::numbering(access)
        .await
        .map_err(|e| e.to_string())?;
    let mut placed: Vec<(String, String, Footprint, f64)> = Vec::new();
    for (station, footprint) in stations.iter().zip(&footprints) {
        let fixture = fixture_create::create(
            access,
            &mut numbering,
            NewFixture {
                manufacturer: &definition.manufacturer,
                model: &definition.model,
                mode_name: request.mode_name,
                fixture_path: request.fixture_path,
                footprint: *footprint,
                pinned: false,
                name: Naming::Minted(request.label_prefix),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

        let label = fixture.label.clone().unwrap_or_default();
        let node = Node {
            id: fixture.id.clone(),
            kind: NodeKind::Fixture,
            catalog_ref: Some(fixture.id.clone()),
            label: fixture.label.clone(),
            params: BTreeMap::from([
                ("u".to_string(), *station),
                ("v".to_string(), 0.0),
                ("trim".to_string(), 0.0),
            ])
            .into_iter()
            .collect(),
        };
        let edge = Edge {
            parent: host_id.clone(),
            my_socket: clamp.name.clone(),
            their_socket: host_socket.clone(),
            roll: 0.0,
        };
        // The invariants are `luma_scene`'s and are checked *before* the edge
        // is written; a refusal here leaves the transaction to roll back
        // everything this loop has done so far, which is the whole of "never
        // partial placement".
        graph.insert(node.clone());
        graph
            .attach(&node.id, edge.clone(), sockets)
            .map_err(|e| e.to_string())?;

        venue_graph_db::set_params(
            access,
            &node.id,
            &node
                .params
                .iter()
                .map(|(key, value)| (key.to_string(), Some(value)))
                .collect(),
        )
        .await?;
        venue_graph_db::upsert_edge(
            access,
            &node.id,
            &edge.parent,
            &edge.my_socket,
            &edge.their_socket,
            edge.roll,
        )
        .await?;
        placed.push((fixture.id, label, *footprint, *station));
    }

    // One solve and one derivation over the finished venue, rather than a
    // guess per fixture: which group a fixture lands in is a fact about the
    // rig it is now part of.
    let solved = crate::venue_graph::resolved(access, fixtures_root).await?;
    let paths = group_paths(access, fixtures_root).await?;

    Ok(Report {
        ok: true,
        host_node: host_id,
        host_socket,
        fixtures: placed
            .into_iter()
            .map(|(id, label, footprint, along)| Placed {
                label,
                universe: footprint.universe(),
                address: footprint.address(),
                along_m: along,
                group_path: paths.get(&id).cloned().unwrap_or_default(),
                id,
            })
            .collect(),
        fit: None,
        warnings: solved
            .warnings()
            .iter()
            .map(|w| format!("{}: {:?}", w.node, w.warning))
            .collect(),
        dangling: solved
            .dangling()
            .iter()
            .map(|d| format!("{}.{}", d.node, d.socket))
            .collect(),
        unplaced: solved.unplaced().iter().map(|u| u.node.clone()).collect(),
    })
}

/// The definition, and how many channels the named mode is wide.
fn definition_and_width(
    fixtures_root: &Path,
    request: &Request<'_>,
) -> Result<(FixtureDefinition, u16), String> {
    let definition =
        fixture_service::get_fixture_definition(fixtures_root, Path::new(request.fixture_path))?;
    let mode = definition
        .modes
        .iter()
        .find(|mode| mode.name == request.mode_name)
        .ok_or_else(|| format!("`{}` has no mode `{}`", definition.model, request.mode_name))?;
    let channels = u16::try_from(mode.channels.len())
        .map_err(|_| format!("`{}` is wider than a universe", request.mode_name))?;
    Ok((definition.clone(), channels))
}

/// How wide the fixture's body is, in metres.
///
/// QLC+ authors physical dimensions in **millimetres**, which is the one
/// conversion in this file and the reason it is a named function rather than a
/// `/ 1000.0` at the call site.
#[must_use]
pub fn body_width_m(definition: &FixtureDefinition) -> f64 {
    definition
        .physical
        .as_ref()
        .and_then(|physical| physical.dimensions.as_ref())
        .map(|dimensions| f64::from(dimensions.width) / 1000.0)
        .filter(|width| width.is_finite() && *width > 0.0)
        .unwrap_or(DEFAULT_FIXTURE_WIDTH_M)
}

/// The run a host belongs to — itself if it is one, else its nearest run or
/// tower ancestor. `None` for a host that is neither, whose fixtures the
/// allocator treats as run-less and addresses from universe 1.
///
/// The same "nearest ancestor" walk [`luma_scene::patch`] does after the fact;
/// it is asked here because the fixtures do not exist yet, so there is nothing
/// for that walk to start from.
fn run_of(solved: &ResolvedVenue, host: &str) -> Option<String> {
    let mut cursor = Some(host);
    while let Some(id) = cursor {
        let pose = solved.pose(id)?;
        if matches!(pose.kind, NodeKind::Run | NodeKind::Tower) {
            return Some(id.to_string());
        }
        cursor = pose.parent.as_deref();
    }
    None
}

/// Each fixture's deepest derived group path.
///
/// Deepest, because that is the group the human means when they ask where a
/// light landed: `spots / left wing / top` says more than `spots`, and every
/// ancestor is implied by it.
async fn group_paths(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let tree = derive_groups(&group_service::venue_facts(fixtures_root, access).await?);
    let mut deepest: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for group in &tree.groups {
        for member in &group.members {
            let held = deepest.entry(member.clone()).or_default();
            if group.path.len() > held.len() {
                *held = group.path.clone();
            }
        }
    }
    Ok(deepest)
}
