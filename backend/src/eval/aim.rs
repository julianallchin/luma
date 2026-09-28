//! After the score: move in black, then the solver that turns each head's aim
//! into pan and tilt. See `docs/specs/aim.md`, "Solver".
//!
//! Aims are room directions: U stage right, V downstage, Z up. The kinematics
//! work in data space (`+Y` upstage), so a direction crosses between the two
//! through [`luma_patterns::Cell::stage_coordinates`], which flips `y` and is
//! its own inverse.
//!
//! Pan and tilt come out as [`PrimitiveState::position`] in degrees, centred
//! on home: the articulation `fixture_kinematics` turns a head by, which the
//! stage draws the beam from and `fixtures::engine` maps onto the DMX range.

use std::collections::HashMap;
use std::sync::Mutex;

use fixture_kinematics::{aim_at, Mount};
use glam::Vec3;
use luma_patterns::{aim::slerp, Cell};

use crate::eval::composite::nothing;
use crate::eval::{Arena, CompiledAnnotation};
use crate::models::fixtures::FixtureDefinition;
use crate::models::universe::{PrimitiveState, UniverseState};

/// Move-in-black sample step, in seconds: about 1/64 beat at 120 BPM.
const LIT_STEP: f32 = 1.0 / 128.0;

/// Times sampled per batch when the lit table is built.
const LIT_CHUNK: usize = 2048;

/// A later frame more than this far ahead of the last one is a seek.
const SEEK_AHEAD: f32 = 1.0;

/// An earlier frame more than this far behind the last one is a seek. Two
/// readers of one scene (the stage and the DMX output) sample close to the
/// same clock, a little out of order.
const SEEK_BACK: f32 = 0.05;

/// Two candidates whose beams agree to within this (in cosine, about 0.08°)
/// reach the aim equally well; the one nearer the previous output wins.
const SAME_REACH: f64 = 1e-6;

/// One head the solver can turn.
#[derive(Clone, Debug)]
pub struct Head {
    /// The frame the head hangs in, in data space: the same pose the stage
    /// draws the beam from.
    mount: Mount,
    /// How far pan and tilt reach either way from home, in degrees. Zero on
    /// an axis the mode has no channel for.
    reach: [f64; 2],
}

impl Head {
    /// A head of a placed fixture, or `None` when its mode has neither pan
    /// nor tilt.
    #[must_use]
    pub fn new(mount: Mount, definition: &FixtureDefinition, mode_name: &str) -> Option<Self> {
        let mode = definition.modes.iter().find(|m| m.name == mode_name)?;
        let moves = definition.moves(mode);
        if moves == [false, false] {
            return None;
        }
        let range = definition.focus_range();
        Some(Self::with_range(
            mount,
            std::array::from_fn(|axis| {
                if moves[axis] {
                    f64::from(range[axis])
                } else {
                    0.0
                }
            }),
        ))
    }

    /// A head hung at `mount` whose pan and tilt each turn `range` degrees
    /// end to end, centred on home.
    #[must_use]
    pub fn with_range(mount: Mount, range: [f64; 2]) -> Self {
        Self {
            mount,
            reach: range.map(|degrees| degrees / 2.0),
        }
    }

    /// Where the head points with pan and tilt at the middle of their ranges,
    /// as a room direction.
    #[must_use]
    pub fn home(&self) -> [f64; 3] {
        Cell::stage_coordinates(self.mount.normal().as_dvec3().to_array())
    }

    /// Pan and tilt, in degrees, that point the head along the room
    /// direction `aim`.
    ///
    /// Every pan that can point the head there is a candidate: the direct
    /// solution, pan ± 180° with the tilt mirrored, and each of those ± 360°,
    /// inside the pan range. So are the two ends of the pan range and the
    /// previous pan, for an aim the head cannot reach and for an aim along
    /// the home axis, where any pan does. Each candidate takes the tilt that
    /// brings the beam nearest the aim, inside the tilt range. The candidate
    /// nearest the aim wins; among those that reach it equally, the one
    /// nearest `previous`.
    #[must_use]
    pub fn solve(&self, aim: [f64; 3], previous: [f32; 2]) -> [f32; 2] {
        let data = Cell::stage_coordinates(aim).map(|v| v as f32);
        let direct = aim_at(&self.mount, Vec3::from(data));
        // The aim in the head's own frame, from the direct solution:
        // `Rz(-pan) · Rx(tilt) · -Z` is `(sin t sin p, sin t cos p, -cos t)`.
        let (sin_t, cos_t) = f64::from(direct.tilt_rad()).sin_cos();
        let (sin_p, cos_p) = f64::from(direct.pan_rad()).sin_cos();
        let local = [sin_t * sin_p, sin_t * cos_p, -cos_t];

        let [pan_reach, tilt_reach] = self.reach;
        let pan = f64::from(direct.pan_rad()).to_degrees();
        let previous = previous.map(f64::from);
        let pans = (-4..=4)
            .map(|turns| pan + 180.0 * f64::from(turns))
            .filter(|p| p.abs() <= pan_reach)
            .chain([
                -pan_reach,
                pan_reach,
                previous[0].clamp(-pan_reach, pan_reach),
            ]);

        let candidates: Vec<([f64; 2], f64)> = pans
            .map(|pan| {
                // Along one pan the beam sweeps a great circle through home;
                // `a sin t + b cos t` is its cosine to the aim at tilt `t`.
                let (sin, cos) = pan.to_radians().sin_cos();
                let (a, b) = (local[0] * sin + local[1] * cos, -local[2]);
                let tilt = a.atan2(b).to_degrees().clamp(-tilt_reach, tilt_reach);
                let (sin_t, cos_t) = tilt.to_radians().sin_cos();
                ([pan, tilt], a * sin_t + b * cos_t)
            })
            .collect();
        let nearest = candidates
            .iter()
            .map(|(_, reach)| *reach)
            .fold(f64::NEG_INFINITY, f64::max);
        let distance =
            |at: &[f64; 2]| (at[0] - previous[0]).powi(2) + (at[1] - previous[1]).powi(2);
        candidates
            .iter()
            .filter(|(_, reach)| *reach >= nearest - SAME_REACH)
            .map(|(at, _)| *at)
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .unwrap_or([0.0; 2])
            .map(|v| v as f32)
    }
}

/// The heads a scene aims, by primitive id (`"<fixture>:<head>"`).
#[derive(Clone, Debug, Default)]
pub struct Rig {
    heads: HashMap<String, Head>,
}

impl Rig {
    pub fn insert(&mut self, id: String, head: Head) {
        self.heads.insert(id, head);
    }

    pub fn extend(&mut self, other: &Rig) {
        self.heads.extend(
            other
                .heads
                .iter()
                .map(|(id, head)| (id.clone(), head.clone())),
        );
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heads.is_empty()
    }

    #[must_use]
    pub fn head(&self, id: &str) -> Option<&Head> {
        self.heads.get(id)
    }

    /// The heads among `ids` that can move, posed as the venue places them.
    ///
    /// # Errors
    /// Fails when the venue cannot be solved or the patch cannot be read. A
    /// head that is unplaced or whose definition does not load is left out.
    pub async fn load<'a>(
        access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
        fixtures_root: &std::path::Path,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, String> {
        let venue = crate::venue_graph::resolved(access, fixtures_root).await?;
        let patch = crate::database::local::fixtures::get_patched_fixtures(access).await?;
        let mut definitions: HashMap<&str, Option<FixtureDefinition>> = HashMap::new();
        let mut rig = Self::default();
        for id in ids {
            let fixture_id = id.split_once(':').map_or(id, |(fixture, _)| fixture);
            let Some(fixture) = patch.iter().find(|f| f.id == fixture_id) else {
                continue;
            };
            let Some(pose) = venue.pose(fixture_id) else {
                continue;
            };
            let definition = definitions
                .entry(fixture.fixture_path.as_str())
                .or_insert_with(|| {
                    crate::fixtures::parser::parse_definition(
                        &fixtures_root.join(&fixture.fixture_path),
                    )
                    .map_err(|error| {
                        log::warn!("[aim] {}: {error}", fixture.fixture_path);
                    })
                    .ok()
                });
            let Some(definition) = definition else {
                continue;
            };
            let mount = crate::fixtures::layout::fixture_mount(pose);
            if let Some(head) = Head::new(mount, definition, &fixture.mode_name) {
                rig.insert(id.to_owned(), head);
            }
        }
        Ok(rig)
    }
}

/// Whether a head gives any light. Dark is exactly zero.
fn lit(state: &PrimitiveState) -> bool {
    state.dimmer > 0.0 && state.color.iter().any(|c| *c > 0.0)
}

/// When each aimed head is lit, sampled every [`LIT_STEP`]: runs of lit
/// sample times, first and last sample of each, in order.
#[derive(Debug, Default)]
struct Lit {
    runs: HashMap<String, Vec<(f32, f32)>>,
}

impl Lit {
    /// Sample the composite of `layers` and record when each head in `rig`
    /// is lit.
    fn build(
        layers: &[CompiledAnnotation],
        rig: &Rig,
        scratch: &mut Arena,
    ) -> Result<Self, String> {
        // Per head: whether the last sample was lit, and the runs so far.
        let mut heads: Vec<(&str, bool, Vec<(f32, f32)>)> = rig
            .heads
            .keys()
            .map(|id| (id.as_str(), false, Vec::new()))
            .collect();
        let start = layers
            .iter()
            .map(|a| a.span.0)
            .fold(f32::INFINITY, f32::min);
        let end = layers
            .iter()
            .map(|a| a.span.1)
            .fold(f32::NEG_INFINITY, f32::max);
        if end > start {
            let count = ((end - start) / LIT_STEP).ceil() as usize + 1;
            for first in (0..count).step_by(LIT_CHUNK) {
                let times: Vec<f32> = (first..count.min(first + LIT_CHUNK))
                    .map(|i| start + i as f32 * LIT_STEP)
                    .collect();
                let frames = super::scene::composite(layers, &times, scratch, Some(rig))?;
                for (t, frame) in times.iter().zip(&frames) {
                    for (id, was, runs) in &mut heads {
                        let now = frame.primitives.get(*id).is_some_and(lit);
                        match (now, *was, runs.last_mut()) {
                            (true, true, Some(run)) => run.1 = *t,
                            (true, _, _) => runs.push((*t, *t)),
                            _ => {}
                        }
                        *was = now;
                    }
                }
            }
        }
        let runs = heads
            .into_iter()
            .filter(|(_, _, runs)| !runs.is_empty())
            .map(|(id, _, runs)| (id.to_owned(), runs))
            .collect();
        Ok(Self { runs })
    }

    /// The time whose aim a head dark at `t` takes: its next lit sample, or
    /// its last lit sample once it is never lit again. `None` when the table
    /// has it lit at `t` (it is dark only between samples) or never lit.
    fn source(&self, id: &str, t: f32) -> Option<f32> {
        let runs = self.runs.get(id)?;
        let next = runs.partition_point(|run| run.1 < t);
        match runs.get(next) {
            Some(run) if run.0 <= t => None,
            Some(run) => Some(run.0),
            None => runs.last().map(|run| run.1),
        }
    }
}

/// Pan and tilt last sent per head, and the time they were sent at.
#[derive(Debug, Default)]
struct Solver {
    previous: HashMap<String, [f32; 2]>,
    at: Option<f32>,
}

impl Solver {
    /// Turn each aimed head's aim into pan and tilt. After a seek, every head
    /// starts again from home.
    fn solve(&mut self, rig: &Rig, t: f32, frame: &mut UniverseState) {
        let contiguous = self
            .at
            .is_some_and(|at| t >= at - SEEK_BACK && t <= at + SEEK_AHEAD);
        if !contiguous {
            self.previous.clear();
        }
        self.at = Some(t);
        for (id, state) in &mut frame.primitives {
            let Some(head) = rig.head(id) else {
                continue;
            };
            // No aim holds the head at home, where nothing moves it from.
            if let Some(aim) = state.aim {
                let target = slerp(
                    head.home(),
                    aim.direction.map(f64::from),
                    f64::from(aim.weight.clamp(0.0, 1.0)),
                );
                let previous = self.previous.get(id).copied().unwrap_or([0.0; 2]);
                state.position = head.solve(target, previous);
                // Timing comes from the clips: the motors run flat out.
                state.speed = 1.0;
            }
            match self.previous.get_mut(id) {
                Some(previous) => *previous = state.position,
                None => {
                    self.previous.insert(id.clone(), state.position);
                }
            }
        }
    }
}

/// What a scene needs to aim its heads: the rig, when each head is lit, and
/// the solver's memory of what it last sent.
#[derive(Debug)]
pub(crate) struct Aiming {
    rig: Rig,
    lit: Lit,
    solver: Mutex<Solver>,
}

impl Aiming {
    /// Everything [`Self::apply`] needs, or `None` when no layer aims a head
    /// in `rig`.
    pub(crate) fn new(layers: &[CompiledAnnotation], mut rig: Rig) -> Result<Option<Self>, String> {
        let aimed: std::collections::HashSet<&str> = layers
            .iter()
            .filter(|layer| layer.plan.outputs.aim)
            .flat_map(|layer| layer.plan.primitive_ids.iter().map(String::as_str))
            .collect();
        rig.heads.retain(|id, _| aimed.contains(id.as_str()));
        if rig.is_empty() {
            return Ok(None);
        }
        // Only layers that can light an aimed head decide when it is dark.
        let lighting: Vec<CompiledAnnotation> = layers
            .iter()
            .filter(|layer| layer.plan.outputs.color || layer.plan.outputs.dimmer)
            .filter(|layer| {
                layer
                    .plan
                    .primitive_ids
                    .iter()
                    .any(|id| rig.heads.contains_key(id))
            })
            .cloned()
            .collect();
        let lit = Lit::build(&lighting, &rig, &mut Arena::default())?;
        Ok(Some(Self {
            rig,
            lit,
            solver: Mutex::default(),
        }))
    }

    pub(crate) fn rig(&self) -> &Rig {
        &self.rig
    }

    /// Move in black, then solve, for `frames` rendered from `layers` at
    /// `times`.
    ///
    /// While a head gives no light it takes the aim it has at its next lit
    /// moment, or after its last one, the aim it had then. A fade still above
    /// zero is lit and keeps its own aim.
    pub(crate) fn apply(
        &self,
        layers: &[CompiledAnnotation],
        times: &[f32],
        frames: &mut [UniverseState],
        scratch: &mut Arena,
    ) -> Result<(), String> {
        let mut moves: Vec<(usize, &str, f32)> = Vec::new();
        for (k, (t, frame)) in times.iter().zip(frames.iter()).enumerate() {
            for id in self.lit.runs.keys() {
                if frame.primitives.get(id).is_some_and(lit) {
                    continue;
                }
                if let Some(source) = self.lit.source(id, *t) {
                    moves.push((k, id, source));
                }
            }
        }
        if !moves.is_empty() {
            let mut sources: Vec<f32> = moves.iter().map(|m| m.2).collect();
            sources.sort_by(f32::total_cmp);
            sources.dedup();
            let aims = super::scene::composite(layers, &sources, scratch, Some(&self.rig))?;
            for (k, id, source) in moves {
                let at = sources
                    .binary_search_by(|t| t.total_cmp(&source))
                    .expect("every source was sampled");
                let aim = aims[at].primitives.get(id).and_then(|state| state.aim);
                frames[k]
                    .primitives
                    .entry(id.to_owned())
                    .or_insert_with(nothing)
                    .aim = aim;
            }
        }
        let mut solver = self
            .solver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (t, frame) in times.iter().zip(frames.iter_mut()) {
            solver.solve(&self.rig, *t, frame);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{lighting::compile_clip, Scene, Scope};
    use crate::models::fixtures::{Channel, Focus, Mode, ModeChannel, Physical};
    use luma_patterns as p;
    use luma_scene::venue::{NodeKind, NodePose, Params};
    use std::f64::consts::{FRAC_PI_2, PI};
    use std::sync::Arc;

    /// A moving head with pan and tilt, `pan_max` and `tilt_max` degrees.
    fn mover(pan_max: u32, tilt_max: u32) -> FixtureDefinition {
        let channel = |name: &str, preset: &str| Channel {
            name: name.into(),
            preset: Some(preset.into()),
            group: None,
            capabilities: vec![],
        };
        FixtureDefinition {
            manufacturer: "Test".into(),
            model: "Mover".into(),
            type_: "Moving Head".into(),
            channels: vec![
                channel("Pan", "PositionPan"),
                channel("Tilt", "PositionTilt"),
                channel("Speed", "SpeedPanTiltFastSlow"),
            ],
            modes: vec![Mode {
                name: "m".into(),
                channels: ["Pan", "Tilt", "Speed"]
                    .iter()
                    .enumerate()
                    .map(|(number, name)| ModeChannel {
                        number: number as u32,
                        name: (*name).into(),
                    })
                    .collect(),
                heads: vec![],
            }],
            physical: Some(Physical {
                dimensions: None,
                layout: None,
                bulb: None,
                lens: None,
                focus: Some(Focus {
                    type_: Some("Head".into()),
                    pan_max: Some(pan_max),
                    tilt_max: Some(tilt_max),
                }),
                technical: None,
            }),
        }
    }

    /// A fixture posed as the venue solve poses one: the stored data-space
    /// position and Euler triple, as the three-space world transform.
    fn pose(rot: [f64; 3]) -> NodePose {
        NodePose {
            node: "fx".into(),
            kind: NodeKind::Fixture,
            catalog_ref: None,
            label: None,
            parent: None,
            world: luma_scene::coords::three_pose_from_data_d([1.0, 2.0, 5.0], rot),
            array_index: None,
            params: Params::default(),
        }
    }

    fn head(pose: &NodePose, definition: &FixtureDefinition) -> Head {
        Head::new(
            crate::fixtures::layout::fixture_mount(pose),
            definition,
            "m",
        )
        .unwrap()
    }

    fn degrees_between(a: [f64; 3], b: [f64; 3]) -> f64 {
        let (a, b) = (p::aim::unit(a), p::aim::unit(b));
        (a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    }

    /// The beam the stage draws for `position`, as a room direction: the
    /// renderer's own `beam_direction`, from the pose's stored triple.
    fn drawn(pose: &NodePose, definition: &FixtureDefinition, position: [f32; 2]) -> [f64; 3] {
        let (_, rot) = pose.data_pose();
        luma_render::luminaire::beam_direction(
            Some(&crate::stage_render::definition(definition)),
            rot.map(|v| v as f32),
            Some(position),
        )
        .as_dvec3()
        .to_array()
    }

    #[test]
    fn the_stage_draws_the_beam_along_the_solved_aim() {
        let definition = mover(540, 270);
        let poses = [
            ("hung", [0.0, 0.0, 0.0]),
            ("floor", [PI, 0.0, 0.0]),
            ("downstage face", [FRAC_PI_2, 0.0, 0.0]),
            ("hung, turned 90°", [0.0, 0.0, FRAC_PI_2]),
            ("side face", [0.0, FRAC_PI_2, 0.0]),
            ("tilted and turned", [0.4, -0.3, 1.1]),
        ];
        for (name, rot) in poses {
            let pose = pose(rot);
            let head = head(&pose, &definition);
            let home = head.home();
            assert!(
                degrees_between(drawn(&pose, &definition, [0.0, 0.0]), home) < 0.5,
                "{name}: home is where the stage draws pan and tilt 0"
            );
            // Aims within 120° of home: inside a 270° tilt.
            for yaw in [-150.0, -60.0, 0.0, 35.0, 90.0, 170.0] {
                for pitch in [10.0, 45.0, 80.0, 120.0] {
                    let aim =
                        p::aim::lean(home, p::aim::offset(p::aim::frame(home).0, yaw, 0.0), pitch);
                    let position = head.solve(aim, [0.0, 0.0]);
                    let got = drawn(&pose, &definition, position);
                    assert!(
                        degrees_between(got, aim) < 0.5,
                        "{name}: aim {aim:?} solved to {position:?}, drawn along {got:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_hung_head_points_down_at_home_and_a_floor_head_up() {
        let definition = mover(540, 270);
        let close = |a: [f64; 3], b: [f64; 3]| degrees_between(a, b) < 1e-3;
        assert!(close(
            head(&pose([0.0; 3]), &definition).home(),
            [0.0, 0.0, -1.0]
        ));
        assert!(close(
            head(&pose([PI, 0.0, 0.0]), &definition).home(),
            [0.0, 0.0, 1.0]
        ));
        // Clamped to the downstage face of a truss: home is the audience.
        let face = head(&pose([FRAC_PI_2, 0.0, 0.0]), &definition);
        assert!(close(face.home(), [0.0, 1.0, 0.0]), "{:?}", face.home());
    }

    #[test]
    fn the_solver_takes_the_pair_nearest_the_last_output() {
        let definition = mover(540, 270);
        let pose = pose([0.0; 3]);
        let head = head(&pose, &definition);
        let aim = drawn(&pose, &definition, [170.0, 60.0]);
        // From home the mirrored pair is the nearer: pan − 180° with the tilt
        // negated.
        let from_home = head.solve(aim, [0.0, 0.0]);
        assert!((from_home[0] + 10.0).abs() < 0.01, "{from_home:?}");
        assert!((from_home[1] + 60.0).abs() < 0.01, "{from_home:?}");
        // Coming from 170°, a move past 180° keeps going rather than spinning
        // back the long way.
        let from_170 = head.solve(aim, [169.0, 60.0]);
        assert!((from_170[0] - 170.0).abs() < 0.01, "{from_170:?}");
        let beyond = drawn(&pose, &definition, [190.0, 60.0]);
        let kept = head.solve(beyond, from_170);
        assert!((kept[0] - 190.0).abs() < 0.01, "{kept:?}");
        assert!((kept[1] - 60.0).abs() < 0.01, "{kept:?}");
        // Past the end of the pan range (±270°) the other pair takes over.
        let far = drawn(&pose, &definition, [280.0, 60.0]);
        let wrapped = head.solve(far, [265.0, 60.0]);
        assert!(wrapped[0].abs() <= 270.0, "{wrapped:?}");
        assert!(degrees_between(drawn(&pose, &definition, wrapped), far) < 0.5);
    }

    #[test]
    fn an_aim_out_of_reach_clamps_to_the_nearest_reachable() {
        // Tilt ±90°: a hung head reaches the horizon and no higher.
        let definition = mover(540, 180);
        let pose = pose([0.0; 3]);
        let head = head(&pose, &definition);
        let up_and_out = p::aim::unit([0.0, 1.0, 1.0]);
        let position = head.solve(up_and_out, [0.0, 0.0]);
        let got = drawn(&pose, &definition, position);
        assert!(position[1].abs() <= 90.0 + 1e-3, "{position:?}");
        assert!(
            (degrees_between(got, up_and_out) - 45.0).abs() < 0.5,
            "{position:?} draws {got:?}"
        );
        // Pan ±90°: straight behind is reached by tilting through home.
        let narrow = mover(180, 270);
        let head = Head::new(crate::fixtures::layout::fixture_mount(&pose), &narrow, "m").unwrap();
        for aim in [[1.0, 0.2, -0.5], [-0.3, -1.0, -0.2], [0.0, 0.0, 1.0]] {
            let position = head.solve(aim, [0.0, 0.0]);
            assert!(position[0].abs() <= 90.0 + 1e-3, "{position:?}");
            assert!(position[1].abs() <= 135.0 + 1e-3, "{position:?}");
        }
    }

    // -- move in black ------------------------------------------------------

    /// A clip on the one head, compiled on a one-beat-per-second clock.
    fn layer(mut clip: p::Clip, z: i64) -> CompiledAnnotation {
        clip.selection_seed = None;
        let cells = vec![p::Cell {
            id: "fx:0".into(),
            group: "movers".into(),
            world: [1.0, 2.0, 5.0],
            uvz: [1.0, -2.0, 5.0],
        }];
        let prepared = p::PreparedGraph::new(
            &p::standard_library(),
            &clip.graph,
            &clip.inputs,
            p::Frame {
                features: None,
                cells: &cells,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: 0,
            },
        )
        .unwrap();
        let clock = p::BeatTimeline::new((0..=12).map(f64::from).collect(), 0.0).unwrap();
        let plan = compile_clip(&clip, clock, cells, prepared, "lighting").unwrap();
        CompiledAnnotation {
            span: plan.span,
            plan: Arc::new(plan),
            z_index: z,
            blend_mode: p::BlendMode::Replace,
        }
    }

    /// The shipped preset of `form` called `name`.
    fn preset(form: &str, name: &str) -> &'static p::FormPreset {
        p::presets()
            .presets
            .iter()
            .find(|preset| preset.form == form && preset.name == name)
            .expect("a shipped preset")
    }

    fn wash(start: f64, duration: f64, brightness: f64) -> CompiledAnnotation {
        let mut clip = preset("color@1", "Wash").clip(start, duration);
        clip.inputs
            .insert("brightness".into(), p::Value::Proportion(brightness));
        layer(clip, 0)
    }

    fn aim(start: f64, duration: f64, direction: [f64; 3]) -> CompiledAnnotation {
        let mut clip = preset("aim@1", "Position").clip(start, duration);
        clip.inputs
            .insert("direction".into(), p::Value::Vector(direction));
        layer(clip, 1)
    }

    fn rig() -> Rig {
        let mut rig = Rig::default();
        rig.insert("fx:0".into(), head(&pose([0.0; 3]), &mover(540, 270)));
        rig
    }

    const FIRST: [f64; 3] = [0.0, 0.6, -0.8];
    const SECOND: [f64; 3] = [0.6, 0.0, -0.8];

    fn aim_at(scene: &Scene, t: f32) -> PrimitiveState {
        scene.render(&[t], Scope::Composite, &mut Arena::default())[0].primitives["fx:0"].clone()
    }

    fn along(state: &PrimitiveState, direction: [f64; 3]) -> bool {
        let aim = state.aim.expect("the head has an aim");
        degrees_between(aim.direction.map(f64::from), direction) < 0.1
    }

    #[test]
    fn a_head_dark_between_clips_turns_to_the_next_clip_in_black() {
        let scene = Scene::new(vec![
            wash(0.0, 2.0, 1.0),
            aim(0.0, 2.0, FIRST),
            wash(4.0, 2.0, 1.0),
            aim(4.0, 2.0, SECOND),
        ])
        .with_rig(rig())
        .unwrap();
        assert!(along(&aim_at(&scene, 1.0), FIRST));
        // In the gap nothing covers the head: it takes the second aim at once.
        for t in [2.05, 3.0, 3.99] {
            let state = aim_at(&scene, t);
            assert_eq!(state.dimmer, 0.0);
            assert!(along(&state, SECOND), "at {t}: {state:?}");
            let solved = rig().head("fx:0").unwrap().solve(SECOND, [0.0; 2]);
            assert!(
                (state.position[0] - solved[0]).abs() < 0.1
                    && (state.position[1] - solved[1]).abs() < 0.1,
                "at {t}: the DMX position follows the moved aim: {state:?}"
            );
        }
        assert!(along(&aim_at(&scene, 5.0), SECOND));
        // After the last lit moment it keeps the last lit aim.
        assert!(along(&aim_at(&scene, 9.0), SECOND));
    }

    #[test]
    fn a_fading_head_that_still_gives_light_is_not_moved() {
        let scene = Scene::new(vec![
            wash(0.0, 2.0, 1.0),
            // Faint but lit through the gap, under the first aim.
            wash(2.0, 2.0, 0.01),
            aim(0.0, 4.0, FIRST),
            wash(4.0, 2.0, 1.0),
            aim(4.0, 2.0, SECOND),
        ])
        .with_rig(rig())
        .unwrap();
        let state = aim_at(&scene, 3.0);
        assert!(state.dimmer > 0.0);
        assert!(along(&state, FIRST), "{state:?}");
    }

    #[test]
    fn an_offset_over_no_aim_starts_from_home() {
        // A head on the floor: home is straight up, not the fallback down.
        let mut rig = Rig::default();
        rig.insert("fx:0".into(), head(&pose([PI, 0.0, 0.0]), &mover(540, 270)));
        let home = rig.head("fx:0").unwrap().home();
        let circle = |alpha: f64, mode, direction| {
            let mut clip = preset("aim@1", "Circle").clip(0.0, 4.0);
            clip.inputs
                .insert("alpha".into(), p::Value::Proportion(alpha));
            clip.inputs
                .insert("direction".into(), p::Value::Vector(direction));
            let mut layer = layer(clip, 0);
            layer.blend_mode = mode;
            layer
        };
        let offset = |alpha| {
            let scene = Scene::new(vec![circle(alpha, p::BlendMode::Offset, FIRST)])
                .with_rig(rig.clone())
                .unwrap();
            aim_at(&scene, 1.3).aim.expect("an aim")
        };
        let from_home = Scene::new(vec![circle(1.0, p::BlendMode::Replace, home)]);
        let expected = aim_at(&from_home, 1.3).aim.unwrap();
        let full = offset(1.0);
        assert_eq!(full.weight, 1.0);
        assert!(
            degrees_between(
                full.direction.map(f64::from),
                expected.direction.map(f64::from)
            ) < 1e-3,
            "{full:?} != {expected:?}"
        );
        // Over no aim, alpha is the weight, as for a Replace clip.
        assert_eq!(offset(0.5).weight, 0.5);
    }

    #[test]
    fn a_scene_with_no_aim_clip_does_no_aiming() {
        let scene = Scene::new(vec![wash(0.0, 2.0, 1.0)])
            .with_rig(rig())
            .unwrap();
        assert!(scene.rig().is_none());
    }
}
