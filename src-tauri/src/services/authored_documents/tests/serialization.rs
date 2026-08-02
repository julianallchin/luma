use super::super::*;

#[test]
fn tagged_authored_results_serialize_camel_case_fields() {
    let turn = AuthoredTurnCommit::Committed {
        repository_id: "repo".into(),
        commit_id: "commit".into(),
        applied_to_current_projection: true,
        changed: true,
        document: AuthoredProjectedDocument::TrackScore {
            revision: "revision".into(),
        },
    };
    let value = serde_json::to_value(turn).unwrap();
    assert_eq!(value["status"], "committed");
    assert_eq!(value["repositoryId"], "repo");
    assert_eq!(value["commitId"], "commit");
    assert_eq!(value["appliedToCurrentProjection"], true);
    assert!(value.get("repository_id").is_none());

    let merge = AuthoredWorktreeMerge::Merged {
        repository_id: "repo".into(),
        commit_id: "commit".into(),
        applied_to_current_projection: true,
        document: AuthoredProjectedDocument::TrackScore {
            revision: "revision".into(),
        },
    };
    let value = serde_json::to_value(merge).unwrap();
    assert_eq!(value["status"], "merged");
    assert_eq!(value["repositoryId"], "repo");
    assert_eq!(value["commitId"], "commit");
    assert!(value.get("repository_id").is_none());
    assert!(value.get("commit_id").is_none());

    let merge = AuthoredWorktreeMerge::Conflicted {
        conflicts: vec![crate::models::authored_state::AuthoredMergeConflict {
            path: vec![crate::models::authored_state::AuthoredMergePathSegment::ScoreLayer(7)],
            kind: crate::models::authored_state::AuthoredMergeConflictKind::ConcurrentEdit,
            base: crate::models::authored_state::AuthoredMergeValue::Missing,
            ours: crate::models::authored_state::AuthoredMergeValue::Missing,
            theirs: crate::models::authored_state::AuthoredMergeValue::Missing,
            detail: None,
        }],
    };
    let value = serde_json::to_value(merge).unwrap();
    assert_eq!(value["status"], "conflicted");
    assert_eq!(value["conflicts"][0]["path"][0]["kind"], "score_layer");
    assert_eq!(value["conflicts"][0]["path"][0]["value"], 7);
}
