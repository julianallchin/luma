//! Canonical graph preparation and fixture/diagnostic output adapters.
use super::{Arena, OutputBinding, Plan};
use crate::models::universe::{HeadAim, PrimitiveState, UniverseState};
use luma_patterns as p;
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
fn plan(
    prepared: p::PreparedGraph,
    clock: p::BeatTimeline,
    ids: Vec<String>,
    output: &str,
    span: (f32, f32),
) -> Result<Plan, String> {
    let program = Program {
        prepared,
        clock,
        ids: ids.clone(),
        output: output.into(),
    };
    let initial = program.sample(&[span.0])?;
    let writes = initial
        .get(output)
        .and_then(p::EvaluatedValue::lighting)
        .ok_or("graph did not produce fixture output")?
        .writes();
    Ok(Plan {
        program: Some(Arc::new(program)),
        primitive_ids: ids,
        outputs: OutputBinding {
            color: writes[0],
            dimmer: writes[1],
            position: writes[2],
            strobe: writes[3],
            speed: writes[4],
            aim: writes[5],
        },
        span,
    })
}
/// A clip's plan over `cells`. Every clip graph writes its lighting to the
/// one output terminal.
pub(crate) fn compile_clip(
    clip: &p::Clip,
    clock: p::BeatTimeline,
    cells: Vec<p::Cell>,
    prepared: p::PreparedGraph,
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
    plan(prepared, clock, ids, p::clip_graph::OUTPUT, span)
}
impl Program {
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

    /// Each head's [`p::Turn`] at each of `times`, one map per time, for an
    /// aim clip that blends with Offset.
    pub(crate) fn turns(
        &self,
        times: &[f32],
        scratch: &mut Arena,
    ) -> Result<Vec<BTreeMap<String, p::Turn>>, String> {
        if times.is_empty() {
            return Ok(vec![]);
        }
        scratch.values = self.sample(times)?;
        let value = scratch
            .values
            .get(p::aim::TURN_OUTPUT)
            .ok_or("an Offset clip needs an aim graph")?;
        let turns = p::Turn::read(value, &self.ids, times.len()).map_err(|e| e.to_string())?;
        Ok(turns
            .into_iter()
            .map(|row| self.ids.iter().cloned().zip(row).collect())
            .collect())
    }

    /// The clip's frames at `times`, each with the clip's opacity per head.
    pub(crate) fn layers(
        &self,
        times: &[f32],
        bindings: &OutputBinding,
        scratch: &mut Arena,
    ) -> Result<Vec<Layer>, String> {
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
                // Channel 11 is the aim's weight for aim and the clip's
                // opacity for color and strobe.
                let alpha = if bindings.aim {
                    HashMap::new()
                } else {
                    self.ids
                        .iter()
                        .zip(&rows)
                        .map(|(id, &row)| (id.clone(), tensor[[row, t, 11]] as f32))
                        .collect()
                };
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
                                aim: bindings.aim.then(|| HeadAim {
                                    direction: [v(8), v(9), v(10)],
                                    weight: v(11),
                                }),
                            },
                        )
                    })
                    .collect();
                Layer {
                    frame: UniverseState { primitives },
                    alpha,
                }
            })
            .collect())
    }
}

/// One clip's frame and its opacity per head: the compositor mixes the
/// clip's light and strobe with what is under it by it. A head missing
/// from `alpha` is opaque.
#[derive(Clone, Debug)]
pub struct Layer {
    pub frame: UniverseState,
    pub alpha: HashMap<String, f32>,
}

impl Layer {
    /// The frame over no light: each head's light and strobe scaled by its
    /// opacity.
    pub fn over_nothing(mut self) -> UniverseState {
        for (id, head) in &mut self.frame.primitives {
            let alpha = self.alpha.get(id).copied().unwrap_or(1.0).clamp(0.0, 1.0);
            head.dimmer *= alpha;
            head.strobe *= alpha;
        }
        self.frame
    }
}
/// A clip over every head from `start` for `duration` beats whose graph is
/// `nodes`, in the stored JSON form.
#[cfg(test)]
pub(crate) fn test_clip(nodes: serde_json::Value, start: f64, duration: f64) -> p::Clip {
    serde_json::from_value(serde_json::json!({
        "name": "Test",
        "start": start,
        "duration": duration,
        "seed": 0,
        "selection": p::Selection::all(),
        "z_index": 0,
        "blend_mode": "replace",
        "graph": {"version": 3, "nodes": nodes},
    }))
    .unwrap_or_else(|error| panic!("a test clip: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prepare(clip: &p::Clip, cells: &[p::Cell]) -> p::PreparedGraph {
        p::PreparedGraph::new(
            &p::standard_library(),
            &clip.graph,
            p::Frame {
                features: None,
                cells,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )
        .unwrap()
    }

    #[test]
    fn dynamic_graph_errors_propagate_without_poisoning_later_seeks() {
        // Noise whose speed drops to almost nothing past the clip's middle:
        // its turns then leave the range the noise accepts.
        let clip = test_clip(
            json!({
                "time1": {"kind": "time"},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"},
                                      "shape": {"points": [[0, 0, "hold"], [0.5, 1], [1, 1]]},
                                      "low": 4, "high": 1e-13}},
                "noise1": {"kind": "noise", "inputs": {"speed": {"node": "curve1"}}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "noise1"}, "low": 1, "high": 1}},
                "color1": {"kind": "color",
                           "inputs": {"brightness": 0.5, "alpha": {"node": "curve2"}}},
            }),
            0.,
            4.,
        );
        let cells = vec![p::Cell {
            id: "head".into(),
            group: "wash".into(),
            world: [0.; 3],
            uvz: [0.; 3],
        }];
        let program = prepare(&clip, &cells);
        let clock = p::BeatTimeline::new(vec![0., 0.5, 1., 1.5, 2.], 0.).unwrap();
        let plan = compile_clip(&clip, clock, cells, program).unwrap();
        let scene = crate::eval::Scene::new(vec![crate::eval::CompiledAnnotation {
            span: plan.span,
            plan: Arc::new(plan),
            z_index: 0,
            blend_mode: p::BlendMode::Replace,
        }]);
        let mut scratch = crate::eval::Arena::default();
        let scope = crate::eval::Scope::Composite;
        let before = scene.try_render(&[0.5], scope, &mut scratch).unwrap();
        assert!(scene.try_render(&[0.5, 1.5], scope, &mut scratch).is_err());
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
    fn a_clip_compiles_to_the_batched_core_output() {
        // Dissolve: a shuffled order that goes dark over the clip.
        let mut clip = test_clip(
            json!({
                "shuffle1": {"kind": "shuffle"},
                "space1": {"kind": "space", "settings": {"kind": "order", "wrap": "no"},
                           "inputs": {"heads": {"node": "shuffle1"}}},
                "curve1": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "space1"},
                                      "shape": {"points": [[0, 1], [1, 0]]}}},
                "time1": {"kind": "time", "inputs": {"delay": {"node": "curve1"}}},
                "curve2": {"kind": "curve", "settings": {"kind": "number"},
                           "inputs": {"x": {"node": "time1"},
                                      "shape": {"points": [[0, 1], [0, 0], [1, 0]]}}},
                "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve2"}}},
            }),
            1.,
            3.,
        );
        clip.seed = 129;
        let cells: Vec<_> = (0..24)
            .map(|i| p::Cell {
                id: format!("bar:{i}"),
                group: "bars".into(),
                world: [0.0, 0.0, i as f64],
                uvz: [0.0, 0.0, i as f64],
            })
            .collect();
        let clock = p::BeatTimeline::new(vec![0.0, 0.5, 1.0, 2.0, 3.0, 4.0], 0.0).unwrap();
        let program = prepare(&clip, &cells);
        let plan = compile_clip(&clip, clock.clone(), cells.clone(), program).unwrap();
        let prepared = prepare(&clip, &cells);
        let scene = crate::eval::Scene::new(vec![crate::eval::CompiledAnnotation {
            span: plan.span,
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
        let mut lit = 0;
        for (seconds, frame) in seconds.into_iter().zip(frames) {
            let beat = clock.beat_at(f64::from(seconds)).unwrap();
            if beat < clip.start || beat >= clip.start + clip.duration {
                assert!(frame.primitives.is_empty(), "outside the clip at {beat}");
                continue;
            }
            let direct = prepared.evaluate(beat).unwrap();
            let Some(p::Value::Lighting(values)) = direct.get(p::clip_graph::OUTPUT) else {
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
                lit += usize::from(frame.primitives[id].dimmer > 0.);
            }
        }
        assert!(lit > 0, "the dissolve lights some heads");
    }
}
