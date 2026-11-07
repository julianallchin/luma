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
//! change or when the clock it runs on moves to another song. A fader wired
//! into a clock's period is a speed: each pattern's prepared graph is played
//! live on the set's bars, keeping that clock's line from frame to frame
//! ([`luma_patterns::PreparedGraph::played_live`]), so the speed changes with
//! no jump and on the bar.
//!
//! An instance, a pattern played on targets' groups ([`Instance`]), is the
//! pattern compiled on those groups, its pad node reading the instance's
//! button. It is compiled the first time it plays, off the frame path: a
//! frame that sees the bank has made a new one asks the set's [`Build`],
//! which compiles it and hands the engine the set's [`Layers`] grown by it.
//!
//! A pattern is a score's pattern placed from long before now and running
//! for hours, so whenever it is turned on it is already in step. On the
//! master deck's song it is compiled against that song's beat grid and lands
//! on the song's bars. With no such deck it runs on the tapped tempo: the
//! set is compiled on a fixed grid of [`TAP_GRID_BPM`], and the engine feeds
//! it the tapped beat converted to that grid's seconds, so a new tempo is a
//! new clock, never a new compile.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::controls::{self, Instance, PatternControls, VenueControls};
use crate::database::local::venue_access::AuthorizedVenue;
use crate::eval::aim::Rig;
use crate::eval::{Arena, CompiledAnnotation, Scene};
use crate::models::beat_grid::BeatGrid;
use crate::models::perform::{PerformLayout, PerformPattern};
use crate::models::universe::UniverseState;
use crate::storage::StorageRoot;

/// Bars of pickup before the first downbeat, so a pattern is already
/// running when the song's grid starts. Whole bars, so its clocks count
/// from a bar.
const PICKUP_BARS: f64 = 4.;
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
    /// Which build of the set this is, so an instance compiled for it joins
    /// it and no later one.
    id: u64,
    layers: Arc<Layers>,
    controls: Arc<VenueControls>,
    clock: LiveClock,
    /// The beats the set was compiled on, and the beats in a bar of them,
    /// which the bank reads a deck's beat and bar by.
    timeline: luma_patterns::BeatTimeline,
    beats_per_bar: f64,
    /// Each annotation's level as of the last frame.
    levels: Vec<f32>,
    /// How many instances the bank had made when the set last asked for
    /// them, and where it asks for those made since.
    made: u64,
    wants: tokio::sync::mpsc::UnboundedSender<()>,
}

/// An annotation's pad (or its instance's button), when it has one, and
/// its level control.
type Switch = (Option<String>, String);

/// The set as compiled: its scene, and each annotation's switch, in the
/// scene's order. The set's [`Build`] makes it, and grows it by each
/// instance; the engine plays the same one.
#[derive(Default)]
pub(crate) struct Layers {
    scene: Scene,
    switches: Vec<Switch>,
}

impl Layers {
    /// `layers` aiming the heads of `rig`, bottom of the set first: later
    /// ones composite on top.
    fn new(mut layers: Vec<(CompiledAnnotation, Switch)>, rig: Rig) -> Result<Self, String> {
        layers.sort_by_key(|(annotation, _)| annotation.z_index);
        let (annotations, switches) = layers.into_iter().unzip();
        Ok(Self {
            scene: Scene::new(annotations).with_rig(rig)?,
            switches,
        })
    }

    /// Each annotation and its switch, in order.
    fn iter(&self) -> impl Iterator<Item = (CompiledAnnotation, Switch)> + '_ {
        self.scene
            .annotations
            .iter()
            .cloned()
            .zip(self.switches.iter().cloned())
    }
}

impl LiveSet {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn clock(&self) -> LiveClock {
        self.clock
    }

    /// The tapped beat as seconds on the grid the set was compiled on.
    pub(crate) fn tap_time(&self, now: Instant) -> f32 {
        (self.controls.tempo(now).beat * 60. / TAP_GRID_BPM) as f32
    }

    /// Tell the bank the set's clock, the set being at `time` on the grid
    /// it was compiled on: a deck's beat, or the tapped tempo, which the
    /// bank keeps itself.
    pub(crate) fn tick(&self, time: f32, now: Instant) {
        let deck = match self.clock {
            LiveClock::Deck(_) => self.timeline.beat_at(f64::from(time)).ok(),
            LiveClock::Tap => None,
        };
        self.controls
            .tick(deck.map(|beat| (beat, self.beats_per_bar)), now);
    }

    /// Read each pattern's opacity from the controls: its level × the
    /// master, its pad's envelope being in its graph's alpha already. A
    /// pattern whose pad is all the way off plays nothing. Asks for the
    /// instances the bank has made since the last frame. Answers whether
    /// anything plays.
    pub(crate) fn update_levels(&mut self) -> bool {
        let made = self.controls.made();
        if made != self.made {
            self.made = made;
            let _ = self.wants.send(());
        }
        let master = self.controls.value(controls::perform::MASTER) as f32;
        let mut playing = false;
        for ((pad, level), out) in self.layers.switches.iter().zip(self.levels.iter_mut()) {
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
        self.layers
            .scene
            .render_onto(base, time, &self.levels, scratch)
    }

    /// Play `grown`, the set with more instances, from now.
    pub(crate) fn grow(&mut self, grown: Arc<Layers>) {
        self.levels = vec![0.; grown.switches.len()];
        self.layers = grown;
    }
}

/// What a set was compiled from and what it compiled, kept while it plays to
/// compile each instance the bank makes, the first time it is played.
pub(crate) struct Build {
    id: u64,
    controls: Arc<VenueControls>,
    patterns: Vec<PerformPattern>,
    track: Option<(u8, String)>,
    grid: Option<BeatGrid>,
    timeline: luma_patterns::BeatTimeline,
    beats_per_bar: f64,
    layers: Arc<Layers>,
    /// The instances compiled, by button.
    instances: HashSet<String>,
}

impl Build {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Compile `pattern` on its own selection, or as `instance` (by its
    /// button) on that instance's groups; `None` when it lights nothing
    /// there.
    async fn compile(
        &self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
        storage: &StorageRoot,
        pattern: &PerformPattern,
        instance: Option<(&str, &Instance)>,
    ) -> Result<Option<(CompiledAnnotation, Switch)>, String> {
        let mut clip = pattern.as_clip(-PICKUP_BARS * self.beats_per_bar, BEATS);
        let mut source = PatternControls::new(self.controls.clone(), &pattern.id);
        let mut pad = controls::perform::pad(pattern);
        if let Some((button, instance)) = instance {
            clip.selection = luma_patterns::Selection::new(instance.on.join(" | "));
            if let Some(node) = pattern.graph.pad() {
                source = source.with_pad(&node.node, button);
            }
            pad = Some(button.to_owned());
        }
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
                "Perform pattern {} lights nothing on {} in this venue",
                pattern.name,
                clip.selection.expression
            );
            return Ok(None);
        }
        let library = luma_patterns::standard_library();
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
        .with_controls(Arc::new(source))
        .played_live(self.beats_per_bar);
        let requests = prepared.feature_requests().to_vec();
        let prepared = if requests.is_empty() {
            prepared
        } else {
            let features: Arc<dyn luma_patterns::FeatureSource> = match (&self.track, &self.grid) {
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
        let plan =
            crate::eval::lighting::compile_clip(&clip, self.timeline.clone(), cells, prepared)
                .map_err(|error| format!("{}: {error}", pattern.name))?;
        let annotation = CompiledAnnotation {
            span: plan.span,
            plan: Arc::new(plan),
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
        };
        Ok(Some((
            annotation,
            (pad, controls::perform::level(&pattern.id)),
        )))
    }

    /// Compile each instance the bank holds that the set does not yet. One
    /// that fails is left out, and why logged, so it never stops the set.
    /// Answers how many instances the bank had made by then.
    async fn add_instances(
        &mut self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
        storage: &StorageRoot,
        layers: &mut Vec<(CompiledAnnotation, Switch)>,
    ) -> u64 {
        let (made, instances) = self.controls.instances();
        for (button, instance) in &instances {
            if self.instances.contains(button) {
                continue;
            }
            let Some(pattern) = self.patterns.iter().find(|p| p.id == instance.pattern_id) else {
                continue;
            };
            match self
                .compile(
                    access,
                    fixtures_root,
                    storage,
                    pattern,
                    Some((button, instance)),
                )
                .await
            {
                Ok(compiled) => layers.extend(compiled),
                Err(error) => log::warn!("a Perform instance did not compile: {error}"),
            }
            self.instances.insert(button.clone());
        }
        made
    }

    /// Compile the instances the bank has made since, and answer the set
    /// grown by them; `None` when there are none.
    pub(crate) async fn grow(
        &mut self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
        storage: &StorageRoot,
    ) -> Result<Option<Arc<Layers>>, String> {
        let mut added = Vec::new();
        self.add_instances(access, fixtures_root, storage, &mut added)
            .await;
        if added.is_empty() {
            return Ok(None);
        }
        let mut rig = aimed(access, fixtures_root, &added).await?;
        rig.extend(self.layers.scene.rig().unwrap_or(&Rig::default()));
        let layers = self.layers.iter().chain(added).collect();
        self.layers = Arc::new(Layers::new(layers, rig)?);
        Ok(Some(self.layers.clone()))
    }
}

/// The heads that `layers` aim.
async fn aimed(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &std::path::Path,
    layers: &[(CompiledAnnotation, Switch)],
) -> Result<Rig, String> {
    let aimed: BTreeSet<&str> = layers
        .iter()
        .map(|(annotation, _)| annotation)
        .filter(|annotation| annotation.plan.outputs.aim)
        .flat_map(|annotation| annotation.plan.primitive_ids.iter().map(String::as_str))
        .collect();
    if aimed.is_empty() {
        return Ok(Rig::default());
    }
    Rig::load(access, fixtures_root, aimed).await
}

/// Compile the venue's set, its patterns and the instances `controls` holds,
/// as build `id`. `track` is the master deck's song; `None` puts it on the
/// tapped tempo, as does a song with no beat grid. Answers the clock the set
/// runs on, and the set with its build, `None` when nothing of it lights
/// anything. The set asks for the instances made since on `wants`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn compile(
    id: u64,
    access: &mut impl AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &StorageRoot,
    controls: Arc<VenueControls>,
    patterns: &[PerformPattern],
    layout: &PerformLayout,
    track: Option<(u8, &str)>,
    wants: tokio::sync::mpsc::UnboundedSender<()>,
) -> Result<(LiveClock, Option<(LiveSet, Build)>), String> {
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
    let timeline = match &grid {
        Some(grid) => grid.timeline().map_err(|error| error.to_string())?,
        // Two beats are enough: the timeline extrapolates the rest.
        None => luma_patterns::BeatTimeline::new(vec![0., 60. / TAP_GRID_BPM], 0.)
            .map_err(|error| error.to_string())?,
    };
    let beats_per_bar = grid
        .as_ref()
        .map(|grid| f64::from(grid.beats_per_bar))
        .filter(|beats| *beats > 0.)
        .unwrap_or(controls::TAPPED_BEATS_PER_BAR);
    controls.install(patterns, layout);
    let mut build = Build {
        id,
        controls: controls.clone(),
        patterns: patterns.to_vec(),
        track: track.map(|(deck, track_id)| (deck, track_id.to_owned())),
        grid,
        timeline: timeline.clone(),
        beats_per_bar,
        layers: Arc::default(),
        instances: HashSet::new(),
    };
    let mut layers = Vec::new();
    for pattern in &build.patterns {
        let compiled = build
            .compile(access, fixtures_root, storage, pattern, None)
            .await?;
        layers.extend(compiled);
    }
    let made = build
        .add_instances(access, fixtures_root, storage, &mut layers)
        .await;
    if layers.is_empty() {
        return Ok((live_clock, None));
    }
    let rig = aimed(access, fixtures_root, &layers).await?;
    build.layers = Arc::new(Layers::new(layers, rig)?);
    let set = LiveSet {
        id,
        levels: vec![0.; build.layers.switches.len()],
        layers: build.layers.clone(),
        controls,
        clock: live_clock,
        timeline,
        beats_per_bar,
        made,
        wants,
    };
    Ok((live_clock, Some((set, build))))
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
            graph: if padded {
                controls::tests::with_pad_at_once(graph)
            } else {
                graph
            },
        }
    }

    #[test]
    fn an_instance_grown_in_keeps_its_switch_beside_its_annotation() {
        let annotation = |z| {
            crate::eval::composite::tests::wash(
                [1.; 3],
                1.,
                1.,
                luma_patterns::BlendMode::Replace,
                z,
            )
        };
        let switch = |pad: &str| (Some(pad.to_owned()), format!("{pad} level"));
        let built = Layers::new(
            vec![
                (annotation(0), switch("low")),
                (annotation(1), switch("high")),
            ],
            Rig::default(),
        )
        .unwrap();
        // An instance of the lower pattern composites with it, under the
        // higher one, and its switch goes with it.
        let grown = Layers::new(
            built
                .iter()
                .chain([(annotation(0), switch("low@front"))])
                .collect(),
            Rig::default(),
        )
        .unwrap();
        let order: Vec<(i64, &str)> = grown
            .scene
            .annotations
            .iter()
            .zip(&grown.switches)
            .map(|(annotation, (pad, _))| (annotation.z_index, pad.as_deref().unwrap()))
            .collect();
        assert_eq!(order, [(0, "low"), (0, "low@front"), (1, "high")]);
    }

    #[test]
    fn a_pattern_plays_at_its_level_times_master_while_its_pad_is_up() {
        let controls = Arc::new(VenueControls::default());
        let set = [pattern("a", true), pattern("b", true), pattern("c", false)];
        controls.install(&set, &PerformLayout::default());
        let pad = |pattern: &PerformPattern| controls::perform::pad(pattern).unwrap();
        let (wants, _wanted) = tokio::sync::mpsc::unbounded_channel();
        let mut live = LiveSet {
            id: 0,
            made: 0,
            wants,
            layers: Arc::new(Layers {
                scene: Scene::new(Vec::new()),
                switches: set
                    .iter()
                    .map(|p| (controls::perform::pad(p), controls::perform::level(&p.id)))
                    .collect(),
            }),
            controls: controls.clone(),
            clock: LiveClock::Tap,
            timeline: luma_patterns::BeatTimeline::new(vec![0., 0.5], 0.).unwrap(),
            beats_per_bar: controls::TAPPED_BEATS_PER_BAR,
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
