//! The graph edit vocabulary: every mutation of an authored graph — human,
//! keyboard, undo, or the graph agent — is one of a closed set of [`Edit`]s,
//! applied by [`apply`].
//!
//! The rules of what a legal graph is live *here*, behind one function, not in
//! the editors: type compatibility, single-slot input replacement, the
//! insert-between edge cleanup, node id minting, param defaults, and the
//! `pattern_args` protections. The web app wrote several of these in two or
//! three places each and they drifted; this module is the single owner the
//! ports call instead (see `docs/design/graph-editor-interaction.md` §4, §10).

use std::collections::HashMap;

use serde_json::Value;

use super::{
    Edge, Graph, NodeInstance, NodeTypeDef, ParamType, PatternArgDef, PatternArgType, PortDef,
    PortType,
};

/// One mutation of a graph document. Closed on purpose: an editor, an undo
/// stack and an agent that can each only say these six things cannot disagree
/// about what a legal graph is.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Edit {
    /// Create a node of `type_id` at a graph-space position, with its
    /// catalogue defaults filled in and a fresh human-readable id minted.
    AddNode { type_id: String, at: (f64, f64) },
    /// Remove a node and every edge touching it. Refused for `pattern_args`.
    RemoveNode { id: String },
    /// Move a node. Position is presentation, not topology.
    MoveNode { id: String, to: (f64, f64) },
    /// Set one param value on a node.
    SetParam {
        node: String,
        param: String,
        value: Value,
    },
    /// Wire an output to an input. An input is a single slot: connecting over
    /// an occupied one replaces the previous edge, and wiring A→N when N→B
    /// exists removes a now-redundant direct A→B edge (the insert-between
    /// gesture).
    Connect { from: PortRef, to: PortRef },
    /// Remove one edge by id. Edge ids are host-owned — the seam may rewrite
    /// a locally minted one on save — so the id here is whatever the current
    /// document says.
    Disconnect { edge: String },
}

/// One end of a wire: a node and one of its ports. Direction is carried by
/// position — [`Edit::Connect`]'s `from` is always an output, `to` an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRef {
    pub node: String,
    pub port: String,
}

impl PortRef {
    pub fn new(node: impl Into<String>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
        }
    }
}

/// What an applied edit did — the facts a caller cannot recover from the edit
/// it handed in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Changed {
    /// Topology changed (a node or edge appeared or disappeared), as opposed
    /// to a value or a position moving. What a preview runner keys its
    /// structural flag on.
    pub structural: bool,
    /// The id [`apply`] minted: the node of an `AddNode`, the edge of a
    /// `Connect`. `None` for edits that name everything they touch.
    pub minted: Option<String>,
}

impl Changed {
    fn structural(minted: Option<String>) -> Self {
        Self {
            structural: true,
            minted,
        }
    }

    fn value() -> Self {
        Self {
            structural: false,
            minted: None,
        }
    }
}

/// Why an [`Edit`] was refused. The graph is untouched on every error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    #[error("the catalogue has no node type {0:?}")]
    UnknownType(String),
    #[error("the graph has no node {0:?}")]
    UnknownNode(String),
    #[error("node {node:?} has no {} port {port:?}", if *output { "output" } else { "input" })]
    UnknownPort {
        node: String,
        port: String,
        output: bool,
    },
    #[error("cannot connect {from:?} ({from_type:?}) to {to:?} ({to_type:?})")]
    Incompatible {
        from: String,
        to: String,
        from_type: PortType,
        to_type: PortType,
    },
    #[error("the graph has no edge {0:?}")]
    UnknownEdge(String),
    #[error("the pattern_args node is synthetic and cannot be removed")]
    Protected,
}

/// Can a wire carry a signal from an `a`-typed output into a `b`-typed input?
///
/// Exact type equality, as `connection-validation.ts` rules (direction is
/// carried by [`Edit::Connect`]'s field positions, not checked here). One
/// predicate, two readers: [`apply`] uses it to refuse an illegal `Connect`,
/// and an editor uses it to highlight legal drop targets during a wire drag —
/// the same rule in both places by construction.
#[must_use]
pub fn compatible(a: &PortType, b: &PortType) -> bool {
    a == b
}

/// The synthetic `pattern_args` definition: its ports *are* the pattern's
/// argument list. Mirrors `pattern-args-node-def.ts`, including the rule that
/// palettes and gradients both surface as `Stops`. `None` when the pattern has
/// no arguments — the node then has no ports and renders as a bare header.
#[must_use]
pub fn pattern_args_def(args: &[PatternArgDef]) -> Option<NodeTypeDef> {
    if args.is_empty() {
        return None;
    }
    Some(NodeTypeDef {
        id: "pattern_args".to_string(),
        name: "Pattern Args".to_string(),
        description: None,
        category: Some("Input".to_string()),
        inputs: Vec::new(),
        outputs: args
            .iter()
            .map(|arg| PortDef {
                id: arg.id.clone(),
                name: arg.name.clone(),
                port_type: match arg.arg_type {
                    PatternArgType::Selection => PortType::Selection,
                    PatternArgType::Palette | PatternArgType::Gradient => PortType::Stops,
                    PatternArgType::Color | PatternArgType::Scalar => PortType::Signal,
                },
            })
            .collect(),
        params: Vec::new(),
    })
}

/// Apply one [`Edit`] to `graph`, enforcing every rule the module docs list.
/// `types` is the node catalogue keyed by type id; `pattern_args` is resolved
/// against the graph's own argument list, never the catalogue.
///
/// # Errors
///
/// Returns an [`EditError`] and leaves the graph untouched when the edit names
/// something that does not exist, connects incompatible or wrong-direction
/// ports, or removes the protected `pattern_args` node.
pub fn apply(
    graph: &mut Graph,
    types: &HashMap<String, NodeTypeDef>,
    edit: Edit,
) -> Result<Changed, EditError> {
    match edit {
        Edit::AddNode { type_id, at } => {
            // `pattern_args` has no catalogue entry, so it cannot be added —
            // which is the rule: it is synthetic and one per graph.
            let definition = types
                .get(&type_id)
                .ok_or_else(|| EditError::UnknownType(type_id.clone()))?;
            let id = mint_node_id(graph, &type_id);
            graph.nodes.push(NodeInstance {
                id: id.clone(),
                type_id,
                params: default_params(definition),
                position_x: Some(at.0),
                position_y: Some(at.1),
            });
            Ok(Changed::structural(Some(id)))
        }
        Edit::RemoveNode { id } => {
            let node = graph
                .nodes
                .iter()
                .position(|node| node.id == id)
                .ok_or_else(|| EditError::UnknownNode(id.clone()))?;
            if graph.nodes[node].type_id == "pattern_args" {
                return Err(EditError::Protected);
            }
            graph.nodes.remove(node);
            graph
                .edges
                .retain(|edge| edge.from_node != id && edge.to_node != id);
            Ok(Changed::structural(None))
        }
        Edit::MoveNode { id, to } => {
            let node = graph
                .nodes
                .iter_mut()
                .find(|node| node.id == id)
                .ok_or_else(|| EditError::UnknownNode(id.clone()))?;
            node.position_x = Some(to.0);
            node.position_y = Some(to.1);
            Ok(Changed::value())
        }
        Edit::SetParam { node, param, value } => {
            let node = graph
                .nodes
                .iter_mut()
                .find(|instance| instance.id == node)
                .ok_or_else(|| EditError::UnknownNode(node.clone()))?;
            node.params.insert(param, value);
            Ok(Changed::value())
        }
        Edit::Connect { from, to } => {
            let from_type = port_type(graph, types, &from, true)?;
            let to_type = port_type(graph, types, &to, false)?;
            if !compatible(&from_type, &to_type) {
                return Err(EditError::Incompatible {
                    from: format!("{}:{}", from.node, from.port),
                    to: format!("{}:{}", to.node, to.port),
                    from_type,
                    to_type,
                });
            }
            // An input is a scalar slot, not a multi-edge collection —
            // rewiring it replaces the previous source.
            graph
                .edges
                .retain(|edge| edge.to_node != to.node || edge.to_port != to.port);
            let id = mint_edge_id(&from, &to);
            graph.edges.push(Edge {
                id: id.clone(),
                from_node: from.node.clone(),
                from_port: from.port.clone(),
                to_node: to.node.clone(),
                to_port: to.port.clone(),
            });
            remove_direct_edges_if_split(graph, &from.node, &to.node);
            Ok(Changed::structural(Some(id)))
        }
        Edit::Disconnect { edge } => {
            let at = graph
                .edges
                .iter()
                .position(|candidate| candidate.id == edge)
                .ok_or_else(|| EditError::UnknownEdge(edge.clone()))?;
            graph.edges.remove(at);
            Ok(Changed::structural(None))
        }
    }
}

/// The next `{type_id}_{n}` id, against the ids already in the graph — the
/// per-graph reading of `node-builder.ts`'s session counter: `n` is one past
/// the highest suffix any node of this type already carries. Legacy ids that
/// do not follow the scheme are opaque and do not participate.
fn mint_node_id(graph: &Graph, type_id: &str) -> String {
    let high = graph
        .nodes
        .iter()
        .filter_map(|node| {
            node.id
                .strip_prefix(type_id)
                .and_then(|rest| rest.strip_prefix('_'))
                .and_then(|n| n.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0);
    format!("{type_id}_{n}", n = high + 1)
}

/// A locally minted edge id: the canonical `from:port->to:port` fingerprint
/// (`graph-checkpoint.ts` spells edges this way). A placeholder — edge ids are
/// host-owned and the seam may rewrite it on save.
fn mint_edge_id(from: &PortRef, to: &PortRef) -> String {
    format!("{}:{}->{}:{}", from.node, from.port, to.node, to.port)
}

/// Catalogue defaults for a fresh node, exactly as `node-builder.ts` fills
/// them: numbers get `default_number` or 0, text gets `default_text` or the
/// empty string, and enums get nothing until the user picks.
fn default_params(definition: &NodeTypeDef) -> HashMap<String, Value> {
    definition
        .params
        .iter()
        .filter_map(|param| {
            let value = match &param.param_type {
                ParamType::Number => Value::from(f64::from(param.default_number.unwrap_or(0.))),
                ParamType::Text => Value::from(param.default_text.clone().unwrap_or_default()),
                ParamType::Enum { .. } => return None,
            };
            Some((param.id.clone(), value))
        })
        .collect()
}

/// The type of one named port on one node, resolving `pattern_args` against
/// the graph's own argument list and everything else against the catalogue.
fn port_type(
    graph: &Graph,
    types: &HashMap<String, NodeTypeDef>,
    at: &PortRef,
    output: bool,
) -> Result<PortType, EditError> {
    let node = graph
        .nodes
        .iter()
        .find(|node| node.id == at.node)
        .ok_or_else(|| EditError::UnknownNode(at.node.clone()))?;
    let synthetic;
    let definition = if node.type_id == "pattern_args" {
        synthetic = pattern_args_def(&graph.args);
        synthetic.as_ref()
    } else {
        types.get(&node.type_id)
    }
    .ok_or_else(|| EditError::UnknownType(node.type_id.clone()))?;
    let ports = if output {
        &definition.outputs
    } else {
        &definition.inputs
    };
    ports
        .iter()
        .find(|port| port.id == at.port)
        .map(|port| port.port_type.clone())
        .ok_or_else(|| EditError::UnknownPort {
            node: at.node.clone(),
            port: at.port.clone(),
            output,
        })
}

/// The insert-between cleanup, after a connect: wiring A→N when the graph also
/// holds N→B removes any direct A→B edge whose ports match the path through N
/// — and symmetrically for N→B against existing A→N. Mirrors
/// `removeDirectEdgesIfSplit` (`react-flow-editor.tsx`); the handle wildcards
/// there collapse away because every edge here names both ports.
fn remove_direct_edges_if_split(graph: &mut Graph, source: &str, target: &str) {
    let mut doomed: Vec<String> = Vec::new();
    {
        let edges = &graph.edges;
        let mut consider = |from: &str, middle: &str, to: &str| {
            for in_edge in edges
                .iter()
                .filter(|e| e.from_node == from && e.to_node == middle)
            {
                for out_edge in edges
                    .iter()
                    .filter(|e| e.from_node == middle && e.to_node == to)
                {
                    for direct in edges.iter().filter(|e| {
                        e.from_node == from
                            && e.to_node == to
                            && e.from_port == in_edge.from_port
                            && e.to_port == out_edge.to_port
                    }) {
                        doomed.push(direct.id.clone());
                    }
                }
            }
        };
        // The new edge as A→N: does N already feed some B?
        let outgoing: Vec<String> = edges
            .iter()
            .filter(|e| e.from_node == target)
            .map(|e| e.to_node.clone())
            .collect();
        for b in outgoing {
            consider(source, target, &b);
        }
        // The new edge as N→B: does some A already feed N?
        let incoming: Vec<String> = edges
            .iter()
            .filter(|e| e.to_node == source)
            .map(|e| e.from_node.clone())
            .collect();
        for a in incoming {
            consider(&a, source, target);
        }
    }
    if !doomed.is_empty() {
        graph.edges.retain(|edge| !doomed.contains(&edge.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(id: &str, inputs: &[(&str, PortType)], outputs: &[(&str, PortType)]) -> NodeTypeDef {
        let port = |(pid, ty): &(&str, PortType)| PortDef {
            id: (*pid).into(),
            name: (*pid).into(),
            port_type: ty.clone(),
        };
        NodeTypeDef {
            id: id.into(),
            name: id.into(),
            description: None,
            category: None,
            inputs: inputs.iter().map(port).collect(),
            outputs: outputs.iter().map(port).collect(),
            params: vec![
                super::super::ParamDef {
                    id: "gain".into(),
                    name: "Gain".into(),
                    param_type: ParamType::Number,
                    default_number: Some(0.5),
                    default_text: None,
                    range: None,
                },
                super::super::ParamDef {
                    id: "mode".into(),
                    name: "Mode".into(),
                    param_type: ParamType::Enum {
                        options: Vec::new(),
                    },
                    default_number: None,
                    default_text: None,
                    range: None,
                },
            ],
        }
    }

    fn catalogue() -> HashMap<String, NodeTypeDef> {
        [
            def(
                "osc",
                &[("in", PortType::Signal)],
                &[("out", PortType::Signal)],
            ),
            def(
                "paint",
                &[("in", PortType::Signal), ("color", PortType::Color)],
                &[("out", PortType::Color)],
            ),
            def(
                "mix",
                &[("in_a", PortType::Signal), ("in_b", PortType::Signal)],
                &[("out", PortType::Signal)],
            ),
        ]
        .into_iter()
        .map(|d| (d.id.clone(), d))
        .collect()
    }

    fn node(id: &str, type_id: &str) -> NodeInstance {
        NodeInstance {
            id: id.into(),
            type_id: type_id.into(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        }
    }

    fn edge(from: &str, from_port: &str, to: &str, to_port: &str) -> Edge {
        Edge {
            id: format!("{from}:{from_port}->{to}:{to_port}"),
            from_node: from.into(),
            from_port: from_port.into(),
            to_node: to.into(),
            to_port: to_port.into(),
        }
    }

    fn graph(nodes: Vec<NodeInstance>, edges: Vec<Edge>) -> Graph {
        Graph {
            nodes,
            edges,
            args: Vec::new(),
        }
    }

    fn connect(from: (&str, &str), to: (&str, &str)) -> Edit {
        Edit::Connect {
            from: PortRef::new(from.0, from.1),
            to: PortRef::new(to.0, to.1),
        }
    }

    /// An input is a single slot: connecting over an occupied one replaces
    /// the previous edge rather than accumulating a second.
    #[test]
    fn connecting_an_occupied_input_replaces_the_edge() {
        let types = catalogue();
        let mut g = graph(
            vec![
                node("osc_1", "osc"),
                node("osc_2", "osc"),
                node("osc_3", "osc"),
            ],
            vec![edge("osc_1", "out", "osc_3", "in")],
        );
        let changed = apply(&mut g, &types, connect(("osc_2", "out"), ("osc_3", "in"))).unwrap();
        assert!(changed.structural);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from_node, "osc_2");
        assert_eq!(changed.minted.as_deref(), Some(g.edges[0].id.as_str()));
    }

    /// The insert-between gesture end to end: with A→B in place, wiring A→N
    /// and then N→B leaves exactly the A→N→B chain — no direct edge survives.
    #[test]
    fn inserting_a_node_between_two_removes_the_direct_edge() {
        let types = catalogue();
        let mut g = graph(
            vec![
                node("osc_1", "osc"),
                node("osc_2", "osc"),
                node("osc_3", "osc"),
            ],
            vec![edge("osc_1", "out", "osc_3", "in")],
        );
        apply(&mut g, &types, connect(("osc_1", "out"), ("osc_2", "in"))).unwrap();
        apply(&mut g, &types, connect(("osc_2", "out"), ("osc_3", "in"))).unwrap();
        let spelled: Vec<String> = g
            .edges
            .iter()
            .map(|e| format!("{}->{}", e.from_node, e.to_node))
            .collect();
        assert_eq!(spelled, vec!["osc_1->osc_2", "osc_2->osc_3"]);
    }

    /// The split cleanup on its own, apart from the single-slot rule: the A→N
    /// edge lands second and does not touch B's input, so only
    /// `removeDirectEdgesIfSplit` can remove the direct A→B edge. The seed
    /// holds two edges into one slot — a shape `Connect` never produces but a
    /// hand-authored or legacy document can hold, which is exactly the state
    /// the cleanup exists to repair.
    #[test]
    fn splitting_an_edge_removes_the_direct_one() {
        let types = catalogue();
        let mut g = graph(
            vec![
                node("osc_1", "osc"),
                node("osc_2", "osc"),
                node("osc_3", "osc"),
            ],
            vec![
                edge("osc_1", "out", "osc_3", "in"),
                edge("osc_2", "out", "osc_3", "in"),
            ],
        );
        // Connecting osc_1→osc_2 completes an osc_1→osc_2→osc_3 path, so the
        // direct osc_1→osc_3 edge is redundant and removed.
        apply(&mut g, &types, connect(("osc_1", "out"), ("osc_2", "in"))).unwrap();
        let spelled: Vec<String> = g
            .edges
            .iter()
            .map(|e| format!("{}->{}", e.from_node, e.to_node))
            .collect();
        assert_eq!(spelled, vec!["osc_2->osc_3", "osc_1->osc_2"]);
    }

    /// The cleanup matches ports, not just nodes: a direct edge into a
    /// *different* input of B is a parallel signal, not a redundant one, and
    /// survives the split.
    #[test]
    fn a_direct_edge_into_another_slot_survives_the_split() {
        let types = catalogue();
        let mut g = graph(
            vec![
                node("osc_1", "osc"),
                node("osc_2", "osc"),
                node("mix_1", "mix"),
            ],
            vec![
                edge("osc_1", "out", "mix_1", "in_a"),
                edge("osc_1", "out", "osc_2", "in"),
            ],
        );
        apply(&mut g, &types, connect(("osc_2", "out"), ("mix_1", "in_b"))).unwrap();
        assert_eq!(g.edges.len(), 3, "the in_a edge is not on the split path");
    }

    /// Port types must match exactly; a mismatch is refused and the graph is
    /// untouched.
    #[test]
    fn incompatible_types_are_refused() {
        let types = catalogue();
        let mut g = graph(
            vec![node("paint_1", "paint"), node("osc_1", "osc")],
            Vec::new(),
        );
        let err = apply(&mut g, &types, connect(("paint_1", "out"), ("osc_1", "in"))).unwrap_err();
        assert!(matches!(err, EditError::Incompatible { .. }));
        assert!(g.edges.is_empty());

        // Direction is structural: an input cannot be a `from`, an output
        // cannot be a `to`.
        let err = apply(&mut g, &types, connect(("osc_1", "in"), ("osc_1", "out"))).unwrap_err();
        assert!(matches!(err, EditError::UnknownPort { output: true, .. }));
    }

    /// Ids mint as `{type_id}_{n}` one past the highest existing suffix;
    /// legacy ids that don't follow the scheme are opaque.
    #[test]
    fn minted_ids_step_past_existing_ones() {
        let types = catalogue();
        let mut g = graph(
            vec![
                node("osc_2", "osc"),
                node("osc_7", "osc"),
                node("node-3", "osc"),
            ],
            Vec::new(),
        );
        let changed = apply(
            &mut g,
            &types,
            Edit::AddNode {
                type_id: "osc".into(),
                at: (10., 20.),
            },
        )
        .unwrap();
        assert_eq!(changed.minted.as_deref(), Some("osc_8"));
        let added = g.nodes.iter().find(|n| n.id == "osc_8").unwrap();
        assert_eq!(added.position_x, Some(10.));
        assert_eq!(added.position_y, Some(20.));
        // Catalogue defaults: numbers land, enums wait for a pick.
        assert_eq!(added.params.get("gain"), Some(&Value::from(0.5)));
        assert!(!added.params.contains_key("mode"));
    }

    /// `pattern_args` is synthetic and one per graph: it cannot be removed,
    /// and it cannot be added (the catalogue does not carry it).
    #[test]
    fn pattern_args_is_protected() {
        let types = catalogue();
        let mut g = graph(vec![node("pattern_args", "pattern_args")], Vec::new());
        assert_eq!(
            apply(
                &mut g,
                &types,
                Edit::RemoveNode {
                    id: "pattern_args".into()
                }
            ),
            Err(EditError::Protected)
        );
        assert_eq!(g.nodes.len(), 1);
        assert!(matches!(
            apply(
                &mut g,
                &types,
                Edit::AddNode {
                    type_id: "pattern_args".into(),
                    at: (0., 0.)
                }
            ),
            Err(EditError::UnknownType(_))
        ));
    }

    /// Removing a node takes every edge touching it along.
    #[test]
    fn removing_a_node_removes_its_edges() {
        let types = catalogue();
        let mut g = graph(
            vec![
                node("osc_1", "osc"),
                node("osc_2", "osc"),
                node("osc_3", "osc"),
            ],
            vec![
                edge("osc_1", "out", "osc_2", "in"),
                edge("osc_2", "out", "osc_3", "in"),
            ],
        );
        apply(&mut g, &types, Edit::RemoveNode { id: "osc_2".into() }).unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert!(g.edges.is_empty());
    }

    /// Connecting out of a `pattern_args` port resolves against the graph's
    /// own argument list, not the catalogue.
    #[test]
    fn pattern_args_ports_come_from_the_graph() {
        let types = catalogue();
        let mut g = graph(
            vec![node("pattern_args", "pattern_args"), node("osc_1", "osc")],
            Vec::new(),
        );
        g.args.push(PatternArgDef {
            id: "energy".into(),
            name: "Energy".into(),
            arg_type: PatternArgType::Scalar,
            default_value: Value::Null,
        });
        apply(
            &mut g,
            &types,
            connect(("pattern_args", "energy"), ("osc_1", "in")),
        )
        .unwrap();
        assert_eq!(g.edges.len(), 1);
    }
}
