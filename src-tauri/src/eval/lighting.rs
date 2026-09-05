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
    prepared: p::PreparedPattern,
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
fn build(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    ctx: &ResidentContext,
    ids: &[String],
    low: &mut Lowerer,
) -> Result<(), String> {
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
    let uv = super::ops::spatial::rig_uv(&ctx.positions);
    let cells: Vec<_> = ids
        .iter()
        .zip(&ctx.positions)
        .zip(uv)
        .map(|((id, world), uv)| p::Cell {
            id: id.clone(),
            group: "selection".into(),
            world: world.map(f64::from),
            uvz: [f64::from(uv[0]), f64::from(uv[1]), f64::from(world[2])],
        })
        .collect();
    let prepared = p::PreparedPattern::new(
        &library,
        "__score_pattern",
        &BTreeMap::new(),
        p::Frame {
            cells: &cells,
            beat: start,
            clip_start: start,
            seed: crate::eval::context::seed_for(None, "lighting"),
        },
    )
    .map_err(|e| e.to_string())?;
    let rgb = low.emit(
        OpKind::Lighting(Arc::new(Program {
            prepared,
            clock,
            ids: ids.to_vec(),
        })),
        vec![],
        low.n,
        3,
        Phase::Kernel,
        "lighting",
        "rgb",
    );
    let dimmer = low.emit(
        OpKind::Color(super::ops::color::ColorOp::HsvValue),
        vec![rgb],
        low.n,
        1,
        Phase::Kernel,
        "lighting",
        "dimmer",
    );
    let color = low.emit(
        OpKind::Color(super::ops::color::ColorOp::HsvNormalize),
        vec![rgb],
        low.n,
        3,
        Phase::Kernel,
        "lighting",
        "color",
    );
    low.outputs.dimmer = Some(dimmer);
    low.outputs.color = Some(color);
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
                            if let Some(rgb) = values.get(id) {
                                for (ch, v) in rgb.iter().enumerate() {
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
}
