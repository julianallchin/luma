//! The Perform set, compiled for the engine.
//!
//! Every pattern of the set is compiled once, whether it plays or not; what
//! plays is read from the venue's controls at every frame, so pressing a
//! pad, fading it, or a MIDI key doing either, never recompiles. A pattern's
//! graph's fader and button nodes read values of the pattern's own
//! ([`controls::PatternControls`]); its pad is one of those buttons, wired
//! into its output's alpha, so the graph fades itself in and out and the set
//! scales it by its level × the Perform master. A pattern with no pad plays
//! at its level whenever the set is open. The set recompiles when its patterns
//! change or when the clock it runs on moves to another song.
//!
//! A pattern is a score's pattern placed from long before now and running
//! for hours, so whenever it is turned on it is already in step. On the
//! master deck's song it is compiled against that song's beat grid and lands
//! on the song's bars. With no such deck it runs on the tapped tempo: the
//! set is compiled on a fixed grid of [`TAP_GRID_BPM`], and the engine feeds
//! it the tapped beat converted to that grid's seconds, so a new tempo is a
//! new clock, never a new compile.

use std::sync::Arc;
use std::time::Instant;

use crate::controls::{self, PatternControls, VenueControls};
use crate::database::local::venue_access::AuthorizedVenue;
use crate::eval::{Arena, CompiledAnnotation, Scene};
use crate::models::perform::{PerformLayout, PerformPattern};
use crate::models::universe::UniverseState;
use crate::storage::StorageRoot;

/// Four bars of pickup before the first downbeat, so a pattern is already
/// running when the song's grid starts.
const FROM_BEAT: f64 = -16.;
/// Far past any set: about nine hours at 180 BPM.
const BEATS: f64 = 100_000.;
/// The grid a set on the tapped tempo is compiled on.
const TAP_GRID_BPM: f64 = 120.;

/// What time the set reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveClock {
    /// The deck's song position: the set was compiled on its beat grid.
    Deck(u8),
    /// The venue's tapped tempo.
    Tap,
}

/// The compiled set and the controls that play it.
pub(crate) struct LiveSet {
    scene: Scene,
    /// Each annotation's pad, when it has one, and level control, in the
    /// scene's order.
    switches: Vec<(Option<String>, String)>,
    controls: Arc<VenueControls>,
    clock: LiveClock,
    /// Each annotation's level as of the last frame.
    levels: Vec<f32>,
}

impl LiveSet {
    pub(crate) fn clock(&self) -> LiveClock {
        self.clock
    }

    /// The tapped beat as seconds on the grid the set was compiled on.
    pub(crate) fn tap_time(&self, now: Instant) -> f32 {
        (self.controls.tempo(now).beat * 60. / TAP_GRID_BPM) as f32
    }

    /// Read each pattern's opacity from the controls: its level × the
    /// master, its pad's envelope being in its graph's alpha already. A
    /// pattern whose pad is all the way off plays nothing. Answers whether
    /// anything plays.
    pub(crate) fn update_levels(&mut self) -> bool {
        let master = self.controls.value(controls::perform::MASTER) as f32;
        let mut playing = false;
        for ((pad, level), out) in self.switches.iter().zip(self.levels.iter_mut()) {
            let silent = pad
                .as_ref()
                .is_some_and(|pad| self.controls.value(pad) <= 0.);
            *out = if silent {
                0.
            } else {
                self.controls.value(level) as f32 * master
            };
            playing |= *out > 0.;
        }
        playing
    }

    /// The set over `base` at `time`, at the levels of the last
    /// [`Self::update_levels`].
    pub(crate) fn render_onto(
        &self,
        base: UniverseState,
        time: f32,
        scratch: &mut Arena,
    ) -> UniverseState {
        self.scene.render_onto(base, time, &self.levels, scratch)
    }
}

/// Compile the venue's set. `track` is the master deck's song; `None` puts
/// it on the tapped tempo, as does a song with no beat grid. Answers the
/// clock the set runs on, and the set, `None` when no pattern of it lights
/// anything.
pub(crate) async fn compile(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &StorageRoot,
    patterns: &[PerformPattern],
    layout: &PerformLayout,
    track: Option<(u8, &str)>,
) -> Result<(LiveClock, Option<LiveSet>), String> {
    let library = luma_patterns::standard_library();
    let grid = match track {
        Some((_, track_id)) => {
            crate::database::local::tracks::get_track_beats_for_connection(
                access.connection(),
                track_id,
            )
            .await?
        }
        None => None,
    };
    let live_clock = match (track, &grid) {
        (Some((deck, _)), Some(_)) => LiveClock::Deck(deck),
        _ => LiveClock::Tap,
    };
    let clock = match &grid {
        Some(grid) => grid.timeline().map_err(|error| error.to_string())?,
        // Two beats are enough: the timeline extrapolates the rest.
        None => luma_patterns::BeatTimeline::new(vec![0., 60. / TAP_GRID_BPM], 0.)
            .map_err(|error| error.to_string())?,
    };
    let controls = controls::venue(access.venue_id());
    controls.install(patterns, layout);
    // Bottom of the set first: later annotations composite on top.
    let mut ordered: Vec<&PerformPattern> = patterns.iter().collect();
    ordered.sort_by_key(|pattern| pattern.z_index);
    let mut compiled = Vec::new();
    let mut switches = Vec::new();
    for pattern in ordered {
        let clip = pattern.as_clip(FROM_BEAT, BEATS);
        let elements = clip.graph.laser().map(|laser| laser.count());
        let cells = crate::services::composable_patterns::resolve_cells(
            access,
            fixtures_root,
            std::slice::from_ref(&clip.selection),
            clip.seed,
            elements,
        )
        .await?;
        if cells.is_empty() {
            log::warn!(
                "Perform pattern {} lights nothing in this venue",
                pattern.name
            );
            continue;
        }
        let prepared = luma_patterns::PreparedGraph::new(
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
        )
        .map_err(|error| format!("{}: {error}", pattern.name))?
        .with_controls(Arc::new(PatternControls::new(
            controls.clone(),
            &pattern.id,
        )));
        let requests = prepared.feature_requests().to_vec();
        let prepared = if requests.is_empty() {
            prepared
        } else {
            let features: Arc<dyn luma_patterns::FeatureSource> = match (track, &grid) {
                (Some((_, track_id)), Some(grid)) => {
                    crate::eval::track_features::prepare(access, storage, track_id, grid, &requests)
                        .await?
                }
                // No song to listen to: every band pulses on the beat.
                _ => Arc::new(crate::services::graph_scores::BeatPulse),
            };
            prepared
                .with_features(features)
                .map_err(|error| format!("{}: {error}", pattern.name))?
        };
        let plan = crate::eval::lighting::compile_clip(&clip, clock.clone(), cells, prepared)
            .map_err(|error| format!("{}: {error}", pattern.name))?;
        compiled.push(CompiledAnnotation {
            span: plan.span,
            plan: Arc::new(plan),
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
        });
        switches.push((
            controls::perform::pad(pattern),
            controls::perform::level(&pattern.id),
        ));
    }
    if compiled.is_empty() {
        return Ok((live_clock, None));
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
    let levels = vec![0.; switches.len()];
    let set = LiveSet {
        scene: Scene::new(compiled).with_rig(rig)?,
        switches,
        controls,
        clock: live_clock,
        levels,
    };
    Ok((live_clock, Some(set)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::perform::{PerformLayout, PerformPattern};

    /// A pattern of `id`, with a pad when `padded`.
    fn pattern(id: &str, padded: bool) -> PerformPattern {
        let graph = luma_patterns::ClipGraph::from_json(
            r#"{"version": 3, "nodes": {"color1": {"kind": "color"}}}"#,
        )
        .unwrap();
        PerformPattern {
            id: id.into(),
            venue_id: "venue".into(),
            name: id.into(),
            selection: luma_patterns::Selection {
                expression: "all".into(),
            },
            z_index: 0,
            blend_mode: luma_patterns::BlendMode::Replace,
            seed: 1,
            graph: if padded { graph.with_pad() } else { graph },
        }
    }

    #[test]
    fn a_pattern_plays_at_its_level_times_master_while_its_pad_is_up() {
        let controls = Arc::new(VenueControls::default());
        let set = [pattern("a", true), pattern("b", true), pattern("c", false)];
        controls.install(&set, &PerformLayout::default());
        let pad = |pattern: &PerformPattern| controls::perform::pad(pattern).unwrap();
        let mut live = LiveSet {
            scene: Scene::new(Vec::new()),
            switches: set
                .iter()
                .map(|p| (controls::perform::pad(p), controls::perform::level(&p.id)))
                .collect(),
            controls: controls.clone(),
            clock: LiveClock::Tap,
            levels: vec![0.; 3],
        };
        assert!(live.update_levels(), "c has no pad: it plays");
        assert_eq!(live.levels, vec![0., 0., 1.], "a and b are off");

        controls.press(&pad(&set[0]), true, Instant::now());
        controls.set(&controls::perform::level("a"), 0.5);
        live.update_levels();
        assert_eq!(
            live.levels,
            vec![0.5, 0., 1.],
            "a's level, not its envelope"
        );
        controls.set(controls::perform::MASTER, 0.5);
        live.update_levels();
        assert_eq!(live.levels, vec![0.25, 0., 0.5]);

        controls.set(&controls::perform::level("c"), 0.);
        controls.press(&pad(&set[0]), true, Instant::now());
        assert!(!live.update_levels(), "a toggled off, c faded out");
    }
}
