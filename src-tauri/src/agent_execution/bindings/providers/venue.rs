//! `luma.venue` — the room: fixtures, stage pieces, groups, and the position of
//! every primitive.
//!
//! The positions tensor is the load-bearing part. Its `primitive` axis is
//! labeled with the **exact** ordered ids the evaluator produces
//! ([`resolve_primitive_ids`] with the `all` expression), so a graph view and a
//! position row can be joined by identity rather than by index luck (design
//! §8.5). Re-deriving head expansion here would be a second source of truth and
//! would silently drift.
//!
//! `luma.venue.attributes` is deliberately absent: `ResidentContext.attributes`
//! is never populated, so advertising it would be a promise of zeros.

use std::collections::HashMap;

use serde::Serialize;

use super::{inline, put_f32, unavailable, ProviderCtx, NO_VENUE};
use crate::agent_execution::artifacts::ArtifactStore;
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::agent_execution::bindings::manifest::{AxisSpec, Provenance};
use crate::agent_execution::venue_host::environment_record;
use crate::database::local;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::eval::context::resolve_primitive_ids_with_access;
use crate::eval::ops::spatial::rig_uv;
use fixture_kinematics::StageDirection;
use glam::DVec3;
use luma_scene::venue::{NodePose, ResolvedVenue};
use luma_scene::View;

/// One patched fixture, with what its pose *means* alongside the pose itself.
///
/// `facing` and `facing_word` are derived, never stored: they are
/// `fixture_kinematics`'s answer for this mount, so an agent asking "which
/// fixtures point at the house" gets the renderer's answer rather than doing
/// its own arithmetic on `rotation` and reaching a different one.
#[derive(Serialize)]
struct FixtureBinding {
    id: String,
    label: Option<String>,
    manufacturer: String,
    model: String,
    mode: String,
    universe: i64,
    address: i64,
    num_channels: i64,
    /// Absent when the fixture is patched but unplaced — it is in the tray.
    position: Option<[f64; 3]>,
    /// Euler triple in the stored convention, radians.
    rotation: Option<[f64; 3]>,
    /// Unit vector, data space, that a parked head emits along.
    facing: Option<[f64; 3]>,
    /// The same direction as a stage word: `house`, `upstage`, `stage-left`,
    /// `stage-right`, `up`, `down`.
    facing_word: Option<&'static str>,
}

/// One stage piece in the same world frame as [`FixtureBinding::position`].
///
/// `position`/`rotation` are the **resolved** pose — poses exist nowhere else,
/// so there is no chain for an agent asked to find the booth to walk.
/// `parent_id` and the socket pair are kept so the *relation* is legible too:
/// "the mover is on the downstage truss" is the sentence the flattened metres
/// could never say.
#[derive(Serialize)]
struct PieceBinding {
    id: String,
    /// The graph's own alphabet: `stage`, `run`, `tower`, `piece`, `array`.
    kind: String,
    /// Snap/palette taxonomy: `floor`, `truss`, `speaker`, `cdj`, `mixer`, ...
    catalog_kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
    position: [f64; 3],
    rotation: [f64; 3],
    /// Unit vector, data space, that this node's mount frame faces.
    facing: [f64; 3],
    parent_id: Option<String>,
    my_socket: Option<String>,
    their_socket: Option<String>,
}

/// A node with no pose, by the root of its branch.
#[derive(Serialize)]
struct UnplacedBinding {
    id: String,
    kind: String,
    label: Option<String>,
    /// How many nodes hang off it, not counting itself.
    descendants: usize,
}

#[derive(Serialize)]
struct GroupBinding {
    id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    fixtures: Vec<GroupFixtureBinding>,
}

#[derive(Serialize)]
struct GroupFixtureBinding {
    id: String,
    label: String,
    /// Primitive ids of the heads that are in the group. Shorter than
    /// `head_count` ⇒ only part of the fixture belongs to it.
    heads: Vec<String>,
    head_count: i64,
}

pub async fn provide(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
) -> Result<(), String> {
    let Some(venue_id) = ctx.scope.venue_id.as_deref() else {
        for path in [
            "venue.id",
            "venue.name",
            "venue.environment",
            "venue.fixtures",
            "venue.pieces",
            "venue.unplaced",
            "venue.groups",
            "venue.positions",
            "venue.uv",
        ] {
            unavailable(b, path, NO_VENUE)?;
        }
        views(b)?;
        return Ok(());
    };

    // Convert off the old schema first: everything below reads through one
    // read transaction, and the conversion needs a write one. Cheap and
    // idempotent after the first time this venue is looked at.
    if let Err(e) = crate::venue_graph::ensure_migrated(ctx.pool, venue_id, ctx.resource_root).await
    {
        log::warn!("[venue] {venue_id} could not be converted to a graph: {e}");
    }

    let mut access = match VenueAccess::<Read>::read(ctx.pool, VenueResource::Venue(venue_id)).await
    {
        Ok(access) => access,
        Err(error) => {
            for path in [
                "venue.id",
                "venue.name",
                "venue.environment",
                "venue.fixtures",
                "venue.pieces",
                "venue.unplaced",
                "venue.groups",
                "venue.positions",
                "venue.uv",
            ] {
                unavailable(b, path, format!("the venue is not available: {error}"))?;
            }
            views(b)?;
            return Ok(());
        }
    };

    match local::venues::get_venue(&mut access).await {
        Ok(venue) => {
            inline(b, "venue.id", &venue.id)?;
            inline(b, "venue.name", &venue.name)?;
            // The light the room is in, off the row that was already read.
            // Static for the cell on purpose: it is the environment every
            // picture this cell takes comes out under, and `venue.environment`
            // is the live read for a program that has just moved it.
            inline(
                b,
                "venue.environment",
                environment_record(venue.environment),
            )?;
        }
        Err(e) => {
            inline(b, "venue.id", venue_id)?;
            for path in ["venue.name", "venue.environment"] {
                unavailable(b, path, format!("the venue could not be loaded: {e}"))?;
            }
        }
    }

    // One solve, both records. `venue.fixtures` and `venue.pieces` are two
    // halves of the same walk, and solving twice would be two answers to a
    // question with one.
    match crate::venue_graph::resolved(&mut access, ctx.resource_root).await {
        Ok(venue) => {
            fixtures(b, &mut access, &venue).await?;
            pieces(b, &venue)?;
            unplaced(b, &venue)?;
        }
        Err(e) => {
            for path in ["venue.fixtures", "venue.pieces", "venue.unplaced"] {
                unavailable(b, path, format!("the venue could not be resolved: {e}"))?;
            }
        }
    }

    views(b)?;
    groups(b, ctx, &mut access).await?;
    positions(b, ctx, store, &mut access).await
}

/// The patch, with where each fixture ended up.
///
/// `facing` and `facing_word` are derived, never stored: a fixture's rest
/// direction is the outward normal of the socket it hangs from, so they are
/// what the resolver says and what the renderer draws. Every consumer that used
/// to do its own arithmetic on `rotation` got a different answer.
///
/// A patched fixture nobody has placed is reported with no pose at all rather
/// than one at the origin — it is in the tray, and pretending otherwise is how
/// venues ended up with fixtures piled at `(0, 0, 0)`.
async fn fixtures(
    b: &mut BindingBuilder,
    access: &mut VenueAccess<'_, Read>,
    venue: &ResolvedVenue,
) -> Result<(), String> {
    let rows = match local::fixtures::get_patched_fixtures(access).await {
        Ok(rows) => rows,
        Err(e) => {
            return unavailable(
                b,
                "venue.fixtures",
                format!("the venue's fixtures could not be loaded: {e}"),
            )
        }
    };
    let bindings: Vec<FixtureBinding> = rows
        .iter()
        .map(|f| {
            let placed = venue.pose(&f.id);
            let (position, rotation) = placed.map(NodePose::data_pose).unzip();
            let facing = placed.map(|pose| {
                let (_, basis) = pose.data_basis();
                basis * DVec3::NEG_Z
            });
            FixtureBinding {
                id: f.id.clone(),
                label: f.label.clone(),
                manufacturer: f.manufacturer.clone(),
                model: f.model.clone(),
                mode: f.mode_name.clone(),
                universe: f.universe,
                address: f.address,
                num_channels: f.num_channels,
                position,
                rotation,
                facing: facing.map(|v| v.to_array()),
                facing_word: facing.map(|v| StageDirection::of(v.as_vec3()).label()),
            }
        })
        .collect();
    inline(b, "venue.fixtures", &bindings)
}

/// The set design: everything in the room that is not a light.
///
/// The pose is the resolver's, the same one `render(view="dj")` draws, so the
/// two agree about where the booth is by construction rather than by two copies
/// of the same walk staying in step. Which poses are objects at all is
/// [`NodePose::is_set_piece`] — the renderer's answer, not a second filter, so
/// an array anchor, which carries its members' `catalog_ref` and has no
/// geometry of its own, is not listed as an N+1th piece.
fn pieces(b: &mut BindingBuilder, venue: &ResolvedVenue) -> Result<(), String> {
    let bindings: Vec<PieceBinding> = venue
        .poses()
        .filter(|pose| pose.is_set_piece())
        .map(|pose| {
            let (position, rotation) = pose.data_pose();
            let (_, basis) = pose.data_basis();
            PieceBinding {
                id: pose.node.clone(),
                kind: pose.kind.as_str().to_string(),
                catalog_kind: crate::stage_render::catalog_kind(pose.catalog_ref.as_deref())
                    .to_string(),
                catalog_ref: pose.catalog_ref.clone(),
                label: pose.label.clone(),
                position,
                rotation,
                facing: (basis * DVec3::NEG_Z).to_array(),
                parent_id: pose.parent.clone(),
                my_socket: None,
                their_socket: None,
            }
        })
        .collect();
    inline(b, "venue.pieces", &bindings)
}

/// Everything the room has but has not placed: the patch tray, and any branch
/// a `detach` left hanging.
///
/// Reported by the root of each branch rather than per node — one reason, said
/// once — with the branch's size alongside, because "the wing is gone" and "the
/// wing is unplaced, 6 pieces" are different sentences and only the second is
/// true.
fn unplaced(b: &mut BindingBuilder, venue: &ResolvedVenue) -> Result<(), String> {
    let bindings: Vec<UnplacedBinding> = venue
        .unplaced()
        .iter()
        .map(|u| UnplacedBinding {
            id: u.node.clone(),
            kind: u.kind.as_str().to_string(),
            label: u.label.clone(),
            descendants: u.descendants,
        })
        .collect();
    inline(b, "venue.unplaced", &bindings)
}

/// The camera names `luma.venue.render(view=...)` accepts.
///
/// Sourced from [`View::ALL`] so the vocabulary is declared once, in the crate
/// that implements it. A hand-written list in Python would be a second source
/// of truth that drifts the first time a view is added.
fn views(b: &mut BindingBuilder) -> Result<(), String> {
    let names: Vec<&str> = View::ALL.iter().map(|view| view.name()).collect();
    inline(b, "venue.views", &names)
}

async fn groups(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    access: &mut VenueAccess<'_, Read>,
) -> Result<(), String> {
    let root = ctx.resource_root.to_path_buf();
    match crate::services::groups::get_grouped_hierarchy_with_path(&root, access).await {
        Ok(nodes) => {
            let bindings: Vec<GroupBinding> = nodes
                .into_iter()
                .map(|g| GroupBinding {
                    id: g.group_id,
                    name: g.group_name,
                    axis_lr: g.axis_lr,
                    axis_fb: g.axis_fb,
                    axis_ab: g.axis_ab,
                    fixtures: g
                        .fixtures
                        .into_iter()
                        .map(|f| GroupFixtureBinding {
                            id: f.id,
                            label: f.label,
                            heads: f.heads.into_iter().map(|h| h.id).collect(),
                            head_count: f.head_count,
                        })
                        .collect(),
                })
                .collect();
            inline(b, "venue.groups", &bindings)
        }
        Err(e) => unavailable(
            b,
            "venue.groups",
            format!("the venue's groups could not be loaded: {e}"),
        ),
    }
}

async fn positions(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    access: &mut VenueAccess<'_, Read>,
) -> Result<(), String> {
    // The `all` selection: no nodes, no edges, no args ⇒ the whole venue in the
    // evaluator's own order.
    let resolved = resolve_primitive_ids_with_access(
        access,
        ctx.resource_root,
        &[],
        &[],
        &HashMap::new(),
        None,
    )
    .await;

    if resolved.is_empty() {
        for path in ["venue.positions", "venue.uv"] {
            unavailable(
                b,
                path,
                "the venue has no patched fixtures, so it resolves to no primitives",
            )?;
        }
        return Ok(());
    }

    let ids: Vec<String> = resolved.iter().map(|(id, _)| id.clone()).collect();
    let data: Vec<f32> = resolved.iter().flat_map(|(_, p)| *p).collect();
    put_f32(
        b,
        store,
        "venue.positions",
        &data,
        vec![
            AxisSpec::labels("primitive", ids.clone()),
            AxisSpec::labels("coordinate", vec!["x".into(), "y".into(), "z".into()]),
        ],
        Some("m"),
        Provenance::new("venue_layout").with_note(
            "world positions in meters, Z-up; primitive ids are '<fixture id>:<head index>' \
             in the evaluator's own resolution order",
        ),
    )?;

    // The same primitives in the rig's own frame — the space patterns should be
    // authored in (`get_attribute u`/`v`). Same axis labels, same order, so it
    // joins to `venue.positions` by identity.
    let world: Vec<[f32; 3]> = resolved.iter().map(|(_, p)| *p).collect();
    let uv: Vec<f32> = rig_uv(&world).into_iter().flatten().collect();
    put_f32(
        b,
        store,
        "venue.uv",
        &uv,
        vec![
            AxisSpec::labels("primitive", ids),
            AxisSpec::labels("coordinate", vec!["u".into(), "v".into()]),
        ],
        None,
        Provenance::new("venue_layout").with_note(
            "rig-intrinsic pattern space, both 0..1 over the whole venue: u runs along the \
             first principal component of the horizontal (XY) spread, sign-canonicalized to \
             point +X; v is normalized height. Matches the `u`/`v` get_attribute values when \
             the selection is the whole venue — a narrower selection refits its own axis",
        ),
    )
}
