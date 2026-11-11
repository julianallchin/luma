//! A venue's live controls, listed. See `crate::controls`.

use crate::controls::Control;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::dispatch::AppServices;
use crate::error::CommandError;

/// Every control a clip of the venue names, and every one set, with its value.
pub async fn control_list(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<Control>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(crate::controls::list(&mut access, &services.controls).await?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use crate::dispatch::{dispatch, AppServices};

    /// A signed-in library whose one venue has a score with a clip reading
    /// the control `drop`.
    async fn library() -> (tempfile::TempDir, AppServices, String) {
        let directory = tempfile::tempdir().unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let services = crate::test_support::signed_in(directory.path(), repo).await;
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
             VALUES ('clip', ?, ?, '', ?, 0, 4, '1', '{\"all\":true}', 'replace')",
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

        services.controls.venue(&venue).set("drop", 1.5);
        let listed = dispatch(&services, "control_list", &json!({ "venueId": venue }))
            .await
            .unwrap();
        assert_eq!(
            listed,
            json!([{ "name": "drop", "value": 1.0 }]),
            "values clamp to 0–1"
        );
    }
}
