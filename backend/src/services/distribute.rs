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
//! | which universe and address | [`luma_scene::patch`] |
//! | what is it called | [`crate::services::fixture_create::ModelNumbering`] |
//! | which group does it land in | [`crate::services::group_derivation`] |
//!
//! # Addresses are derived twice, and the second time is the answer
//!
//! [`luma_scene::patch::next_addresses`] is asked where the row *would* go,
//! because at that moment the rows do not exist and there is nothing for the
//! allocator to order. Once they do exist, the host's run is put through
//! [`luma_scene::patch::allocate`] — the one allocator — and the answer written
//! down. That is what makes two distributions on one truss interleave in
//! physical order rather than in the order somebody typed them, and it is why
//! this module has no addressing rule of its own to disagree with.
//!
//! # Refuse, never squeeze
//!
//! A distribution that will not go writes **nothing** and says why
//! ([`Refusal`]): the face is too short, and how long it would have to be — a
//! length the host can actually be built at, so feeding it back into the run's
//! `span` and re-running the same call succeeds — or there is already a row
//! where this one would sit, and which fixtures are in the way.
//!
//! The refusal is a *report*, not an error: nothing was wrong with the call,
//! the truss is short or the truss is full. Only the things the design doc
//! calls hard errors — a socket that does not exist, a polarity that forbids
//! the joint — come back as [`CommandError`]s, and they come back before any
//! row is written.

use std::collections::BTreeMap;
use std::path::Path;

use luma_render::face::host_face;
use luma_scene::distribute::{offsets, Fit, Layout};
use luma_scene::patch::{allocate, next_addresses, Footprint};
use luma_scene::venue::{
    DanglingSocket, Edge, EdgeError, Node, NodeKind, NodeWarning, ResolvedVenue, UnplacedNode,
    VenueGraph, FLOOR_SOCKET,
};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{VenueAccess, Write};
use crate::database::local::venue_graph as venue_graph_db;
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::services::fixture_create::{self, Naming, NewFixture};
use crate::services::group_derivation::derive_groups;
use crate::services::patch;
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
    /// Metres along the host face from its middle, ascending across the row.
    ///
    /// Not the same axis as the address order: addresses run along the *run*,
    /// and a face whose tangent opposes it counts the other way. Sorting a
    /// venue's fixtures by this and reading their addresses is only monotone
    /// on a face that agrees with its run.
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

/// A stretch of a host face a row of fixtures already holds.
#[derive(Debug, Clone)]
pub struct Occupied {
    /// The fixture in the way, by its label.
    pub label: String,
    /// The metres along the face its body spans, from the face's middle.
    pub from_m: f64,
    pub to_m: f64,
}

/// Why a distribution wrote nothing.
///
/// One field rather than a nullable per reason: a distribution is refused for
/// exactly one cause, and "too long *and* in the way" is not a report anybody
/// can act on. `refusal.is_none()` is the whole of "it worked", so there is no
/// second `ok` flag to disagree with it.
#[derive(Debug, Clone)]
pub enum Refusal {
    /// The row is longer than the face.
    TooLong(FitFailure),
    /// The row would sit on top of one already on this face. The 1-D check
    /// along the face, which is the only axis a distribution moves in; two rows
    /// on *different* faces of the same truss never meet here.
    Overlap {
        /// The metres the new row would have claimed.
        from_m: f64,
        to_m: f64,
        /// What is already there, in face order.
        held_by: Vec<Occupied>,
    },
}

/// What one distribution did.
#[derive(Debug, Clone)]
pub struct Report {
    pub host_node: String,
    pub host_socket: String,
    /// Every fixture placed — all of them, or, when [`Self::refusal`] is set,
    /// none: a distribution is all of its fixtures or nothing at all.
    pub fixtures: Vec<Placed>,
    pub refusal: Option<Refusal>,
    /// Whatever the solve had to decide, once the row was placed.
    pub warnings: Vec<NodeWarning>,
    /// What the *layout* decided for the caller, in words: a window clipped to
    /// the face, ends held half a body inside it. The row is placed either way
    /// — this is the difference between what was asked and what hangs there.
    pub announce: Vec<String>,
    /// Open structural sockets left in the venue, as the resolver reports them.
    pub dangling: Vec<DanglingSocket>,
    /// Subtrees the solve could not reach — the tray, and anything detached.
    pub unplaced: Vec<UnplacedNode>,
}

impl Report {
    /// A refusal: the reason, and nothing written.
    fn refused(host_node: String, host_socket: String, refusal: Refusal) -> Report {
        Report {
            host_node,
            host_socket,
            fixtures: Vec::new(),
            refusal: Some(refusal),
            warnings: Vec::new(),
            announce: Vec::new(),
            dangling: Vec::new(),
            unplaced: Vec::new(),
        }
    }
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
    // Only the socket's *name* and *type* are wanted here — what it mates
    // with, and what the edge is called. The standoff belongs to the housing
    // and reaches the pose through the node's own supply lookup, so this
    // probe deliberately asks for none.
    let clamp = luma_render::catalog::fixture_clamp(0.0);
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
            return Ok(Report::refused(
                host_id,
                host_socket,
                Refusal::TooLong(FitFailure {
                    needed_m,
                    available_m,
                    extend_node: host.id.clone(),
                }),
            ));
        }
    };

    let rows = fixtures_db::get_patched_fixtures(access).await?;
    if let Some((from_m, to_m)) = band(&stations, width) {
        let held_by: Vec<Occupied> = occupied(&graph, &rows, fixtures_root, &host_id, &host_socket)
            .into_iter()
            .filter(|held| held.from_m < to_m - TOUCHING_M && from_m < held.to_m - TOUCHING_M)
            .collect();
        if !held_by.is_empty() {
            return Ok(Report::refused(
                host_id,
                host_socket,
                Refusal::Overlap {
                    from_m,
                    to_m,
                    held_by,
                },
            ));
        }
    }

    // Addresses come from the allocator, against the venue as it stands — the
    // rows do not exist yet, so there is nothing for `allocate` to order them
    // by and `next_addresses` is the door built for exactly this caller. What
    // it gives back is provisional; see the re-derivation below.
    let solved = luma_scene::venue::resolve(&graph, sockets);
    let run = run_of(&solved, &host_id);
    let footprints = next_addresses(
        &solved,
        &patch::inputs(&rows),
        run.as_deref(),
        channels,
        stations.len(),
    );
    if footprints.len() < stations.len() {
        return Err(format!(
            "there is no room in the patch for {count} more fixtures of {channels} channels: \
             {found} of them fit, so {short} would have nowhere to go",
            count = stations.len(),
            found = footprints.len(),
            short = stations.len() - footprints.len(),
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
            catalog_ref: Some(request.fixture_path.to_string()),
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

    // Two distributions on one run interleave in **physical** order, not
    // creation order (`docs/specs/venue-builder-gauntlet.md` §5).
    // `next_addresses` could only ever append, because the rows it was asked
    // about did not exist yet. They do now, so the run's addressing is derived
    // again — by the one allocator, over the finished venue — and written down.
    //
    // Only the host's own run is rewritten. A distribution is not an auto-patch:
    // re-addressing a truss on the other side of the room because somebody hung
    // two pars over here is a surprise nobody asked for, and pins are preserved
    // either way because `allocate` reserves them first.
    let solved = crate::venue_graph::resolved(access, fixtures_root).await?;
    if let Some(run) = run.as_deref() {
        let rows = fixtures_db::get_patched_fixtures(access).await?;
        let allocation = allocate(&solved, &patch::inputs(&rows));
        for assignment in &allocation.assignments {
            if assignment.pinned || assignment.run.as_deref() != Some(run) {
                continue;
            }
            let universe = i64::from(assignment.footprint.universe());
            let address = i64::from(assignment.footprint.address());
            let stands_there = rows.iter().any(|row| {
                row.id == assignment.fixture && row.universe == universe && row.address == address
            });
            if stands_there {
                continue;
            }
            fixtures_db::update_fixture_address(
                access,
                &assignment.fixture,
                universe,
                address,
                false,
            )
            .await?;
        }
    }

    // The report says what the *database* says, rather than what the allocator
    // would have said: a run-less host keeps the provisional footprints, and
    // there is no third account of where a fixture is patched.
    let addressed = fixtures_db::get_patched_fixtures(access).await?;
    let paths = group_paths(access, fixtures_root).await?;

    Ok(Report {
        host_node: host_id,
        host_socket,
        fixtures: placed
            .into_iter()
            .map(|(id, label, footprint, along)| {
                let row = addressed.iter().find(|row| row.id == id);
                Placed {
                    label,
                    universe: row
                        .and_then(|row| u16::try_from(row.universe).ok())
                        .unwrap_or_else(|| footprint.universe()),
                    address: row
                        .and_then(|row| u16::try_from(row.address).ok())
                        .unwrap_or_else(|| footprint.address()),
                    along_m: along,
                    group_path: paths.get(&id).cloned().unwrap_or_default(),
                    id,
                }
            })
            .collect(),
        refusal: None,
        warnings: solved.warnings().to_vec(),
        announce: laid_where(request.layout, &stations, width),
        dangling: solved.dangling().to_vec(),
        unplaced: solved.unplaced().to_vec(),
    })
}

/// What a `span=` window turned into, where it turned into something else.
///
/// Two things move a row inside the window it named and neither is visible in
/// the result: the window is clipped to the face it is on, and a fixture is
/// placed by its **centre**, so each end holds half a body inside. Both are
/// right; both are also a rig half a metre narrower than the one that was
/// asked for, and the design's rule for that is to build it and say so.
fn laid_where(layout: Layout, stations: &[f64], width_m: f64) -> Vec<String> {
    let Layout::Span(a, b) = layout else {
        return Vec::new();
    };
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let (Some(first), Some(last)) = (stations.first(), stations.last()) else {
        return Vec::new();
    };
    if (first - lo).abs() < LAYOUT_EPSILON_M && (hi - last).abs() < LAYOUT_EPSILON_M {
        return Vec::new();
    }
    vec![format!(
        "span=({lo:.2}, {hi:.2}) laid out from {first:.2} m to {last:.2} m: a fixture is \
         placed by its centre and keeps half its {width_m:.2} m body inside the window, and \
         the window itself is clipped to the face"
    )]
}

/// How far a laid row may sit from the window it named before it is worth
/// saying so. Half a centimetre: a builder cares about the half-metre a body
/// width costs and not about float drift.
const LAYOUT_EPSILON_M: f64 = 5e-3;

/// Slack, in metres, below which two bodies count as merely touching rather
/// than overlapping. A row laid flush against another is a rig somebody built
/// on purpose; a nanometre of float drift between two solves is not.
const TOUCHING_M: f64 = 1e-6;

/// The metres a row of bodies claims along its face: the outer two centres
/// pushed out by half a body each. `None` for a row of nothing.
fn band(stations: &[f64], width_m: f64) -> Option<(f64, f64)> {
    let first = stations.first()?;
    let last = stations.last()?;
    Some((first - width_m / 2.0, last + width_m / 2.0))
}

/// What is already hanging on one host face, in face order.
///
/// Read off the graph rather than the solve, because the question is about a
/// face and its `u` — a fixture's *pose* has already left the face's frame, and
/// asking in world space would have to undo the whole walk to get back here.
///
/// A fixture whose definition cannot be read is measured at
/// [`DEFAULT_FIXTURE_WIDTH_M`], the same fallback a new one gets: an
/// unmeasurable body is still a body in the way.
fn occupied(
    graph: &VenueGraph,
    rows: &[PatchedFixture],
    fixtures_root: &Path,
    host: &str,
    socket: &str,
) -> Vec<Occupied> {
    let mut widths: BTreeMap<&str, f64> = BTreeMap::new();
    let mut held: Vec<Occupied> = graph
        .nodes()
        .filter(|node| node.kind == NodeKind::Fixture)
        .filter(|node| {
            graph
                .edge(&node.id)
                .is_some_and(|edge| edge.parent == host && edge.their_socket == socket)
        })
        .filter_map(|node| {
            let row = rows.iter().find(|row| row.id == node.id)?;
            let width = *widths
                .entry(row.fixture_path.as_str())
                .or_insert_with(|| width_of(fixtures_root, &row.fixture_path));
            let u = node.params.get("u", 0.0);
            Some(Occupied {
                label: row.label.clone().unwrap_or_else(|| node.id.clone()),
                from_m: u - width / 2.0,
                to_m: u + width / 2.0,
            })
        })
        .collect();
    held.sort_by(|a, b| a.from_m.total_cmp(&b.from_m));
    held
}

/// One definition's body width, or the fallback if it cannot be read.
fn width_of(fixtures_root: &Path, fixture_path: &str) -> f64 {
    fixture_service::get_fixture_definition(fixtures_root, Path::new(fixture_path))
        .map_or(DEFAULT_FIXTURE_WIDTH_M, |definition| {
            body_width_m(&definition)
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
    let tree = derive_groups(&group_service::solve(fixtures_root, access).await?.facts);
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
