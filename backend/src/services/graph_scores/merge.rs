//! Merge authored identities, not arbitrary JSON leaves. A connection, typed
//! value, selection, or port declaration is indivisible; splicing its fields
//! could silently produce a meaning neither author intended.
use super::GraphScoreDocument;
use crate::models::authored_state::{
    AuthoredMergeConflict as Conflict, AuthoredMergeConflictKind as Kind,
    AuthoredMergePathSegment as Segment, AuthoredMergeValue as Operand,
};
use crate::services::authored_sync_merge::{SyncMergeResolution, TotalSyncMerge};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(crate) fn strict(
    base: &GraphScoreDocument,
    ours: &GraphScoreDocument,
    theirs: &GraphScoreDocument,
) -> Result<GraphScoreDocument, Vec<Conflict>> {
    merge(base, ours, theirs, false)
}

pub(crate) fn total(
    base: &GraphScoreDocument,
    current: &GraphScoreDocument,
    proposal: &GraphScoreDocument,
) -> TotalSyncMerge<GraphScoreDocument> {
    match merge(base, current, proposal, true) {
        Ok(value) => TotalSyncMerge {
            value,
            resolution: SyncMergeResolution::Structural,
        },
        Err(_) => match GraphScoreDocument::new(proposal.score.clone()) {
            Ok(value) => TotalSyncMerge {
                value,
                resolution: SyncMergeResolution::WholeProposalFallback,
            },
            Err(_) => TotalSyncMerge {
                value: current.clone(),
                resolution: SyncMergeResolution::KeptCurrentFallback,
            },
        },
    }
}

fn merge(
    base: &GraphScoreDocument,
    ours: &GraphScoreDocument,
    theirs: &GraphScoreDocument,
    later_wins: bool,
) -> Result<GraphScoreDocument, Vec<Conflict>> {
    let mut values = Vec::new();
    for document in [base, ours, theirs] {
        let checked = GraphScoreDocument::new(document.score.clone())
            .map_err(|error| vec![invalid(error)])?;
        values.push(
            serde_json::to_value(checked.score)
                .map_err(|error| vec![invalid(error.to_string())])?,
        );
    }
    let mut conflicts = Vec::new();
    let value = merge_value(
        Some(&values[0]),
        Some(&values[1]),
        Some(&values[2]),
        &[],
        later_wins,
        &mut conflicts,
    )
    .expect("score root cannot be removed");
    if !conflicts.is_empty() {
        return Err(conflicts);
    }
    let score = serde_json::from_value(value).map_err(|error| vec![invalid(error.to_string())])?;
    GraphScoreDocument::new(score).map_err(|error| vec![invalid(error)])
}

fn invalid(detail: String) -> Conflict {
    Conflict {
        path: vec![Segment::ScoreDocument],
        kind: Kind::InvalidInput,
        base: Operand::Missing,
        ours: Operand::Missing,
        theirs: Operand::Missing,
        detail: Some(detail),
    }
}

fn merge_value(
    base: Option<&Value>,
    ours: Option<&Value>,
    theirs: Option<&Value>,
    path: &[String],
    later_wins: bool,
    conflicts: &mut Vec<Conflict>,
) -> Option<Value> {
    if ours == theirs || theirs == base {
        return ours.cloned();
    }
    if ours == base {
        return theirs.cloned();
    }
    if let (Some(Value::Object(b)), Some(Value::Object(o)), Some(Value::Object(t))) =
        (base, ours, theirs)
    {
        if structural(path) && !coupled_change(b, o, t, path) {
            let keys: BTreeSet<_> = b.keys().chain(o.keys()).chain(t.keys()).collect();
            let mut result = Map::new();
            for key in keys {
                let mut child = path.to_vec();
                child.push(key.clone());
                if let Some(value) = merge_value(
                    b.get(key),
                    o.get(key),
                    t.get(key),
                    &child,
                    later_wins,
                    conflicts,
                ) {
                    result.insert(key.clone(), value);
                }
            }
            return Some(Value::Object(result));
        }
    }
    if !later_wins {
        let kind = if base.is_none() {
            Kind::AddAdd
        } else if ours.is_none() || theirs.is_none() {
            Kind::DeleteModify
        } else if structural(path) {
            Kind::SemanticDependency
        } else {
            Kind::ConcurrentEdit
        };
        let operand = |value: Option<&Value>| {
            value
                .cloned()
                .map(Operand::Present)
                .unwrap_or(Operand::Missing)
        };
        conflicts.push(Conflict {
            path: path.iter().cloned().map(Segment::Field).collect(),
            kind,
            base: operand(base),
            ours: operand(ours),
            theirs: operand(theirs),
            detail: None,
        });
    }
    theirs.cloned()
}

fn structural(path: &[String]) -> bool {
    let p: Vec<_> = path.iter().map(String::as_str).collect();
    match p.as_slice() {
        []
        | ["definitions"]
        | ["clips"]
        | ["clips", _]
        | ["clips", _, "inputs"]
        | ["definitions", _]
        | ["definitions", _, "inputs"]
        | ["definitions", _, "outputs"]
        | ["definitions", _, "body"]
        | ["definitions", _, "body", "body"]
        | ["definitions", _, "body", "body", "nodes"]
        | ["definitions", _, "body", "body", "nodes", _]
        | ["definitions", _, "body", "body", "nodes", _, "inputs"]
        | ["definitions", _, "body", "body", "outputs"] => true,
        _ => false,
    }
}

fn coupled_change(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
    path: &[String],
) -> bool {
    let reference = if path.len() == 2 && path[0] == "clips" {
        "graph"
    } else if path.len() == 6 && path[4] == "nodes" {
        "definition"
    } else {
        return false;
    };
    // Changing which function is called also changes the meaning of its inputs,
    // even when the new function happens to use the same input names and types.
    (ours.get(reference) != base.get(reference) && theirs.get("inputs") != base.get("inputs"))
        || (theirs.get(reference) != base.get(reference)
            && ours.get("inputs") != base.get("inputs"))
}
