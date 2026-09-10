//! Saved graph import followed by the single canonical tensor compiler.
use crate::eval::{Plan, ResidentContext};
use crate::models::node_graph::Graph;
#[cfg(test)]
use crate::models::node_graph::{Edge, NodeInstance};
use luma_patterns as p;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
#[derive(Debug)]
pub enum CompileError {
    Graph(String),
}
pub fn compile_pattern(
    graph: &Graph,
    args: &HashMap<String, Value>,
    ctx: ResidentContext,
    primitive_ids: Vec<String>,
) -> Result<Plan, CompileError> {
    build(graph, args, ctx, primitive_ids).map_err(CompileError::Graph)
}
fn build(
    graph: &Graph,
    args: &HashMap<String, Value>,
    ctx: ResidentContext,
    ids: Vec<String>,
) -> Result<Plan, String> {
    if graph.nodes.is_empty() {
        return Ok(Plan {
            program: None,
            primitive_ids: ids,
            outputs: Default::default(),
            ctx,
            views: vec![],
        });
    }
    let mut source = graph.clone();
    for arg in &mut source.args {
        if let Some(value) = args.get(&arg.id) {
            arg.default_value = value.clone();
        }
    }
    let score = crate::node_graph::migration::pattern(&source, "Pattern")?
        .ok_or("graph contains a node without a canonical migration")?;
    let library = score
        .library(&p::standard_library())
        .map_err(|e| e.to_string())?;
    let clock = if let Some(grid) = &ctx.beat_grid {
        if grid.beats.len() >= 2 {
            grid.timeline()
        } else {
            p::BeatTimeline::new(vec![0., 0.5], 0.)
        }
    } else {
        p::BeatTimeline::new(vec![0., 0.5], 0.)
    }
    .map_err(|e| e.to_string())?;
    let start = clock
        .beat_at(f64::from(ctx.span.0))
        .map_err(|e| e.to_string())?;
    let duration = clock
        .beat_at(f64::from(ctx.span.1))
        .map_err(|e| e.to_string())?
        - start;
    let cells: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let world = ctx
                .positions
                .get(i)
                .copied()
                .unwrap_or([0.; 3])
                .map(f64::from);
            p::Cell {
                id: id.clone(),
                group: "selection".into(),
                world,
                uvz: p::Cell::stage_coordinates(world),
            }
        })
        .collect();
    let prepared = p::PreparedGraph::new(
        &library,
        crate::node_graph::migration::ROOT,
        &BTreeMap::new(),
        p::Frame {
            cells: &cells,
            features: None,
            beat: start,
            clip_start: start,
            clip_duration: duration,
            seed: ctx.seed,
        },
    )
    .map_err(|e| e.to_string())?;
    let features = super::track_features::TrackFeatures::from_resident(
        &ctx,
        clock.clone(),
        prepared.feature_requests(),
    )?;
    let prepared = prepared
        .with_features(features)
        .map_err(|e| e.to_string())?;
    super::lighting::plan(prepared, clock, ids, "lighting", ctx)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{eval, Arena};

    fn node(id: &str, type_id: &str, params: &[(&str, Value)]) -> NodeInstance {
        NodeInstance {
            id: id.into(),
            type_id: type_id.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            position_x: None,
            position_y: None,
        }
    }
    fn edge(from: &str, fp: &str, to: &str, tp: &str) -> Edge {
        Edge {
            id: format!("{from}:{fp}->{to}:{tp}"),
            from_node: from.into(),
            from_port: fp.into(),
            to_node: to.into(),
            to_port: tp.into(),
        }
    }

    /// First E2E proof: compile `gradient` and match its golden frame-for-frame.
    /// Portable (reads only the committed fixture).
    #[test]
    fn gradient_matches_golden() {
        let path = format!(
            "{}/tests/golden/fixtures/gradient.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let golden: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("golden")).expect("json");

        let primitive_ids: Vec<String> = golden["frames"][0]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["primitive_id"].as_str().unwrap().to_string())
            .collect();
        let times: Vec<f32> = golden["sample_times"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_f64().unwrap() as f32)
            .collect();
        let span = (
            golden["start_time"].as_f64().unwrap() as f32,
            golden["end_time"].as_f64().unwrap() as f32,
        );
        let mut args = HashMap::new();
        args.insert(
            "gradient".to_string(),
            golden["arg_values"]["gradient"].clone(),
        );

        let nodes = vec![
            node("pattern_args", "pattern_args", &[]),
            node("s0", "scalar", &[("value", Value::from(0.0))]),
            node("s1", "scalar", &[("value", Value::from(1.0))]),
            node("ramp", "ramp_between", &[]),
            node("sp", "sample_palette", &[]),
            node("apply", "apply_color", &[]),
            node("view", "view_signal", &[]),
        ];
        let edges = vec![
            edge("s0", "out", "ramp", "start"),
            edge("s1", "out", "ramp", "end"),
            edge("ramp", "out", "sp", "u"),
            edge("pattern_args", "gradient", "sp", "stops"),
            edge("sp", "out", "apply", "signal"),
            edge("sp", "out", "view", "in"),
            edge("pattern_args", "selection", "apply", "selection"),
        ];

        let ctx = ResidentContext {
            span,
            ..Default::default()
        };
        let graph = Graph {
            nodes,
            edges,
            args: vec![
                crate::models::node_graph::PatternArgDef {
                    id: "gradient".into(),
                    name: "Gradient".into(),
                    arg_type: crate::models::node_graph::PatternArgType::Gradient,
                    default_value: args["gradient"].clone(),
                },
                crate::models::node_graph::PatternArgDef {
                    id: "selection".into(),
                    name: "Selection".into(),
                    arg_type: crate::models::node_graph::PatternArgType::Selection,
                    default_value: crate::models::selection::Selection::all().to_value(),
                },
            ],
        };
        let plan = compile_pattern(&graph, &args, ctx, primitive_ids).unwrap();
        let mut arena = Arena::default();

        // The wired `view_signal` node surfaces the sampled palette as a preview
        // tap (the graph editor's viewer data path).
        let views = crate::eval::eval_views(&plan, &times, &mut arena).unwrap();
        let sig = views.get("view").expect("view_signal tap missing");
        assert_eq!(sig.t, times.len());
        assert_eq!(sig.data.len(), sig.n * sig.t * sig.c);
        assert!(sig.data.iter().all(|v| v.is_finite()));

        let frames = eval(&plan, &times, &mut arena);

        const TOL: f32 = 2.0e-2;
        for (fi, gframe) in golden["frames"].as_array().unwrap().iter().enumerate() {
            for gp in gframe["primitives"].as_array().unwrap() {
                let id = gp["primitive_id"].as_str().unwrap();
                let got = &frames[fi].primitives[id];
                let gd = gp["dimmer"].as_f64().unwrap() as f32;
                assert!(
                    (got.dimmer - gd).abs() < TOL,
                    "frame {fi} {id} dimmer {} vs {}",
                    got.dimmer,
                    gd
                );
                let gc = gp["color"].as_array().unwrap();
                for ch in 0..3 {
                    let c = gc[ch].as_f64().unwrap() as f32;
                    assert!(
                        (got.color[ch] - c).abs() < TOL,
                        "frame {fi} {id} ch{ch} {} vs {}",
                        got.color[ch],
                        c
                    );
                }
            }
        }
    }

    /// Lower `definition` alone, with `param` forced to `value` and every other
    /// param at its default. Signal inputs are fed a scalar so a node that
    /// requires one still reaches its param handling.
    fn lower_alone(
        definition: &crate::models::node_graph::NodeTypeDef,
        param: &str,
        value: &str,
    ) -> Result<(), CompileError> {
        use crate::models::node_graph::{ParamType, PortType};

        let params: Vec<(&str, Value)> = definition
            .params
            .iter()
            .map(|p| {
                let v = if p.id == param {
                    Value::from(value)
                } else {
                    match &p.param_type {
                        ParamType::Number => Value::from(p.default_number.unwrap_or(0.0)),
                        ParamType::Text | ParamType::Enum { .. } => {
                            Value::from(p.default_text.clone().unwrap_or_default())
                        }
                    }
                };
                (p.id.as_str(), v)
            })
            .collect();

        let mut nodes = vec![node("subject", &definition.id, &params)];
        let mut edges = Vec::new();
        for (i, port) in definition.inputs.iter().enumerate() {
            if !matches!(port.port_type, PortType::Signal) {
                continue;
            }
            let feed = format!("feed{i}");
            nodes.push(node(&feed, "scalar", &[("value", Value::from(1.0))]));
            edges.push(edge(&feed, "out", "subject", &port.id));
        }

        let ctx = ResidentContext {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0]],
            span: (0.0, 1.0),
            ..Default::default()
        };
        let ids = vec!["p0".to_string(), "p1".to_string()];
        if let Some(output) = definition.outputs.first() {
            nodes.push(node("view", "view_signal", &[]));
            edges.push(edge("subject", &output.id, "view", "in"));
        }
        compile_pattern(
            &Graph {
                nodes,
                edges,
                args: vec![],
            },
            &HashMap::new(),
            ctx,
            ids,
        )
        .map(|_| ())
    }

    /// The picker and the compiler are one list. Every option a node definition
    /// offers must lower, and a string outside the option set must not — so a
    /// new op or attribute cannot ship half-wired in either direction, and a
    /// hand-authored option list added later fails here rather than in a user's
    /// graph.
    #[test]
    fn enum_options_are_exactly_what_lowers() {
        use crate::models::node_graph::ParamType;

        let mut checked = 0;
        for definition in crate::node_graph::nodes::get_node_types() {
            for param in &definition.params {
                let ParamType::Enum { options } = &param.param_type else {
                    continue;
                };
                assert!(
                    !options.is_empty(),
                    "{}.{}: empty option set",
                    definition.id,
                    param.id
                );
                if let Some(id) = definition
                    .id
                    .strip_prefix(crate::node_graph::lighting::PREFIX)
                {
                    let library = luma_patterns::migration::v2_library();
                    let kind = library.definitions[id].inputs[&param.id].value_type;
                    for option in options {
                        crate::node_graph::lighting::decode(kind, &Value::from(option.id.clone()))
                            .unwrap();
                    }
                    assert!(crate::node_graph::lighting::decode(
                        kind,
                        &Value::from("__no_such_option__")
                    )
                    .is_err());
                    checked += 1;
                    continue;
                }
                for option in options {
                    if let Err(e) = lower_alone(&definition, &param.id, &option.id) {
                        panic!(
                            "{}.{} offers '{}' but it does not lower: {e:?}",
                            definition.id, param.id, option.id
                        );
                    }
                }
                assert!(
                    lower_alone(&definition, &param.id, "__no_such_option__").is_err(),
                    "{}.{} accepts a value no picker offers",
                    definition.id,
                    param.id
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no Enum params found — the guard is vacuous");
    }
}
