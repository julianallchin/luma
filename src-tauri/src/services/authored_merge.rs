//! Pure, storage-independent three-way merges for authored Luma state.
//!
//! This module deliberately knows nothing about Git, SQLite, accounts, or
//! worktree leases. It merges three already-decoded domain snapshots and
//! returns either one deterministic canonical value or structured conflicts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::models::node_graph::{Edge, Graph, NodeInstance, PatternArgDef};
use crate::services::graph_documents::validate_graph_structure;
use crate::services::track_edits::{revision_for_clips, TrackClip, TrackDocument};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const PATTERN_ARGS_NODE_ID: &str = "pattern_args";

/// Which input snapshot contains a malformed duplicate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeInput {
    Base,
    Ours,
    Theirs,
}

/// A typed segment in the path to a merge conflict.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MergePathSegment {
    Input(MergeInput),
    TrackClip(String),
    TrackArgument(String),
    GraphNode(String),
    NodeParameter(String),
    GraphEdge { to_node: String, to_port: String },
    PatternArgument(String),
    Field(String),
}

/// Stable, machine-readable location of a conflict.
#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MergePath(pub Vec<MergePathSegment>);

impl MergePath {
    fn one(segment: MergePathSegment) -> Self {
        Self(vec![segment])
    }

    fn child(&self, segment: MergePathSegment) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self(segments)
    }
}

/// The domain reason a three-way merge could not choose one meaning.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeConflictKind {
    /// One input contains the same stable identity more than once.
    DuplicateKey,
    /// Both sides independently added different values at the same identity.
    AddAdd,
    /// One side deleted an entity while the other changed it.
    DeleteModify,
    /// Both sides changed the same scalar to different values.
    ConcurrentEdit,
    /// Independently changed fields are semantically coupled.
    SemanticDependency,
    /// A merged edge references a node that does not exist.
    DanglingEndpoint,
    /// Independently valid inputs combine into an invalid domain value.
    InvalidInput,
}

/// A conflict operand distinguishes an absent entity/key from a present JSON
/// `null`, including after serialization across the Tauri boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum MergeValue {
    Missing,
    Present(Value),
}

/// One deterministic, structured conflict.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeConflict {
    pub path: MergePath,
    pub kind: MergeConflictKind,
    pub base: MergeValue,
    pub ours: MergeValue,
    pub theirs: MergeValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A clean merge has exactly one canonical `merged` value and no conflicts.
/// A conflicted merge deliberately exposes no partially selected value.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeOutcome<T> {
    pub merged: Option<T>,
    pub conflicts: Vec<MergeConflict>,
}

impl<T> MergeOutcome<T> {
    pub fn into_result(self) -> Result<T, Vec<MergeConflict>> {
        self.merged.ok_or(self.conflicts)
    }
}

fn finish<T>(value: T, mut conflicts: Vec<MergeConflict>) -> MergeOutcome<T> {
    conflicts.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.kind.cmp(&right.kind))
            .then(left.detail.cmp(&right.detail))
            .then(conflict_value_key(left).cmp(&conflict_value_key(right)))
    });
    conflicts.dedup();
    if conflicts.is_empty() {
        MergeOutcome {
            merged: Some(value),
            conflicts,
        }
    } else {
        MergeOutcome {
            merged: None,
            conflicts,
        }
    }
}

fn conflict_value_key(conflict: &MergeConflict) -> String {
    format!(
        "{}\0{}\0{}",
        merge_value_key(&conflict.base),
        merge_value_key(&conflict.ours),
        merge_value_key(&conflict.theirs)
    )
}

fn merge_value_key(value: &MergeValue) -> String {
    match value {
        MergeValue::Missing => "<missing>".to_string(),
        MergeValue::Present(value) => crate::canonical_json::to_string(value),
    }
}

fn encoded<T: Serialize>(value: Option<&T>) -> MergeValue {
    match value {
        None => MergeValue::Missing,
        Some(value) => MergeValue::Present(
            serde_json::to_value(value)
                .unwrap_or_else(|error| Value::String(format!("<unserializable: {error}>"))),
        ),
    }
}

fn push_conflict<T: Serialize>(
    conflicts: &mut Vec<MergeConflict>,
    path: MergePath,
    kind: MergeConflictKind,
    base: Option<&T>,
    ours: Option<&T>,
    theirs: Option<&T>,
) {
    conflicts.push(MergeConflict {
        path,
        kind,
        base: encoded(base),
        ours: encoded(ours),
        theirs: encoded(theirs),
        detail: None,
    });
}

fn push_structural_conflict(
    conflicts: &mut Vec<MergeConflict>,
    path: MergePath,
    kind: MergeConflictKind,
    detail: impl Into<String>,
) {
    conflicts.push(MergeConflict {
        path,
        kind,
        base: MergeValue::Missing,
        ours: MergeValue::Missing,
        theirs: MergeValue::Missing,
        detail: Some(detail.into()),
    });
}

fn merge_scalar<T, Equals>(
    base: &T,
    ours: &T,
    theirs: &T,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
    equals: Equals,
) -> T
where
    T: Clone + Serialize,
    Equals: Fn(&T, &T) -> bool,
{
    if equals(ours, theirs) {
        ours.clone()
    } else if equals(ours, base) {
        theirs.clone()
    } else if equals(theirs, base) {
        ours.clone()
    } else {
        push_conflict(
            conflicts,
            path,
            MergeConflictKind::ConcurrentEdit,
            Some(base),
            Some(ours),
            Some(theirs),
        );
        // The value is discarded when any conflict exists. Choosing ours keeps
        // construction deterministic without exposing a partial merge.
        ours.clone()
    }
}

fn merge_optional_atomic<T>(
    base: Option<&T>,
    ours: Option<&T>,
    theirs: Option<&T>,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> Option<T>
where
    T: Clone + PartialEq + Serialize,
{
    match (base, ours, theirs) {
        (None, None, None) => None,
        (None, Some(ours), None) => Some(ours.clone()),
        (None, None, Some(theirs)) => Some(theirs.clone()),
        (None, Some(ours), Some(theirs)) if ours == theirs => Some(ours.clone()),
        (None, Some(ours), Some(theirs)) => {
            push_conflict(
                conflicts,
                path,
                MergeConflictKind::AddAdd,
                None,
                Some(ours),
                Some(theirs),
            );
            Some(ours.clone())
        }
        (Some(_base), None, None) => None,
        (Some(base), None, Some(theirs)) if theirs == base => None,
        (Some(base), Some(ours), None) if ours == base => None,
        (Some(base), None, Some(theirs)) => {
            push_conflict(
                conflicts,
                path,
                MergeConflictKind::DeleteModify,
                Some(base),
                None,
                Some(theirs),
            );
            Some(theirs.clone())
        }
        (Some(base), Some(ours), None) => {
            push_conflict(
                conflicts,
                path,
                MergeConflictKind::DeleteModify,
                Some(base),
                Some(ours),
                None,
            );
            Some(ours.clone())
        }
        (Some(base), Some(ours), Some(theirs)) => Some(merge_scalar(
            base,
            ours,
            theirs,
            path,
            conflicts,
            PartialEq::eq,
        )),
    }
}

/// Merge three authored track documents by stable clip and argument identity.
pub fn merge_track_documents(
    base: &TrackDocument,
    ours: &TrackDocument,
    theirs: &TrackDocument,
) -> MergeOutcome<TrackDocument> {
    let mut conflicts = Vec::new();
    let base_clips = index_track_clips(MergeInput::Base, &base.clips, &mut conflicts);
    let our_clips = index_track_clips(MergeInput::Ours, &ours.clips, &mut conflicts);
    let their_clips = index_track_clips(MergeInput::Theirs, &theirs.clips, &mut conflicts);

    if !conflicts.is_empty() {
        return finish(
            TrackDocument {
                revision: String::new(),
                clips: Vec::new(),
            },
            conflicts,
        );
    }

    let ids: BTreeSet<String> = base_clips
        .keys()
        .chain(our_clips.keys())
        .chain(their_clips.keys())
        .cloned()
        .collect();
    let mut clips = Vec::new();

    for id in ids {
        let path = MergePath::one(MergePathSegment::TrackClip(id.clone()));
        match (
            base_clips.get(&id),
            our_clips.get(&id),
            their_clips.get(&id),
        ) {
            (None, None, None) => {}
            (None, Some(ours), None) => clips.push((*ours).clone()),
            (None, None, Some(theirs)) => clips.push((*theirs).clone()),
            (None, Some(ours), Some(theirs)) if track_clip_eq(ours, theirs) => {
                clips.push((*ours).clone())
            }
            (None, Some(ours), Some(theirs)) => {
                push_conflict(
                    &mut conflicts,
                    path,
                    MergeConflictKind::AddAdd,
                    None,
                    Some(*ours),
                    Some(*theirs),
                );
                clips.push((*ours).clone());
            }
            (Some(_base), None, None) => {}
            (Some(base), None, Some(theirs)) if track_clip_eq(base, theirs) => {}
            (Some(base), Some(ours), None) if track_clip_eq(base, ours) => {}
            (Some(base), None, Some(theirs)) => {
                push_conflict(
                    &mut conflicts,
                    path,
                    MergeConflictKind::DeleteModify,
                    Some(*base),
                    None,
                    Some(*theirs),
                );
                clips.push((*theirs).clone());
            }
            (Some(base), Some(ours), None) => {
                push_conflict(
                    &mut conflicts,
                    path,
                    MergeConflictKind::DeleteModify,
                    Some(*base),
                    Some(*ours),
                    None,
                );
                clips.push((*ours).clone());
            }
            (Some(base), Some(ours), Some(theirs)) => {
                clips.push(merge_track_clip(base, ours, theirs, path, &mut conflicts))
            }
        }
    }

    canonical_sort_clips(&mut clips);
    let revision = revision_for_clips(&clips);
    finish(TrackDocument { revision, clips }, conflicts)
}

fn index_track_clips<'a>(
    input: MergeInput,
    clips: &'a [TrackClip],
    conflicts: &mut Vec<MergeConflict>,
) -> BTreeMap<String, &'a TrackClip> {
    let mut indexed = BTreeMap::new();
    for clip in clips {
        if indexed.insert(clip.id.clone(), clip).is_some() {
            push_structural_conflict(
                conflicts,
                MergePath(vec![
                    MergePathSegment::Input(input),
                    MergePathSegment::TrackClip(clip.id.clone()),
                ]),
                MergeConflictKind::DuplicateKey,
                format!("duplicate track clip id {}", clip.id),
            );
        }
    }
    indexed
}

fn merge_track_clip(
    base: &TrackClip,
    ours: &TrackClip,
    theirs: &TrackClip,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> TrackClip {
    let ours_changed_pattern = ours.pattern_id != base.pattern_id;
    let theirs_changed_pattern = theirs.pattern_id != base.pattern_id;
    let ours_changed_args = !crate::canonical_json::equivalent(&ours.args, &base.args);
    let theirs_changed_args = !crate::canonical_json::equivalent(&theirs.args, &base.args);

    if (ours_changed_pattern && !theirs_changed_pattern && theirs_changed_args)
        || (theirs_changed_pattern && !ours_changed_pattern && ours_changed_args)
    {
        push_conflict(
            conflicts,
            path.clone(),
            MergeConflictKind::SemanticDependency,
            Some(base),
            Some(ours),
            Some(theirs),
        );
    }

    TrackClip {
        id: base.id.clone(),
        pattern_id: merge_scalar(
            &base.pattern_id,
            &ours.pattern_id,
            &theirs.pattern_id,
            path.child(MergePathSegment::Field("patternId".into())),
            conflicts,
            PartialEq::eq,
        ),
        start_time: merge_scalar(
            &base.start_time,
            &ours.start_time,
            &theirs.start_time,
            path.child(MergePathSegment::Field("startTime".into())),
            conflicts,
            |left, right| left.to_bits() == right.to_bits(),
        ),
        end_time: merge_scalar(
            &base.end_time,
            &ours.end_time,
            &theirs.end_time,
            path.child(MergePathSegment::Field("endTime".into())),
            conflicts,
            |left, right| left.to_bits() == right.to_bits(),
        ),
        z_index: merge_scalar(
            &base.z_index,
            &ours.z_index,
            &theirs.z_index,
            path.child(MergePathSegment::Field("zIndex".into())),
            conflicts,
            PartialEq::eq,
        ),
        blend_mode: merge_scalar(
            &base.blend_mode,
            &ours.blend_mode,
            &theirs.blend_mode,
            path.child(MergePathSegment::Field("blendMode".into())),
            conflicts,
            PartialEq::eq,
        ),
        args: merge_track_args(
            &base.args,
            &ours.args,
            &theirs.args,
            path.child(MergePathSegment::Field("args".into())),
            conflicts,
        ),
    }
}

fn merge_track_args(
    base: &Value,
    ours: &Value,
    theirs: &Value,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> Value {
    let (Some(base), Some(ours), Some(theirs)) =
        (base.as_object(), ours.as_object(), theirs.as_object())
    else {
        return merge_scalar(
            base,
            ours,
            theirs,
            path,
            conflicts,
            crate::canonical_json::equivalent,
        );
    };

    let keys: BTreeSet<String> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .cloned()
        .collect();
    let mut merged = Map::new();
    for key in keys {
        if let Some(value) = merge_optional_json(
            base.get(&key),
            ours.get(&key),
            theirs.get(&key),
            path.child(MergePathSegment::TrackArgument(key.clone())),
            conflicts,
        ) {
            merged.insert(key, value);
        }
    }
    Value::Object(merged)
}

fn merge_optional_json(
    base: Option<&Value>,
    ours: Option<&Value>,
    theirs: Option<&Value>,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> Option<Value> {
    match (base, ours, theirs) {
        (None, None, None) => None,
        (None, Some(ours), None) => Some(ours.clone()),
        (None, None, Some(theirs)) => Some(theirs.clone()),
        (None, Some(ours), Some(theirs)) if crate::canonical_json::equivalent(ours, theirs) => {
            Some(ours.clone())
        }
        (None, Some(ours), Some(theirs)) => {
            push_conflict(
                conflicts,
                path,
                MergeConflictKind::AddAdd,
                None,
                Some(ours),
                Some(theirs),
            );
            Some(ours.clone())
        }
        (Some(_base), None, None) => None,
        (Some(base), None, Some(theirs)) if crate::canonical_json::equivalent(theirs, base) => None,
        (Some(base), Some(ours), None) if crate::canonical_json::equivalent(ours, base) => None,
        (Some(base), None, Some(theirs)) => {
            push_conflict(
                conflicts,
                path,
                MergeConflictKind::DeleteModify,
                Some(base),
                None,
                Some(theirs),
            );
            Some(theirs.clone())
        }
        (Some(base), Some(ours), None) => {
            push_conflict(
                conflicts,
                path,
                MergeConflictKind::DeleteModify,
                Some(base),
                Some(ours),
                None,
            );
            Some(ours.clone())
        }
        (Some(base), Some(ours), Some(theirs)) => Some(merge_scalar(
            base,
            ours,
            theirs,
            path,
            conflicts,
            crate::canonical_json::equivalent,
        )),
    }
}

fn track_clip_eq(left: &TrackClip, right: &TrackClip) -> bool {
    left.id == right.id
        && left.pattern_id == right.pattern_id
        && left.start_time.to_bits() == right.start_time.to_bits()
        && left.end_time.to_bits() == right.end_time.to_bits()
        && left.z_index == right.z_index
        && left.blend_mode == right.blend_mode
        && crate::canonical_json::equivalent(&left.args, &right.args)
}

fn canonical_sort_clips(clips: &mut [TrackClip]) {
    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
struct EdgeInputKey {
    to_node: String,
    to_port: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
struct EdgeSource {
    from_node: String,
    from_port: String,
}

#[derive(Default)]
struct GraphIndex {
    nodes: BTreeMap<String, NodeInstance>,
    args: BTreeMap<String, PatternArgDef>,
    edges: BTreeMap<EdgeInputKey, EdgeSource>,
}

/// Merge three graph snapshots by node, input-slot, parameter, and argument
/// identity. Node definitions are deliberately not needed here; known types,
/// ports, and port compatibility belong to the later DB-aware validator.
pub fn merge_graphs(base: &Graph, ours: &Graph, theirs: &Graph) -> MergeOutcome<Graph> {
    let mut conflicts = Vec::new();
    let base = index_graph(MergeInput::Base, base, &mut conflicts);
    let ours = index_graph(MergeInput::Ours, ours, &mut conflicts);
    let theirs = index_graph(MergeInput::Theirs, theirs, &mut conflicts);

    if !conflicts.is_empty() {
        return finish(
            Graph {
                nodes: Vec::new(),
                edges: Vec::new(),
                args: Vec::new(),
            },
            conflicts,
        );
    }

    let (nodes, deleted_nodes) = merge_nodes(&base, &ours, &theirs, &mut conflicts);
    let (args, deleted_args) = merge_pattern_args(&base, &ours, &theirs, &mut conflicts);

    let mut cascaded_edges =
        deleted_node_edge_dependencies(&base, &ours, &theirs, &deleted_nodes, &mut conflicts);
    cascaded_edges.extend(deleted_arg_edge_dependencies(
        &base,
        &ours,
        &theirs,
        &deleted_args,
        &mut conflicts,
    ));
    type_vs_use_dependencies(&base, &ours, &theirs, &mut conflicts);

    let mut merged_edges = merge_edges(&base, &ours, &theirs, &cascaded_edges, &mut conflicts);
    merged_edges.retain(|key, source| {
        if cascaded_edges.contains(key) {
            return false;
        }
        if deleted_nodes.contains(&key.to_node) || deleted_nodes.contains(&source.from_node) {
            return false;
        }
        if source.from_node == PATTERN_ARGS_NODE_ID && deleted_args.contains(&source.from_port) {
            return false;
        }
        true
    });

    for (key, source) in &merged_edges {
        let missing_from = !nodes.contains_key(&source.from_node);
        let missing_to = !nodes.contains_key(&key.to_node);
        if missing_from || missing_to {
            let missing = match (missing_from, missing_to) {
                (true, true) => format!(
                    "edge source {} and target {} do not exist",
                    source.from_node, key.to_node
                ),
                (true, false) => format!("edge source {} does not exist", source.from_node),
                (false, true) => format!("edge target {} does not exist", key.to_node),
                (false, false) => unreachable!(),
            };
            push_structural_conflict(
                &mut conflicts,
                edge_path(key),
                MergeConflictKind::DanglingEndpoint,
                missing,
            );
        }
    }

    let nodes: Vec<NodeInstance> = nodes.into_values().collect();
    let args: Vec<PatternArgDef> = args.into_values().collect();
    let mut edges: Vec<Edge> = merged_edges
        .into_iter()
        .map(|(key, source)| Edge {
            id: canonical_edge_id(&source, &key),
            from_node: source.from_node,
            from_port: source.from_port,
            to_node: key.to_node,
            to_port: key.to_port,
        })
        .collect();
    edges.sort_by(|left, right| {
        left.to_node
            .cmp(&right.to_node)
            .then(left.to_port.cmp(&right.to_port))
            .then(left.from_node.cmp(&right.from_node))
            .then(left.from_port.cmp(&right.from_port))
    });

    let mut seen_edge_ids = HashSet::new();
    for edge in &edges {
        if !seen_edge_ids.insert(edge.id.clone()) {
            push_structural_conflict(
                &mut conflicts,
                edge_path(&EdgeInputKey {
                    to_node: edge.to_node.clone(),
                    to_port: edge.to_port.clone(),
                }),
                MergeConflictKind::DuplicateKey,
                format!("canonical edge id collision: {}", edge.id),
            );
        }
    }

    let graph = Graph { nodes, edges, args };
    if conflicts.is_empty() {
        if let Err(issues) = validate_graph_structure(&graph) {
            for issue in issues {
                push_structural_conflict(
                    &mut conflicts,
                    MergePath::one(MergePathSegment::Field(issue.path)),
                    MergeConflictKind::InvalidInput,
                    issue.message,
                );
            }
        }
    }

    finish(graph, conflicts)
}

fn index_graph(input: MergeInput, graph: &Graph, conflicts: &mut Vec<MergeConflict>) -> GraphIndex {
    let mut indexed = GraphIndex::default();
    for node in &graph.nodes {
        if indexed
            .nodes
            .insert(node.id.clone(), node.clone())
            .is_some()
        {
            push_structural_conflict(
                conflicts,
                MergePath(vec![
                    MergePathSegment::Input(input),
                    MergePathSegment::GraphNode(node.id.clone()),
                ]),
                MergeConflictKind::DuplicateKey,
                format!("duplicate graph node id {}", node.id),
            );
        }
    }
    for arg in &graph.args {
        if indexed.args.insert(arg.id.clone(), arg.clone()).is_some() {
            push_structural_conflict(
                conflicts,
                MergePath(vec![
                    MergePathSegment::Input(input),
                    MergePathSegment::PatternArgument(arg.id.clone()),
                ]),
                MergeConflictKind::DuplicateKey,
                format!("duplicate pattern argument id {}", arg.id),
            );
        }
    }

    let mut edge_ids = HashSet::new();
    for edge in &graph.edges {
        if !edge_ids.insert(edge.id.clone()) {
            push_structural_conflict(
                conflicts,
                MergePath(vec![
                    MergePathSegment::Input(input),
                    MergePathSegment::Field(format!("edgeId:{}", edge.id)),
                ]),
                MergeConflictKind::DuplicateKey,
                format!("duplicate graph edge id {}", edge.id),
            );
        }

        let key = EdgeInputKey {
            to_node: edge.to_node.clone(),
            to_port: edge.to_port.clone(),
        };
        let source = EdgeSource {
            from_node: edge.from_node.clone(),
            from_port: edge.from_port.clone(),
        };
        if indexed.edges.insert(key.clone(), source).is_some() {
            push_structural_conflict(
                conflicts,
                MergePath(vec![
                    MergePathSegment::Input(input),
                    MergePathSegment::GraphEdge {
                        to_node: key.to_node,
                        to_port: key.to_port,
                    },
                ]),
                MergeConflictKind::DuplicateKey,
                "more than one edge targets the same input",
            );
        }
    }
    indexed
}

fn merge_nodes(
    base: &GraphIndex,
    ours: &GraphIndex,
    theirs: &GraphIndex,
    conflicts: &mut Vec<MergeConflict>,
) -> (BTreeMap<String, NodeInstance>, BTreeSet<String>) {
    let ids: BTreeSet<String> = base
        .nodes
        .keys()
        .chain(ours.nodes.keys())
        .chain(theirs.nodes.keys())
        .cloned()
        .collect();
    let mut merged = BTreeMap::new();
    let mut deleted = BTreeSet::new();

    for id in ids {
        let path = MergePath::one(MergePathSegment::GraphNode(id.clone()));
        match (
            base.nodes.get(&id),
            ours.nodes.get(&id),
            theirs.nodes.get(&id),
        ) {
            (None, None, None) => {}
            (None, Some(ours), None) => {
                merged.insert(id, ours.clone());
            }
            (None, None, Some(theirs)) => {
                merged.insert(id, theirs.clone());
            }
            (None, Some(ours), Some(theirs)) if node_semantics_eq(ours, theirs) => {
                let mut node = ours.clone();
                set_layout(&mut node, merge_layout(None, layout(ours), layout(theirs)));
                merged.insert(id, node);
            }
            (None, Some(ours), Some(theirs)) => {
                push_conflict(
                    conflicts,
                    path,
                    MergeConflictKind::AddAdd,
                    None,
                    Some(ours),
                    Some(theirs),
                );
                merged.insert(id, ours.clone());
            }
            (Some(_base), None, None) => {
                deleted.insert(id);
            }
            (Some(base), None, Some(theirs)) if node_semantics_eq(base, theirs) => {
                deleted.insert(id);
            }
            (Some(base), Some(ours), None) if node_semantics_eq(base, ours) => {
                deleted.insert(id);
            }
            (Some(base), None, Some(theirs)) => {
                push_conflict(
                    conflicts,
                    path,
                    MergeConflictKind::DeleteModify,
                    Some(base),
                    None,
                    Some(theirs),
                );
                merged.insert(id, theirs.clone());
            }
            (Some(base), Some(ours), None) => {
                push_conflict(
                    conflicts,
                    path,
                    MergeConflictKind::DeleteModify,
                    Some(base),
                    Some(ours),
                    None,
                );
                merged.insert(id, ours.clone());
            }
            (Some(base), Some(ours), Some(theirs)) => {
                merged.insert(id, merge_node(base, ours, theirs, path, conflicts));
            }
        }
    }
    (merged, deleted)
}

fn merge_node(
    base: &NodeInstance,
    ours: &NodeInstance,
    theirs: &NodeInstance,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> NodeInstance {
    let ours_changed_type = ours.type_id != base.type_id;
    let theirs_changed_type = theirs.type_id != base.type_id;
    let ours_changed_params = ours.params != base.params;
    let theirs_changed_params = theirs.params != base.params;
    if (ours_changed_type && !theirs_changed_type && theirs_changed_params)
        || (theirs_changed_type && !ours_changed_type && ours_changed_params)
    {
        push_conflict(
            conflicts,
            path.clone(),
            MergeConflictKind::SemanticDependency,
            Some(base),
            Some(ours),
            Some(theirs),
        );
    }

    let mut node = NodeInstance {
        id: base.id.clone(),
        type_id: merge_scalar(
            &base.type_id,
            &ours.type_id,
            &theirs.type_id,
            path.child(MergePathSegment::Field("typeId".into())),
            conflicts,
            PartialEq::eq,
        ),
        params: merge_node_params(&base.params, &ours.params, &theirs.params, &path, conflicts),
        position_x: None,
        position_y: None,
    };
    set_layout(
        &mut node,
        merge_layout(Some(layout(base)), layout(ours), layout(theirs)),
    );
    node
}

fn merge_node_params(
    base: &HashMap<String, Value>,
    ours: &HashMap<String, Value>,
    theirs: &HashMap<String, Value>,
    path: &MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> HashMap<String, Value> {
    let keys: BTreeSet<String> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .cloned()
        .collect();
    let mut merged = HashMap::new();
    for key in keys {
        if let Some(value) = merge_optional_atomic(
            base.get(&key),
            ours.get(&key),
            theirs.get(&key),
            path.child(MergePathSegment::NodeParameter(key.clone())),
            conflicts,
        ) {
            merged.insert(key, value);
        }
    }
    merged
}

fn node_semantics_eq(left: &NodeInstance, right: &NodeInstance) -> bool {
    left.id == right.id && left.type_id == right.type_id && left.params == right.params
}

type Layout = (Option<f64>, Option<f64>);

fn layout(node: &NodeInstance) -> Layout {
    (node.position_x, node.position_y)
}

fn layout_eq(left: Layout, right: Layout) -> bool {
    optional_f64_eq(left.0, right.0) && optional_f64_eq(left.1, right.1)
}

fn optional_f64_eq(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.to_bits() == right.to_bits(),
        _ => false,
    }
}

/// Layout is nonsemantic: normal one-sided edits merge, while a true divergent
/// move deterministically keeps ours without creating a blocking conflict.
fn merge_layout(base: Option<Layout>, ours: Layout, theirs: Layout) -> Layout {
    if layout_eq(ours, theirs) {
        ours
    } else if base.is_some_and(|base| layout_eq(ours, base)) {
        theirs
    } else {
        // This covers `theirs == base` and true divergence. Both choose ours.
        ours
    }
}

fn set_layout(node: &mut NodeInstance, layout: Layout) {
    node.position_x = layout.0;
    node.position_y = layout.1;
}

fn merge_pattern_args(
    base: &GraphIndex,
    ours: &GraphIndex,
    theirs: &GraphIndex,
    conflicts: &mut Vec<MergeConflict>,
) -> (BTreeMap<String, PatternArgDef>, BTreeSet<String>) {
    let ids: BTreeSet<String> = base
        .args
        .keys()
        .chain(ours.args.keys())
        .chain(theirs.args.keys())
        .cloned()
        .collect();
    let mut merged = BTreeMap::new();
    let mut deleted = BTreeSet::new();

    for id in ids {
        let path = MergePath::one(MergePathSegment::PatternArgument(id.clone()));
        match (base.args.get(&id), ours.args.get(&id), theirs.args.get(&id)) {
            (None, None, None) => {}
            (None, Some(ours), None) => {
                merged.insert(id, ours.clone());
            }
            (None, None, Some(theirs)) => {
                merged.insert(id, theirs.clone());
            }
            (None, Some(ours), Some(theirs)) if pattern_arg_eq(ours, theirs) => {
                merged.insert(id, ours.clone());
            }
            (None, Some(ours), Some(theirs)) => {
                push_conflict(
                    conflicts,
                    path,
                    MergeConflictKind::AddAdd,
                    None,
                    Some(ours),
                    Some(theirs),
                );
                merged.insert(id, ours.clone());
            }
            (Some(_base), None, None) => {
                deleted.insert(id);
            }
            (Some(base), None, Some(theirs)) if pattern_arg_eq(base, theirs) => {
                deleted.insert(id);
            }
            (Some(base), Some(ours), None) if pattern_arg_eq(base, ours) => {
                deleted.insert(id);
            }
            (Some(base), None, Some(theirs)) => {
                push_conflict(
                    conflicts,
                    path,
                    MergeConflictKind::DeleteModify,
                    Some(base),
                    None,
                    Some(theirs),
                );
                merged.insert(id, theirs.clone());
            }
            (Some(base), Some(ours), None) => {
                push_conflict(
                    conflicts,
                    path,
                    MergeConflictKind::DeleteModify,
                    Some(base),
                    Some(ours),
                    None,
                );
                merged.insert(id, ours.clone());
            }
            (Some(base), Some(ours), Some(theirs)) => {
                merged.insert(id, merge_pattern_arg(base, ours, theirs, path, conflicts));
            }
        }
    }
    (merged, deleted)
}

fn merge_pattern_arg(
    base: &PatternArgDef,
    ours: &PatternArgDef,
    theirs: &PatternArgDef,
    path: MergePath,
    conflicts: &mut Vec<MergeConflict>,
) -> PatternArgDef {
    let ours_changed_type = ours.arg_type != base.arg_type;
    let theirs_changed_type = theirs.arg_type != base.arg_type;
    let ours_changed_default = ours.default_value != base.default_value;
    let theirs_changed_default = theirs.default_value != base.default_value;
    if (ours_changed_type && !theirs_changed_type && theirs_changed_default)
        || (theirs_changed_type && !ours_changed_type && ours_changed_default)
    {
        push_conflict(
            conflicts,
            path.clone(),
            MergeConflictKind::SemanticDependency,
            Some(base),
            Some(ours),
            Some(theirs),
        );
    }

    PatternArgDef {
        id: base.id.clone(),
        name: merge_scalar(
            &base.name,
            &ours.name,
            &theirs.name,
            path.child(MergePathSegment::Field("name".into())),
            conflicts,
            PartialEq::eq,
        ),
        arg_type: merge_scalar(
            &base.arg_type,
            &ours.arg_type,
            &theirs.arg_type,
            path.child(MergePathSegment::Field("argType".into())),
            conflicts,
            PartialEq::eq,
        ),
        default_value: merge_scalar(
            &base.default_value,
            &ours.default_value,
            &theirs.default_value,
            path.child(MergePathSegment::Field("defaultValue".into())),
            conflicts,
            PartialEq::eq,
        ),
    }
}

fn pattern_arg_eq(left: &PatternArgDef, right: &PatternArgDef) -> bool {
    left.id == right.id
        && left.name == right.name
        && left.arg_type == right.arg_type
        && left.default_value == right.default_value
}

fn deleted_node_edge_dependencies(
    base: &GraphIndex,
    ours: &GraphIndex,
    theirs: &GraphIndex,
    deleted_nodes: &BTreeSet<String>,
    conflicts: &mut Vec<MergeConflict>,
) -> BTreeSet<EdgeInputKey> {
    let mut cascaded = BTreeSet::new();
    for node_id in deleted_nodes {
        for (key, source) in &base.edges {
            if edge_incident(key, source, node_id) {
                cascaded.insert(key.clone());
            }
        }

        let ours_deleted = !ours.nodes.contains_key(node_id);
        let theirs_deleted = !theirs.nodes.contains_key(node_id);
        if ours_deleted {
            detect_other_edge_changes_for_deleted_node(
                node_id,
                base,
                ours,
                theirs,
                MergeInput::Ours,
                conflicts,
            );
        }
        if theirs_deleted {
            detect_other_edge_changes_for_deleted_node(
                node_id,
                base,
                theirs,
                ours,
                MergeInput::Theirs,
                conflicts,
            );
        }
    }
    cascaded
}

fn detect_other_edge_changes_for_deleted_node(
    node_id: &str,
    base: &GraphIndex,
    deleting: &GraphIndex,
    other: &GraphIndex,
    deleting_side: MergeInput,
    conflicts: &mut Vec<MergeConflict>,
) {
    let keys: BTreeSet<EdgeInputKey> = base
        .edges
        .keys()
        .chain(other.edges.keys())
        .cloned()
        .collect();
    for key in keys {
        let base_source = base.edges.get(&key);
        let other_source = other.edges.get(&key);
        let base_incident = base_source.is_some_and(|source| edge_incident(&key, source, node_id));
        let other_incident =
            other_source.is_some_and(|source| edge_incident(&key, source, node_id));
        let modified_base_incident =
            base_incident && other_source.is_some() && other_source != base_source;
        let added_incident = !base_incident && other_incident;
        if modified_base_incident || added_incident {
            let deleting_source = deleting.edges.get(&key);
            let (ours, theirs) = match deleting_side {
                MergeInput::Ours => (deleting_source, other_source),
                MergeInput::Theirs => (other_source, deleting_source),
                MergeInput::Base => unreachable!("the merge base cannot delete from itself"),
            };
            conflicts.push(MergeConflict {
                path: edge_path(&key),
                kind: MergeConflictKind::SemanticDependency,
                base: encoded(base_source),
                ours: encoded(ours),
                theirs: encoded(theirs),
                detail: Some(format!(
                    "node {node_id} was deleted while an incident edge was added or modified"
                )),
            });
        }
    }
}

fn deleted_arg_edge_dependencies(
    base: &GraphIndex,
    ours: &GraphIndex,
    theirs: &GraphIndex,
    deleted_args: &BTreeSet<String>,
    conflicts: &mut Vec<MergeConflict>,
) -> BTreeSet<EdgeInputKey> {
    let mut cascaded = BTreeSet::new();
    for arg_id in deleted_args {
        for (key, source) in &base.edges {
            if edge_uses_arg(source, arg_id) {
                cascaded.insert(key.clone());
            }
        }

        if !ours.args.contains_key(arg_id) {
            detect_other_edge_changes_for_deleted_arg(
                arg_id,
                base,
                ours,
                theirs,
                MergeInput::Ours,
                conflicts,
            );
        }
        if !theirs.args.contains_key(arg_id) {
            detect_other_edge_changes_for_deleted_arg(
                arg_id,
                base,
                theirs,
                ours,
                MergeInput::Theirs,
                conflicts,
            );
        }
    }
    cascaded
}

fn detect_other_edge_changes_for_deleted_arg(
    arg_id: &str,
    base: &GraphIndex,
    deleting: &GraphIndex,
    other: &GraphIndex,
    deleting_side: MergeInput,
    conflicts: &mut Vec<MergeConflict>,
) {
    let keys: BTreeSet<EdgeInputKey> = base
        .edges
        .keys()
        .chain(other.edges.keys())
        .cloned()
        .collect();
    for key in keys {
        let base_source = base.edges.get(&key);
        let other_source = other.edges.get(&key);
        let base_use = base_source.is_some_and(|source| edge_uses_arg(source, arg_id));
        let other_use = other_source.is_some_and(|source| edge_uses_arg(source, arg_id));
        let modified_base_use = base_use && other_source.is_some() && other_source != base_source;
        let added_use = !base_use && other_use;
        if modified_base_use || added_use {
            let deleting_source = deleting.edges.get(&key);
            let (ours, theirs) = match deleting_side {
                MergeInput::Ours => (deleting_source, other_source),
                MergeInput::Theirs => (other_source, deleting_source),
                MergeInput::Base => unreachable!("the merge base cannot delete from itself"),
            };
            conflicts.push(MergeConflict {
                path: edge_path(&key),
                kind: MergeConflictKind::SemanticDependency,
                base: encoded(base_source),
                ours: encoded(ours),
                theirs: encoded(theirs),
                detail: Some(format!(
                    "pattern argument {arg_id} was deleted while one of its uses was added or modified"
                )),
            });
        }
    }
}

fn type_vs_use_dependencies(
    base: &GraphIndex,
    ours: &GraphIndex,
    theirs: &GraphIndex,
    conflicts: &mut Vec<MergeConflict>,
) {
    for (arg_id, base_arg) in &base.args {
        let (Some(our_arg), Some(their_arg)) = (ours.args.get(arg_id), theirs.args.get(arg_id))
        else {
            continue;
        };
        let ours_changed_type = our_arg.arg_type != base_arg.arg_type;
        let theirs_changed_type = their_arg.arg_type != base_arg.arg_type;
        let ours_changed_use = arg_use_set(&ours.edges, arg_id) != arg_use_set(&base.edges, arg_id);
        let theirs_changed_use =
            arg_use_set(&theirs.edges, arg_id) != arg_use_set(&base.edges, arg_id);
        if (ours_changed_type && !theirs_changed_type && theirs_changed_use)
            || (theirs_changed_type && !ours_changed_type && ours_changed_use)
        {
            push_conflict(
                conflicts,
                MergePath::one(MergePathSegment::PatternArgument(arg_id.clone())),
                MergeConflictKind::SemanticDependency,
                Some(base_arg),
                Some(our_arg),
                Some(their_arg),
            );
        }
    }
}

fn arg_use_set(edges: &BTreeMap<EdgeInputKey, EdgeSource>, arg_id: &str) -> BTreeSet<EdgeInputKey> {
    edges
        .iter()
        .filter(|(_, source)| edge_uses_arg(source, arg_id))
        .map(|(key, _)| key.clone())
        .collect()
}

fn merge_edges(
    base: &GraphIndex,
    ours: &GraphIndex,
    theirs: &GraphIndex,
    cascaded: &BTreeSet<EdgeInputKey>,
    conflicts: &mut Vec<MergeConflict>,
) -> BTreeMap<EdgeInputKey, EdgeSource> {
    let keys: BTreeSet<EdgeInputKey> = base
        .edges
        .keys()
        .chain(ours.edges.keys())
        .chain(theirs.edges.keys())
        .cloned()
        .collect();
    let mut merged = BTreeMap::new();
    for key in keys {
        if cascaded.contains(&key) {
            continue;
        }
        if let Some(source) = merge_optional_atomic(
            base.edges.get(&key),
            ours.edges.get(&key),
            theirs.edges.get(&key),
            edge_path(&key),
            conflicts,
        ) {
            merged.insert(key, source);
        }
    }
    merged
}

fn edge_incident(key: &EdgeInputKey, source: &EdgeSource, node_id: &str) -> bool {
    key.to_node == node_id || source.from_node == node_id
}

fn edge_uses_arg(source: &EdgeSource, arg_id: &str) -> bool {
    source.from_node == PATTERN_ARGS_NODE_ID && source.from_port == arg_id
}

fn edge_path(key: &EdgeInputKey) -> MergePath {
    MergePath::one(MergePathSegment::GraphEdge {
        to_node: key.to_node.clone(),
        to_port: key.to_port.clone(),
    })
}

fn canonical_edge_id(source: &EdgeSource, key: &EdgeInputKey) -> String {
    format!(
        "{}:{}->{}:{}",
        source.from_node, source.from_port, key.to_node, key.to_port
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{
        merge_graphs, merge_track_documents, MergeConflictKind, MergeInput, MergePathSegment,
        PATTERN_ARGS_NODE_ID,
    };
    use crate::models::node_graph::{
        BlendMode, Edge, Graph, NodeInstance, PatternArgDef, PatternArgType,
    };
    use crate::services::track_edits::{revision_for_clips, TrackClip, TrackDocument};

    fn clip(id: &str) -> TrackClip {
        TrackClip {
            id: id.to_string(),
            pattern_id: "pattern-a".to_string(),
            start_time: 1.0,
            end_time: 2.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            args: json!({"color": "#ff0000", "amount": 0.5}),
        }
    }

    fn document(clips: Vec<TrackClip>) -> TrackDocument {
        TrackDocument {
            revision: "input revision is deliberately ignored".to_string(),
            clips,
        }
    }

    fn node(id: &str, type_id: &str, params: ValueParams) -> NodeInstance {
        NodeInstance {
            id: id.to_string(),
            type_id: type_id.to_string(),
            params: params.0,
            position_x: Some(0.0),
            position_y: Some(0.0),
        }
    }

    struct ValueParams(HashMap<String, serde_json::Value>);

    impl<const N: usize> From<[(&str, serde_json::Value); N]> for ValueParams {
        fn from(values: [(&str, serde_json::Value); N]) -> Self {
            Self(
                values
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect(),
            )
        }
    }

    fn edge(from_node: &str, from_port: &str, to_node: &str, to_port: &str) -> Edge {
        Edge {
            id: format!("arbitrary-{}-{}", from_node, to_node),
            from_node: from_node.to_string(),
            from_port: from_port.to_string(),
            to_node: to_node.to_string(),
            to_port: to_port.to_string(),
        }
    }

    fn arg(id: &str, arg_type: PatternArgType, default: serde_json::Value) -> PatternArgDef {
        PatternArgDef {
            id: id.to_string(),
            name: id.to_string(),
            arg_type,
            default_value: default,
        }
    }

    fn graph(nodes: Vec<NodeInstance>, edges: Vec<Edge>, args: Vec<PatternArgDef>) -> Graph {
        Graph { nodes, edges, args }
    }

    fn graph_fixture() -> Graph {
        graph(
            vec![
                node("a", "source", [("gain", json!(1))].into()),
                node("b", "sink", [("mix", json!(0.5))].into()),
                node("pattern_args", "pattern_args", [].into()),
            ],
            vec![edge("a", "out", "b", "in")],
            vec![arg("amount", PatternArgType::Scalar, json!(0.5))],
        )
    }

    fn kinds(conflicts: &[super::MergeConflict]) -> Vec<MergeConflictKind> {
        conflicts.iter().map(|conflict| conflict.kind).collect()
    }

    #[test]
    fn track_independent_fields_and_arguments_merge() {
        let base_clip = clip("clip");
        let mut ours = base_clip.clone();
        ours.start_time = 1.25;
        ours.z_index = 3;
        ours.args["color"] = json!("#00ff00");
        let mut theirs = base_clip.clone();
        theirs.end_time = 2.5;
        theirs.blend_mode = BlendMode::Add;
        theirs.args["amount"] = json!(0.75);

        let result = merge_track_documents(
            &document(vec![base_clip]),
            &document(vec![ours]),
            &document(vec![theirs]),
        )
        .into_result()
        .unwrap();
        let merged = &result.clips[0];
        assert_eq!(merged.start_time, 1.25);
        assert_eq!(merged.end_time, 2.5);
        assert_eq!(merged.z_index, 3);
        assert_eq!(merged.blend_mode, BlendMode::Add);
        assert_eq!(merged.args, json!({"amount": 0.75, "color": "#00ff00"}));
        assert_eq!(result.revision, revision_for_clips(&result.clips));
    }

    #[test]
    fn track_all_scalar_fields_report_concurrent_edits() {
        for field in ["patternId", "startTime", "endTime", "zIndex", "blendMode"] {
            let base_clip = clip("clip");
            let mut ours = base_clip.clone();
            let mut theirs = base_clip.clone();
            match field {
                "patternId" => {
                    ours.pattern_id = "pattern-b".into();
                    theirs.pattern_id = "pattern-c".into();
                }
                "startTime" => {
                    ours.start_time = 1.1;
                    theirs.start_time = 1.2;
                }
                "endTime" => {
                    ours.end_time = 2.1;
                    theirs.end_time = 2.2;
                }
                "zIndex" => {
                    ours.z_index = 1;
                    theirs.z_index = 2;
                }
                "blendMode" => {
                    ours.blend_mode = BlendMode::Add;
                    theirs.blend_mode = BlendMode::Screen;
                }
                _ => unreachable!(),
            }
            let outcome = merge_track_documents(
                &document(vec![base_clip]),
                &document(vec![ours]),
                &document(vec![theirs]),
            );
            assert!(outcome.merged.is_none(), "{field}");
            assert!(outcome.conflicts.iter().any(|conflict| {
                conflict.kind == MergeConflictKind::ConcurrentEdit
                    && conflict.path.0.last() == Some(&MergePathSegment::Field(field.to_string()))
            }));
        }
    }

    #[test]
    fn track_argument_json_values_are_atomic() {
        let mut base_clip = clip("clip");
        base_clip.args = json!({"gradient": {"stops": [0, 1], "mode": "oklab"}});
        let mut ours = base_clip.clone();
        ours.args["gradient"]["stops"] = json!([0, 0.5, 1]);
        let mut theirs = base_clip.clone();
        theirs.args["gradient"]["mode"] = json!("rgb");

        let outcome = merge_track_documents(
            &document(vec![base_clip]),
            &document(vec![ours]),
            &document(vec![theirs]),
        );
        assert_eq!(
            kinds(&outcome.conflicts),
            vec![MergeConflictKind::ConcurrentEdit]
        );
        assert!(matches!(
            outcome.conflicts[0].path.0.last(),
            Some(MergePathSegment::TrackArgument(id)) if id == "gradient"
        ));
    }

    #[test]
    fn track_argument_add_delete_matrix() {
        let mut base_clip = clip("clip");
        base_clip.args = json!({"existing": 1});

        let mut ours = base_clip.clone();
        ours.args.as_object_mut().unwrap().remove("existing");
        let clean_delete = merge_track_documents(
            &document(vec![base_clip.clone()]),
            &document(vec![ours.clone()]),
            &document(vec![base_clip.clone()]),
        )
        .into_result()
        .unwrap();
        assert_eq!(clean_delete.clips[0].args, json!({}));

        let mut changed = base_clip.clone();
        changed.args["existing"] = json!(2);
        let delete_modify = merge_track_documents(
            &document(vec![base_clip.clone()]),
            &document(vec![ours]),
            &document(vec![changed]),
        );
        assert!(kinds(&delete_modify.conflicts).contains(&MergeConflictKind::DeleteModify));

        let mut empty_args = base_clip.clone();
        empty_args.args = json!({});
        let mut add_ours = empty_args.clone();
        add_ours.args["new"] = json!(1);
        let mut add_theirs = empty_args.clone();
        add_theirs.args["new"] = json!(2);
        let add_add = merge_track_documents(
            &document(vec![empty_args]),
            &document(vec![add_ours]),
            &document(vec![add_theirs]),
        );
        assert!(kinds(&add_add.conflicts).contains(&MergeConflictKind::AddAdd));
    }

    #[test]
    fn track_pattern_change_vs_concurrent_args_change_conflicts_both_ways() {
        for pattern_on_ours in [true, false] {
            let base_clip = clip("clip");
            let mut ours = base_clip.clone();
            let mut theirs = base_clip.clone();
            if pattern_on_ours {
                ours.pattern_id = "pattern-b".into();
                theirs.args["amount"] = json!(0.9);
            } else {
                theirs.pattern_id = "pattern-b".into();
                ours.args["amount"] = json!(0.9);
            }
            let outcome = merge_track_documents(
                &document(vec![base_clip]),
                &document(vec![ours]),
                &document(vec![theirs]),
            );
            assert!(kinds(&outcome.conflicts).contains(&MergeConflictKind::SemanticDependency));
        }
    }

    #[test]
    fn track_add_add_matrix() {
        let empty = document(vec![]);
        let added = clip("new");

        assert_eq!(
            merge_track_documents(&empty, &document(vec![added.clone()]), &empty)
                .into_result()
                .unwrap()
                .clips
                .len(),
            1
        );
        assert_eq!(
            merge_track_documents(&empty, &empty, &document(vec![added.clone()]))
                .into_result()
                .unwrap()
                .clips
                .len(),
            1
        );
        assert_eq!(
            merge_track_documents(
                &empty,
                &document(vec![added.clone()]),
                &document(vec![added.clone()]),
            )
            .into_result()
            .unwrap()
            .clips
            .len(),
            1
        );

        let mut divergent = added.clone();
        divergent.end_time = 3.0;
        let outcome =
            merge_track_documents(&empty, &document(vec![added]), &document(vec![divergent]));
        assert_eq!(kinds(&outcome.conflicts), vec![MergeConflictKind::AddAdd]);
    }

    #[test]
    fn track_delete_matrix() {
        for delete_ours in [true, false] {
            let base_clip = clip("clip");
            let base = document(vec![base_clip.clone()]);
            let unchanged = document(vec![base_clip.clone()]);
            let empty = document(vec![]);
            let clean = if delete_ours {
                merge_track_documents(&base, &empty, &unchanged)
            } else {
                merge_track_documents(&base, &unchanged, &empty)
            };
            assert!(clean.into_result().unwrap().clips.is_empty());

            let mut modified = base_clip.clone();
            modified.end_time = 4.0;
            let modified = document(vec![modified]);
            let conflicted = if delete_ours {
                merge_track_documents(&base, &empty, &modified)
            } else {
                merge_track_documents(&base, &modified, &empty)
            };
            assert_eq!(
                kinds(&conflicted.conflicts),
                vec![MergeConflictKind::DeleteModify]
            );
        }

        let base = document(vec![clip("clip")]);
        assert!(
            merge_track_documents(&base, &document(vec![]), &document(vec![]))
                .into_result()
                .unwrap()
                .clips
                .is_empty()
        );
    }

    #[test]
    fn track_duplicate_ids_are_rejected_in_every_input() {
        for input in [MergeInput::Base, MergeInput::Ours, MergeInput::Theirs] {
            let duplicate = document(vec![clip("same"), clip("same")]);
            let empty = document(vec![]);
            let outcome = match input {
                MergeInput::Base => merge_track_documents(&duplicate, &empty, &empty),
                MergeInput::Ours => merge_track_documents(&empty, &duplicate, &empty),
                MergeInput::Theirs => merge_track_documents(&empty, &empty, &duplicate),
            };
            assert_eq!(
                kinds(&outcome.conflicts),
                vec![MergeConflictKind::DuplicateKey]
            );
            assert!(matches!(
                outcome.conflicts[0].path.0.first(),
                Some(MergePathSegment::Input(side)) if *side == input
            ));
        }
    }

    #[test]
    fn track_results_are_canonically_sorted_and_input_order_independent() {
        let base = document(vec![]);
        let mut a = clip("a");
        a.start_time = 4.0;
        a.z_index = 0;
        let mut b = clip("b");
        b.start_time = 1.0;
        b.z_index = 2;
        let mut c = clip("c");
        c.start_time = 1.0;
        c.z_index = 1;

        let left = merge_track_documents(
            &base,
            &document(vec![a.clone(), c.clone(), b.clone()]),
            &base,
        )
        .into_result()
        .unwrap();
        let right = merge_track_documents(&base, &document(vec![b, a, c]), &base)
            .into_result()
            .unwrap();
        let left_ids: Vec<&str> = left.clips.iter().map(|clip| clip.id.as_str()).collect();
        let right_ids: Vec<&str> = right.clips.iter().map(|clip| clip.id.as_str()).collect();
        assert_eq!(left_ids, vec!["c", "b", "a"]);
        assert_eq!(left_ids, right_ids);
        assert_eq!(left.revision, right.revision);
    }

    #[test]
    fn graph_independent_node_param_edits_merge() {
        let base = graph_fixture();
        let mut ours = base.clone();
        ours.nodes[0].params.insert("gain".into(), json!(2));
        let mut theirs = base.clone();
        theirs.nodes[1].params.insert("mix".into(), json!(0.75));

        let merged = merge_graphs(&base, &ours, &theirs).into_result().unwrap();
        assert_eq!(merged.nodes[0].id, "a");
        assert_eq!(merged.nodes[0].params["gain"], json!(2));
        assert_eq!(merged.nodes[1].id, "b");
        assert_eq!(merged.nodes[1].params["mix"], json!(0.75));
    }

    #[test]
    fn graph_node_param_json_values_are_atomic() {
        let mut base = graph_fixture();
        base.nodes[0]
            .params
            .insert("config".into(), json!({"left": 1, "right": 1}));
        let mut ours = base.clone();
        ours.nodes[0].params.get_mut("config").unwrap()["left"] = json!(2);
        let mut theirs = base.clone();
        theirs.nodes[0].params.get_mut("config").unwrap()["right"] = json!(2);
        let outcome = merge_graphs(&base, &ours, &theirs);
        assert_eq!(
            kinds(&outcome.conflicts),
            vec![MergeConflictKind::ConcurrentEdit]
        );
        assert!(matches!(
            outcome.conflicts[0].path.0.last(),
            Some(MergePathSegment::NodeParameter(id)) if id == "config"
        ));
    }

    #[test]
    fn graph_node_param_add_delete_matrix() {
        let mut base = graph_fixture();
        base.nodes[0].params = ValueParams::from([("existing", json!(1))]).0;

        let mut delete = base.clone();
        delete.nodes[0].params.clear();
        let clean = merge_graphs(&base, &delete, &base).into_result().unwrap();
        assert!(clean.nodes[0].params.is_empty());

        let mut changed = base.clone();
        changed.nodes[0].params.insert("existing".into(), json!(2));
        let conflict = merge_graphs(&base, &delete, &changed);
        assert!(kinds(&conflict.conflicts).contains(&MergeConflictKind::DeleteModify));

        let mut no_params = base.clone();
        no_params.nodes[0].params.clear();
        let mut add_ours = no_params.clone();
        add_ours.nodes[0].params.insert("new".into(), json!(1));
        let mut add_theirs = no_params.clone();
        add_theirs.nodes[0].params.insert("new".into(), json!(2));
        let conflict = merge_graphs(&no_params, &add_ours, &add_theirs);
        assert!(kinds(&conflict.conflicts).contains(&MergeConflictKind::AddAdd));
    }

    #[test]
    fn graph_type_change_vs_params_change_conflicts_both_ways() {
        for type_on_ours in [true, false] {
            let base = graph_fixture();
            let mut ours = base.clone();
            let mut theirs = base.clone();
            if type_on_ours {
                ours.nodes[0].type_id = "other-source".into();
                theirs.nodes[0].params.insert("gain".into(), json!(2));
            } else {
                theirs.nodes[0].type_id = "other-source".into();
                ours.nodes[0].params.insert("gain".into(), json!(2));
            }
            let outcome = merge_graphs(&base, &ours, &theirs);
            assert!(kinds(&outcome.conflicts).contains(&MergeConflictKind::SemanticDependency));
        }
    }

    #[test]
    fn graph_node_add_add_and_delete_modify_matrices() {
        let empty = graph(vec![], vec![], vec![]);
        let added = node("new", "source", [].into());
        assert_eq!(
            merge_graphs(&empty, &graph(vec![added.clone()], vec![], vec![]), &empty,)
                .into_result()
                .unwrap()
                .nodes
                .len(),
            1
        );

        let mut divergent = added.clone();
        divergent.type_id = "sink".into();
        let outcome = merge_graphs(
            &empty,
            &graph(vec![added], vec![], vec![]),
            &graph(vec![divergent], vec![], vec![]),
        );
        assert_eq!(kinds(&outcome.conflicts), vec![MergeConflictKind::AddAdd]);

        for delete_ours in [true, false] {
            let base = graph(
                vec![node("n", "source", [("gain", json!(1))].into())],
                vec![],
                vec![],
            );
            let empty = graph(vec![], vec![], vec![]);
            let mut modified = base.clone();
            modified.nodes[0].params.insert("gain".into(), json!(2));
            let outcome = if delete_ours {
                merge_graphs(&base, &empty, &modified)
            } else {
                merge_graphs(&base, &modified, &empty)
            };
            assert_eq!(
                kinds(&outcome.conflicts),
                vec![MergeConflictKind::DeleteModify]
            );
        }
    }

    #[test]
    fn graph_layout_merges_without_blocking_semantics() {
        let base = graph_fixture();
        let mut ours = base.clone();
        ours.nodes[0].position_x = Some(10.0);
        let mut theirs = base.clone();
        theirs.nodes[0].position_y = Some(20.0);

        // A divergent move is deliberately atomic and ours wins.
        let merged = merge_graphs(&base, &ours, &theirs).into_result().unwrap();
        assert_eq!(merged.nodes[0].position_x, Some(10.0));
        assert_eq!(merged.nodes[0].position_y, Some(0.0));

        let one_sided = merge_graphs(&base, &base, &theirs).into_result().unwrap();
        assert_eq!(one_sided.nodes[0].position_y, Some(20.0));

        let mut same_semantics = ours.clone();
        same_semantics.nodes[0].position_x = Some(99.0);
        let empty = graph(vec![], vec![], vec![]);
        let both_added = merge_graphs(
            &empty,
            &graph(vec![ours.nodes[0].clone()], vec![], vec![]),
            &graph(vec![same_semantics.nodes[0].clone()], vec![], vec![]),
        )
        .into_result()
        .unwrap();
        assert_eq!(both_added.nodes[0].position_x, Some(10.0));
    }

    #[test]
    fn graph_edge_rewire_matrix_uses_target_input_as_identity() {
        let mut base = graph_fixture();
        base.nodes.push(node("c", "source", [].into()));
        base.nodes.push(node("d", "source", [].into()));

        let mut ours = base.clone();
        ours.edges = vec![edge("c", "out", "b", "in")];
        let mut theirs = base.clone();
        theirs.edges = vec![edge("d", "out", "b", "in")];
        let conflict = merge_graphs(&base, &ours, &theirs);
        assert_eq!(
            kinds(&conflict.conflicts),
            vec![MergeConflictKind::ConcurrentEdit]
        );

        let same = merge_graphs(&base, &ours, &ours).into_result().unwrap();
        assert_eq!(same.edges[0].from_node, "c");
        assert_eq!(same.edges[0].id, "c:out->b:in");

        let one_sided = merge_graphs(&base, &base, &ours).into_result().unwrap();
        assert_eq!(one_sided.edges[0].from_node, "c");
    }

    #[test]
    fn graph_edge_add_delete_matrix() {
        let mut nodes_only = graph_fixture();
        nodes_only.edges.clear();
        nodes_only.nodes.push(node("c", "source", [].into()));

        let mut add_ours = nodes_only.clone();
        add_ours.edges = vec![edge("a", "out", "b", "in")];
        let mut add_theirs = nodes_only.clone();
        add_theirs.edges = vec![edge("c", "out", "b", "in")];
        let add_add = merge_graphs(&nodes_only, &add_ours, &add_theirs);
        assert_eq!(kinds(&add_add.conflicts), vec![MergeConflictKind::AddAdd]);

        let base = add_ours;
        let mut rewired = base.clone();
        rewired.edges = vec![edge("c", "out", "b", "in")];
        let deleted = nodes_only;
        let delete_modify = merge_graphs(&base, &deleted, &rewired);
        assert_eq!(
            kinds(&delete_modify.conflicts),
            vec![MergeConflictKind::DeleteModify]
        );
    }

    #[test]
    fn graph_edges_on_different_inputs_merge_and_sort_canonically() {
        let mut base = graph_fixture();
        base.nodes.push(node("c", "sink", [].into()));
        let mut ours = base.clone();
        ours.edges.push(edge("a", "out", "c", "z"));
        let mut theirs = base.clone();
        theirs.edges.push(edge("a", "out", "c", "a"));

        let merged = merge_graphs(&base, &ours, &theirs).into_result().unwrap();
        let slots: Vec<(&str, &str)> = merged
            .edges
            .iter()
            .map(|edge| (edge.to_node.as_str(), edge.to_port.as_str()))
            .collect();
        assert_eq!(slots, vec![("b", "in"), ("c", "a"), ("c", "z")]);
        assert!(merged.edges.iter().all(|edge| edge.id
            == format!(
                "{}:{}->{}:{}",
                edge.from_node, edge.from_port, edge.to_node, edge.to_port
            )));
    }

    #[test]
    fn graph_node_delete_cascades_unchanged_edges() {
        let base = graph_fixture();
        let ours = graph(
            base.nodes
                .iter()
                .filter(|node| node.id != "a")
                .cloned()
                .collect(),
            vec![],
            base.args.clone(),
        );
        let merged = merge_graphs(&base, &ours, &base).into_result().unwrap();
        assert!(!merged.nodes.iter().any(|node| node.id == "a"));
        assert!(merged.edges.is_empty());
    }

    #[test]
    fn graph_node_delete_vs_incident_edge_change_conflicts() {
        let mut base = graph_fixture();
        base.nodes.push(node("c", "source", [].into()));
        let ours = graph(
            base.nodes
                .iter()
                .filter(|node| node.id != "a")
                .cloned()
                .collect(),
            vec![],
            base.args.clone(),
        );
        let mut theirs = base.clone();
        theirs.edges = vec![edge("c", "out", "b", "in")];
        let outcome = merge_graphs(&base, &ours, &theirs);
        assert!(kinds(&outcome.conflicts).contains(&MergeConflictKind::SemanticDependency));
    }

    #[test]
    fn graph_pattern_arg_fields_merge_and_type_default_conflicts() {
        let base = graph_fixture();
        let mut ours = base.clone();
        ours.args[0].name = "level".into();
        let mut theirs = base.clone();
        theirs.args.push(arg(
            "color",
            PatternArgType::Color,
            json!({"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}),
        ));
        let merged = merge_graphs(&base, &ours, &theirs).into_result().unwrap();
        assert_eq!(merged.args[0].id, "amount");
        assert_eq!(merged.args[0].name, "level");
        assert_eq!(merged.args[1].id, "color");

        for type_on_ours in [true, false] {
            let mut ours = base.clone();
            let mut theirs = base.clone();
            if type_on_ours {
                ours.args[0].arg_type = PatternArgType::Color;
                theirs.args[0].default_value = json!(0.9);
            } else {
                theirs.args[0].arg_type = PatternArgType::Color;
                ours.args[0].default_value = json!(0.9);
            }
            let outcome = merge_graphs(&base, &ours, &theirs);
            assert!(kinds(&outcome.conflicts).contains(&MergeConflictKind::SemanticDependency));
        }
    }

    #[test]
    fn graph_pattern_arg_add_delete_and_scalar_conflict_matrix() {
        let base = graph_fixture();

        let mut delete = base.clone();
        delete.args.clear();
        delete.nodes.retain(|node| node.id != PATTERN_ARGS_NODE_ID);
        let clean = merge_graphs(&base, &delete, &base).into_result().unwrap();
        assert!(clean.args.is_empty());

        let mut changed = base.clone();
        changed.args[0].name = "changed".into();
        let delete_modify = merge_graphs(&base, &delete, &changed);
        assert_eq!(
            kinds(&delete_modify.conflicts),
            vec![MergeConflictKind::DeleteModify]
        );

        let no_args = delete.clone();
        let mut add_ours = no_args.clone();
        add_ours
            .nodes
            .push(node(PATTERN_ARGS_NODE_ID, PATTERN_ARGS_NODE_ID, [].into()));
        add_ours
            .args
            .push(arg("new", PatternArgType::Scalar, json!(1)));
        let mut add_theirs = no_args.clone();
        add_theirs
            .nodes
            .push(node(PATTERN_ARGS_NODE_ID, PATTERN_ARGS_NODE_ID, [].into()));
        add_theirs
            .args
            .push(arg("new", PatternArgType::Scalar, json!(2)));
        let add_add = merge_graphs(&no_args, &add_ours, &add_theirs);
        assert_eq!(kinds(&add_add.conflicts), vec![MergeConflictKind::AddAdd]);

        let mut name_ours = base.clone();
        name_ours.args[0].name = "left".into();
        let mut name_theirs = base.clone();
        name_theirs.args[0].name = "right".into();
        let concurrent = merge_graphs(&base, &name_ours, &name_theirs);
        assert_eq!(
            kinds(&concurrent.conflicts),
            vec![MergeConflictKind::ConcurrentEdit]
        );
    }

    #[test]
    fn graph_pattern_arg_delete_cascades_unchanged_uses() {
        let mut base = graph_fixture();
        base.edges = vec![edge("pattern_args", "amount", "b", "in")];
        let mut ours = base.clone();
        ours.args.clear();
        ours.edges.clear();
        ours.nodes.retain(|node| node.id != PATTERN_ARGS_NODE_ID);

        let merged = merge_graphs(&base, &ours, &base).into_result().unwrap();
        assert!(merged.args.is_empty());
        assert!(merged.edges.is_empty());
        assert!(!merged
            .nodes
            .iter()
            .any(|node| node.id == PATTERN_ARGS_NODE_ID));
    }

    #[test]
    fn graph_pattern_arg_delete_or_type_change_vs_use_edit_conflicts() {
        let mut base = graph_fixture();
        base.nodes.push(node("c", "sink", [].into()));
        base.edges = vec![edge("pattern_args", "amount", "b", "in")];

        let mut delete_arg = base.clone();
        delete_arg.args.clear();
        delete_arg.edges.clear();
        let mut add_use = base.clone();
        add_use
            .edges
            .push(edge("pattern_args", "amount", "c", "in"));
        let deleted = merge_graphs(&base, &delete_arg, &add_use);
        assert!(kinds(&deleted.conflicts).contains(&MergeConflictKind::SemanticDependency));

        for type_on_ours in [true, false] {
            let mut type_change = base.clone();
            type_change.args[0].arg_type = PatternArgType::Color;
            let outcome = if type_on_ours {
                merge_graphs(&base, &type_change, &add_use)
            } else {
                merge_graphs(&base, &add_use, &type_change)
            };
            assert!(kinds(&outcome.conflicts).contains(&MergeConflictKind::SemanticDependency));
        }
    }

    #[test]
    fn graph_duplicate_nodes_args_edge_ids_and_input_slots_are_rejected() {
        let base = graph_fixture();
        let mut cases = Vec::new();

        let mut duplicate_nodes = base.clone();
        duplicate_nodes.nodes.push(duplicate_nodes.nodes[0].clone());
        cases.push(duplicate_nodes);

        let mut duplicate_args = base.clone();
        duplicate_args.args.push(duplicate_args.args[0].clone());
        cases.push(duplicate_args);

        let mut duplicate_edge_ids = base.clone();
        let mut second = edge("pattern_args", "amount", "a", "gain");
        second.id = duplicate_edge_ids.edges[0].id.clone();
        duplicate_edge_ids.edges.push(second);
        cases.push(duplicate_edge_ids);

        let mut duplicate_slots = base.clone();
        duplicate_slots
            .edges
            .push(edge("pattern_args", "amount", "b", "in"));
        cases.push(duplicate_slots);

        let empty = graph(vec![], vec![], vec![]);
        for case in cases {
            let outcome = merge_graphs(&empty, &case, &empty);
            assert!(
                kinds(&outcome.conflicts).contains(&MergeConflictKind::DuplicateKey),
                "{:?}",
                outcome.conflicts
            );
        }
    }

    #[test]
    fn graph_duplicate_keys_are_rejected_in_every_input() {
        let mut duplicate = graph_fixture();
        duplicate.nodes.push(duplicate.nodes[0].clone());
        let empty = graph(vec![], vec![], vec![]);
        for input in [MergeInput::Base, MergeInput::Ours, MergeInput::Theirs] {
            let outcome = match input {
                MergeInput::Base => merge_graphs(&duplicate, &empty, &empty),
                MergeInput::Ours => merge_graphs(&empty, &duplicate, &empty),
                MergeInput::Theirs => merge_graphs(&empty, &empty, &duplicate),
            };
            assert!(matches!(
                outcome.conflicts[0].path.0.first(),
                Some(MergePathSegment::Input(side)) if *side == input
            ));
        }
    }

    #[test]
    fn graph_dangling_endpoints_are_rejected_after_merge() {
        let base = graph_fixture();
        let mut ours = base.clone();
        ours.edges.push(edge("missing", "out", "b", "other"));
        let outcome = merge_graphs(&base, &ours, &base);
        assert_eq!(
            kinds(&outcome.conflicts),
            vec![MergeConflictKind::DanglingEndpoint]
        );
    }

    #[test]
    fn graph_merge_rejects_a_cycle_created_only_by_combining_valid_sides() {
        let base = graph(
            vec![
                node("a", "source", [].into()),
                node("b", "source", [].into()),
            ],
            vec![],
            vec![],
        );
        let mut ours = base.clone();
        ours.edges.push(edge("a", "out", "b", "in"));
        let mut theirs = base.clone();
        theirs.edges.push(edge("b", "out", "a", "in"));

        let outcome = merge_graphs(&base, &ours, &theirs);
        assert!(outcome.merged.is_none());
        assert!(outcome.conflicts.iter().any(|conflict| {
            conflict.kind == MergeConflictKind::InvalidInput
                && conflict
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("cycle"))
        }));
    }

    #[test]
    fn graph_output_is_deterministic_under_input_reordering() {
        let base = graph(vec![], vec![], vec![]);
        let first = graph_fixture();
        let mut reordered = first.clone();
        reordered.nodes.reverse();
        reordered.args.reverse();
        reordered.edges.reverse();

        let left = merge_graphs(&base, &first, &base).into_result().unwrap();
        let right = merge_graphs(&base, &reordered, &base)
            .into_result()
            .unwrap();
        assert_eq!(
            serde_json::to_value(left).unwrap(),
            serde_json::to_value(right).unwrap()
        );
    }
}
