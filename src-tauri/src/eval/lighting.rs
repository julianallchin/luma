//! Typed lighting graphs in the normal score renderer. Compilation freezes
//! mapping once; playback, scrubbing and heatmaps evaluate the same program.
use super::{
    compile::{CompileError, Lowerer},
    ops::KernelCtx,
    OpKind, Phase, ResidentContext,
};
use crate::{
    models::node_graph::{Edge, NodeInstance},
    node_graph::lighting::{decode, PREFIX},
};
use luma_patterns::{self as p, Binding, Body, Definition, Output, Rate, ValueType};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct Program {
    prepared: p::PreparedGraph,
    clock: p::BeatTimeline,
    ids: Vec<String>,
}

pub fn lower(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    ctx: &ResidentContext,
    ids: &[String],
    low: &mut Lowerer,
) -> Result<(), CompileError> {
    build(nodes, edges, args, ctx, ids, low).map_err(CompileError::Graph)
}
/// The authoring check and playback lower exactly the same typed program.
pub(crate) fn library_for_graph(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
) -> Result<p::Library, String> {
    let mut library = p::standard_library();
    let mut graph = p::Graph {
        nodes: BTreeMap::new(),
        outputs: BTreeMap::new(),
    };
    let mut output = None;
    for node in nodes {
        if node.type_id == "pattern_args" {
            continue;
        }
        let name = node.type_id.strip_prefix(PREFIX).ok_or_else(|| {
            format!(
                "{} is a legacy node; use typed lighting components in this Pattern",
                node.type_id
            )
        })?;
        let def = library
            .definitions
            .get(name)
            .ok_or_else(|| format!("Unknown lighting node {name}"))?;
        for key in node.params.keys() {
            if !def.inputs.contains_key(key) {
                return Err(format!("Unknown input {}.{key}", node.id));
            }
        }
        let mut inputs = BTreeMap::new();
        for (id, input) in &def.inputs {
            let feeding: Vec<_> = edges
                .iter()
                .filter(|edge| edge.to_node == node.id && edge.to_port == *id)
                .collect();
            if feeding.len() > 1 {
                return Err(format!("{} has more than one connection to {id}", node.id));
            }
            let binding = if let Some(edge) = feeding.first() {
                let source = nodes
                    .iter()
                    .find(|node| node.id == edge.from_node)
                    .ok_or("Connection source is missing")?;
                if source.type_id == "pattern_args" {
                    let value = args
                        .get(&edge.from_port)
                        .ok_or_else(|| format!("Missing exposed input {}", edge.from_port))?;
                    Binding::Value {
                        value: decode(input.value_type, value)?,
                    }
                } else {
                    Binding::Connection {
                        node: edge.from_node.clone(),
                        output: edge.from_port.clone(),
                    }
                }
            } else if let Some(value) = node.params.get(id) {
                Binding::Value {
                    value: decode(input.value_type, value)?,
                }
            } else {
                continue;
            };
            inputs.insert(id.clone(), binding);
        }
        if let Some(port) = def.lighting_output() {
            if !edges
                .iter()
                .any(|edge| edge.from_node == node.id && edge.from_port == port)
            {
                if output.is_some() {
                    return Err("A Pattern needs one unconnected Lighting output; combine the outputs with Add Lighting".into());
                }
                output = Some((node.id.clone(), port.to_string()));
            }
        }
        graph.nodes.insert(
            node.id.clone(),
            p::Node {
                definition: name.into(),
                inputs,
            },
        );
    }
    let (node, port) = output.ok_or("A Pattern needs one Lighting output")?;
    graph.outputs.insert(
        "lighting".into(),
        Binding::Connection { node, output: port },
    );
    library.definitions.insert(
        "__score_pattern".into(),
        Definition {
            name: "Score Pattern".into(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([(
                "lighting".into(),
                Output {
                    value_type: ValueType::Lighting,
                    rate: Rate::Frame,
                },
            )]),
            body: Body::Graph(graph),
        },
    );
    library
        .validate("__score_pattern")
        .map_err(|e| e.to_string())?;
    Ok(library)
}

fn build(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    ctx: &ResidentContext,
    ids: &[String],
    low: &mut Lowerer,
) -> Result<(), String> {
    let library = library_for_graph(nodes, edges, args)?;
    let grid = ctx
        .beat_grid
        .as_ref()
        .ok_or("Lighting patterns require an analyzed beat grid")?;
    let clock = p::BeatTimeline::new(
        grid.beats.iter().map(|t| f64::from(*t)).collect(),
        f64::from(
            grid.downbeats
                .first()
                .copied()
                .unwrap_or(grid.downbeat_offset),
        ),
    )
    .map_err(|e| e.to_string())?;
    let start = clock
        .beat_at(f64::from(ctx.span.0))
        .map_err(|e| e.to_string())?;
    let cells: Vec<_> = ids
        .iter()
        .zip(&ctx.positions)
        .map(|(id, world)| p::Cell {
            id: id.clone(),
            group: "selection".into(),
            world: world.map(f64::from),
            uvz: p::Cell::stage_coordinates(world.map(f64::from)),
        })
        .collect();
    let prepared = p::PreparedGraph::new(
        &library,
        "__score_pattern",
        &BTreeMap::new(),
        p::Frame {
            cells: &cells,
            beat: start,
            clip_start: start,
            seed: ctx.seed,
        },
    )
    .map_err(|e| e.to_string())?;
    let initial = prepared.evaluate(start).map_err(|e| e.to_string())?;
    let writes = match initial.get("lighting") {
        Some(p::Value::Lighting(values)) => values
            .values()
            .next()
            .map(|v| v.writes())
            .unwrap_or([false; 5]),
        _ => return Err("graph did not produce fixture output".into()),
    };
    let packed = low.emit(
        OpKind::Lighting(Arc::new(Program {
            prepared,
            clock,
            ids: ids.to_vec(),
        })),
        vec![],
        low.n,
        8,
        Phase::Kernel,
        "lighting",
        "capabilities",
    );
    for (index, (name, start, width)) in [
        ("color", 0, 3),
        ("dimmer", 3, 1),
        ("position", 4, 2),
        ("strobe", 6, 1),
        ("speed", 7, 1),
    ]
    .into_iter()
    .enumerate()
    {
        if !writes[index] {
            continue;
        }
        let slot = low.emit(
            OpKind::SelectApply(super::ops::select_apply::SelectApplyOp::Channels { start }),
            vec![packed],
            low.n,
            width,
            Phase::Kernel,
            "lighting",
            name,
        );
        match index {
            0 => low.outputs.color = Some(slot),
            1 => low.outputs.dimmer = Some(slot),
            2 => low.outputs.position = Some(slot),
            3 => low.outputs.strobe = Some(slot),
            4 => low.outputs.speed = Some(slot),
            _ => unreachable!(),
        }
    }
    Ok(())
}
impl Program {
    pub fn run(&self, ctx: &KernelCtx) -> Vec<f32> {
        let mut out = ctx.out_buf();
        for (k, t) in ctx.times.iter().enumerate() {
            let frame = self
                .clock
                .beat_at(f64::from(*t))
                .and_then(|beat| self.prepared.evaluate(beat));
            match frame {
                Ok(frame) => {
                    if let Some(p::Value::Lighting(values)) = frame.get("lighting") {
                        for (i, id) in self.ids.iter().enumerate() {
                            if let Some(value) = values.get(id) {
                                let color = value.color.unwrap_or([1.0; 3]);
                                let position = value.position.unwrap_or([0.0; 2]);
                                let packed = [
                                    color[0],
                                    color[1],
                                    color[2],
                                    value.dimmer.unwrap_or(0.0).clamp(0.0, 1.0),
                                    position[0],
                                    position[1],
                                    value.strobe.unwrap_or(0.0),
                                    value.speed.unwrap_or(1.0),
                                ];
                                for (ch, v) in packed.iter().enumerate() {
                                    out[ctx.out_idx(i, k, ch)] = *v as f32;
                                }
                            }
                        }
                    }
                }
                Err(error) => log::error!("Lighting frame at {t}: {error}"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::BeatGrid;
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
        let plan = crate::eval::compile::compile_pattern(
            &graph.nodes,
            &graph.edges,
            &args,
            ctx,
            ids.clone(),
        )
        .unwrap();
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
            let plan = crate::eval::compile::compile_pattern(
                &graph.nodes,
                &graph.edges,
                &args,
                context,
                ids.clone(),
            )
            .unwrap();
            assert_eq!(
                [
                    plan.outputs.color.is_some(),
                    plan.outputs.dimmer.is_some(),
                    plan.outputs.position.is_some(),
                    plan.outputs.strobe.is_some(),
                    plan.outputs.speed.is_some()
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
