//! Compiling a score into a scene, and previewing one of its clips.
//!
//! A score itself is rows — see [`crate::database::local::scores::rows`]. This
//! module is the read side: it turns the in-memory document into the
//! compositor's plans, with the venue geometry and beat grid of one authorized
//! read.
use luma_patterns::{standard_library, Score};

/// Compile the canonical score directly into the existing compositor's plans.
/// Geometry, group selections and the beat grid come from one authorized read.
pub(crate) async fn prepare_scene(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
) -> Result<crate::eval::Scene, String> {
    Ok(
        prepare_scene_data(access, fixtures_root, storage, track_id, score, false)
            .await?
            .scene,
    )
}

/// A compiled scene, the track features it reads, and the cells each clip
/// lights.
struct SceneData {
    scene: crate::eval::Scene,
    features: Option<std::sync::Arc<crate::eval::track_features::TrackFeatures>>,
    cells: std::collections::BTreeMap<String, Vec<luma_patterns::Cell>>,
}

async fn prepare_scene_data(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
    include_empty: bool,
) -> Result<SceneData, String> {
    score
        .validate(&standard_library())
        .map_err(|error| error.to_string())?;
    if score.clips.is_empty() {
        return Ok(SceneData {
            scene: crate::eval::Scene::default(),
            features: None,
            cells: Default::default(),
        });
    }
    let grid =
        crate::services::tracks::get_track_beats_for_connection(access.connection(), track_id)
            .await?
            .ok_or("analyze the track before placing effects on its musical grid")?;
    let clock = grid.timeline().map_err(|error| error.to_string())?;
    let library = score
        .library(&standard_library())
        .map_err(|error| error.to_string())?;
    let mut compiled = Vec::new();
    let mut clip_cells = std::collections::BTreeMap::new();
    let mut prepared_clips = Vec::new();
    let mut requests = Vec::new();
    let mut domains = std::collections::BTreeMap::new();
    for (id, clip) in &score.clips {
        let key = (
            serde_json::to_string(&clip.selection).map_err(|error| error.to_string())?,
            clip.selection_seed.unwrap_or(clip.seed),
        );
        let cells = if let Some(cells) = domains.get(&key) {
            Vec::<luma_patterns::Cell>::clone(cells)
        } else {
            let cells = super::composable_patterns::resolve_cells(
                access,
                fixtures_root,
                std::slice::from_ref(&clip.selection),
                clip.selection_seed.unwrap_or(clip.seed),
            )
            .await?;
            domains.insert(key, cells.clone());
            cells
        };
        if cells.is_empty() && !include_empty {
            continue;
        }
        let prepared = luma_patterns::PreparedGraph::new(
            &library,
            &clip.graph,
            &clip.inputs,
            luma_patterns::Frame {
                cells: &cells,
                features: None,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )
        .map_err(|e| format!("clip {id}: {e}"))?;
        for request in prepared.feature_requests() {
            if !requests.contains(request) {
                requests.push(request.clone());
            }
        }
        prepared_clips.push((id, clip, cells, prepared));
    }
    let features = if requests.is_empty() {
        None
    } else {
        Some(
            crate::eval::track_features::prepare(access, storage, track_id, &grid, &requests)
                .await?,
        )
    };
    for (id, clip, cells, prepared) in prepared_clips {
        let prepared = match &features {
            Some(features) => prepared
                .with_features(features.clone())
                .map_err(|e| format!("clip {id}: {e}"))?,
            None => prepared,
        };
        let output = library.definitions[&clip.graph]
            .lighting_output()
            .ok_or("clip graph must produce fixture output")?;
        clip_cells.insert(id.clone(), cells.clone());
        let plan =
            crate::eval::lighting::compile_clip(clip, clock.clone(), cells, prepared, output)
                .map_err(|error| format!("clip {id}: {error}"))?;
        compiled.push(crate::eval::CompiledAnnotation {
            span: plan.span,
            plan: std::sync::Arc::new(plan),
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
        });
    }
    Ok(SceneData {
        scene: crate::eval::Scene::new(compiled),
        features,
        cells: clip_cells,
    })
}

/// An isolated, seekable clip program. It never installs a scene on the output
/// engine; native previews retain it and sample any absolute track time.
#[derive(Debug)]
pub struct ClipPreview {
    pub scene: crate::eval::Scene,
    pub span: (f32, f32),
    pub inspection: Result<Option<crate::eval::lighting::Inspection>, String>,
}

pub(crate) async fn prepare_clip_preview(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
    clip_id: &str,
) -> Result<ClipPreview, String> {
    Ok(
        prepare_single_clip(access, fixtures_root, storage, track_id, score, clip_id)
            .await?
            .0,
    )
}

/// [`prepare_clip_preview`], and the cells the clip lights.
async fn prepare_single_clip(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
    clip_id: &str,
) -> Result<(ClipPreview, Vec<luma_patterns::Cell>), String> {
    let clip = score
        .clips
        .get(clip_id)
        .ok_or_else(|| format!("unknown clip {clip_id}"))?;
    let mut single = score.clone();
    single.clips.retain(|id, _| id == clip_id);
    let SceneData {
        scene,
        features,
        mut cells,
    } = prepare_scene_data(access, fixtures_root, storage, track_id, &single, true).await?;
    let cells = cells.remove(clip_id).unwrap_or_default();
    let span = if let Some(annotation) = scene.annotations.first() {
        annotation.span
    } else {
        // Empty selections still have a real clip clock and can be scrubbed.
        let grid =
            crate::services::tracks::get_track_beats_for_connection(access.connection(), track_id)
                .await?
                .ok_or("analyze the track before previewing its musical grid")?;
        let clock = grid.timeline().map_err(|error| error.to_string())?;
        (
            clock.seconds_at(clip.start).map_err(|e| e.to_string())? as f32,
            clock
                .seconds_at(clip.start + clip.duration)
                .map_err(|e| e.to_string())? as f32,
        )
    };
    if !span.0.is_finite() || !span.1.is_finite() || span.1 <= span.0 {
        return Err("clip duration cannot be represented on the playback timeline".into());
    }
    // Build plots once when the clip changes, never on the audio/render tick.
    // An inspection failure does not prevent transport or a valid earlier seek.
    let mut inspection = scene
        .annotations
        .first()
        .and_then(|annotation| {
            annotation
                .plan
                .program
                .as_ref()
                .map(|program| program.inspect(span))
        })
        .unwrap_or(Ok(None));
    if let Ok(Some(data)) = &mut inspection {
        let mut audio = features
            .as_ref()
            .map(|f| f.inspection_sources())
            .unwrap_or_default();
        for (name, value) in &data.values {
            let Ok(luma_patterns::Value::AudioSource(source)) = value.sample(0) else {
                continue;
            };
            let result = if (1..data.times.len()).any(|i| {
                value.sample(i).ok() != Some(luma_patterns::Value::AudioSource(source.clone()))
            }) {
                Err("spectrogram inspection needs a fixed audio source".into())
            } else {
                match audio.load(access, storage, track_id, &source).await {
                    Ok(()) => audio
                        .get(&source)
                        .map_err(|e| e.to_string())
                        .and_then(|audio| {
                            crate::audio::melspec::inspect(&audio.samples, audio.sample_rate, span)
                                .map(std::sync::Arc::new)
                        }),
                    Err(error) => Err(error),
                }
            };
            data.spectrograms.insert(name.clone(), result);
        }
    }
    Ok((
        ClipPreview {
            scene,
            span,
            inspection,
        },
        cells,
    ))
}

pub(crate) async fn preview_clip(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
    clip_id: &str,
) -> Result<crate::models::patterns::AnnotationPreview, String> {
    let clip = score
        .clips
        .get(clip_id)
        .ok_or_else(|| format!("unknown clip {clip_id}"))?;
    let (ClipPreview { scene, span, .. }, cells) =
        prepare_single_clip(access, fixtures_root, storage, track_id, score, clip_id).await?;
    strip(clip_id, clip, &scene, span, &cells)
}

/// Sample a single-clip scene across its span into the timeline's strip.
fn strip(
    clip_id: &str,
    clip: &luma_patterns::Clip,
    scene: &crate::eval::Scene,
    span: (f32, f32),
    cells: &[luma_patterns::Cell],
) -> Result<crate::models::patterns::AnnotationPreview, String> {
    let width = (clip.duration * 16.0).ceil().clamp(8.0, 512.0) as usize;
    let mut frames = Vec::new();
    if let Some(annotation) = scene.annotations.first() {
        if (annotation.plan.primitive_ids.len()).saturating_mul(width) > 1_000_000 {
            return Err("clip preview exceeds one million head samples".into());
        }
        let times: Vec<_> = (0..width)
            .map(|index| span.0 + (span.1 - span.0) * index as f32 / width as f32)
            .collect();
        frames = scene.try_render(
            &times,
            crate::eval::Scope::Single(0),
            &mut crate::eval::Arena::default(),
        )?;
    }
    // A form clip's strip orders heads along a line through the rig; a clip
    // with its own graph keeps the brightness order it always had.
    let order = luma_patterns::is_form(&clip.graph)
        .then(|| crate::annotation_preview::head_order(clip, cells));
    Ok(crate::annotation_preview::render_preview(
        clip_id.to_owned(),
        &frames,
        order.as_ref(),
    ))
}

/// Heads in the stand-in rig of [`stand_in_strip`].
const STAND_IN_HEADS: usize = 16;

/// The strip of `preset` over `beats` beats on a stand-in rig: a straight
/// line of heads at 120 BPM. It needs no venue, track or score, so a preset
/// browser always has a picture to show before, or without, the real rig's.
pub fn stand_in_strip(
    preset: &luma_patterns::FormPreset,
    beats: f64,
) -> Result<crate::models::patterns::AnnotationPreview, String> {
    synthetic_strip(&preset.clip(0.0, beats), &line_cells(STAND_IN_HEADS))
}

/// `heads` heads evenly along U, in order.
fn line_cells(heads: usize) -> Vec<luma_patterns::Cell> {
    (0..heads)
        .map(|i| {
            let u = i as f64 / (heads.max(2) - 1) as f64;
            luma_patterns::Cell {
                id: format!("head-{i:02}"),
                group: "row".into(),
                world: [u, 0., 2.],
                uvz: [u, 0., 1.],
            }
        })
        .collect()
}

/// A single clip's strip over `cells`, on a 120 BPM grid, with no track
/// features.
fn synthetic_strip(
    clip: &luma_patterns::Clip,
    cells: &[luma_patterns::Cell],
) -> Result<crate::models::patterns::AnnotationPreview, String> {
    // 120 BPM: a beat every half second, far past any clip here.
    let beats = (clip.start + clip.duration).ceil() as usize + 64;
    let clock = luma_patterns::BeatTimeline::new((0..beats).map(|i| i as f64 * 0.5).collect(), 0.)
        .map_err(|error| error.to_string())?;
    let library = standard_library();
    let prepared = luma_patterns::PreparedGraph::new(
        &library,
        &clip.graph,
        &clip.inputs,
        luma_patterns::Frame {
            cells,
            features: None,
            beat: clip.start,
            clip_start: clip.start,
            clip_duration: clip.duration,
            seed: clip.seed,
        },
    )
    .map_err(|error| error.to_string())?;
    let output = library
        .definitions
        .get(&clip.graph)
        .and_then(|definition| definition.lighting_output())
        .ok_or("clip graph must produce fixture output")?;
    let plan = crate::eval::lighting::compile_clip(clip, clock, cells.to_vec(), prepared, output)
        .map_err(|error| error.to_string())?;
    let span = plan.span;
    let scene = crate::eval::Scene::new(vec![crate::eval::CompiledAnnotation {
        span,
        plan: std::sync::Arc::new(plan),
        z_index: 0,
        blend_mode: clip.blend_mode,
    }]);
    strip("clip", clip, &scene, span, cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strip of a shipped preset over `heads` heads in a row along U,
    /// shuffled so selection order says nothing about where a head is.
    fn preset_strip(preset: &str, heads: usize) -> crate::models::patterns::AnnotationPreview {
        let mut cells = line_cells(heads);
        cells.reverse();
        cells.swap(1, heads / 2);
        let clip = luma_patterns::presets()
            .preset(preset)
            .expect("a shipped preset")
            .clip(0.0, 4.0);
        synthetic_strip(&clip, &cells).unwrap()
    }

    #[test]
    fn every_shipped_preset_has_a_stand_in_strip() {
        for preset in &luma_patterns::presets().presets {
            let strip = stand_in_strip(preset, 8.0)
                .unwrap_or_else(|error| panic!("{}: {error}", preset.name));
            assert_eq!(strip.width, 128, "{}", preset.name);
        }
    }

    /// The row with the most light in each column, or `None` for a dark one.
    fn brightest(preview: &crate::models::patterns::AnnotationPreview) -> Vec<Option<u32>> {
        (0..preview.width)
            .map(|col| {
                let at = |row: u32| {
                    let i = ((row * preview.width + col) * 4) as usize;
                    preview.pixels[i..i + 3]
                        .iter()
                        .map(|v| u32::from(*v))
                        .sum::<u32>()
                };
                (0..preview.height)
                    .max_by_key(|row| at(*row))
                    .filter(|row| at(*row) > 0)
            })
            .collect()
    }

    #[test]
    fn a_chase_strip_is_a_diagonal() {
        let preview = preset_strip("Chase", 48);
        assert_eq!((preview.width, preview.height), (64, 32));
        // One stroke crosses the rig over the first two beats: 32 columns.
        let rows: Vec<u32> = brightest(&preview)[..32]
            .iter()
            .flatten()
            .copied()
            .collect();
        assert!(rows.len() > 16, "{rows:?}");
        assert!(rows.windows(2).all(|pair| pair[1] >= pair[0]), "{rows:?}");
        assert!(rows.last().unwrap() - rows[0] >= 24, "{rows:?}");
    }

    #[test]
    fn a_gradient_strip_is_bands_constant_over_time() {
        let preview = preset_strip("Gradient", 48);
        let pixel = |row: u32, col: u32| {
            let i = ((row * preview.width + col) * 4) as usize;
            preview.pixels[i..i + 3].to_vec()
        };
        for row in 0..preview.height {
            assert_eq!(pixel(row, 0), pixel(row, preview.width - 1), "row {row}");
        }
        // Magenta at one end of the rig, blue at the other.
        assert!(pixel(0, 0)[0] > 200 && pixel(preview.height - 1, 0)[2] > 200);
    }
}
