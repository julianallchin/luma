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
//! Each pattern is one layer, compiled on the groups the bank says it plays
//! on (none: its own selection), with its stored blend. When where it plays
//! changes, the frame that sees it asks the set's [`Build`], which compiles
//! that pattern again off the frame path and hands the engine the set's
//! [`Layers`] anew. Until then the old layer plays on only if it already
//! plays, so an effect never lights where it was not asked to.
//!
//! The bank's base color lies under every layer: each light's color, with
//! no light of its own, for a layer that takes the color below.
//!
//! A pattern is a score's pattern placed from long before now and running
//! for hours, so whenever it is turned on it is already in step. On the
//! master deck's song it is compiled against that song's beat grid and lands
//! on the song's bars. With no such deck it runs on the tapped tempo: the
//! set is compiled on a fixed grid of [`TAP_GRID_BPM`], and the engine feeds
//! it the tapped beat converted to that grid's seconds, so a new tempo is a
//! new clock, never a new compile.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use crate::controls::{self, PatternControls, VenueControls};
use crate::database::local::venue_access::AuthorizedVenue;
use crate::eval::aim::Rig;
use crate::eval::{Arena, CompiledAnnotation, Scene};
use crate::models::beat_grid::BeatGrid;
use crate::models::perform::{CellContent, PerformLayout, PerformPattern};
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
    /// Which build of the set this is, so layers compiled for it join it
    /// and no later one.
    id: u64,
    layers: Arc<Layers>,
    controls: Arc<VenueControls>,
    clock: LiveClock,
    /// The beats the set was compiled on, and the beats in a bar of them,
    /// which the bank reads a deck's beat and bar by.
    timeline: luma_patterns::BeatTimeline,
    beats_per_bar: f64,
    /// Each layer's level as of the last frame.
    levels: Vec<f32>,
    /// The bank's count of changes when the set last asked for them, and
    /// where it asks.
    changed: u64,
    wants: tokio::sync::mpsc::UnboundedSender<()>,
}

/// What plays a layer: its pattern, its pad when it has one, its level
/// control, and the groups it was compiled on.
#[derive(Clone, Debug)]
struct Switch {
    pattern: String,
    pad: Option<Pad>,
    level: String,
    groups: Vec<String>,
}

/// How a pattern's pad plays its layer.
#[derive(Clone, Debug)]
enum Pad {
    /// Its envelope, by this control, is the layer's alpha: all the way
    /// off, the layer plays nothing.
    Gate(String),
    /// Its presses start events of a press clock, which play out after it
    /// is let go (a hit).
    Trigger,
}

impl Switch {
    fn new(pattern: &PerformPattern, groups: Vec<String>) -> Self {
        let pad = controls::perform::pad(pattern).map(|pad| match pattern.graph.pad_gates() {
            true => Pad::Gate(pad),
            false => Pad::Trigger,
        });
        Self {
            pattern: pattern.id.clone(),
            pad,
            level: controls::perform::level(&pattern.id),
            groups,
        }
    }
}

/// The set as compiled: its scene, and each layer's switch, in the scene's
/// order. The set's [`Build`] makes it; the engine plays the same one.
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

    /// Read each layer's opacity from the controls: its level × the master,
    /// its pad's envelope (or its hits) being in its graph's alpha already.
    /// A pattern whose pad gates it and is all the way off plays nothing,
    /// nor does a layer compiled on other groups than its pattern plays on
    /// now, unless it already plays and is no hit: a hit's next press must
    /// light where it was asked to. Asks for what the bank changed since the
    /// last frame. Answers whether anything plays, or the base colors
    /// anything.
    pub(crate) fn update_levels(&mut self) -> bool {
        let changed = self.controls.changed();
        if changed != self.changed {
            self.changed = changed;
            let _ = self.wants.send(());
        }
        let master = self.controls.value(controls::perform::MASTER) as f32;
        let mut playing = false;
        for (switch, out) in self.layers.switches.iter().zip(self.levels.iter_mut()) {
            let silent =
                matches!(&switch.pad, Some(Pad::Gate(pad)) if self.controls.value(pad) <= 0.);
            let keeps = *out > 0. && !matches!(switch.pad, Some(Pad::Trigger));
            let moved = !keeps && !self.controls.plays_on(&switch.pattern, &switch.groups);
            *out = if silent || moved {
                0.
            } else {
                self.controls.value(&switch.level) as f32 * master
            };
            playing |= *out > 0.;
        }
        playing || self.controls.has_base()
    }

    /// The set over `base` at `time`, at the levels of the last
    /// [`Self::update_levels`], the base color under it as of `now`.
    pub(crate) fn render_onto(
        &self,
        mut base: UniverseState,
        time: f32,
        now: Instant,
        scratch: &mut Arena,
    ) -> UniverseState {
        self.controls.color_base(&mut base, now);
        self.layers
            .scene
            .render_onto(base, time, &self.levels, scratch)
    }

    /// Play `layers`, the set compiled anew, from now. A layer keeps its
    /// level where it is the same pattern's.
    pub(crate) fn replace(&mut self, layers: Arc<Layers>) {
        let was: HashMap<&str, f32> = self
            .layers
            .switches
            .iter()
            .zip(&self.levels)
            .map(|(switch, level)| (switch.pattern.as_str(), *level))
            .collect();
        let levels = layers
            .switches
            .iter()
            .map(|switch| was.get(switch.pattern.as_str()).copied().unwrap_or(0.))
            .collect();
        self.levels = levels;
        self.layers = layers;
    }
}

/// One pattern as the set's build last compiled it: the groups it was
/// compiled on, and its layer, `None` when it lights nothing there.
struct Compiled {
    groups: Vec<String>,
    layer: Option<CompiledAnnotation>,
}

/// What a set was compiled from and what it compiled, kept while it plays to
/// compile a pattern again when the bank moves it.
pub(crate) struct Build {
    id: u64,
    controls: Arc<VenueControls>,
    patterns: Vec<PerformPattern>,
    track: Option<(u8, String)>,
    grid: Option<BeatGrid>,
    timeline: luma_patterns::BeatTimeline,
    beats_per_bar: f64,
    /// Each pattern's, beside `patterns`.
    compiled: Vec<Compiled>,
    layers: Arc<Layers>,
}

impl Build {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Compile `pattern` on `groups` (none: its own selection); `None` when
    /// it lights nothing there.
    async fn compile(
        &self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
        storage: &StorageRoot,
        pattern: &PerformPattern,
        groups: &[String],
    ) -> Result<Option<CompiledAnnotation>, String> {
        let mut clip = pattern.as_clip(-PICKUP_BARS * self.beats_per_bar, BEATS);
        if !groups.is_empty() {
            clip.selection = luma_patterns::Selection::new(groups.join(" | "));
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
        let source = PatternControls::new(self.controls.clone(), &pattern.id);
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
        Ok(Some(CompiledAnnotation {
            span: plan.span,
            plan: Arc::new(plan),
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
        }))
    }

    /// Bring each pattern to where the bank plays it: compile again each
    /// whose groups changed. Answers whether anything changed.
    async fn refresh(
        &mut self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
        storage: &StorageRoot,
    ) -> Result<bool, String> {
        let mut changed = false;
        for at in 0..self.patterns.len() {
            let pattern = &self.patterns[at];
            let Some(groups) = self.controls.groups(&pattern.id) else {
                continue;
            };
            if self
                .compiled
                .get(at)
                .is_some_and(|was| was.groups == groups)
            {
                continue;
            }
            let layer = self
                .compile(access, fixtures_root, storage, pattern, &groups)
                .await?;
            let compiled = Compiled { groups, layer };
            match self.compiled.get_mut(at) {
                Some(was) => *was = compiled,
                None => self.compiled.push(compiled),
            }
            changed = true;
        }
        Ok(changed)
    }

    /// The set as compiled now, the heads it aims loaded with `access`.
    async fn layers(
        &self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
    ) -> Result<Layers, String> {
        let layers: Vec<(CompiledAnnotation, Switch)> = self
            .patterns
            .iter()
            .zip(&self.compiled)
            .filter_map(|(pattern, compiled)| {
                let switch = Switch::new(pattern, compiled.groups.clone());
                Some((compiled.layer.clone()?, switch))
            })
            .collect();
        let rig = aimed(access, fixtures_root, &layers).await?;
        Layers::new(layers, rig)
    }

    /// Compile again what the bank moved, and answer the set anew; `None`
    /// when nothing moved.
    pub(crate) async fn rebuild(
        &mut self,
        access: &mut impl AuthorizedVenue,
        fixtures_root: &std::path::Path,
        storage: &StorageRoot,
    ) -> Result<Option<Arc<Layers>>, String> {
        if !self.refresh(access, fixtures_root, storage).await? {
            return Ok(None);
        }
        self.layers = Arc::new(self.layers(access, fixtures_root).await?);
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

/// Each group of `layout`'s group cells, and [`controls::ALL`], to its
/// lights. A group that does not resolve lights nothing, and why is logged.
async fn group_lights(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &std::path::Path,
    layout: &PerformLayout,
) -> HashMap<String, Arc<[String]>> {
    let mut groups: BTreeSet<&str> = layout
        .pages
        .iter()
        .flat_map(|page| &page.cells)
        .filter_map(|cell| match &cell.content {
            CellContent::Group { group, .. } => Some(group.as_str()),
            _ => None,
        })
        .collect();
    groups.insert(controls::ALL);
    let mut lights = HashMap::new();
    for group in groups {
        let selection = luma_patterns::Selection::new(group);
        let resolved = crate::eval::context::resolve_selection_primitives_with_access(
            access,
            fixtures_root,
            &selection,
            0,
            None,
        )
        .await;
        match resolved {
            Ok(primitives) => {
                let ids: Arc<[String]> = primitives.into_iter().map(|(id, _)| id).collect();
                lights.insert(group.to_owned(), ids);
            }
            Err(error) => log::warn!("Perform group {group} lights nothing: {error}"),
        }
    }
    lights
}

/// Compile the venue's set as build `id`, each pattern where and how
/// `controls` plays it. `track` is the master deck's song; `None` puts it
/// on the tapped tempo, as does a song with no beat grid. Answers the clock
/// the set runs on, and the set with its build. The set asks on `wants` for
/// what the bank changes after.
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
) -> Result<(LiveClock, (LiveSet, Build)), String> {
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
    let lights = group_lights(access, fixtures_root, layout).await;
    controls.install(patterns, layout, lights);
    let mut build = Build {
        id,
        controls: controls.clone(),
        patterns: patterns.to_vec(),
        track: track.map(|(deck, track_id)| (deck, track_id.to_owned())),
        grid,
        timeline: timeline.clone(),
        beats_per_bar,
        compiled: Vec::new(),
        layers: Arc::default(),
    };
    let changed = controls.changed();
    build.refresh(access, fixtures_root, storage).await?;
    build.layers = Arc::new(build.layers(access, fixtures_root).await?);
    let set = LiveSet {
        id,
        levels: vec![0.; build.layers.switches.len()],
        layers: build.layers.clone(),
        controls,
        clock: live_clock,
        timeline,
        beats_per_bar,
        changed,
        wants,
    };
    Ok((live_clock, (set, build)))
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

    fn switch(pattern: &PerformPattern, groups: &[&str]) -> Switch {
        Switch::new(
            pattern,
            groups.iter().map(|group| (*group).to_owned()).collect(),
        )
    }

    /// A set of `switches` over no scene, played by `controls`.
    fn live(controls: &Arc<VenueControls>, switches: Vec<Switch>) -> LiveSet {
        let (wants, _wanted) = tokio::sync::mpsc::unbounded_channel();
        LiveSet {
            id: 0,
            changed: controls.changed(),
            wants,
            levels: vec![0.; switches.len()],
            layers: Arc::new(Layers {
                scene: Scene::new(Vec::new()),
                switches,
            }),
            controls: controls.clone(),
            clock: LiveClock::Tap,
            timeline: luma_patterns::BeatTimeline::new(vec![0., 0.5], 0.).unwrap(),
            beats_per_bar: controls::TAPPED_BEATS_PER_BAR,
        }
    }

    #[test]
    fn layers_stack_by_z_with_their_switches() {
        let annotation = |z| {
            crate::eval::composite::tests::wash(
                [1.; 3],
                1.,
                1.,
                luma_patterns::BlendMode::Replace,
                z,
            )
        };
        let (low, high) = (pattern("low", true), pattern("high", true));
        let built = Layers::new(
            vec![
                (annotation(1), switch(&high, &[])),
                (annotation(0), switch(&low, &[])),
            ],
            Rig::default(),
        )
        .unwrap();
        let order: Vec<(i64, &str)> = built
            .scene
            .annotations
            .iter()
            .zip(&built.switches)
            .map(|(annotation, switch)| (annotation.z_index, switch.pattern.as_str()))
            .collect();
        assert_eq!(order, [(0, "low"), (1, "high")]);
    }

    #[test]
    fn a_pattern_plays_at_its_level_times_master_while_its_pad_is_up() {
        let controls = Arc::new(VenueControls::default());
        let set = [pattern("a", true), pattern("b", true), pattern("c", false)];
        controls.install(&set, &PerformLayout::default(), HashMap::new());
        let pad = |pattern: &PerformPattern| controls::perform::pad(pattern).unwrap();
        let mut live = live(&controls, set.iter().map(|p| switch(p, &[])).collect());
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

    #[test]
    fn a_layer_compiled_elsewhere_lights_only_while_it_already_plays() {
        let controls = Arc::new(VenueControls::default());
        let wash = pattern("wash", true);
        let layout = PerformLayout {
            pages: vec![crate::models::perform::LayoutPage {
                id: "page".into(),
                name: "Main".into(),
                columns: 2,
                rows: 1,
                one_at_a_time: Vec::new(),
                cells: vec![crate::models::perform::Cell {
                    id: "front".into(),
                    column: 0,
                    row: 0,
                    width: 1,
                    height: 1,
                    content: CellContent::Group {
                        label: String::new(),
                        group: "front".into(),
                    },
                }],
            }],
        };
        controls.install(std::slice::from_ref(&wash), &layout, HashMap::new());
        let pad = controls::perform::pad(&wash).unwrap();
        let mut live = live(&controls, vec![switch(&wash, &[])]);
        let (wants, mut wanted) = tokio::sync::mpsc::unbounded_channel();
        live.wants = wants;
        let at = Instant::now();

        // Off, then on at front: the layer on its own selection stays dark
        // until the one on front lands, and the set asks for it.
        controls.press(&controls::perform::cell("front"), true, at);
        controls.press(&controls::perform::cell("front"), false, at);
        controls.press(&pad, true, at);
        live.update_levels();
        assert_eq!(live.levels, vec![0.]);
        assert!(wanted.try_recv().is_ok(), "the set asked for it");
        live.replace(Arc::new(Layers {
            scene: Scene::new(Vec::new()),
            switches: vec![switch(&wash, &["front"])],
        }));
        live.update_levels();
        assert_eq!(live.levels, vec![1.]);

        // On, moved to its own selection too: the front layer plays on
        // until the next lands.
        controls.choose(&[]);
        controls.press(&pad, true, at);
        controls.press(&pad, true, at);
        live.update_levels();
        assert_eq!(live.levels, vec![1.], "already playing, it plays on");
    }

    #[test]
    fn a_hit_plays_out_after_its_pad_is_let_go_and_never_where_it_was() {
        let controls = Arc::new(VenueControls::default());
        let hit = PerformPattern {
            graph: luma_patterns::ClipGraph::from_json(
                r#"{"version": 3, "nodes": {
                    "pad": {"kind": "button", "settings": {"name": "pad", "mode": "flash"}},
                    "k": {"kind": "time", "inputs": {"on": {"node": "pad"}, "duration": 1}},
                    "fade": {"kind": "curve", "inputs": {"x": {"node": "k"}, "shape": {"points": [[0, 1], [1, 0]]}}},
                    "color1": {"kind": "color", "inputs": {"alpha": {"node": "fade"}}}}}"#,
            )
            .unwrap(),
            ..pattern("hit", false)
        };
        assert!(!hit.graph.pad_gates(), "its pad only starts events");
        let layout = PerformLayout {
            pages: vec![crate::models::perform::LayoutPage {
                id: "page".into(),
                name: "Main".into(),
                columns: 2,
                rows: 1,
                one_at_a_time: Vec::new(),
                cells: vec![crate::models::perform::Cell {
                    id: "front".into(),
                    column: 0,
                    row: 0,
                    width: 1,
                    height: 1,
                    content: CellContent::Group {
                        label: String::new(),
                        group: "front".into(),
                    },
                }],
            }],
        };
        controls.install(std::slice::from_ref(&hit), &layout, HashMap::new());
        let pad = controls::perform::pad(&hit).unwrap();
        let mut live = live(&controls, vec![switch(&hit, &[])]);
        let at = Instant::now();
        controls.press(&pad, true, at);
        controls.press(&pad, false, at);
        live.update_levels();
        assert_eq!(live.levels, vec![1.], "let go, its hit plays out");

        // Pressed again on front: the layer on its own selection would
        // light the new hit there, so it goes dark until front's lands.
        controls.press(&controls::perform::cell("front"), true, at);
        controls.press(&controls::perform::cell("front"), false, at);
        controls.press(&pad, true, at);
        live.update_levels();
        assert_eq!(live.levels, vec![0.]);
    }
}
