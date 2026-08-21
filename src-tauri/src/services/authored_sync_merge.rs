//! Total, deterministic convergence merges for cross-device authored state.
//!
//! Agent and subagent merges use `authored_merge` and report typed conflicts.
//! Device synchronization has a different contract: proposals are processed in
//! server order, so `proposal` is always the later writer. Independent changes
//! are combined structurally and an overlapping field resolves to `proposal`.
//! The composed value is then validated as a whole. If composition introduces
//! an invalid value (including a graph cycle), the complete proposal is the
//! terminal fallback. If the proposal itself is invalid, the current value is
//! retained and the proposal can be quarantined as an integrated no-op.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::models::node_graph::{Edge, Graph, NodeInstance, PatternArgDef};
use crate::services::graph_documents::canonicalize_graph_structure;
use crate::services::track_edits::{
    revision_for_clips, validate_track_draft_envelope, TrackClip, TrackDocument,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMergeResolution {
    /// Stable identities and independent fields from both branches survived.
    Structural,
    /// The structural result was invalid, so the later proposal won wholesale.
    WholeProposalFallback,
    /// The proposal was invalid/unreadable, so integration retains current.
    KeptCurrentFallback,
}

#[derive(Clone, Debug)]
pub struct TotalSyncMerge<T> {
    pub value: T,
    pub resolution: SyncMergeResolution,
}

/// Deterministically merge score semantics. The integration boundary performs
/// database-aware validation (known patterns and duration) before publishing
/// the result.
pub fn merge_track_total(
    base: &TrackDocument,
    current: &TrackDocument,
    proposal: &TrackDocument,
) -> TotalSyncMerge<TrackDocument> {
    if validate_track_draft_envelope(&proposal.clips).is_err() {
        return TotalSyncMerge {
            value: current.clone(),
            resolution: SyncMergeResolution::KeptCurrentFallback,
        };
    }

    let base = track_map(&base.clips);
    let current = track_map(&current.clips);
    let proposal_map = track_map(&proposal.clips);
    let ids: BTreeSet<_> = base
        .keys()
        .chain(current.keys())
        .chain(proposal_map.keys())
        .cloned()
        .collect();
    let mut clips = Vec::with_capacity(ids.len());

    for id in ids {
        let base_value = base.get(&id).and_then(encoded);
        let current_value = current.get(&id).and_then(encoded);
        let proposal_value = proposal_map.get(&id).and_then(encoded);
        if let Some(merged) = merge_entity_value(
            base_value.as_ref(),
            current_value.as_ref(),
            proposal_value.as_ref(),
            merge_track_clip,
        ) {
            match serde_json::from_value::<TrackClip>(merged) {
                Ok(clip) => clips.push(clip),
                Err(_) => {
                    return TotalSyncMerge {
                        value: proposal.clone(),
                        resolution: SyncMergeResolution::WholeProposalFallback,
                    };
                }
            }
        }
    }

    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
    if validate_track_draft_envelope(&clips).is_err() {
        return TotalSyncMerge {
            value: proposal.clone(),
            resolution: SyncMergeResolution::WholeProposalFallback,
        };
    }
    TotalSyncMerge {
        value: TrackDocument {
            revision: revision_for_clips(&clips),
            clips,
        },
        resolution: SyncMergeResolution::Structural,
    }
}

/// Deterministically merge a graph by node, argument, and destination-input
/// identity. Full structural validation is part of this pure boundary, so a
/// cycle assembled from independently acyclic branches takes the complete
/// later proposal as the terminal result.
pub fn merge_graph_total(base: &Graph, current: &Graph, proposal: &Graph) -> TotalSyncMerge<Graph> {
    let Some(base) = graph_value(base) else {
        return whole_graph_fallback(current, proposal);
    };
    let Some(current_value) = graph_value(current) else {
        return whole_graph_fallback(current, proposal);
    };
    let Some(proposal_value) = graph_value(proposal) else {
        return TotalSyncMerge {
            value: current.clone(),
            resolution: SyncMergeResolution::KeptCurrentFallback,
        };
    };
    let candidate =
        merge_graph_value(&base, &current_value, &proposal_value).and_then(graph_from_value);
    if let Some(candidate) = candidate {
        if let Ok(candidate) = canonicalize_graph_structure(&candidate) {
            return TotalSyncMerge {
                value: candidate,
                resolution: SyncMergeResolution::Structural,
            };
        }
    }
    whole_graph_fallback(current, proposal)
}

fn track_map(clips: &[TrackClip]) -> BTreeMap<String, &TrackClip> {
    // Inputs accepted as authored revisions are unique. Keeping the last value
    // here makes this helper total for corrupt input; validation selects the
    // terminal whole-document fallback rather than blocking integration.
    clips.iter().map(|clip| (clip.id.clone(), clip)).collect()
}

fn encoded<T: Serialize>(value: &&T) -> Option<Value> {
    serde_json::to_value(*value).ok()
}

fn equivalent(left: Option<&Value>, right: Option<&Value>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => crate::canonical_json::equivalent(left, right),
        _ => false,
    }
}

/// Three-way presence merge where the proposal is the server-ordered later
/// writer. The caller chooses the meaningful structural boundary for a
/// concurrent modify/modify; arbitrary JSON objects are never recursively
/// spliced by accident.
fn merge_entity_value(
    base: Option<&Value>,
    current: Option<&Value>,
    proposal: Option<&Value>,
    merge_modified: fn(&Value, &Value, &Value) -> Option<Value>,
) -> Option<Value> {
    if equivalent(current, proposal) {
        return current.cloned();
    }
    if equivalent(current, base) {
        return proposal.cloned();
    }
    if equivalent(proposal, base) {
        return current.cloned();
    }

    match (base, current, proposal) {
        (Some(base), Some(current), Some(proposal)) => {
            merge_modified(base, current, proposal).or_else(|| Some(proposal.clone()))
        }
        _ => proposal.cloned(),
    }
}

fn merge_atomic_value(
    base: Option<&Value>,
    current: Option<&Value>,
    proposal: Option<&Value>,
) -> Option<Value> {
    if equivalent(current, proposal) {
        current.cloned()
    } else if equivalent(current, base) {
        proposal.cloned()
    } else if equivalent(proposal, base) {
        current.cloned()
    } else {
        proposal.cloned()
    }
}

fn merge_object_fields(
    base: &Value,
    current: &Value,
    proposal: &Value,
    map_fields: &[&str],
) -> Option<Value> {
    let base = base.as_object()?;
    let current = current.as_object()?;
    let proposal = proposal.as_object()?;
    let keys: BTreeSet<_> = base
        .keys()
        .chain(current.keys())
        .chain(proposal.keys())
        .cloned()
        .collect();
    let mut merged = Map::new();
    for key in keys {
        let value = if map_fields.contains(&key.as_str()) {
            merge_atomic_map(base.get(&key), current.get(&key), proposal.get(&key))
        } else {
            merge_atomic_value(base.get(&key), current.get(&key), proposal.get(&key))
        };
        if let Some(value) = value {
            merged.insert(key, value);
        }
    }
    Some(Value::Object(merged))
}

/// Merge a map by stable key while keeping each mapped typed value atomic.
fn merge_atomic_map(
    base: Option<&Value>,
    current: Option<&Value>,
    proposal: Option<&Value>,
) -> Option<Value> {
    if equivalent(current, proposal) {
        return current.cloned();
    }
    if equivalent(current, base) {
        return proposal.cloned();
    }
    if equivalent(proposal, base) {
        return current.cloned();
    }
    let base = base.and_then(Value::as_object);
    let (Some(current), Some(proposal)) = (
        current.and_then(Value::as_object),
        proposal.and_then(Value::as_object),
    ) else {
        return proposal.cloned();
    };
    let keys: BTreeSet<_> = base
        .into_iter()
        .flat_map(|value| value.keys())
        .chain(current.keys())
        .chain(proposal.keys())
        .cloned()
        .collect();
    let mut merged = Map::new();
    for key in keys {
        if let Some(value) = merge_atomic_value(
            base.and_then(|base| base.get(&key)),
            current.get(&key),
            proposal.get(&key),
        ) {
            merged.insert(key, value);
        }
    }
    Some(Value::Object(merged))
}

fn merge_track_clip(base: &Value, current: &Value, proposal: &Value) -> Option<Value> {
    // `args` is a stable argument-id map. Each typed argument payload is one
    // value: Selection/Color objects and arrays must not become hybrids.
    merge_object_fields(base, current, proposal, &["args"])
}

fn merge_graph_value(base: &Value, current: &Value, proposal: &Value) -> Option<Value> {
    let base = base.as_object()?;
    let current = current.as_object()?;
    let proposal = proposal.as_object()?;
    let nodes = merge_entity_map(
        base.get("nodes")?,
        current.get("nodes")?,
        proposal.get("nodes")?,
        merge_graph_node,
    )?;
    let args = merge_entity_map(
        base.get("args")?,
        current.get("args")?,
        proposal.get("args")?,
        merge_graph_argument,
    )?;
    let edges = merge_entity_map(
        base.get("edges")?,
        current.get("edges")?,
        proposal.get("edges")?,
        merge_whole_entity,
    )?;
    Some(Value::Object(Map::from_iter([
        ("nodes".into(), nodes),
        ("edges".into(), edges),
        ("args".into(), args),
    ])))
}

fn merge_entity_map(
    base: &Value,
    current: &Value,
    proposal: &Value,
    merge_modified: fn(&Value, &Value, &Value) -> Option<Value>,
) -> Option<Value> {
    let base = base.as_object()?;
    let current = current.as_object()?;
    let proposal = proposal.as_object()?;
    let keys: BTreeSet<_> = base
        .keys()
        .chain(current.keys())
        .chain(proposal.keys())
        .cloned()
        .collect();
    let mut merged = Map::new();
    for key in keys {
        if let Some(value) = merge_entity_value(
            base.get(&key),
            current.get(&key),
            proposal.get(&key),
            merge_modified,
        ) {
            merged.insert(key, value);
        }
    }
    Some(Value::Object(merged))
}

fn merge_graph_node(base: &Value, current: &Value, proposal: &Value) -> Option<Value> {
    // Node parameters merge by stable parameter key; each parameter payload
    // remains atomic for the same reason score arguments do.
    merge_object_fields(base, current, proposal, &["params"])
}

fn merge_graph_argument(base: &Value, current: &Value, proposal: &Value) -> Option<Value> {
    merge_object_fields(base, current, proposal, &[])
}

fn merge_whole_entity(_base: &Value, _current: &Value, proposal: &Value) -> Option<Value> {
    Some(proposal.clone())
}

fn graph_value(graph: &Graph) -> Option<Value> {
    let mut nodes = Map::new();
    for node in &graph.nodes {
        if nodes
            .insert(node.id.clone(), serde_json::to_value(node).ok()?)
            .is_some()
        {
            return None;
        }
    }
    let mut args = Map::new();
    for arg in &graph.args {
        if args
            .insert(arg.id.clone(), serde_json::to_value(arg).ok()?)
            .is_some()
        {
            return None;
        }
    }
    let mut edges = Map::new();
    for edge in &graph.edges {
        let key = edge_key(&edge.to_node, &edge.to_port);
        if edges
            .insert(key, serde_json::to_value(edge).ok()?)
            .is_some()
        {
            return None;
        }
    }
    Some(Value::Object(Map::from_iter([
        ("nodes".into(), Value::Object(nodes)),
        ("edges".into(), Value::Object(edges)),
        ("args".into(), Value::Object(args)),
    ])))
}

fn graph_from_value(value: Value) -> Option<Graph> {
    let mut value = value.as_object()?.clone();
    let mut nodes: Vec<NodeInstance> = value
        .remove("nodes")?
        .as_object()?
        .values()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .ok()?;
    let mut edges: Vec<Edge> = value
        .remove("edges")?
        .as_object()?
        .values()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .ok()?;
    let mut args: Vec<PatternArgDef> = value
        .remove("args")?
        .as_object()?
        .values()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .ok()?;
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    args.sort_by(|left, right| left.id.cmp(&right.id));
    edges.sort_by(|left, right| {
        left.to_node
            .cmp(&right.to_node)
            .then(left.to_port.cmp(&right.to_port))
            .then(left.from_node.cmp(&right.from_node))
            .then(left.from_port.cmp(&right.from_port))
            .then(left.id.cmp(&right.id))
    });
    Some(Graph { nodes, edges, args })
}

fn edge_key(to_node: &str, to_port: &str) -> String {
    format!("{}:{to_node}{to_port}", to_node.len())
}

fn whole_graph_fallback(current: &Graph, proposal: &Graph) -> TotalSyncMerge<Graph> {
    if let Ok(proposal) = canonicalize_graph_structure(proposal) {
        TotalSyncMerge {
            value: proposal,
            resolution: SyncMergeResolution::WholeProposalFallback,
        }
    } else {
        TotalSyncMerge {
            value: current.clone(),
            resolution: SyncMergeResolution::KeptCurrentFallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::node_graph::NodeInstance;
    use serde_json::json;
    use std::collections::HashMap;

    fn clip(id: &str, start: f64, end: f64, z: i64, args: Value) -> TrackClip {
        TrackClip {
            id: id.into(),
            pattern_id: "wash".into(),
            start_time: start,
            end_time: end,
            z_index: z,
            blend_mode: crate::models::node_graph::BlendMode::Replace,
            args,
        }
    }

    fn track(clips: Vec<TrackClip>) -> TrackDocument {
        TrackDocument {
            revision: revision_for_clips(&clips),
            clips,
        }
    }

    fn node(id: &str) -> NodeInstance {
        NodeInstance {
            id: id.into(),
            type_id: "test".into(),
            params: HashMap::new(),
            position_x: None,
            position_y: None,
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            id: format!("{from}:out->{to}:in"),
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
        }
    }

    #[test]
    fn track_merges_independent_fields_and_proposal_wins_overlap() {
        let base = track(vec![clip("a", 0.0, 1.0, 0, json!({"level": 0.5}))]);
        let current = track(vec![clip("a", 0.0, 2.0, 0, json!({"level": 0.5}))]);
        let proposal = track(vec![clip("a", 0.0, 1.0, 3, json!({"level": 0.8}))]);
        let merged = merge_track_total(&base, &current, &proposal);
        assert_eq!(merged.resolution, SyncMergeResolution::Structural);
        assert_eq!(merged.value.clips[0].end_time, 2.0);
        assert_eq!(merged.value.clips[0].z_index, 3);
        assert_eq!(merged.value.clips[0].args, json!({"level": 0.8}));
    }

    #[test]
    fn later_delete_wins_delete_modify() {
        let base = track(vec![clip("a", 0.0, 1.0, 0, json!({}))]);
        let current = track(vec![clip("a", 0.0, 2.0, 0, json!({}))]);
        let proposal = track(vec![]);
        assert!(merge_track_total(&base, &current, &proposal)
            .value
            .clips
            .is_empty());
    }

    #[test]
    fn typed_argument_values_are_atomic_but_argument_keys_compose() {
        let base = track(vec![clip(
            "a",
            0.0,
            1.0,
            0,
            json!({
                "shape": {"x": 0, "y": 0},
                "current_only": 0,
                "proposal_only": 0
            }),
        )]);
        let current = track(vec![clip(
            "a",
            0.0,
            1.0,
            0,
            json!({
                "shape": {"x": 1, "y": 0},
                "current_only": 1,
                "proposal_only": 0
            }),
        )]);
        let proposal = track(vec![clip(
            "a",
            0.0,
            1.0,
            0,
            json!({
                "shape": {"x": 0, "y": 2},
                "current_only": 0,
                "proposal_only": 2
            }),
        )]);

        let merged = merge_track_total(&base, &current, &proposal);
        assert_eq!(
            merged.value.clips[0].args,
            json!({
                "shape": {"x": 0, "y": 2},
                "current_only": 1,
                "proposal_only": 2
            })
        );
    }

    #[test]
    fn cycle_from_structural_composition_falls_back_to_whole_proposal() {
        let base = Graph {
            nodes: vec![node("a"), node("b"), node("c")],
            edges: vec![],
            args: vec![],
        };
        let current = Graph {
            nodes: base.nodes.clone(),
            edges: vec![edge("a", "b"), edge("b", "c")],
            args: vec![],
        };
        let proposal = Graph {
            nodes: base.nodes.clone(),
            edges: vec![edge("c", "a")],
            args: vec![],
        };
        let merged = merge_graph_total(&base, &current, &proposal);
        assert_eq!(
            merged.resolution,
            SyncMergeResolution::WholeProposalFallback
        );
        assert_eq!(merged.value.edges.len(), 1);
        assert_eq!(merged.value.edges[0].from_node, "c");
    }
}
