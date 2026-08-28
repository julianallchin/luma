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
use crate::database::local;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::eval::context::resolve_primitive_ids_with_access;
use crate::eval::ops::spatial::rig_uv;
use crate::stage_render::flatten_pieces;
use luma_scene::View;

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
    position: [f64; 3],
}

/// One stage piece in the same world frame as [`FixtureBinding::position`].
///
/// `position`/`rotation` are the *flattened* pose — a piece parented to a truss
/// stores its pose in the truss's local space, and an agent asked to find the
/// booth cannot be expected to walk that chain. `parent_id` is kept so the
/// attachment is still legible.
#[derive(Serialize)]
struct PieceBinding {
    id: String,
    /// Snap/palette taxonomy: `floor`, `truss`, `speaker`, `cdj`, `mixer`, ...
    kind: String,
    mesh_path: String,
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    parent_id: Option<String>,
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
            "venue.fixtures",
            "venue.pieces",
            "venue.groups",
            "venue.positions",
            "venue.uv",
        ] {
            unavailable(b, path, NO_VENUE)?;
        }
        views(b)?;
        return Ok(());
    };

    let mut access = match VenueAccess::<Read>::read(ctx.pool, VenueResource::Venue(venue_id)).await
    {
        Ok(access) => access,
        Err(error) => {
            for path in [
                "venue.id",
                "venue.name",
                "venue.fixtures",
                "venue.pieces",
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
        }
        Err(e) => {
            inline(b, "venue.id", venue_id)?;
            unavailable(
                b,
                "venue.name",
                format!("the venue could not be loaded: {e}"),
            )?;
        }
    }

    match local::fixtures::get_patched_fixtures(&mut access).await {
        Ok(fixtures) => {
            let bindings: Vec<FixtureBinding> = fixtures
                .iter()
                .map(|f| FixtureBinding {
                    id: f.id.clone(),
                    label: f.label.clone(),
                    manufacturer: f.manufacturer.clone(),
                    model: f.model.clone(),
                    mode: f.mode_name.clone(),
                    universe: f.universe,
                    address: f.address,
                    num_channels: f.num_channels,
                    position: [f.pos_x, f.pos_y, f.pos_z],
                })
                .collect();
            inline(b, "venue.fixtures", &bindings)?;
        }
        Err(e) => unavailable(
            b,
            "venue.fixtures",
            format!("the venue's fixtures could not be loaded: {e}"),
        )?,
    }

    pieces(b, &mut access).await?;
    views(b)?;
    groups(b, ctx, &mut access).await?;
    positions(b, ctx, store, &mut access).await
}

/// The set design: everything in the room that is not a light.
///
/// The world pose comes from [`flatten_pieces`], the same parent-chain
/// flattening the renderer draws with, so `render(view="dj")` and this record
/// agree about where the booth is. The taxonomy and the attachment are read off
/// the rows, which `flatten_pieces` returns one-for-one and in order.
async fn pieces(b: &mut BindingBuilder, access: &mut VenueAccess<'_, Read>) -> Result<(), String> {
    let rows = match local::stage::get_stage_pieces(access).await {
        Ok(rows) => rows,
        Err(e) => {
            return unavailable(
                b,
                "venue.pieces",
                format!("the venue's stage pieces could not be loaded: {e}"),
            )
        }
    };
    let bindings: Vec<PieceBinding> = flatten_pieces(&rows)
        .into_iter()
        .zip(&rows)
        .map(|(flat, row)| PieceBinding {
            id: flat.id,
            kind: row.kind.clone(),
            mesh_path: flat.mesh_path,
            position: flat.pos,
            rotation: flat.rot,
            scale: flat.scale,
            parent_id: row.parent_piece_id.clone(),
        })
        .collect();
    inline(b, "venue.pieces", &bindings)
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
