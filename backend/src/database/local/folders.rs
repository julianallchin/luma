//! A venue's folders and the songs they hold (`folders`, `folder_tracks`).
//!
//! A link names a track that is a song of the folder's venue: one with a score
//! there. That is checked here, at the write. A link whose song later loses its
//! last score in the venue stays, and shows again if the song comes back.

use std::collections::HashMap;

use uuid::Uuid;

use crate::database::local::deletes;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::folders::Folder;

/// The venue's folders, by name, each with its songs.
pub async fn list_folders(access: &mut impl AuthorizedVenue) -> Result<Vec<Folder>, String> {
    let venue_id = access.venue_id().to_owned();
    let folders: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, name FROM folders WHERE venue_id = ?
         ORDER BY name COLLATE NOCASE, created_at, id",
    )
    .bind(&venue_id)
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to list folders: {error}"))?;
    let links: Vec<(String, String)> =
        sqlx::query_as("SELECT folder_id, track_id FROM folder_tracks WHERE venue_id = ?")
            .bind(&venue_id)
            .fetch_all(&mut *access.connection())
            .await
            .map_err(|error| format!("Failed to list folder songs: {error}"))?;
    let mut songs: HashMap<String, Vec<String>> = HashMap::new();
    for (folder_id, track_id) in links {
        songs.entry(folder_id).or_default().push(track_id);
    }
    Ok(folders
        .into_iter()
        .map(|(id, name)| Folder {
            track_ids: songs.remove(&id).unwrap_or_default(),
            id,
            venue_id: venue_id.clone(),
            name,
        })
        .collect())
}

/// A new, empty folder.
pub async fn create_folder(
    access: &mut VenueAccess<'_, Write>,
    name: &str,
) -> Result<Folder, String> {
    let folder = Folder {
        id: Uuid::new_v4().to_string(),
        venue_id: access.venue_id().to_owned(),
        name: name.to_owned(),
        track_ids: Vec::new(),
    };
    let uid = access.principal().map(str::to_owned);
    sqlx::query("INSERT INTO folders (id, uid, venue_id, name) VALUES (?, ?, ?, ?)")
        .bind(&folder.id)
        .bind(uid)
        .bind(&folder.venue_id)
        .bind(&folder.name)
        .execute(&mut *access.connection())
        .await
        .map_err(|error| format!("Failed to create folder: {error}"))?;
    Ok(folder)
}

/// Answers how many folders it renamed: one, or none when `id` is not this
/// venue's.
pub async fn rename_folder(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    name: &str,
) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    sqlx::query("UPDATE folders SET name = ? WHERE id = ? AND venue_id = ?")
        .bind(name)
        .bind(id)
        .bind(venue_id)
        .execute(&mut *access.connection())
        .await
        .map(|done| done.rows_affected())
        .map_err(|error| format!("Failed to rename folder: {error}"))
}

/// Delete a folder and its links. Its songs and their scores stay.
pub async fn delete_folder(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, String> {
    let venue_id = access.venue_id().to_owned();
    deletes::delete_where(
        access.connection(),
        "folders",
        "id = ? AND venue_id = ?",
        &[id, &venue_id],
    )
    .await
    .map(|deleted| deleted as u64)
}

/// Put a song in a folder, or take it out. Idempotent both ways.
pub async fn set_folder_track(
    access: &mut VenueAccess<'_, Write>,
    folder_id: &str,
    track_id: &str,
    linked: bool,
) -> Result<(), String> {
    let venue_id = access.venue_id().to_owned();
    if !linked {
        deletes::delete_where(
            access.connection(),
            "folder_tracks",
            "folder_id = ? AND track_id = ?",
            &[folder_id, track_id],
        )
        .await?;
        return Ok(());
    }
    let is_song: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scores WHERE track_id = ? AND venue_id = ?)",
    )
    .bind(track_id)
    .bind(&venue_id)
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to read the venue's songs: {error}"))?;
    if !is_song {
        return Err("The track is not a song of this venue".into());
    }
    let uid = access.principal().map(str::to_owned);
    sqlx::query(
        "INSERT INTO folder_tracks (folder_id, track_id, uid, venue_id) VALUES (?, ?, ?, ?)
         ON CONFLICT (folder_id, track_id) DO NOTHING",
    )
    .bind(folder_id)
    .bind(track_id)
    .bind(uid)
    .bind(venue_id)
    .execute(&mut *access.connection())
    .await
    .map_err(|error| format!("Failed to put the song in the folder: {error}"))?;
    Ok(())
}
