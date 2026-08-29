//! Loading, solving and converting a venue graph.
//!
//! [`luma_scene::venue`] owns the model and the maths;
//! [`crate::database::local::venue_graph`] owns the rows. This module is the
//! one place they meet, plus the two things neither can do alone: supply the
//! geometry a socket resolves against, and convert what the old schema held.
//!
//! # Solve on every read
//!
//! There is no pose cache. A solve is a depth-first walk with a socket-frame
//! build and a 4x4 multiply per node, tens of microseconds against a 16 ms
//! frame, and a cache would be a second source of truth with no way to tell it
//! is stale (design doc, Performance).

use std::path::Path;
use std::sync::OnceLock;

use glam::{DMat4, DVec3};
use luma_render::catalog::{fixture_clamp, VenueSockets, FIXTURE_CLAMP_SOCKET};
use luma_scene::coords::three_pose_from_data_d;
use luma_scene::sockets::ResolvedSocket;
use luma_scene::venue::{
    invert_placement, place_on, resolve as resolve_graph, root_socket, Edge, Node, NodeKind,
    NodeSockets, Params, ResolvedVenue, SurfacePlacement, VenueGraph, FLOOR_SOCKET, RIG_SOCKET,
};

use crate::database::local;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::models::stage::StagePiece;

/// The catalog's geometry, resolved once per process.
///
/// Eager and shared because [`VenueSockets::load`] reads every GLB in the
/// palette, and a venue is solved on every read: paying that per solve would be
/// the one thing that makes the solve not free.
///
/// # Errors
/// Fails if a catalog piece's GLB is missing, unreadable, or measures empty.
pub fn sockets(fixtures_root: &Path) -> Result<&'static VenueSockets, String> {
    static SOCKETS: OnceLock<Result<VenueSockets, String>> = OnceLock::new();
    SOCKETS
        .get_or_init(|| {
            VenueSockets::load(crate::stage_render::meshes_root(Some(fixtures_root)))
                .map_err(|e| format!("the stage catalog could not be resolved: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Read a venue's graph and solve it.
///
/// # Errors
/// Fails if the rows cannot be read, if the catalog cannot be resolved, or if
/// the venue has no root — which only happens if [`migrate`] has never run.
pub async fn resolved(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &Path,
) -> Result<ResolvedVenue, String> {
    let rows = local::venue_graph::get_graph(access).await?;
    let graph = rows
        .to_graph()
        .ok_or_else(|| "this venue has no graph root".to_string())?;
    Ok(resolve_graph(&graph, sockets(fixtures_root)?))
}

/// Convert this venue off the old schema if it has never been converted.
///
/// The counterpart to [`resolved`] for a caller that owns the pool: [`migrate`]
/// needs a write transaction, so a caller already holding a read one cannot ask
/// for it and must treat "no graph root" as an error. Reading the marker first
/// keeps the `BEGIN IMMEDIATE` off every solve — the conversion happens once per
/// venue, the solve happens on every read.
///
/// # Errors
/// Fails if the venue cannot be authorized, or as [`migrate`].
pub async fn ensure_migrated(
    pool: &sqlx::SqlitePool,
    venue_id: &str,
    fixtures_root: &Path,
) -> Result<(), String> {
    let mut read = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;
    let converted = local::venue_graph::root_id(&mut read).await?.is_some();
    drop(read);
    if converted {
        return Ok(());
    }
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id)).await?;
    if migrate(&mut access, fixtures_root).await? {
        access.commit().await?;
    }
    Ok(())
}

/// Read a venue's graph as the model, without solving it.
///
/// # Errors
/// As [`resolved`].
pub async fn graph(access: &mut impl AuthorizedVenue) -> Result<VenueGraph, String> {
    local::venue_graph::get_graph(access)
        .await?
        .to_graph()
        .ok_or_else(|| "this venue has no graph root".to_string())
}

// ---------------------------------------------------------------------------
// The conversion off `stage_pieces` and `fixtures.pos_*`
// ---------------------------------------------------------------------------

/// Build this venue's graph out of the old schema, if it has not been built.
///
/// Idempotent and lazy: the root node's existence is the marker, so a venue
/// that arrives later — pulled from a sync, restored from a backup — converts
/// the first time it is opened rather than needing a migration to have caught
/// it. It runs inside the caller's write transaction, so a failure leaves the
/// old rows untouched.
///
/// `stage_pieces` and `fixtures.pos_*`/`rot_*` are **read and left alone**. See
/// the header of `migrations/20260829000000_venue_graph.sql` for why they are
/// not dropped here.
///
/// # Errors
/// Fails if the old rows cannot be read, the catalog cannot be resolved, or a
/// write is refused.
pub async fn migrate(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
) -> Result<bool, String> {
    if local::venue_graph::root_id(access).await?.is_some() {
        return Ok(false);
    }
    let sockets = sockets(fixtures_root)?;
    let venue_id = access.venue_id().to_string();
    let pieces = local::stage::get_stage_pieces(access).await?;
    let fixtures = local::fixtures::get_patched_fixtures(access).await?;

    let root_id = format!("{venue_id}:venue");
    local::venue_graph::insert_node_with_id(access, &root_id, NodeKind::Venue.as_str(), None, None)
        .await?;

    // Every old piece's world pose, in the frame the chain composed in. This is
    // `stage_render::flatten_pieces`'s arithmetic, in `f64` — the pass has to
    // reproduce the old poses to 1e-6, and `f32` runs out of mantissa at a few
    // tens of metres.
    let worlds = flatten_old_poses(&pieces);

    // Pass 1: the nodes, under their old ids, so a selection or a group naming
    // a stage piece still names the same thing.
    for piece in &pieces {
        local::venue_graph::insert_node_with_id(
            access,
            &piece.id,
            kind_of(&piece.kind).as_str(),
            Some(&piece.mesh_path),
            piece.label.as_deref(),
        )
        .await?;
    }
    for fixture in &fixtures {
        local::venue_graph::insert_node_with_id(
            access,
            &fixture.id,
            NodeKind::Fixture.as_str(),
            Some(&fixture.id),
            fixture.label.as_deref(),
        )
        .await?;
    }

    // Pass 2: the edges. A node must exist before anything names it, which is
    // why this is two passes and not one.
    for (piece, world) in pieces.iter().zip(&worlds) {
        let node = Node {
            id: piece.id.clone(),
            kind: kind_of(&piece.kind),
            catalog_ref: Some(piece.mesh_path.clone()),
            label: piece.label.clone(),
            params: Params::default(),
        };
        let parent = piece
            .parent_piece_id
            .as_deref()
            .and_then(|id| pieces.iter().position(|p| p.id == id))
            .map(|i| (pieces[i].clone(), worlds[i]));

        let placed = match parent {
            // A parented piece: recover *which socket met which*, the fact the
            // old schema threw away the moment the drag ended.
            Some((parent_piece, parent_world)) => infer_attachment(
                &node,
                *world,
                &Node {
                    id: parent_piece.id.clone(),
                    kind: kind_of(&parent_piece.kind),
                    catalog_ref: Some(parent_piece.mesh_path.clone()),
                    label: None,
                    params: Params::default(),
                },
                parent_world,
                sockets,
            ),
            None => None,
        };

        let (parent_id, edge, placement) = match placed {
            Some(found) => (found.parent_id, found.edge, found.placement),
            // Free, or a parent whose sockets no longer explain the pose: land
            // it on the venue floor at the same spot.
            None => match on_the_floor(&node, *world, sockets) {
                Some((edge, placement)) => (root_id.clone(), edge, placement),
                // Nothing on this piece can sit on anything. Keep it in the
                // tree at the origin rather than dropping it — the label and
                // the mesh are still what the venue says it owns.
                None => (
                    root_id.clone(),
                    Edge {
                        parent: root_id.clone(),
                        my_socket: FLOOR_SOCKET.into(),
                        their_socket: FLOOR_SOCKET.into(),
                        roll: 0.0,
                    },
                    SurfacePlacement {
                        u: 0.0,
                        v: 0.0,
                        yaw: 0.0,
                        trim: 0.0,
                    },
                ),
            },
        };
        write_placement(access, &piece.id, &parent_id, &edge, placement).await?;
    }

    for fixture in &fixtures {
        let world = three_pose_from_data_d(
            [fixture.pos_x, fixture.pos_y, fixture.pos_z],
            [fixture.rot_x, fixture.rot_y, fixture.rot_z],
        );
        let (edge, placement) = fixture_placement(world);
        write_placement(access, &fixture.id, &root_id, &edge, placement).await?;
    }

    Ok(true)
}

/// The old parent-chain flattening, in `f64` and without the scale.
///
/// `scale` is dropped: `stage_pieces.scale` has an insert-only writer that has
/// only ever been handed `1.0` (there is no update path for it), and a piece
/// that is not its own size is not something the catalog can describe.
fn flatten_old_poses(pieces: &[StagePiece]) -> Vec<DMat4> {
    let local = |p: &StagePiece| {
        three_pose_from_data_d([p.pos_x, p.pos_y, p.pos_z], [p.rot_x, p.rot_y, p.rot_z])
    };
    pieces
        .iter()
        .map(|piece| {
            let mut model = local(piece);
            let mut parent = piece.parent_piece_id.as_deref();
            let mut budget = pieces.len();
            while let Some(id) = parent {
                let (Some(up), 1..) = (pieces.iter().find(|p| p.id == id), budget) else {
                    break;
                };
                budget -= 1;
                model = local(up) * model;
                parent = up.parent_piece_id.as_deref();
            }
            model
        })
        .collect()
}

/// The old `kind` string as a node kind.
///
/// Only the two structural ones are distinguished: a deck is what other things
/// stand on and a truss is what they hang from, and the rest are furniture.
fn kind_of(old: &str) -> NodeKind {
    match old {
        "floor" => NodeKind::Stage,
        "truss" => NodeKind::Run,
        _ => NodeKind::Piece,
    }
}

struct Attachment {
    parent_id: String,
    edge: Edge,
    placement: SurfacePlacement,
}

/// How close a recovered mate has to reproduce the old pose to count.
///
/// A millimetre over a stage: the old pose was produced by the same solver, so
/// the right pair reproduces it to float noise and every wrong pair misses by
/// the size of a piece.
const ATTACHMENT_TOLERANCE_M: f64 = 1e-6;

/// Which socket met which — recovered by trying every compatible pair and
/// keeping the one that reproduces the stored pose.
///
/// The Rust of `inferAttachmentSocketLocal`. It is a search rather than a
/// lookup because the old schema recorded the *result* of the mate and not the
/// mate, and there is no other way back.
fn infer_attachment(
    node: &Node,
    world: DMat4,
    parent: &Node,
    parent_world: DMat4,
    sockets: &VenueSockets,
) -> Option<Attachment> {
    let mine = sockets.sockets(node);
    let theirs = sockets.sockets(parent);
    let mut best: Option<(f64, Attachment)> = None;

    for held in &mine {
        if !held.socket_type.polarity().can_be_held() {
            continue;
        }
        for host in &theirs {
            if !held.socket_type.mates(host.socket_type) {
                continue;
            }
            let placement = invert_placement(world, parent_world, host, held, node.kind);
            let residual = residual_of(world, parent_world, host, held, node.kind, placement);
            // Tie-break on how much of the pose the placement had to carry:
            // a bolted pair explains it with zeroes, a surface fallback with
            // metres, and the bolt is what the builder actually did.
            let score = residual + 1e-9 * (placement.u.abs() + placement.v.abs());
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((
                    score,
                    Attachment {
                        parent_id: parent.id.clone(),
                        edge: Edge {
                            parent: parent.id.clone(),
                            my_socket: held.name.clone(),
                            their_socket: host.name.clone(),
                            roll: placement.yaw,
                        },
                        placement,
                    },
                ));
            }
        }
    }
    best.filter(|(score, _)| *score <= ATTACHMENT_TOLERANCE_M)
        .map(|(_, attachment)| attachment)
}

/// How far the placement's own pose lands from the one it was inverted out of.
fn residual_of(
    world: DMat4,
    parent_world: DMat4,
    host: &ResolvedSocket,
    held: &ResolvedSocket,
    kind: NodeKind,
    placement: SurfacePlacement,
) -> f64 {
    let rebuilt = place_on(parent_world, host, held, kind, placement);
    // Compare the whole frame, not just the origin: a piece rotated 180° about
    // its own mount has the same origin and is upside down.
    (0..4)
        .map(|c| (rebuilt.col(c) - world.col(c)).length())
        .fold(0.0, f64::max)
}

/// A piece with nowhere to be but the floor.
fn on_the_floor(
    node: &Node,
    world: DMat4,
    sockets: &VenueSockets,
) -> Option<(Edge, SurfacePlacement)> {
    let floor = root_socket(FLOOR_SOCKET)?;
    let held = sockets
        .sockets(node)
        .into_iter()
        .find(|s| s.socket_type.mates(floor.socket_type))?;
    let placement = invert_placement(world, DMat4::IDENTITY, &floor, &held, node.kind);
    Some((
        Edge {
            parent: String::new(),
            my_socket: held.name.clone(),
            their_socket: FLOOR_SOCKET.into(),
            roll: placement.yaw,
        },
        placement,
    ))
}

/// Where a patched fixture's old pose puts it: on the floor if its beam points
/// up, on the grid if it points down.
///
/// A fixture's stored pose *is* a mount frame, and beam = mount normal, so the
/// only question the old row can answer is which of the venue's two surfaces it
/// was hanging off. A beam pointing sideways belonged to a truss that the graph
/// has no record of; it lands on the nearer of the two and the builder re-hangs
/// it. That is the design doc's phase-0 decision — every rest direction moves —
/// showing up in the data.
fn fixture_placement(world: DMat4) -> (Edge, SurfacePlacement) {
    let clamp = fixture_clamp();
    // The beam comes out in the socket layer's frame, where data-space *up* is
    // `+Y`. Reading `.z` here would be reading the depth axis — which is the
    // exact mistake the design doc's audit found three copies of.
    let beam = world * DVec3::NEG_Y.extend(0.0);
    let name = if beam.y >= 0.0 { FLOOR_SOCKET } else { RIG_SOCKET };
    let host = root_socket(name).expect("both root sockets exist");
    let placement = invert_placement(world, DMat4::IDENTITY, &host, &clamp, NodeKind::Fixture);
    (
        Edge {
            parent: String::new(),
            my_socket: FIXTURE_CLAMP_SOCKET.into(),
            their_socket: name.into(),
            roll: placement.yaw,
        },
        placement,
    )
}

async fn write_placement(
    access: &mut VenueAccess<'_, Write>,
    node_id: &str,
    parent_id: &str,
    edge: &Edge,
    placement: SurfacePlacement,
) -> Result<(), String> {
    local::venue_graph::upsert_edge(
        access,
        node_id,
        parent_id,
        &edge.my_socket,
        &edge.their_socket,
        edge.roll,
    )
    .await?;
    let params = [
        ("u", placement.u),
        ("v", placement.v),
        ("trim", placement.trim),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), Some(v)))
    .collect();
    local::venue_graph::set_params(access, node_id, &params).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::local::venue_access::{Read, VenueResource};
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    const FIXTURES_ROOT: &str = "resources/fixtures";

    /// One old-schema venue: a deck on the floor, a truss standing on the
    /// deck's corner, a piece of equipment sitting on the deck top, and two
    /// fixtures — one aimed down, one aimed up.
    ///
    /// Every pose here is one the old builder could actually produce, which is
    /// the population the pass has to reproduce exactly. A piece rolled off its
    /// mount axis is not: the graph has no way to say it, and the conversion
    /// says so rather than pretending.
    const SEED: &str = "
        INSERT INTO venues (id, uid, name) VALUES ('v', 'alice', 'Old');
        INSERT INTO stage_pieces
            (id, uid, venue_id, mesh_path, kind, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, parent_piece_id)
        VALUES
            ('deck', 'alice', 'v', 'stage_lab/stage_praticavel_2x1x1.glb', 'floor', 'Deck',
             2.0, -3.0, 0.5, 0.0, 0.0, 0.0, NULL),
            ('cdj', 'alice', 'v', 'stage_lab/cdj_3000x.glb', 'cdj', 'Left deck',
             0.25, 0.1, 0.65, 0.0, 0.0, 0.0, NULL);
        INSERT INTO fixtures
            (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path,
             pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
        VALUES
            ('flown', 'alice', 'v', 1, 1, 8, 'Test', 'Mover', 'Basic', 'test.qxf',
             1.5, 2.0, 6.0, 0.0, 0.0, 0.0),
            ('uplight', 'alice', 'v', 1, 9, 8, 'Test', 'Par', 'Basic', 'test.qxf',
             -1.0, 4.0, 0.1, 3.141592653589793, 0.0, 0.0);
    ";

    async fn seeded() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("venue-graph.db"))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::raw_sql(SEED).execute(&pool).await.unwrap();
        (directory, pool)
    }

    /// The old world poses, computed the way the old renderer computed them.
    async fn old_poses(pool: &sqlx::SqlitePool) -> Vec<(String, [f64; 3], [f64; 3])> {
        let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        let pieces = local::stage::get_stage_pieces(&mut access).await.unwrap();
        let fixtures = local::fixtures::get_patched_fixtures(&mut access)
            .await
            .unwrap();
        let worlds = flatten_old_poses(&pieces);
        pieces
            .iter()
            .zip(&worlds)
            .map(|(piece, world)| {
                let (pos, rot) = luma_scene::coords::data_pose_of_d(*world);
                (piece.id.clone(), pos, rot)
            })
            .chain(fixtures.iter().map(|f| {
                (
                    f.id.clone(),
                    [f.pos_x, f.pos_y, f.pos_z],
                    [f.rot_x, f.rot_y, f.rot_z],
                )
            }))
            .collect()
    }

    /// The load-bearing one: the conversion's proof.
    ///
    /// Every old pose comes back out of the resolver, to 1e-6. That is the
    /// whole contract of the pass — a venue that opens after the migration must
    /// be the venue that closed before it.
    ///
    /// **Fixture facing is deliberately excluded**, and only facing: the design
    /// doc's phase 0 moves every rest direction ("beam = mount normal"), so a
    /// fixture's *position* is pinned here and its direction is expected to
    /// change. Render goldens recapture for exactly that reason.
    #[tokio::test]
    async fn migration_round_trips_old_poses() {
        let (_dir, pool) = seeded().await;
        let before = old_poses(&pool).await;

        let mut access = VenueAccess::<Write>::write(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        assert!(migrate(&mut access, Path::new(FIXTURES_ROOT))
            .await
            .unwrap());
        access.commit().await.unwrap();

        let mut access = VenueAccess::<Read>::read(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        let venue = resolved(&mut access, Path::new(FIXTURES_ROOT))
            .await
            .unwrap();

        for (id, position, rotation) in &before {
            let pose = venue
                .pose(id)
                .unwrap_or_else(|| panic!("{id} is missing from the solved venue"));
            let (got_position, got_rotation) = pose.data_pose();
            for axis in 0..3 {
                assert!(
                    (got_position[axis] - position[axis]).abs() < 1e-6,
                    "{id} position: {got_position:?} vs {position:?}"
                );
            }
            if pose.kind != NodeKind::Fixture {
                for axis in 0..3 {
                    assert!(
                        (got_rotation[axis] - rotation[axis]).abs() < 1e-6,
                        "{id} rotation: {got_rotation:?} vs {rotation:?}"
                    );
                }
            }
        }
    }

    /// A fixture keeps which way it was pointing, up or down — the one thing
    /// about its facing the old row could actually say.
    #[tokio::test]
    async fn a_flown_fixture_stays_flown() {
        let (_dir, pool) = seeded().await;
        let mut access = VenueAccess::<Write>::write(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        migrate(&mut access, Path::new(FIXTURES_ROOT))
            .await
            .unwrap();
        access.commit().await.unwrap();

        let mut access = VenueAccess::<Read>::read(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        let venue = resolved(&mut access, Path::new(FIXTURES_ROOT))
            .await
            .unwrap();
        let beam = |id: &str| {
            let (_, basis) = venue.pose(id).unwrap().data_basis();
            (basis * DVec3::NEG_Z).z
        };
        assert!(beam("flown") < -0.99, "a flown mover points down");
        assert!(beam("uplight") > 0.99, "an uplighter points up");
    }

    /// Idempotent: the root's existence is the marker, so a second pass is a
    /// no-op rather than a second copy of the room.
    #[tokio::test]
    async fn migrating_twice_changes_nothing() {
        let (_dir, pool) = seeded().await;
        for expected in [true, false] {
            let mut access = VenueAccess::<Write>::write(&pool, VenueResource::Venue("v"))
                .await
                .unwrap();
            assert_eq!(
                migrate(&mut access, Path::new(FIXTURES_ROOT))
                    .await
                    .unwrap(),
                expected
            );
            access.commit().await.unwrap();
        }
        let mut access = VenueAccess::<Read>::read(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        let venue = resolved(&mut access, Path::new(FIXTURES_ROOT))
            .await
            .unwrap();
        // The room, two pieces, two fixtures.
        assert_eq!(venue.poses().count(), 5);
    }

    /// The relation, not just the pose: a piece that was parented comes out
    /// attached, with the socket pair the old builder must have used.
    #[tokio::test]
    async fn a_parented_piece_keeps_its_joint() {
        let (_dir, pool) = seeded().await;
        // Re-parent the CDJ onto the deck, in the deck's local space, so the
        // pass has an attachment to recover rather than a free placement.
        sqlx::query(
            "UPDATE stage_pieces SET parent_piece_id = 'deck', pos_x = 0.25, pos_y = 0.1,
                                     pos_z = 0.15 WHERE id = 'cdj'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut access = VenueAccess::<Write>::write(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        migrate(&mut access, Path::new(FIXTURES_ROOT))
            .await
            .unwrap();
        access.commit().await.unwrap();

        let mut access = VenueAccess::<Read>::read(&pool, VenueResource::Venue("v"))
            .await
            .unwrap();
        let rows = local::venue_graph::get_graph(&mut access).await.unwrap();
        let edge = rows
            .edges
            .iter()
            .find(|e| e.child_id == "cdj")
            .expect("the cdj is placed");
        assert_eq!(edge.parent_id, "deck", "the parent link survives");
        assert_eq!(
            edge.my_socket, "mount",
            "a cdj meets a surface through its mount"
        );
    }
}
