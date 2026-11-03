//! A venue's live controls: list them, and set one. See `crate::controls`.

use crate::controls::Control;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::{AppServices, CommandError};

/// Every control a clip of the venue names, and every one set, with its value.
pub async fn control_list(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<Control>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(crate::controls::list(&mut access).await?)
}

/// Set control `name` to `value`, 0–1. Every scene of the venue reads it at
/// its next frame; nothing recompiles.
pub async fn control_set(
    services: &AppServices,
    venue_id: String,
    name: String,
    value: f64,
) -> Result<(), CommandError> {
    let _access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    if !luma_patterns::clip_graph::is_control_name(&name) {
        return Err(CommandError::Invalid(format!(
            "expected a control name in snake_case, such as drop or front_level; got \"{name}\""
        )));
    }
    crate::controls::venue(&venue_id).set(&name, value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices};

    const OWNER: &str = "11111111-2222-3333-4444-555555555555";

    /// A signed-in library whose one venue has a score with a clip reading
    /// the control `drop`.
    async fn library() -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let db = database::init_app_db_at(directory.path()).await.unwrap();
        let state_db = state::init_state_db_at(directory.path()).await.unwrap();
        auth::install_test_session(&state_db.0, OWNER).await;
        auth::bootstrap_headless_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.path().to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let services = AppServices::headless(db, state_db, storage, repo, workspaces);
        let venue = dispatch(
            &services,
            "create_venue",
            &json!({ "name": "Room", "description": null }),
        )
        .await
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM venues WHERE id = ?")
            .bind(&venue)
            .fetch_one(&services.db.0)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('track', ?, 'h', '/t.wav')",
        )
        .bind(&uid)
        .execute(&services.db.0)
        .await
        .unwrap();
        let score = dispatch(
            &services,
            "create_score",
            &json!({
                "requestId": uuid::Uuid::new_v4().to_string(),
                "trackId": "track",
                "venueId": venue,
                "name": "Draft",
            }),
        )
        .await
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let graph = json!({"version": 3, "nodes": {
            "fader": {"kind": "control", "settings": {"name": "drop"}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "fader"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}});
        sqlx::query(
            "INSERT INTO clips (id, uid, score_id, graph, graph_json, start, duration, seed,
                                selection_json, blend_mode)
             VALUES ('clip', ?, ?, '', ?, 0, 4, '1', '{\"expression\":\"all\"}', 'replace')",
        )
        .bind(&uid)
        .bind(&score)
        .bind(graph.to_string())
        .execute(&services.db.0)
        .await
        .unwrap();
        (directory, services, venue)
    }

    #[tokio::test]
    async fn a_control_a_clip_names_is_listed_and_takes_a_value() {
        let (_directory, services, venue) = library().await;
        let listed = dispatch(&services, "control_list", &json!({ "venueId": venue }))
            .await
            .unwrap();
        assert_eq!(listed, json!([{ "name": "drop", "value": 0.0 }]));

        dispatch(
            &services,
            "control_set",
            &json!({ "venueId": venue, "name": "drop", "value": 1.5 }),
        )
        .await
        .unwrap();
        let listed = dispatch(&services, "control_list", &json!({ "venueId": venue }))
            .await
            .unwrap();
        assert_eq!(
            listed,
            json!([{ "name": "drop", "value": 1.0 }]),
            "values clamp to 0–1"
        );

        let refused = dispatch(
            &services,
            "control_set",
            &json!({ "venueId": venue, "name": "Drop Now", "value": 0.5 }),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.kind(), "invalid");
    }
}
