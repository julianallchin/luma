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

/// A compiled scene and the cells each clip lights.
struct SceneData {
    scene: crate::eval::Scene,
    cells: std::collections::BTreeMap<String, Vec<luma_patterns::Cell>>,
}

/// `single` is a one-clip preview: it keeps a clip that lights no cells and
/// fails with the clip's error. A whole score instead leaves out each clip
/// that fails, logs why, and plays the rest, so one bad clip never darkens
/// the score.
async fn prepare_scene_data(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
    single: bool,
) -> Result<SceneData, String> {
    let library = standard_library();
    if single {
        score
            .validate(&library)
            .map_err(|error| error.to_string())?;
    }
    // A clip that fails leaves the scene; see above.
    let skip = |error: String| -> Result<(), String> {
        if single {
            return Err(error);
        }
        log::warn!("score clip left out of the scene: {error}");
        Ok(())
    };
    let score = &Score {
        clips: score
            .clips
            .iter()
            .filter(
                |(id, clip)| match Score::validate_clip(&library, id, clip) {
                    Ok(()) => true,
                    Err(error) => {
                        log::warn!("score clip left out of the scene: {error}");
                        false
                    }
                },
            )
            .map(|(id, clip)| (id.clone(), clip.clone()))
            .collect(),
    };
    if score.clips.is_empty() {
        return Ok(SceneData {
            scene: crate::eval::Scene::default(),
            cells: Default::default(),
        });
    }
    let grid =
        crate::services::tracks::get_track_beats_for_connection(access.connection(), track_id)
            .await?
            .ok_or("analyze the track before placing effects on its musical grid")?;
    let clock = grid.timeline().map_err(|error| error.to_string())?;
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
        if cells.is_empty() && !single {
            continue;
        }
        let prepared = match luma_patterns::PreparedGraph::new(
            &library,
            &clip.graph,
            luma_patterns::Frame {
                cells: &cells,
                features: None,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                skip(format!("clip {id}: {error}"))?;
                continue;
            }
        };
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
            Some(features) => match prepared.with_features(features.clone()) {
                Ok(prepared) => prepared,
                Err(error) => {
                    skip(format!("clip {id}: {error}"))?;
                    continue;
                }
            },
            None => prepared,
        };
        let plan =
            match crate::eval::lighting::compile_clip(clip, clock.clone(), cells.clone(), prepared)
            {
                Ok(plan) => plan,
                Err(error) => {
                    skip(format!("clip {id}: {error}"))?;
                    continue;
                }
            };
        clip_cells.insert(id.clone(), cells);
        compiled.push(crate::eval::CompiledAnnotation {
            span: plan.span,
            plan: std::sync::Arc::new(plan),
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
        });
    }
    let aimed: std::collections::BTreeSet<&str> = compiled
        .iter()
        .filter(|annotation| annotation.plan.outputs.aim)
        .flat_map(|annotation| annotation.plan.primitive_ids.iter().map(String::as_str))
        .collect();
    let rig = if aimed.is_empty() {
        crate::eval::aim::Rig::default()
    } else {
        crate::eval::aim::Rig::load(access, fixtures_root, aimed).await?
    };
    Ok(SceneData {
        scene: crate::eval::Scene::new(compiled).with_rig(rig)?,
        cells: clip_cells,
    })
}

/// An isolated, seekable clip program. It never installs a scene on the output
/// engine; native previews retain it and sample any absolute track time.
#[derive(Debug)]
pub struct ClipPreview {
    pub scene: crate::eval::Scene,
    pub span: (f32, f32),
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
    let SceneData { scene, mut cells } =
        prepare_scene_data(access, fixtures_root, storage, track_id, &single, true).await?;
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
    Ok((ClipPreview { scene, span }, cells))
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
    let aim = aims(&clip.graph);
    let mut frames = Vec::new();
    if let Some(annotation) = scene.annotations.first() {
        if (annotation.plan.primitive_ids.len()).saturating_mul(width) > 1_000_000 {
            return Err("clip preview exceeds one million head samples".into());
        }
        // A column per step for a heatmap. An aim's curves also need the
        // clip's last moment, and the solver's pan and tilt, which only the
        // composite gives.
        let (count, scope) = if aim {
            (width + 1, crate::eval::Scope::Composite)
        } else {
            (width, crate::eval::Scope::Single(0))
        };
        let times: Vec<_> = (0..count)
            .map(|index| {
                (span.0 + (span.1 - span.0) * index as f32 / width as f32).min(span.1.next_down())
            })
            .collect();
        frames = scene.try_render(&times, scope, &mut crate::eval::Arena::default())?;
    }
    // An aim gives no light: its picture is where the beams point, and on a
    // rig that can move, the pan and tilt that take them there.
    if aim {
        let mut preview =
            crate::annotation_preview::render_aim_preview(clip_id.to_owned(), &frames, cells);
        preview.aim = scene
            .rig()
            .and_then(|rig| crate::annotation_preview::aim_curves(&frames, cells, rig));
        return Ok(preview);
    }
    // The strip orders heads along a line through the rig.
    let order = crate::annotation_preview::head_order(cells);
    Ok(crate::annotation_preview::render_preview(
        clip_id.to_owned(),
        &frames,
        Some(&order),
    ))
}

/// Whether the graph's output node is an aim.
fn aims(graph: &luma_patterns::ClipGraph) -> bool {
    graph.output_kind() == Some(luma_patterns::clip_graph::Kind::Aim)
}

/// Heads in the stand-in rig of [`stand_in_strip`].
const STAND_IN_HEADS: usize = 16;
/// How far a stand-in head turns, pan then tilt, end to end, in degrees: a
/// common moving head's.
const STAND_IN_RANGE: [f64; 2] = [540., 270.];

/// The strip of a clip `graph`, such as a preset's, over `beats` beats on a
/// stand-in rig: a straight line of moving heads at 120 BPM. It needs no
/// venue, track or score, so a preset browser always has a picture to show
/// before, or without, the real rig's.
pub fn stand_in_strip(
    graph: &luma_patterns::ClipGraph,
    beats: f64,
) -> Result<crate::models::patterns::AnnotationPreview, String> {
    let mut cells = line_cells(STAND_IN_HEADS);
    // An aim reads against the room: the line hangs a metre up and a metre
    // upstage of center stage, so a point at the origin is in front of it.
    if aims(graph) {
        for cell in &mut cells {
            cell.uvz = [cell.uvz[0] - 0.5, -1., 1.];
            cell.world = [cell.uvz[0], 1., 1.];
        }
    }
    synthetic_strip(&stand_in_clip(graph, beats), &cells)
}

/// A clip of `graph` from beat 0 for `beats` beats over every head.
fn stand_in_clip(graph: &luma_patterns::ClipGraph, beats: f64) -> luma_patterns::Clip {
    luma_patterns::Clip {
        name: String::new(),
        start: 0.,
        duration: beats,
        seed: 0,
        selection_seed: None,
        selection: luma_patterns::Selection::all(),
        z_index: 0,
        blend_mode: luma_patterns::BlendMode::Replace,
        graph: graph.clone(),
    }
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

/// Every cell of `cells` as a moving head of [`STAND_IN_RANGE`], hung where
/// the cell is and pointing straight down at home.
fn stand_in_rig(cells: &[luma_patterns::Cell]) -> crate::eval::aim::Rig {
    let mut rig = crate::eval::aim::Rig::default();
    for cell in cells {
        let mount = fixture_kinematics::Mount::from_frame(
            glam::DVec3::from(cell.world).as_vec3(),
            glam::Mat3::IDENTITY,
        );
        rig.insert(
            cell.id.clone(),
            crate::eval::aim::Head::with_range(mount, STAND_IN_RANGE),
        );
    }
    rig
}

/// A stand-in track for a clip that follows a band: every band hits on
/// each beat and decays until the next, like a four-on-the-floor kick.
#[derive(Debug)]
struct BeatPulse;

impl luma_patterns::FeatureSource for BeatPulse {
    fn sample(
        &self,
        _request: &luma_patterns::FeatureRequest,
        beat: f64,
    ) -> luma_patterns::Result<f64> {
        Ok((1. - beat.rem_euclid(1.)).powi(2))
    }
    fn range(&self, _request: &luma_patterns::FeatureRequest) -> luma_patterns::Result<(f64, f64)> {
        Ok((0., 1.))
    }
}

/// A single clip's strip over `cells` as stand-in heads (see
/// [`stand_in_rig`]), on a 120 BPM grid, with [`BeatPulse`] for every band.
/// Only the scene is made up: [`strip`] samples it as it does a real clip's.
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
    let prepared = if prepared.feature_requests().is_empty() {
        prepared
    } else {
        prepared
            .with_features(std::sync::Arc::new(BeatPulse))
            .map_err(|error| error.to_string())?
    };
    let plan = crate::eval::lighting::compile_clip(clip, clock, cells.to_vec(), prepared)
        .map_err(|error| error.to_string())?;
    let span = plan.span;
    let scene = crate::eval::Scene::new(vec![crate::eval::CompiledAnnotation {
        span,
        plan: std::sync::Arc::new(plan),
        z_index: 0,
        blend_mode: clip.blend_mode,
    }])
    .with_rig(stand_in_rig(cells))?;
    strip("clip", clip, &scene, span, cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strip of `graph` over `heads` heads in a row along U, shuffled
    /// so selection order says nothing about where a head is.
    fn graph_strip(
        graph: serde_json::Value,
        heads: usize,
    ) -> crate::models::patterns::AnnotationPreview {
        let mut cells = line_cells(heads);
        cells.reverse();
        cells.swap(1, heads / 2);
        let graph: luma_patterns::ClipGraph = serde_json::from_value(graph).unwrap();
        synthetic_strip(&stand_in_clip(&graph, 4.0), &cells).unwrap()
    }

    #[test]
    fn every_shipped_preset_has_a_stand_in_strip() {
        for preset in &luma_patterns::presets().clips {
            let strip = stand_in_strip(&preset.graph, 8.0)
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
        let preview = graph_strip(
            serde_json::json!({"version": 3, "nodes": {
                "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "space1"},
                                      "shape": {"points": [[0, 1], [1, 0]]}, "low": 60, "high": 360}},
                "time1": {"kind": "time",
                          "inputs": {"every": 2, "phase": {"node": "curve1"}}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"},
                                      "shape": {"points": [[0, 1], [0.1667, 1], [0.1667, 0], [1, 0]]}}},
                "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}}}}),
            48,
        );
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
    fn an_aim_strip_shows_where_the_beams_point_and_how_they_move() {
        let picture = |name: &str| {
            let preset = luma_patterns::presets().clip(name).unwrap();
            let strip = stand_in_strip(&preset.graph, 16.0).unwrap();
            assert_eq!((strip.width, strip.height), (128, 28), "{name}");
            strip.pixels
        };
        // The dim path a moving beam's tip leaves, between the dark ground,
        // the heads and the bright beams.
        let moves = |pixels: &[u8]| pixels.chunks(4).any(|px| (60..120).contains(&px[0]));
        let still = picture("Position");
        assert!(!moves(&still));
        for name in ["Fan", "Converge", "Bloom"] {
            assert_ne!(picture(name), still, "{name}");
        }
        for name in [
            "Bloom", "Sweep", "Nod wave", "Circle", "Figure-8", "Ballyhoo",
        ] {
            assert!(moves(&picture(name)), "{name}");
        }
    }

    #[test]
    fn an_aim_stand_in_has_pan_and_tilt_curves() {
        let aim = |name: &str| {
            let preset = luma_patterns::presets().clip(name).unwrap();
            stand_in_strip(&preset.graph, 16.0)
                .unwrap()
                .aim
                .unwrap_or_else(|| panic!("{name} has no curves"))
        };
        // A fan leans each head its own way.
        let fan = aim("Fan");
        assert!(fan.pan.len() > 1, "{fan:?}");
        assert!(!fan.tilt.is_empty());
        // One point: every head the same way, so one curve a band.
        let position = aim("Position");
        assert_eq!((position.pan.len(), position.tilt.len()), (1, 1));
        // A colour preset aims nothing.
        let wash = luma_patterns::presets().clip("Wash").unwrap();
        assert!(stand_in_strip(&wash.graph, 16.0).unwrap().aim.is_none());
    }

    #[test]
    fn a_gradient_strip_is_bands_constant_over_time() {
        let preview = graph_strip(
            serde_json::json!({"version": 3, "nodes": {
                "space1": {"kind": "space", "settings": {"kind": "line", "wrap": "no"}},
                "curve1": {"kind": "curve", "settings": {"kind": "color"},
                           "inputs": {"x": {"node": "space1"}, "gradient": {"stops": [
                               {"t": 0, "color": [1, 0, 1]}, {"t": 1, "color": [0, 0, 1]}]}}},
                "color1": {"kind": "color", "inputs": {"color": {"node": "curve1"}}}}}),
            48,
        );
        let pixel = |row: u32, col: u32| {
            let i = ((row * preview.width + col) * 4) as usize;
            preview.pixels[i..i + 3].to_vec()
        };
        for row in 0..preview.height {
            assert_eq!(pixel(row, 0), pixel(row, preview.width - 1), "row {row}");
        }
        // Magenta at one end of the rig, blue at the other. Rec. 2020 blue
        // leaves the display gamut, so it shows as the strongest channel.
        let (first, last) = (pixel(0, 0), pixel(preview.height - 1, 0));
        assert!(
            first[0] > 200 && last[2] > last[0] && last[2] > last[1],
            "{first:?} … {last:?}"
        );
    }
}
