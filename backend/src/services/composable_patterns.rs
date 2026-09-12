//! Read-only preparation against one authorized venue snapshot. This preview
//! does not change the user's active score, transport, or hardware output.
use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::models::composable_patterns::{ComposablePreview, ComposablePreviewRequest};
use crate::models::selection::{Selection, Subset};
use crate::models::universe::{PrimitiveState, UniverseState};
use luma_patterns::{standard_library, Cell, Frame, PreparedGraph};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) async fn preview(
    pool: &sqlx::SqlitePool,
    fixtures_root: &Path,
    storage: &crate::storage::StorageRoot,
    request: ComposablePreviewRequest,
) -> Result<ComposablePreview, String> {
    if request.times.is_empty()
        || request.times.len() > 256
        || request.times.iter().any(|t| !t.is_finite())
        || !request.clip_start.is_finite()
        || !request.clip_end.is_finite()
        || request.clip_end <= request.clip_start
    {
        return Err("preview requires 1–256 finite sample times and a finite clip start".into());
    }
    if request.targets.is_empty() {
        return Err("choose at least one target group".into());
    }
    let mut access =
        VenueAccess::<Read>::read(pool, VenueResource::Venue(&request.venue_id)).await?;
    let principal = access.principal().to_owned();
    let beat_grid = crate::services::tracks::get_track_beats_for_connection(
        access.connection(),
        &request.track_id,
    )
    .await?
    .ok_or_else(|| {
        "track has no available beat grid; analyze the track before previewing a musical pattern"
            .to_string()
    })?;
    let clock = beat_grid.timeline().map_err(|error| error.to_string())?;
    let cells = resolve_cells(&mut access, fixtures_root, &request.targets, request.seed).await?;
    if cells.is_empty() {
        return Err("the targets contain no placed, controllable cells".into());
    }
    if cells.len().saturating_mul(request.times.len()) > 1_000_000 {
        return Err("preview exceeds one million cell samples; request fewer times".into());
    }
    let mut library = request.library.unwrap_or_else(standard_library);
    let definition_id = if let Some(definition) = library.definitions.get(&request.definition) {
        if !definition.playable() && definition.placeable() {
            let wrapped = definition
                .clip_instance(&request.definition)
                .map_err(|e| e.to_string())?;
            let mut id = "preview/output".to_owned();
            while library.definitions.contains_key(&id) {
                id.push('_');
            }
            library.definitions.insert(id.clone(), wrapped);
            id
        } else {
            request.definition.clone()
        }
    } else {
        request.definition.clone()
    };
    let definition = library
        .definitions
        .get(&definition_id)
        .ok_or_else(|| format!("unknown graph {}", request.definition))?;
    let lighting = definition
        .lighting_output()
        .ok_or_else(|| "connect this graph's signals to Output before previewing it".to_string())?
        .to_owned();
    let start = clock
        .beat_at(request.clip_start)
        .map_err(|e| e.to_string())?;
    let prepared = PreparedGraph::new(
        &library,
        &definition_id,
        &request.inputs,
        Frame {
            features: None,
            beat: start,
            clip_start: start,
            clip_duration: clock.beat_at(request.clip_end).map_err(|e| e.to_string())? - start,
            seed: request.seed,
            cells: &cells,
        },
    )
    .map_err(|e| e.to_string())?;
    let prepared = if prepared.feature_requests().is_empty() {
        prepared
    } else {
        let features = crate::eval::track_features::prepare(
            &mut access,
            storage,
            &request.track_id,
            &beat_grid,
            prepared.feature_requests(),
        )
        .await?;
        prepared
            .with_features(features)
            .map_err(|e| e.to_string())?
    };
    // Geometry and track data now belong to one authorized snapshot. Frame
    // evaluation holds no SQLite transaction and cannot change live playback.
    drop(access);
    let beats = request
        .times
        .iter()
        .map(|time| clock.beat_at(*time))
        .collect::<luma_patterns::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    let output = prepared.evaluate_batch(&beats).map_err(|e| e.to_string())?;
    let light = output
        .get(&lighting)
        .and_then(|v| v.lighting())
        .ok_or("pattern did not produce fixture output")?;
    let writes = light.writes();
    let frames = (0..beats.len())
        .map(|time| light.sample(time).map(universe).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, String>>()?;
    // A sign-out during preparation must not publish data under a new identity.
    let final_access =
        VenueAccess::<Read>::read(pool, VenueResource::Venue(&request.venue_id)).await?;
    if final_access.principal() != principal {
        return Err("authenticated identity changed during pattern preview".into());
    }
    Ok(ComposablePreview {
        cells,
        beats,
        frames,
        writes: ["color", "dimmer", "position", "strobe", "speed"]
            .into_iter()
            .zip(writes)
            .map(|(name, written)| (name.to_string(), written))
            .collect(),
    })
}
/// Resolve the authored head domain once, shared by saved-score playback and
/// previews. Subsets are applied after expanding fixture housings into heads.
pub(crate) async fn resolve_cells(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &Path,
    targets: &[Selection],
    seed: u64,
) -> Result<Vec<Cell>, String> {
    let mut cells = Vec::new();
    let mut seen = BTreeSet::new();
    for target in targets {
        target.validate().map_err(|error| error.to_string())?;
        let whole = Selection {
            expression: target.expression.clone(),
            subset: Subset::All,
        };
        let primitives = crate::eval::context::resolve_selection_primitives_with_access(
            access,
            fixtures_root,
            &whole,
            seed,
        )
        .await?;
        let primitives = select_heads(primitives, target.subset, seed);
        for (id, position) in primitives {
            if !seen.insert(id.clone()) {
                return Err(format!(
                    "target groups overlap at cell {id}; choose disjoint mapping groups"
                ));
            }
            cells.push(Cell {
                id,
                group: target.expression.clone(),
                world: position.map(f64::from),
                uvz: luma_patterns::Cell::stage_coordinates(position.map(f64::from)),
            });
        }
    }
    Ok(cells)
}

fn select_heads(
    mut heads: Vec<(String, [f32; 3])>,
    subset: Subset,
    seed: u64,
) -> Vec<(String, [f32; 3])> {
    let keep = subset.keep(heads.len());
    if keep == heads.len() {
        return heads;
    }
    let mut rank: Vec<_> = heads
        .iter()
        .enumerate()
        .map(|(index, (id, _))| {
            let mut hash = Sha256::new();
            hash.update(seed.to_le_bytes());
            hash.update(id.as_bytes());
            (hash.finalize(), index)
        })
        .collect();
    rank.sort();
    let selected: BTreeSet<_> = rank
        .into_iter()
        .take(keep)
        .map(|(_, index)| index)
        .collect();
    let mut index = 0;
    heads.retain(|_| {
        let keep = selected.contains(&index);
        index += 1;
        keep
    });
    heads
}
fn universe(lighting: BTreeMap<String, luma_patterns::FixtureOutput>) -> UniverseState {
    UniverseState {
        primitives: lighting
            .into_iter()
            .map(|(id, value)| {
                (
                    id,
                    PrimitiveState {
                        dimmer: value.dimmer.unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                        color: value.color.unwrap_or([1.0; 3]).map(|v| v as f32),
                        position: value.position.unwrap_or([0.0; 2]).map(|v| v as f32),
                        strobe: value.strobe.unwrap_or(0.0) as f32,
                        speed: value.speed.unwrap_or(1.0) as f32,
                    },
                )
            })
            .collect(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subsets_count_heads_not_housings() {
        let heads: Vec<_> = (0..16)
            .map(|n| (format!("one-bar:{n}"), [0.0, 0.0, n as f32]))
            .collect();
        let selected = select_heads(heads.clone(), Subset::Count(4), 10);
        assert_eq!(selected.len(), 4);
        let mut reversed = heads;
        reversed.reverse();
        let set =
            |v: Vec<(String, [f32; 3])>| v.into_iter().map(|(id, _)| id).collect::<BTreeSet<_>>();
        assert_eq!(
            set(selected),
            set(select_heads(reversed, Subset::Count(4), 10))
        );
    }
    #[test]
    fn color_and_dimmer_do_not_apply_coverage_twice() {
        let state = universe(BTreeMap::from([(
            "cell".into(),
            luma_patterns::FixtureOutput::from_rgb([0.1, 0.2, 0.4]),
        )]));
        let head = &state.primitives["cell"];
        assert_eq!(head.dimmer, 0.4);
        assert_eq!(head.color, [0.25, 0.5, 1.0]);
    }
}
