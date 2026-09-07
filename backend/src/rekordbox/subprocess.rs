use rekordbox::rbox::MasterDb;
use std::collections::HashMap;

use super::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};

fn with_db<T>(f: impl FnOnce(&mut MasterDb) -> Result<T, String>) -> Result<T, String> {
    let mut db = MasterDb::open().map_err(|e| format!("Failed to open Rekordbox DB: {}", e))?;
    f(&mut db)
}

fn build_artist_map(db: &mut MasterDb) -> HashMap<String, String> {
    db.get_artists()
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect()
}

fn build_album_map(db: &mut MasterDb) -> HashMap<String, String> {
    db.get_albums()
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect()
}

fn content_to_track(
    c: &rekordbox::rbox::masterdb::models::DjmdContent,
    artist_map: &HashMap<String, String>,
    album_map: &HashMap<String, String>,
) -> RekordboxTrack {
    let filename = c
        .folder_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|f| f.to_string_lossy().to_string());

    RekordboxTrack {
        id: c.id.clone(),
        uuid: c.uuid.clone(),
        file_path: c.folder_path.clone(),
        filename,
        title: c.title.clone(),
        artist: c
            .artist_id
            .as_deref()
            .and_then(|id| artist_map.get(id))
            .cloned(),
        album: c
            .album_id
            .as_deref()
            .and_then(|id| album_map.get(id))
            .cloned(),
        bpm: c.bpm.map(|b| b as f64 / 100.0),
        duration_seconds: c.length.map(|s| s as f64),
        file_size: c.file_size,
        sample_rate: c.sample_rate,
    }
}

pub fn get_library_info() -> Result<RekordboxLibraryInfo, String> {
    with_db(|db| {
        let contents = db.get_contents().map_err(|e| e.to_string())?;
        Ok(RekordboxLibraryInfo {
            track_count: contents.len(),
        })
    })
}

pub fn list_tracks() -> Result<Vec<RekordboxTrack>, String> {
    with_db(|db| {
        let artist_map = build_artist_map(db);
        let album_map = build_album_map(db);
        let contents = db.get_contents().map_err(|e| e.to_string())?;
        Ok(contents
            .iter()
            .map(|c| content_to_track(c, &artist_map, &album_map))
            .collect())
    })
}

pub fn list_playlists() -> Result<Vec<RekordboxPlaylist>, String> {
    with_db(|db| {
        let playlists = db.get_playlists().map_err(|e| e.to_string())?;
        Ok(playlists
            .iter()
            .map(|p| {
                let track_count = db
                    .get_playlist_contents(&p.id)
                    .map(|c| c.len())
                    .unwrap_or(0);
                RekordboxPlaylist {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    parent_id: if p.parent_id == "root" {
                        None
                    } else {
                        Some(p.parent_id.clone())
                    },
                    track_count,
                }
            })
            .collect())
    })
}

pub fn get_playlist_tracks(playlist_id: &str) -> Result<Vec<RekordboxTrack>, String> {
    with_db(|db| {
        let artist_map = build_artist_map(db);
        let album_map = build_album_map(db);
        let contents = db
            .get_playlist_contents(playlist_id)
            .map_err(|e| e.to_string())?;
        Ok(contents
            .iter()
            .map(|c| content_to_track(c, &artist_map, &album_map))
            .collect())
    })
}

pub fn search_tracks(query: &str) -> Result<Vec<RekordboxTrack>, String> {
    with_db(|db| {
        let artist_map = build_artist_map(db);
        let album_map = build_album_map(db);
        let contents = db.get_contents().map_err(|e| e.to_string())?;
        let lq = query.to_lowercase();
        Ok(contents
            .iter()
            .filter(|c| {
                let title_match = c
                    .title
                    .as_deref()
                    .map(|t| t.to_lowercase().contains(&lq))
                    .unwrap_or(false);
                let artist_match = c
                    .artist_id
                    .as_deref()
                    .and_then(|id| artist_map.get(id))
                    .map(|name| name.to_lowercase().contains(&lq))
                    .unwrap_or(false);
                let file_match = c
                    .folder_path
                    .as_deref()
                    .map(|p| p.to_lowercase().contains(&lq))
                    .unwrap_or(false);
                title_match || artist_match || file_match
            })
            .take(200)
            .map(|c| content_to_track(c, &artist_map, &album_map))
            .collect())
    })
}
