/// Subprocess bridge: reads Rekordbox master.db and outputs JSON to stdout.
/// Called by the Luma Tauri app since rbox's libsqlite3-sys conflicts with sqlx.
///
/// Usage:
///   rekordbox_read library-info
///   rekordbox_read list-tracks
///   rekordbox_read list-playlists
///   rekordbox_read playlist-tracks <playlist_id>
///   rekordbox_read search <query>
use rbox::MasterDb;
use serde::Serialize;
use std::collections::HashMap;
use std::process;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RbLibraryInfo {
    track_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RbTrack {
    /// Rekordbox content ID (string in DB)
    id: String,
    /// Stable UUID for cross-library matching
    uuid: String,
    /// Absolute file path
    file_path: Option<String>,
    /// Bare filename
    filename: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    /// BPM as float (stored as int*100 in DB, converted here)
    bpm: Option<f64>,
    /// Duration in seconds (integer in DB, converted to float for consistency)
    duration_seconds: Option<f64>,
    file_size: Option<i32>,
    sample_rate: Option<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RbPlaylist {
    id: String,
    name: String,
    parent_id: Option<String>,
    track_count: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: rekordbox_read <command> [args...]");
        eprintln!("Commands: library-info, list-tracks, list-playlists, playlist-tracks <id>, search <query>");
        process::exit(1);
    }

    let mut db = match MasterDb::open() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("{}", serde_json::json!({"error": e.to_string()}));
            process::exit(1);
        }
    };

    let result = match args[1].as_str() {
        "library-info" => cmd_library_info(&mut db),
        "list-tracks" => cmd_list_tracks(&mut db),
        "list-playlists" => cmd_list_playlists(&mut db),
        "playlist-tracks" => {
            let id = args.get(2).map(|s| s.as_str()).unwrap_or("");
            cmd_playlist_tracks(&mut db, id)
        }
        "search" => {
            let query = args.get(2).map(|s| s.as_str()).unwrap_or("");
            cmd_search(&mut db, query)
        }
        other => {
            eprintln!("Unknown command: {}", other);
            process::exit(1);
        }
    };

    if let Err(e) = result {
        let err_json = serde_json::json!({"error": e});
        println!("{}", err_json);
        process::exit(1);
    }
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
    c: &rbox::masterdb::models::DjmdContent,
    artist_map: &HashMap<String, String>,
    album_map: &HashMap<String, String>,
) -> RbTrack {
    let filename = c
        .folder_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|f| f.to_string_lossy().to_string());

    RbTrack {
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

fn cmd_library_info(db: &mut MasterDb) -> Result<(), String> {
    let contents = db.get_contents().map_err(|e| e.to_string())?;
    let info = RbLibraryInfo {
        track_count: contents.len(),
    };
    println!(
        "{}",
        serde_json::to_string(&info).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn cmd_list_tracks(db: &mut MasterDb) -> Result<(), String> {
    let artist_map = build_artist_map(db);
    let album_map = build_album_map(db);
    let contents = db.get_contents().map_err(|e| e.to_string())?;
    let tracks: Vec<RbTrack> = contents
        .iter()
        .map(|c| content_to_track(c, &artist_map, &album_map))
        .collect();
    println!(
        "{}",
        serde_json::to_string(&tracks).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn cmd_list_playlists(db: &mut MasterDb) -> Result<(), String> {
    let playlists = db.get_playlists().map_err(|e| e.to_string())?;
    let out: Vec<RbPlaylist> = playlists
        .iter()
        .map(|p| {
            let track_count = db
                .get_playlist_contents(&p.id)
                .map(|c| c.len())
                .unwrap_or(0);
            RbPlaylist {
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
        .collect();
    println!(
        "{}",
        serde_json::to_string(&out).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn cmd_playlist_tracks(db: &mut MasterDb, playlist_id: &str) -> Result<(), String> {
    let artist_map = build_artist_map(db);
    let album_map = build_album_map(db);
    let contents = db
        .get_playlist_contents(playlist_id)
        .map_err(|e| e.to_string())?;
    let tracks: Vec<RbTrack> = contents
        .iter()
        .map(|c| content_to_track(c, &artist_map, &album_map))
        .collect();
    println!(
        "{}",
        serde_json::to_string(&tracks).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn cmd_search(db: &mut MasterDb, query: &str) -> Result<(), String> {
    let artist_map = build_artist_map(db);
    let album_map = build_album_map(db);
    let contents = db.get_contents().map_err(|e| e.to_string())?;
    let lq = query.to_lowercase();
    let tracks: Vec<RbTrack> = contents
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
        .collect();
    println!(
        "{}",
        serde_json::to_string(&tracks).map_err(|e| e.to_string())?
    );
    Ok(())
}
