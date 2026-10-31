//! Cover art for a track that is not in the library yet — a row in a DJ
//! library's browser.
//!
//! An imported track's art is extracted once, at import, to a file the UI
//! reads by path. A source row has no such file: its cover lives wherever its
//! library keeps it — a thumbnail under Rekordbox's `share` directory, a PNG
//! blob in Engine DJ's `AlbumArt` table — or only inside the audio file's own
//! tags. This answers "what are this row's cover bytes" for all three, so the
//! UI can decode whatever comes back the same way.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::services::tracks::embedded_picture;

/// Where a source library keeps a track's cover, when it keeps one itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtworkRef {
    /// An image file on disk (Rekordbox's thumbnails).
    File(String),
    /// A row of Engine DJ's `AlbumArt` table.
    EngineDj {
        library_path: String,
        album_art_id: i64,
    },
}

/// The encoded cover for one source row: the library's own image first,
/// else the picture embedded in `audio_path`. `None` when neither has one.
///
/// A library image that has gone missing is not an error — Rekordbox's
/// thumbnails are a cache the user can clear — so it falls through to the
/// tags rather than failing the row.
pub async fn artwork_bytes(
    art: Option<ArtworkRef>,
    audio_path: Option<String>,
) -> Result<Option<Vec<u8>>, String> {
    let stored = match art {
        Some(ArtworkRef::File(path)) => tokio::fs::read(&path).await.ok(),
        Some(ArtworkRef::EngineDj {
            library_path,
            album_art_id,
        }) => {
            let pool = crate::engine_dj::db::open_engine_db(&library_path).await?;
            let bytes = crate::engine_dj::db::album_art(&pool, album_art_id).await;
            pool.close().await;
            bytes?
        }
        None => None,
    };
    if let Some(bytes) = stored.filter(|bytes| !bytes.is_empty()) {
        return Ok(Some(bytes));
    }
    let Some(audio_path) = audio_path else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        embedded_picture(Path::new(&audio_path)).map(|picture| picture.into_data())
    })
    .await
    .map_err(|error| format!("embedded art read failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Engine DJ library is a folder holding `Database2/m.db`; this is
    /// the slice of its schema the cover read touches.
    async fn engine_library(dir: &Path, blob: Option<&[u8]>) -> String {
        let db_dir = dir.join("Database2");
        std::fs::create_dir_all(&db_dir).unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(db_dir.join("m.db"))
            .create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        sqlx::query("CREATE TABLE AlbumArt (id INTEGER PRIMARY KEY, hash TEXT, albumArt BLOB)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO AlbumArt (id, hash, albumArt) VALUES (7, 'h', ?)")
            .bind(blob)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        dir.to_string_lossy().into_owned()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("luma-artwork-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn engine_dj_covers_come_from_its_album_art_table() {
        let dir = scratch("engine");
        let library_path = engine_library(&dir, Some(b"\x89PNG cover")).await;
        let art = ArtworkRef::EngineDj {
            library_path,
            album_art_id: 7,
        };
        assert_eq!(
            artwork_bytes(Some(art), None).await.unwrap().as_deref(),
            Some(&b"\x89PNG cover"[..])
        );
    }

    #[tokio::test]
    async fn an_empty_library_image_falls_through_to_the_tags() {
        // Engine keeps a row per hash even when it found no picture, and a
        // file without tags has none either: no cover, and no error.
        let dir = scratch("engine-empty");
        let library_path = engine_library(&dir, None).await;
        let audio = dir.join("untagged.wav");
        std::fs::write(&audio, b"RIFF").unwrap();
        let art = ArtworkRef::EngineDj {
            library_path,
            album_art_id: 7,
        };
        let bytes = artwork_bytes(Some(art), Some(audio.to_string_lossy().into_owned()))
            .await
            .unwrap();
        assert_eq!(bytes, None);
    }

    #[tokio::test]
    async fn a_missing_thumbnail_is_not_an_error() {
        let dir = scratch("missing");
        let art = ArtworkRef::File(dir.join("gone.jpg").to_string_lossy().into_owned());
        assert_eq!(artwork_bytes(Some(art), None).await.unwrap(), None);
    }
}
