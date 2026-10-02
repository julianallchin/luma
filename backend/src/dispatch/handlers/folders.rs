//! A venue's folders: named sets of its songs (`docs/specs/venue-tabs.md`,
//! phase 2).

use crate::database::local::folders as db;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::dispatch::handlers::fixtures::require_changed;
use crate::dispatch::{AppServices, CommandError};
use crate::models::folders::Folder;

pub async fn list_folders(
    services: &AppServices,
    venue_id: String,
) -> Result<Vec<Folder>, CommandError> {
    let mut access =
        VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    Ok(db::list_folders(&mut access).await?)
}

/// The name is trimmed; an empty one is refused, because a folder is shown by
/// its name.
pub async fn create_folder(
    services: &AppServices,
    venue_id: String,
    name: String,
) -> Result<Folder, CommandError> {
    let name = folder_name(&name)?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let folder = db::create_folder(&mut access, name).await?;
    access.commit().await?;
    Ok(folder)
}

/// The name is trimmed; an empty one is refused.
pub async fn rename_folder(
    services: &AppServices,
    folder_id: String,
    name: String,
) -> Result<(), CommandError> {
    let name = folder_name(&name)?;
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Folder(&folder_id)).await?;
    require_changed(db::rename_folder(&mut access, &folder_id, name).await?)?;
    access.commit().await?;
    Ok(())
}

/// The folder and its links go. Its songs and their scores stay.
pub async fn delete_folder(services: &AppServices, folder_id: String) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Folder(&folder_id)).await?;
    require_changed(db::delete_folder(&mut access, &folder_id).await?)?;
    access.commit().await?;
    Ok(())
}

/// Put a song of the folder's venue in the folder (`linked`), or take it out.
pub async fn set_folder_track(
    services: &AppServices,
    folder_id: String,
    track_id: String,
    linked: bool,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Folder(&folder_id)).await?;
    db::set_folder_track(&mut access, &folder_id, &track_id, linked).await?;
    access.commit().await?;
    Ok(())
}

fn folder_name(name: &str) -> Result<&str, CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::Invalid(
            "A folder name cannot be empty".into(),
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices};

    const OWNER: &str = "11111111-2222-3333-4444-555555555555";

    async fn services(directory: &std::path::Path) -> AppServices {
        let db = database::init_app_db_at(directory).await.unwrap();
        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::install_test_session(&state_db.0, OWNER).await;
        auth::bootstrap_headless_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        AppServices::headless(db, state_db, storage, repo, workspaces)
    }

    async fn call(services: &AppServices, name: &str, args: Value) -> Value {
        dispatch(services, name, &args)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error:?}"))
    }

    /// A venue with two songs (each with a score there) and one track that is
    /// not a song of it.
    async fn venue_with_songs(services: &AppServices) -> String {
        let venue = call(
            services,
            "create_venue",
            json!({ "name": "Room", "description": null }),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES
                ('song-a', ?1, 'a', '/a.wav'),
                ('song-b', ?1, 'b', '/b.wav'),
                ('elsewhere', ?1, 'c', '/c.wav')",
        )
        .bind(OWNER)
        .execute(&services.db.0)
        .await
        .unwrap();
        for track in ["song-a", "song-b"] {
            call(
                services,
                "create_score",
                json!({
                    "requestId": uuid::Uuid::new_v4().to_string(),
                    "trackId": track,
                    "venueId": venue,
                    "name": null,
                }),
            )
            .await;
        }
        venue
    }

    async fn folders(services: &AppServices, venue: &str) -> Vec<(String, Vec<String>)> {
        let listed = call(services, "list_folders", json!({ "venueId": venue })).await;
        listed
            .as_array()
            .unwrap()
            .iter()
            .map(|folder| {
                let mut songs: Vec<String> = folder["trackIds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| id.as_str().unwrap().to_string())
                    .collect();
                songs.sort();
                (folder["name"].as_str().unwrap().to_string(), songs)
            })
            .collect()
    }

    async fn count(services: &AppServices, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(&services.db.0)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_song_in_two_folders_is_one_song_and_deleting_a_folder_keeps_it() {
        let directory = tempfile::tempdir().unwrap();
        let services = services(directory.path()).await;
        let venue = venue_with_songs(&services).await;

        let create = |name: &'static str| {
            let services = &services;
            let venue = venue.clone();
            async move {
                call(
                    services,
                    "create_folder",
                    json!({ "venueId": venue, "name": name }),
                )
                .await["id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            }
        };
        let warmup = create("  Warm up ").await;
        let peak = create("Peak").await;
        assert!(dispatch(
            &services,
            "create_folder",
            &json!({ "venueId": venue, "name": "  " })
        )
        .await
        .is_err());

        for (folder, track) in [(&warmup, "song-a"), (&peak, "song-a"), (&peak, "song-b")] {
            call(
                &services,
                "set_folder_track",
                json!({ "folderId": folder, "trackId": track, "linked": true }),
            )
            .await;
        }
        // Linking twice is one link.
        call(
            &services,
            "set_folder_track",
            json!({ "folderId": warmup, "trackId": "song-a", "linked": true }),
        )
        .await;
        // A track with no score in the venue is not one of its songs.
        assert!(dispatch(
            &services,
            "set_folder_track",
            &json!({ "folderId": warmup, "trackId": "elsewhere", "linked": true }),
        )
        .await
        .is_err());

        assert_eq!(
            folders(&services, &venue).await,
            vec![
                ("Peak".to_string(), vec!["song-a".into(), "song-b".into()]),
                ("Warm up".to_string(), vec!["song-a".into()]),
            ]
        );

        call(
            &services,
            "rename_folder",
            json!({ "folderId": peak, "name": "Encore" }),
        )
        .await;
        call(
            &services,
            "set_folder_track",
            json!({ "folderId": peak, "trackId": "song-b", "linked": false }),
        )
        .await;
        assert_eq!(
            folders(&services, &venue).await,
            vec![
                ("Encore".to_string(), vec!["song-a".into()]),
                ("Warm up".to_string(), vec!["song-a".into()]),
            ]
        );

        let scores_before = count(&services, "SELECT COUNT(*) FROM scores").await;
        call(&services, "delete_folder", json!({ "folderId": peak })).await;
        assert_eq!(
            folders(&services, &venue).await,
            vec![("Warm up".to_string(), vec!["song-a".into()])]
        );
        assert_eq!(
            count(&services, "SELECT COUNT(*) FROM folder_tracks").await,
            1
        );
        assert_eq!(
            count(&services, "SELECT COUNT(*) FROM scores").await,
            scores_before
        );
        assert_eq!(count(&services, "SELECT COUNT(*) FROM tracks").await, 3);
        assert!(
            dispatch(&services, "delete_folder", &json!({ "folderId": peak }))
                .await
                .is_err()
        );
    }

    /// The migration gave every venue a "Testing" folder holding every song it
    /// had. Run against a library migrated up to just before it.
    #[tokio::test]
    async fn the_migration_puts_every_venues_songs_in_testing() {
        use sqlx::migrate::Migrator;
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("folders.db"))
                    .create_if_missing(true)
                    // As the app migrates: see `database::migrate_app_db_at`.
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        let migrator: Migrator = sqlx::migrate!("./migrations");
        let folders = migrator
            .iter()
            .position(|migration| migration.description == "folders")
            .expect("the folders migration");
        let mut before: Migrator = sqlx::migrate!("./migrations");
        before.migrations = migrator.migrations[..folders].to_vec().into();
        before.run(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO venues (id, uid, name) VALUES ('hall', 'u', 'Hall'), ('empty', 'u', 'Empty');
             INSERT INTO tracks (id, uid, track_hash, file_path) VALUES
                ('one', 'u', '1', '/1.wav'), ('two', 'u', '2', '/2.wav');
             INSERT INTO scores (id, uid, track_id, venue_id) VALUES
                ('s1', 'u', 'one', 'hall'), ('s2', 'u', 'one', 'hall'), ('s3', 'u', 'two', 'hall');",
        )
        .execute(&pool)
        .await
        .unwrap();
        migrator.run(&pool).await.unwrap();

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT folder.venue_id, folder.name, COALESCE(group_concat(link.track_id), '')
             FROM folders folder
             LEFT JOIN (SELECT * FROM folder_tracks ORDER BY track_id) link
                    ON link.folder_id = folder.id
             GROUP BY folder.id ORDER BY folder.venue_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("empty".into(), "Testing".into(), String::new()),
                ("hall".into(), "Testing".into(), "one,two".into()),
            ]
        );
    }
}
