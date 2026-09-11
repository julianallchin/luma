//! Canonical graph preparation and fixture/diagnostic output adapters.
use super::{Arena, OutputBinding, Plan, ResidentContext, ViewTap};
use crate::models::node_graph::Signal;
use crate::models::universe::{PrimitiveState, UniverseState};
use luma_patterns as p;
#[cfg(test)]
use luma_patterns::Body;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
#[derive(Clone, Debug)]
pub struct Program {
    prepared: p::PreparedGraph,
    clock: p::BeatTimeline,
    ids: Vec<String>,
    output: String,
}
#[derive(Debug)]
pub struct Inspection {
    pub times: Vec<f32>,
    pub clock: p::BeatTimeline,
    pub clip_start: f64,
    pub values: BTreeMap<String, p::EvaluatedValue>,
    pub spectrograms: BTreeMap<String, Result<Arc<crate::audio::melspec::Spectrogram>, String>>,
}
pub(crate) fn plan(
    prepared: p::PreparedGraph,
    clock: p::BeatTimeline,
    ids: Vec<String>,
    output: &str,
    ctx: ResidentContext,
) -> Result<Plan, String> {
    let program = Program {
        prepared,
        clock,
        ids: ids.clone(),
        output: output.into(),
    };
    let initial = program.sample(&[ctx.span.0])?;
    let writes = initial
        .get(output)
        .and_then(p::EvaluatedValue::lighting)
        .ok_or("graph did not produce fixture output")?
        .writes();
    let views = initial
        .iter()
        .filter_map(|(name, value)| {
            if name == output {
                return None;
            }
            let (n, c, channels) = if let Some(signal) = value.signal() {
                let (n, _, c) = signal.values().dim();
                (n, c, channel_labels(*signal.channels(), c))
            } else if matches!(value.sample(0), Ok(p::Value::Events(_))) {
                (1, 1, vec!["events".into()])
            } else {
                return None;
            };
            Some((
                name.strip_prefix("view/").unwrap_or(name).to_owned(),
                ViewTap {
                    output: name.clone(),
                    n,
                    c,
                    channels,
                },
            ))
        })
        .collect();
    Ok(Plan {
        program: Some(Arc::new(program)),
        primitive_ids: ids,
        outputs: OutputBinding {
            color: writes[0],
            dimmer: writes[1],
            position: writes[2],
            strobe: writes[3],
            speed: writes[4],
        },
        ctx,
        views,
    })
}
pub(crate) fn compile_clip(
    clip: &p::Clip,
    clock: p::BeatTimeline,
    cells: Vec<p::Cell>,
    prepared: p::PreparedGraph,
    output: &str,
) -> Result<Plan, String> {
    let span = (
        clock.seconds_at(clip.start).map_err(|e| e.to_string())? as f32,
        clock
            .seconds_at(clip.start + clip.duration)
            .map_err(|e| e.to_string())? as f32,
    );
    if !span.0.is_finite() || !span.1.is_finite() || span.1 <= span.0 {
        return Err("clip duration cannot be represented on the playback timeline".into());
    }
    let ids = cells.iter().map(|c| c.id.clone()).collect();
    plan(
        prepared,
        clock,
        ids,
        output,
        ResidentContext {
            seed: clip.seed,
            span,
            positions: cells.iter().map(|c| c.world.map(|v| v as f32)).collect(),
            ..Default::default()
        },
    )
}
fn channel_labels(channels: p::Channels, width: usize) -> Vec<String> {
    let names: &[&str] = match channels {
        p::Channels::Rgb => &["r", "g", "b"],
        p::Channels::PanTilt => &["pan", "tilt"],
        p::Channels::Value => &["value"],
        _ => return (0..width).map(|i| format!("ch{i}")).collect(),
    };
    names.iter().map(|s| (*s).into()).collect()
}
impl Program {
    pub(crate) fn inspect(&self, span: (f32, f32)) -> Result<Option<Inspection>, String> {
        let names: Vec<_> = self
            .prepared
            .output_types()
            .iter()
            .filter(|(name, output)| {
                **name != self.output
                    && (output.value_type.signal_type().is_some()
                        || matches!(
                            output.value_type,
                            p::ValueType::Events | p::ValueType::AudioSource
                        ))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if names.is_empty() {
            return Ok(None);
        }
        const SAMPLES: usize = 128;
        if self.prepared.cell_count().saturating_mul(SAMPLES) > 1_000_000 {
            return Err("graph inspection exceeds one million head samples".into());
        }
        let times: Vec<_> = (0..SAMPLES)
            .map(|i| span.0 + (span.1 - span.0) * i as f32 / (SAMPLES - 1) as f32)
            .collect();
        let mut values = self.sample(&times)?;
        values.retain(|name, _| names.contains(name));
        Ok(Some(Inspection {
            times,
            clock: self.clock.clone(),
            clip_start: self
                .clock
                .beat_at(f64::from(span.0))
                .map_err(|e| e.to_string())?,
            values,
            spectrograms: BTreeMap::new(),
        }))
    }

    pub(crate) fn sample(
        &self,
        times: &[f32],
    ) -> Result<BTreeMap<String, p::EvaluatedValue>, String> {
        times
            .iter()
            .map(|t| self.clock.beat_at(f64::from(*t)))
            .collect::<p::Result<Vec<_>>>()
            .and_then(|beats| self.prepared.evaluate_batch(&beats))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn render(
        &self,
        times: &[f32],
        bindings: &OutputBinding,
        scratch: &mut Arena,
    ) -> Result<Vec<UniverseState>, String> {
        if times.is_empty() {
            return Ok(vec![]);
        }
        scratch.values = self.sample(times)?;
        let lighting = scratch
            .values
            .get(&self.output)
            .and_then(p::EvaluatedValue::lighting)
            .ok_or("graph did not produce fixture output")?;
        let rows: BTreeMap<_, _> = lighting
            .fixtures()
            .iter()
            .enumerate()
            .map(|(n, id)| (id, n))
            .collect();
        let tensor = lighting.values();
        let rows = self
            .ids
            .iter()
            .map(|id| {
                rows.get(id)
                    .copied()
                    .ok_or_else(|| format!("graph output is missing fixture {id}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((0..times.len())
            .map(|k| {
                let t = if tensor.dim().1 == 1 { 0 } else { k };
                let primitives = self
                    .ids
                    .iter()
                    .zip(&rows)
                    .map(|(id, &row)| {
                        let v = |ch| tensor[[row, t, ch]] as f32;
                        (
                            id.clone(),
                            PrimitiveState {
                                color: if bindings.color {
                                    [v(0), v(1), v(2)]
                                } else {
                                    [1.; 3]
                                },
                                dimmer: if bindings.dimmer { v(3) } else { 0. },
                                position: if bindings.position {
                                    [v(4), v(5)]
                                } else {
                                    [0.; 2]
                                },
                                strobe: if bindings.strobe { v(6) } else { 0. },
                                speed: if bindings.speed { v(7) } else { 1. },
                            },
                        )
                    })
                    .collect();
                UniverseState { primitives }
            })
            .collect())
    }
    pub(crate) fn views(
        &self,
        times: &[f32],
        taps: &[(String, ViewTap)],
        span: (f32, f32),
        scratch: &mut Arena,
    ) -> Result<HashMap<String, Signal>, String> {
        if taps.is_empty() || times.is_empty() {
            return Ok(HashMap::new());
        }
        scratch.values = self.sample(times)?;
        taps.iter()
            .map(|(name, tap)| {
                let value = &scratch.values[&tap.output];
                let signal = if let Some(signal) = value.signal() {
                    let values = signal.values();
                    let (n, t, c) = values.dim();
                    let rows = match signal.fixtures() {
                        Some(domain) => self
                            .ids
                            .iter()
                            .map(|id| {
                                domain.iter().position(|v| v == id).ok_or_else(|| {
                                    format!("view {} is missing fixture {id}", tap.output)
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        None => (0..n).collect(),
                    };
                    let mut data = Vec::with_capacity(rows.len() * times.len() * c);
                    for row in &rows {
                        for k in 0..times.len() {
                            for ch in 0..c {
                                data.push(values[[*row, if t == 1 { 0 } else { k }, ch]] as f32);
                            }
                        }
                    }
                    Signal {
                        n: rows.len(),
                        t: times.len(),
                        c,
                        data,
                    }
                } else if let Ok(p::Value::Events(events)) = value.sample(0) {
                    self.event_signal(&events, times, span)?
                } else {
                    return Err(format!("{} is not a numerical or event view", tap.output));
                };
                Ok((name.clone(), signal))
            })
            .collect()
    }
    fn event_signal(
        &self,
        events: &p::Events,
        times: &[f32],
        span: (f32, f32),
    ) -> Result<Signal, String> {
        let mut source = events;
        while let p::Events::Targeted { events, .. } = source {
            source = events;
        }
        let mut data = vec![0.; times.len()];
        let start = self
            .clock
            .beat_at(f64::from(span.0))
            .map_err(|e| e.to_string())?;
        let end = self
            .clock
            .beat_at(f64::from(span.1))
            .map_err(|e| e.to_string())?;
        // Event display bins are bounded by the requested grid, even for very dense schedules.
        for (i, value) in data.iter_mut().enumerate() {
            let a = span.0 + (span.1 - span.0) * i as f32 / times.len() as f32;
            let b = span.0 + (span.1 - span.0) * (i + 1) as f32 / times.len() as f32;
            let lo = self
                .clock
                .beat_at(f64::from(a))
                .map_err(|e| e.to_string())?;
            let hi = self
                .clock
                .beat_at(f64::from(b))
                .map_err(|e| e.to_string())?;
            let found = match source {
                p::Events::Beats { times: recorded } => {
                    let events = recorded.as_slice();
                    let index = events.partition_point(|t| *t < lo);
                    events
                        .get(index)
                        .is_some_and(|t| *t < hi || (i + 1 == times.len() && *t == end))
                }
                p::Events::Periodic {
                    repeat,
                    grid_aligned,
                    delay,
                } => {
                    let origin = if *grid_aligned { 0. } else { start } + delay;
                    let event = origin + ((lo - origin) / repeat).ceil() * repeat;
                    event < hi || (i + 1 == times.len() && event == end)
                }
                _ => false,
            };
            *value = if found { 1. } else { 0. };
        }
        Ok(Signal {
            n: 1,
            t: times.len(),
            c: 1,
            data,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::BeatGrid;
    #[test]
    fn dynamic_graph_errors_propagate_without_poisoning_later_seeks() {
        let score: p::Score = serde_json::from_value(serde_json::json!({
            "version":2,"definitions":{"custom":{
                "name":"Runtime error", "inputs":{},
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{"nodes":{
                    "clock":{"definition":"clip_time"},
                    "subtract":{"definition":"core/subtract","inputs":{
                        "a":{"source":"value","value":{"type":"number","value":0.5}},
                        "b":{"source":"connection","node":"clock","output":"progress"}}},
                    "root":{"definition":"core/square_root","inputs":{"value":{"source":"connection","node":"subtract","output":"value"}}},
                    "output":{"definition":"output","inputs":{"dimmer":{"source":"connection","node":"root","output":"value"}}}
                },"outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
            }},"clips":{"clip":{"graph":"custom","start":0,"duration":4,"seed":0}}
        })).unwrap();
        let library = score.library(&p::standard_library()).unwrap();
        let cells = vec![p::Cell {
            id: "head".into(),
            group: "wash".into(),
            world: [0.; 3],
            uvz: [0.; 3],
        }];
        let clip = &score.clips["clip"];
        let program = p::PreparedGraph::new(
            &library,
            &clip.graph,
            &clip.inputs,
            p::Frame {
                features: None,
                cells: &cells,
                beat: 0.,
                clip_start: 0.,
                clip_duration: 4.,
                seed: 0,
            },
        )
        .unwrap();
        let clock = p::BeatTimeline::new(vec![0., 0.5, 1., 1.5, 2.], 0.).unwrap();
        let plan = compile_clip(clip, clock, cells, program, "lighting").unwrap();
        let scene = crate::eval::Scene::new(vec![crate::eval::CompiledAnnotation {
            span: plan.ctx.span,
            plan: Arc::new(plan),
            z_index: 0,
            blend_mode: p::BlendMode::Replace,
        }]);
        let mut scratch = crate::eval::Arena::default();
        let scope = crate::eval::Scope::Composite;
        let before = scene.try_render(&[0.5], scope, &mut scratch).unwrap();
        assert_eq!(
            scene
                .try_render(&[0.5, 1.5], scope, &mut scratch)
                .unwrap_err(),
            "Square root: needs nonnegative values"
        );
        assert!(scene.render(&[1.5], scope, &mut scratch)[0]
            .primitives
            .is_empty());
        let after = scene.try_render(&[0.5], scope, &mut scratch).unwrap();
        assert_eq!(before[0].primitives["head"].dimmer, 0.5);
        assert_eq!(after[0].primitives["head"].dimmer, 0.5);
        let clipped = scene.try_render(&[2.5, 0.5], scope, &mut scratch).unwrap();
        assert!(
            clipped[0].primitives.is_empty(),
            "inactive times never evaluate the graph"
        );
        assert_eq!(clipped[1].primitives["head"].dimmer, 0.5);
    }

    #[test]
    fn graph_score_compiles_local_definitions_and_matches_batched_core_output() {
        let base = p::standard_library();
        let mut score = p::Score::default();
        score
            .insert_effect(&base, "beat_dissolve", "flash", 1.0, 3.0)
            .unwrap();
        // The output name is author-owned; playback must follow the interface.
        let graph = score.definitions.get_mut("flash").unwrap();
        let output = graph.outputs.remove("lighting").unwrap();
        graph.outputs.insert("heads".into(), output);
        if let Body::Graph(graph) = &mut graph.body {
            let output = graph.outputs.remove("lighting").unwrap();
            graph.outputs.insert("heads".into(), output);
        }
        let clip = score.clips.get_mut("flash").unwrap();
        clip.seed = 129;
        clip.inputs
            .insert("grid_aligned".into(), p::Value::Boolean(false));
        let cells: Vec<_> = (0..24)
            .map(|i| p::Cell {
                id: format!("bar:{i}"),
                group: "bars".into(),
                world: [0.0, 0.0, i as f64],
                uvz: [0.0, 0.0, i as f64],
            })
            .collect();
        let library = score.library(&base).unwrap();
        let clock = p::BeatTimeline::new(vec![0.0, 0.5, 1.0, 2.0, 3.0, 4.0], 0.0).unwrap();
        let clip = &score.clips["flash"];
        let program = p::PreparedGraph::new(
            &library,
            &clip.graph,
            &clip.inputs,
            p::Frame {
                cells: &cells,
                features: None,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )
        .unwrap();
        let plan = compile_clip(clip, clock.clone(), cells.clone(), program, "heads").unwrap();
        let prepared = score
            .prepare_clip(&base, "flash", &BTreeMap::new(), &cells)
            .unwrap();
        let scene = crate::eval::Scene::new(vec![crate::eval::CompiledAnnotation {
            span: plan.ctx.span,
            plan: Arc::new(plan),
            z_index: 0,
            blend_mode: clip.blend_mode,
        }]);
        // Rendering a time batch executes each graph operation once. Reordered
        // host rows still follow fixture identity, independently of tensor rows.
        let seconds = [2.75_f32, 0.5, 1.25, 0.875, 2.25, 3.0, 0.0];
        let frames = scene.render(
            &seconds,
            crate::eval::Scope::Composite,
            &mut crate::eval::Arena::default(),
        );
        for (seconds, frame) in seconds.into_iter().zip(frames) {
            let direct = prepared
                .evaluate(clock.beat_at(f64::from(seconds)).unwrap())
                .unwrap();
            let Some(p::Value::Lighting(values)) = direct.get("heads") else {
                assert!(frame.primitives.is_empty());
                continue;
            };
            assert_eq!(frame.primitives.len(), values.len());
            for (id, value) in values {
                assert_eq!(frame.primitives[id].dimmer, value.dimmer.unwrap() as f32);
                assert_eq!(
                    frame.primitives[id].color,
                    value.color.unwrap_or([1.0; 3]).map(|v| v as f32)
                );
            }
        }
    }
    #[test]
    fn stage_mapping_keeps_downstage_and_height_independent() {
        let cells: Vec<_> = [[0., 10., 0.], [0., 0., 0.], [0., 10., 5.]]
            .into_iter()
            .enumerate()
            .map(|(i, world)| p::Cell {
                id: i.to_string(),
                group: "all".into(),
                world: world.map(f64::from),
                uvz: p::Cell::stage_coordinates(world.map(f64::from)),
            })
            .collect();
        let resolve = |source| {
            p::MappingSpec {
                mirror: None,
                source,
                per_group: false,
                reverse: false,
            }
            .resolve(&cells)
            .unwrap()
            .coordinates
            .iter()
            .map(|c| c.position)
            .collect::<Vec<_>>()
        };
        assert_eq!(resolve(p::MappingSource::V), vec![0., 1., 0.]);
        assert_eq!(resolve(p::MappingSource::Z), vec![0., 0., 1.]);
    }

    #[test]
    fn native_pattern_roundtrip_and_per_head_playback() {
        let mut graph = crate::node_graph::lighting::pattern("dissolve_flash").unwrap();
        graph
            .args
            .iter_mut()
            .find(|a| a.id == "travel")
            .unwrap()
            .default_value = serde_json::json!(4.);
        graph
            .args
            .iter_mut()
            .find(|a| a.id == "repeat")
            .unwrap()
            .default_value = serde_json::json!(8.);
        crate::services::graph_documents::canonicalize_graph(&graph).unwrap();
        let semantic = crate::services::graph_documents::semantic_graph_json(&graph).unwrap();
        let layout = crate::services::graph_documents::graph_layout_json(&graph).unwrap();
        let graph = crate::services::graph_documents::graph_from_files(&semantic, &layout).unwrap();
        let args = graph
            .args
            .iter()
            .map(|a| (a.id.clone(), a.default_value.clone()))
            .collect();
        let ids: Vec<_> = (0..24).map(|i| format!("pixel-bar:{i}")).collect();
        let ctx = ResidentContext {
            positions: (0..24).map(|i| [0., 0., i as f32]).collect(),
            beat_grid: Some(BeatGrid {
                beats: (0..20).map(|i| i as f32 / 2.).collect(),
                downbeats: vec![0., 2., 4.],
                bpm: 120.,
                downbeat_offset: 0.,
                beats_per_bar: 4,
            }),
            span: (0., 4.),
            ..Default::default()
        };
        let plan = crate::eval::compile::compile_pattern(&graph, &args, ctx, ids.clone()).unwrap();
        let frames = crate::eval::eval(
            &plan,
            &[0., 1., 1.9, 2.5],
            &mut crate::eval::Arena::default(),
        );
        let lit = |i: usize| {
            ids.iter()
                .filter(|id| {
                    frames[i]
                        .primitives
                        .get(*id)
                        .is_some_and(|v| v.dimmer > 0.01)
                })
                .count()
        };
        assert_eq!(lit(0), 24);
        assert!(lit(1) > 0 && lit(1) < 24);
        assert!(lit(2) < lit(1));
        assert_eq!(lit(3), 0);
    }
    #[test]
    fn attribute_only_graphs_preserve_lower_color_in_the_normal_compositor() {
        let ids: Vec<_> = (0..8).map(|n| format!("bar:{n}")).collect();
        for (effect, expected) in [
            ("write_position", [false, false, true, false, false]),
            ("write_dimmer", [false, true, false, false, false]),
            ("write_strobe", [false, false, false, true, false]),
            ("write_speed", [false, false, false, false, true]),
        ] {
            let graph = crate::node_graph::lighting::pattern(effect).unwrap();
            let mut args: HashMap<_, _> = graph
                .args
                .iter()
                .map(|arg| (arg.id.clone(), arg.default_value.clone()))
                .collect();
            args.insert("value".into(), serde_json::json!(0.0));
            let context = ResidentContext {
                positions: vec![[0.0; 3]; ids.len()],
                beat_grid: Some(BeatGrid {
                    beats: vec![0., 0.5, 1., 1.5, 2.],
                    downbeats: vec![0., 2.],
                    bpm: 120.,
                    downbeat_offset: 0.,
                    beats_per_bar: 4,
                }),
                span: (0., 2.),
                ..Default::default()
            };
            let plan =
                crate::eval::compile::compile_pattern(&graph, &args, context, ids.clone()).unwrap();
            assert_eq!(
                [
                    plan.outputs.color,
                    plan.outputs.dimmer,
                    plan.outputs.position,
                    plan.outputs.strobe,
                    plan.outputs.speed
                ],
                expected
            );
            let top =
                crate::eval::eval(&plan, &[0.5], &mut crate::eval::Arena::default()).remove(0);
            let mut base = crate::models::universe::UniverseState {
                primitives: ids
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            crate::models::universe::PrimitiveState {
                                color: [0.2, 0.4, 1.0],
                                dimmer: 0.7,
                                position: [45., 30.],
                                strobe: 0.5,
                                speed: 1.,
                            },
                        )
                    })
                    .collect(),
            };
            crate::eval::composite::composite_frame(
                &mut base,
                &top,
                &plan.outputs,
                crate::eval::BlendMode::Replace,
                1.0,
                None,
            );
            for value in base.primitives.values() {
                assert_eq!(value.color, [0.2, 0.4, 1.0]);
                assert_eq!(
                    value.dimmer,
                    if effect == "write_dimmer" { 0.0 } else { 0.7 }
                );
                assert_eq!(
                    value.position,
                    if effect == "write_position" {
                        [0.; 2]
                    } else {
                        [45., 30.]
                    }
                );
                assert_eq!(
                    value.strobe,
                    if effect == "write_strobe" { 0.0 } else { 0.5 }
                );
                assert_eq!(value.speed, if effect == "write_speed" { 0.0 } else { 1.0 });
            }
        }
    }
}
