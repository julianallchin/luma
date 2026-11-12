//! `distribute` — the command that builds a rig instead of typing one.
//!
//! One call, one transaction: `count` fixtures are patched, named, placed,
//! bolted to a host face. It is the *only* fixture
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
//! | how long is the face, which way does it run | [`luma_scene::face`] |
//! | where along it does each fixture sit | [`luma_scene::distribute`] |
//! | what pose is that | [`luma_scene::venue::place_on`], through the resolver |
//! | which universe and address | [`luma_scene::patch`] |
//! | what is it called | [`crate::services::fixture_create::ModelNumbering`] |
//! | suggested placement-derived group path | [`crate::services::group_derivation`] |
//!
//! # Addresses are derived twice, and the second time is the answer
//!
//! [`luma_scene::patch::next_addresses`] is asked where the row *would* go,
//! because at that moment the rows do not exist and there is nothing for the
//! allocator to order. Once they do exist, the host's run is put through
//! [`luma_scene::patch::allocate_run`] — the one allocator — and the answer written
//! down. That is what makes two distributions on one truss interleave in
//! physical order rather than in the order somebody typed them, and it is why
//! this module has no addressing rule of its own to disagree with.
//!
//! # Refuse, never squeeze
//!
//! A distribution that will not go writes **nothing** and says why
//! ([`DistributeRefusal`]): the face is too short, and how long it would have to be — a
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

use luma_scene::distribute::{offsets, Fit, Layout};
use luma_scene::face::host_face;
use luma_scene::patch::{allocate_run, next_addresses, Footprint};
use luma_scene::venue::{Edge, EdgeError, Node, NodeKind, ResolvedVenue, VenueGraph, FLOOR_SOCKET};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{VenueAccess, Write};
use crate::database::local::venue_graph as venue_graph_db;
use crate::models::distribute::{
    DistributeOccupied, DistributeRefusal, DistributeReport, DistributedFixture,
};
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::venue_graph::{warning_line, ResolvedDangling};
use crate::services::fixture_create::{self, Naming, NewFixture};
use crate::services::group_derivation::derive_groups;
use crate::services::groups as group_service;
use crate::services::patch;

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
    /// How far off the face's middle the row sits across it, in metres. A
    /// truss face has no room for one; a floor or a deck top does.
    pub across_m: f64,
    /// How high above the face the row is lifted, in metres.
    pub trim_m: f64,
    /// The naming term, if the row should not be named after the model.
    pub label_prefix: Option<&'a str>,
    /// Lights already on this face that the row is made of, in order along
    /// it. They take the first seats and stay the lights they were; any past
    /// `count` leave, and only the seats beyond them get a new light.
    pub relay: &'a [String],
}

/// A refusal: the reason, and nothing written.
fn refused(
    host_node_id: String,
    host_socket: String,
    refusal: DistributeRefusal,
) -> DistributeReport {
    DistributeReport {
        host_node_id,
        host_socket,
        fixtures: Vec::new(),
        refusal: Some(refusal),
        warnings: Vec::new(),
        dangling: Vec::new(),
    }
}

/// Where on a host face a row is asked to go.
#[derive(Debug, Clone, Copy)]
pub struct Place<'a> {
    /// The node whose face hosts the row; `None` is the venue root.
    pub host_node: Option<&'a str>,
    /// The face on it. Defaults to the root's [`FLOOR_SOCKET`].
    pub host_socket: Option<&'a str>,
    pub layout: Layout,
    /// How far off the face's middle the row sits across it, in metres. A
    /// truss face has no room for one; a floor or a deck top does.
    pub across_m: f64,
}

/// A row's seat on a host face, once everything that can refuse it has been
/// asked: the face exists, a fixture clamps to it, the point is on it, the
/// row fits its length, and nothing already sits there.
struct Seats {
    host_id: String,
    host_socket: String,
    /// The fixture's own half of the joint.
    clamp: String,
    /// Metres along the face from its middle, one per light.
    stations: Vec<f64>,
}

/// Seat `count` bodies `width` metres wide at `place`. `moving` are lights
/// that are being moved, and so are not in their own way.
///
/// The one set of placement rules: a new row and a moved light both pass
/// through here, so neither can land where the other would be refused.
async fn seats(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
    graph: &VenueGraph,
    place: Place<'_>,
    count: usize,
    width: f64,
    moving: &[String],
) -> Result<Result<Seats, DistributeReport>, String> {
    let sockets = crate::venue_graph::sockets(fixtures_root)?;
    let host_id = match place.host_node {
        Some(id) => id.to_string(),
        None => graph.root().to_string(),
    };
    let host_socket = place.host_socket.unwrap_or(FLOOR_SOCKET).to_string();
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
    let clamp = luma_scene::catalog::fixture_clamp(0.0);
    if !clamp.socket_type.mates(face.socket.socket_type) {
        return Err(EdgeError::Polarity {
            held: clamp.socket_type,
            host: face.socket.socket_type,
        }
        .to_string());
    }

    // A point off the face's middle line has to still be on the face: a deck
    // top has room for one, a truss face does not.
    if let Some(face_width) = face.width.length_m {
        if place.across_m.abs() > face_width / 2.0 + TOUCHING_M {
            return Err(format!(
                "that point is {:.2} m off the middle of a face {face_width:.2} m wide, so \
                 it is not on `{}`",
                place.across_m.abs(),
                host.label.clone().unwrap_or(host_id.clone())
            ));
        }
    }

    let stations = match offsets(face.feature, place.layout, count, width) {
        Ok(stations) => stations,
        Err(Fit::TooLong {
            needed_m,
            available_m,
        }) => {
            let suggestion = format!(
                "needs {needed_m:.2} m, the face is {available_m:.2} m — trim({}, span={needed_m:.2})",
                host.id
            );
            return Ok(Err(refused(
                host_id,
                host_socket,
                DistributeRefusal::TooLong {
                    needed_m,
                    available_m,
                    extend_node_id: host.id.clone(),
                    suggestion,
                },
            )));
        }
    };

    let rows = fixtures_db::get_patched_fixtures(access).await?;
    if let Some((from_m, to_m)) = band(&stations, width) {
        let held_by: Vec<DistributeOccupied> = occupied(
            graph,
            &rows,
            fixtures_root,
            &host_id,
            &host_socket,
            place.across_m,
            moving,
        )
        .into_iter()
        .filter(|held| held.from_m < to_m - TOUCHING_M && from_m < held.to_m - TOUCHING_M)
        .collect();
        if !held_by.is_empty() {
            let suggestion = format!(
                "{from_m:.2}..{to_m:.2} m of this face is already held by {} — \
                 distribute into a span that clears it",
                held_by
                    .iter()
                    .map(|held| held.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(Err(refused(
                host_id,
                host_socket,
                DistributeRefusal::Overlap {
                    from_m,
                    to_m,
                    held_by,
                    suggestion,
                },
            )));
        }
    }
    Ok(Ok(Seats {
        host_id,
        host_socket,
        clamp: clamp.name,
        stations,
    }))
}

/// Move one placed fixture to `place`, keeping its id, its patch row and so
/// every group that holds it.
///
/// On the face it already hangs on it keeps its turn and its aim. On a new
/// face both are relative to a different surface, so they return to rest and
/// the light points straight out of where it now hangs. Returns whether that
/// happened.
///
/// # Errors
/// As [`distribute`] refuses a row of one: a face that does not exist or does
/// not take a fixture, a point off it, no room, or another light in the way.
pub async fn relocate(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
    node_id: &str,
    place: Place<'_>,
    trim_m: f64,
) -> Result<bool, String> {
    let sockets = crate::venue_graph::sockets(fixtures_root)?;
    let mut graph = crate::venue_graph::graph(access).await?;
    let old = graph
        .node(node_id)
        .filter(|node| node.kind == NodeKind::Fixture)
        .and_then(|_| graph.edge(node_id))
        .cloned()
        .ok_or_else(|| format!("`{node_id}` is not a placed light"))?;
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let width = rows
        .iter()
        .find(|row| row.id == node_id)
        .map_or(DEFAULT_FIXTURE_WIDTH_M, |row| {
            width_of(fixtures_root, &row.fixture_path)
        });
    let seats = seats(
        access,
        fixtures_root,
        &graph,
        place,
        1,
        width,
        &[node_id.to_string()],
    )
    .await?
    .map_err(|report| match report.refusal {
        Some(
            DistributeRefusal::TooLong { suggestion, .. }
            | DistributeRefusal::Overlap { suggestion, .. },
        ) => suggestion,
        None => "that place was refused".to_string(),
    })?;
    let rehung = old.parent != seats.host_id || old.their_socket != seats.host_socket;
    let edge = Edge {
        parent: seats.host_id,
        my_socket: seats.clamp,
        their_socket: seats.host_socket,
        roll: if rehung { 0.0 } else { old.roll },
    };
    graph
        .attach(node_id, edge.clone(), sockets)
        .map_err(|e| e.to_string())?;
    venue_graph_db::set_placement(access, node_id, &(&edge).into()).await?;
    let mut params = BTreeMap::from([
        ("u".to_string(), Some(seats.stations[0])),
        ("v".to_string(), Some(place.across_m)),
        ("trim".to_string(), Some(trim_m)),
    ]);
    if rehung {
        for key in luma_scene::venue::ANGLE_PARAMS {
            params.insert(key.to_string(), None);
        }
    }
    venue_graph_db::set_params(access, node_id, &params).await?;
    Ok(rehung)
}

/// Patch, name and place `count` fixtures along one host face.
///
/// # Errors
/// Invalid faces or joints are rejected before writing. Catalog, database or
/// exhausted-address-space errors abort the caller's transaction.
pub async fn distribute(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
    request: Request<'_>,
) -> Result<DistributeReport, String> {
    let mut graph = crate::venue_graph::graph(access).await?;
    let sockets = crate::venue_graph::sockets(fixtures_root)?;
    let (definition, channels) = definition_and_width(fixtures_root, &request)?;
    let width = body_width_m(&definition);
    let place = Place {
        host_node: request.host_node,
        host_socket: request.host_socket,
        layout: request.layout,
        across_m: request.across_m,
    };
    let Seats {
        host_id,
        host_socket,
        clamp,
        stations,
    } = match seats(
        access,
        fixtures_root,
        &graph,
        place,
        request.count,
        width,
        request.relay,
    )
    .await?
    {
        Ok(seats) => seats,
        Err(report) => return Ok(report),
    };
    let kept = request.relay.len().min(stations.len());
    for id in &request.relay[kept..] {
        fixture_create::delete(access, id)
            .await
            .map_err(|e| e.to_string())?;
        graph.remove(id);
    }
    let rows = fixtures_db::get_patched_fixtures(access).await?;

    // Addresses come from the allocator, against the venue as it stands — the
    // rows do not exist yet; see [`offer`]. What it gives back is provisional;
    // see the re-derivation below.
    let solved = luma_scene::venue::resolve(&graph, sockets);
    let run = run_of(&solved, &host_id);
    let footprints = offer(
        &solved,
        &rows,
        run.as_deref(),
        channels,
        stations.len() - kept,
    )?;

    let mut numbering = fixture_create::numbering(access)
        .await
        .map_err(|e| e.to_string())?;
    let mut placed: Vec<(String, String, Footprint, f64)> = Vec::new();
    // A light that stays keeps its turn, its aim, its address and its
    // groups: only where it sits along the face is written.
    for (id, station) in request.relay[..kept].iter().zip(&stations) {
        let row = rows
            .iter()
            .find(|row| &row.id == id)
            .ok_or_else(|| format!("`{id}` has no patch row"))?;
        let footprint = Footprint::new(
            u16::try_from(row.universe).unwrap_or_default(),
            u16::try_from(row.address).unwrap_or_default(),
            channels,
        )
        .ok_or_else(|| format!("`{id}` is patched outside a universe"))?;
        venue_graph_db::set_params(
            access,
            id,
            &BTreeMap::from([("u".to_string(), Some(*station))]),
        )
        .await?;
        placed.push((
            id.clone(),
            row.label.clone().unwrap_or_default(),
            footprint,
            *station,
        ));
    }
    for (station, footprint) in stations[kept..].iter().zip(&footprints) {
        let edge = Edge {
            parent: host_id.clone(),
            my_socket: clamp.clone(),
            their_socket: host_socket.clone(),
            roll: 0.0,
        };
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
                // One light takes the label as given; a row numbers it.
                name: match request.label_prefix {
                    Some(label) if stations.len() == 1 => Naming::Given(label.to_string()),
                    prefix => Naming::Minted(prefix),
                },
                placement: (&edge).into(),
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
                ("v".to_string(), request.across_m),
                ("trim".to_string(), request.trim_m),
            ])
            .into_iter()
            .collect(),
        };
        // The invariants are `luma_scene`'s; a refusal here leaves the
        // transaction to roll back everything this loop has done so far,
        // which is the whole of "never partial placement".
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
        placed.push((fixture.id, label, *footprint, *station));
    }

    // Two distributions on one run interleave in **physical** order, not
    // creation order (`docs/specs/venue-builder-gauntlet.md` §5); see
    // [`readdress_run`].
    let solved = crate::venue_graph::resolved(access, fixtures_root).await?;
    if let Some(run) = run.as_deref() {
        readdress_run(access, &solved, run).await?;
    }

    // The report says what the *database* says, rather than what the allocator
    // would have said: a run-less host keeps the provisional footprints, and
    // there is no third account of where a fixture is patched.
    let addressed = fixtures_db::get_patched_fixtures(access).await?;
    let paths = group_paths(access, fixtures_root).await?;

    Ok(DistributeReport {
        host_node_id: host_id,
        host_socket,
        fixtures: placed
            .into_iter()
            .map(|(id, label, footprint, along)| {
                let row = addressed.iter().find(|row| row.id == id);
                DistributedFixture {
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
        // The layout's own adjustments read as warnings because that is what
        // they are: something the call decided for the caller.
        warnings: solved
            .warnings()
            .iter()
            .map(warning_line)
            .chain(laid_where(request.layout, &stations, width))
            .collect(),
        dangling: solved
            .dangling()
            .iter()
            .map(ResolvedDangling::from)
            .collect(),
    })
}

/// Where `count` new fixtures of `channels` channels would land on `run`,
/// before their rows exist — so there is nothing for `allocate` to order them
/// by, and [`next_addresses`] is the door built for exactly this. Provisional:
/// once the rows exist, [`readdress_run`] gives the answer.
///
/// # Errors
/// The patch has no room for all `count` of them.
pub(crate) fn offer(
    solved: &ResolvedVenue,
    rows: &[PatchedFixture],
    run: Option<&str>,
    channels: u16,
    count: usize,
) -> Result<Vec<Footprint>, String> {
    let footprints = next_addresses(solved, &patch::inputs(rows), run, channels, count);
    if footprints.len() < count {
        return Err(format!(
            "there is no room in the patch for {count} more fixtures of {channels} channels: \
             {found} of them fit, so {short} would have nowhere to go",
            found = footprints.len(),
            short = count - footprints.len(),
        ));
    }
    Ok(footprints)
}

/// Re-derive `run`'s addressing by the one allocator, over the finished venue,
/// and write it down.
///
/// [`offer`] could only ever append, because the rows it was asked about did
/// not exist yet. They do now. Only this run is rewritten: adding lights here
/// is not an auto-patch, and re-addressing a truss on the other side of the
/// room because somebody hung two pars over here is a surprise nobody asked
/// for. Pins are preserved, and the allocator reserves untouched runs' stored
/// footprints, since only this run's planned addresses are written.
///
/// # Errors
/// A fixture of the run has no room in the patch, or the database's.
pub(crate) async fn readdress_run(
    access: &mut VenueAccess<'_, Write>,
    solved: &ResolvedVenue,
    run: &str,
) -> Result<(), String> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let allocation = allocate_run(solved, &patch::inputs(&rows), run);
    if let Some(luma_scene::patch::Note::NoRoom { fixture }) = allocation
        .notes
        .iter()
        .find(|note| matches!(note, luma_scene::patch::Note::NoRoom { .. }))
    {
        return Err(format!("No DMX address available for fixture {fixture}"));
    }
    for assignment in &allocation.assignments {
        if assignment.pinned {
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
        fixtures_db::update_fixture_address(access, &assignment.fixture, universe, address, false)
            .await?;
    }
    Ok(())
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
    across_m: f64,
    moving: &[String],
) -> Vec<DistributeOccupied> {
    let mut widths: BTreeMap<&str, f64> = BTreeMap::new();
    let mut held: Vec<DistributeOccupied> = graph
        .nodes()
        .filter(|node| node.kind == NodeKind::Fixture && !moving.contains(&node.id))
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
            // A surface holds rows side by side: only a body within its own
            // width of this row's line is in its way.
            if (node.params.get("v", 0.0) - across_m).abs() >= width - TOUCHING_M {
                return None;
            }
            let u = node.params.get("u", 0.0);
            Some(DistributeOccupied {
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
    crate::fixtures::definition(fixtures_root, fixture_path)
        .map_or(DEFAULT_FIXTURE_WIDTH_M, |definition| {
            body_width_m(&definition)
        })
}

/// The definition, and how many channels the named mode is wide.
fn definition_and_width(
    fixtures_root: &Path,
    request: &Request<'_>,
) -> Result<(FixtureDefinition, u16), String> {
    let definition = crate::fixtures::definition(fixtures_root, request.fixture_path)?;
    let mode = definition
        .modes
        .iter()
        .find(|mode| mode.name == request.mode_name)
        .ok_or_else(|| format!("`{}` has no mode `{}`", definition.model, request.mode_name))?;
    let channels = u16::try_from(mode.channels.len())
        .map_err(|_| format!("`{}` is wider than a universe", request.mode_name))?;
    Ok(((*definition).clone(), channels))
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
pub(crate) fn run_of(solved: &ResolvedVenue, host: &str) -> Option<String> {
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

/// Each fixture's deepest suggested placement-derived path.
///
/// This describes where a light landed; it does not create a saved selector
/// group. A caller authors groups explicitly.
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
