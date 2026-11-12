//! Songs imported by 0.7.0 or earlier are still in the format they arrived
//! in: a file import as Luma's own copy of the original, a DJ-library import
//! as a path into that library. Each media pass converts this account's
//! remaining ones into the stored format ([`crate::audio::stored`]), so
//! upload and every reader only ever meet that one.
//!
//! DELETE THIS FILE, and its call in `media.rs`, once no install that Luma
//! still updates can hold such a song: that is, once every install has run a
//! release containing this file for at least one media pass. After that the
//! only way to have one is to update straight from 0.7.0 or earlier, and what
//! happens then is that the song is not uploaded until it is imported again.

use std::path::Path;

use sqlx::SqlitePool;
use uuid::Uuid;

use super::error::SyncError;
use crate::database::local::track_access::{Operate, VisibleTrackAccess};
use crate::database::local::write_admission::{enter_remote_writes, leave_remote_writes};
use crate::storage::StorageRoot;

#[derive(sqlx::FromRow)]
struct LegacyAudio {
    id: String,
    file_path: String,
}

/// Convert every song of `uid` whose audio is on this machine in another
/// format. A song whose file is gone — a DJ library that has moved — stays as
/// it is. One song's failure is collected into `errors` and does not stop the
/// rest.
pub(super) async fn convert(
    pool: &SqlitePool,
    storage: &StorageRoot,
    uid: &str,
    errors: &mut Vec<String>,
) -> Result<(), SyncError> {
    let stored = format!("%.{}", crate::audio::stored::EXTENSION);
    let rows = sqlx::query_as::<_, LegacyAudio>(
        "SELECT id, file_path FROM tracks
         WHERE uid = ? AND file_path NOT LIKE ? AND file_path NOT LIKE '%.stub'",
    )
    .bind(uid)
    .bind(&stored)
    .fetch_all(pool)
    .await?;

    let tracks_dir = storage.tracks_dir();
    for row in rows {
        let source = Path::new(&row.file_path).to_path_buf();
        if !source.is_file() {
            continue;
        }
        let dest = tracks_dir.join(format!(
            "{}.{}",
            Uuid::new_v4(),
            crate::audio::stored::EXTENSION
        ));
        let converted = {
            let (source, dest) = (source.clone(), dest.clone());
            tokio::task::spawn_blocking(move || crate::audio::stored::store(&source, &dest, None))
                .await
                .map_err(|error| error.to_string())
                .and_then(|converted| converted)
        };
        if let Err(error) = converted {
            errors.push(format!("convert audio {}: {error}", row.id));
            continue;
        }

        // Do not publish a conversion across a track edit, deletion or
        // account switch.
        let published = publish(pool, uid, &row, &dest).await;
        match published {
            Ok(true) => {}
            Ok(false) => {
                let _ = std::fs::remove_file(&dest);
                continue;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&dest);
                return Err(error);
            }
        }

        // The original is Luma's to delete only if it is Luma's own copy and
        // no other row still plays from it. A DJ library's file is not.
        let still_used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE file_path = ?")
            .bind(&row.file_path)
            .fetch_one(pool)
            .await?;
        if still_used == 0 && source.starts_with(&tracks_dir) {
            if let Err(error) = std::fs::remove_file(&source) {
                errors.push(format!("remove original audio {}: {error}", row.id));
            }
        }
    }
    Ok(())
}

/// Point `row` at its converted audio. `false` when the row changed under the
/// conversion and was left alone.
async fn publish(
    pool: &SqlitePool,
    uid: &str,
    row: &LegacyAudio,
    dest: &Path,
) -> Result<bool, SyncError> {
    let mut access = VisibleTrackAccess::<Operate>::operate(pool, &row.id)
        .await
        .map_err(SyncError::Local)?;
    if access.principal() != uid {
        return Err(SyncError::AuthRequired);
    }
    let current: Option<String> = sqlx::query_scalar("SELECT file_path FROM tracks WHERE id = ?")
        .bind(&row.id)
        .fetch_optional(access.connection())
        .await?;
    if current.as_deref() != Some(row.file_path.as_str()) {
        return Ok(false);
    }
    // The path is this device's own; it is not an edit to send to the server.
    enter_remote_writes(access.connection())
        .await
        .map_err(SyncError::Local)?;
    sqlx::query("UPDATE tracks SET file_path = ? WHERE id = ?")
        .bind(dest.to_string_lossy().as_ref())
        .bind(&row.id)
        .execute(access.connection())
        .await?;
    leave_remote_writes(access.connection())
        .await
        .map_err(SyncError::Local)?;
    access.commit().await.map_err(SyncError::Local)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::local::{auth, database};

    async fn path(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar("SELECT file_path FROM tracks WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Luma's own copy of an original is replaced; a DJ library's file is
    /// converted from and left where it is. A second pass has nothing to do.
    #[tokio::test]
    async fn old_songs_move_to_the_stored_format_once() {
        let directory = tempfile::tempdir().unwrap();
        let pool = database::init_app_db_at(directory.path()).await.unwrap().0;
        auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        let storage = StorageRoot::from_path(directory.path().to_owned());
        storage.ensure_track_storage().unwrap();
        let own = storage.tracks_dir().join("own.wav");
        let library = directory.path().join("library.wav");
        crate::test_support::write_wav(&own, 1);
        crate::test_support::write_wav(&library, 1);
        for (id, file) in [("own", &own), ("library", &library)] {
            sqlx::query(
                "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES (?, 'alice', ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(file.to_string_lossy().as_ref())
            .execute(&pool)
            .await
            .unwrap();
        }

        let mut errors = Vec::new();
        convert(&pool, &storage, "alice", &mut errors)
            .await
            .unwrap();
        assert_eq!(errors, Vec::<String>::new());
        let converted = [path(&pool, "own").await, path(&pool, "library").await];
        for file in &converted {
            assert!(file.ends_with(".ogg"), "{file}");
            assert!(Path::new(file).starts_with(storage.tracks_dir()), "{file}");
            assert!(Path::new(file).is_file(), "{file}");
        }
        assert!(!own.exists(), "Luma's copy of the original was kept");
        assert!(library.exists(), "a DJ library's file was deleted");

        convert(&pool, &storage, "alice", &mut errors)
            .await
            .unwrap();
        assert_eq!(errors, Vec::<String>::new());
        assert_eq!(
            [path(&pool, "own").await, path(&pool, "library").await],
            converted
        );
    }
}
