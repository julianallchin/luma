//! `luma.venue` — the room: fixtures, groups, and the position of every
//! primitive.
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
use crate::eval::context::resolve_primitive_ids;

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
            "venue.groups",
            "venue.positions",
        ] {
            unavailable(b, path, NO_VENUE)?;
        }
        return Ok(());
    };

    match local::venues::get_venue(ctx.pool, venue_id).await {
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

    match local::fixtures::get_fixtures_for_venue(ctx.pool, venue_id).await {
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

    groups(b, ctx, venue_id).await?;
    positions(b, ctx, store, venue_id).await
}

async fn groups(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    venue_id: &str,
) -> Result<(), String> {
    let root = ctx.resource_root.to_path_buf();
    match crate::services::groups::get_grouped_hierarchy_with_path(&root, ctx.pool, venue_id).await
    {
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
    venue_id: &str,
) -> Result<(), String> {
    // The `all` selection: no nodes, no edges, no args ⇒ the whole venue in the
    // evaluator's own order.
    let resolved = resolve_primitive_ids(
        ctx.pool,
        venue_id,
        ctx.resource_root,
        &[],
        &[],
        &HashMap::new(),
    )
    .await;

    if resolved.is_empty() {
        return unavailable(
            b,
            "venue.positions",
            "the venue has no patched fixtures, so it resolves to no primitives",
        );
    }

    let ids: Vec<String> = resolved.iter().map(|(id, _)| id.clone()).collect();
    let data: Vec<f32> = resolved.iter().flat_map(|(_, p)| *p).collect();
    put_f32(
        b,
        store,
        "venue.positions",
        &data,
        vec![
            AxisSpec::labels("primitive", ids),
            AxisSpec::labels("coordinate", vec!["x".into(), "y".into(), "z".into()]),
        ],
        Some("m"),
        Provenance::new("venue_layout").with_note(
            "world positions in meters, Z-up; primitive ids are '<fixture id>:<head index>' \
             in the evaluator's own resolution order",
        ),
    )
}
