//! Human-reviewed beat grids, independent of replaceable analysis artifacts.
use sqlx::SqlitePool;

use super::track_access::{Operate, Read, VisibleTrackAccess};
use crate::models::beat_grid::BeatGrid;
use crate::models::tracks::{BeatValidation, BeatValidationReason, BeatValidationVerdict};

pub async fn get(pool: &SqlitePool, track_id: &str) -> Result<Option<BeatValidation>, String> {
    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let principal = access.principal().to_owned();
    let row: Option<(String, BeatValidationVerdict, Option<BeatValidationReason>)> =
        sqlx::query_as(
            "SELECT v.grid_json, v.verdict, v.reason FROM track_beat_validations v
         JOIN tracks t ON t.id = v.track_id AND t.track_hash = v.track_hash
         WHERE v.track_id = ? AND v.uid = ? AND v.verdict != 'unreviewed'",
        )
        .bind(track_id)
        .bind(principal)
        .fetch_optional(access.connection())
        .await
        .map_err(|e| format!("Failed to load beat validation: {e}"))?;
    access.finish().await?;
    row.map(|(json, verdict, reason)| {
        serde_json::from_str(&json)
            .map(|grid| BeatValidation {
                grid,
                verdict,
                reason,
            })
            .map_err(|e| format!("Invalid validated grid: {e}"))
    })
    .transpose()
}

pub async fn set(
    pool: &SqlitePool,
    track_id: &str,
    reviewed: &BeatGrid,
    verdict: BeatValidationVerdict,
    reason: Option<BeatValidationReason>,
) -> Result<(), String> {
    // The ownership check, grid comparison and snapshot share a write transaction:
    // reanalysis or an account switch cannot race the grid the person reviewed.
    let mut access = VisibleTrackAccess::<Operate>::operate(pool, track_id).await?;
    let principal = access.principal().to_owned();
    let owns: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE id = ? AND uid = ?)")
            .bind(track_id)
            .bind(&principal)
            .fetch_one(access.connection())
            .await
            .map_err(|e| format!("Failed to check track owner: {e}"))?;
    if !owns {
        return Err("Only the track owner can validate its beat grid".into());
    }
    let current = crate::database::local::tracks::get_track_beats_for_connection(
        access.connection(),
        track_id,
    )
    .await?
    .ok_or("This track has no beat grid to validate")?;
    if &current != reviewed {
        return Err("The beat grid changed. Reopen the track before validating it.".into());
    }
    if current.beats.is_empty() || current.downbeats.is_empty() {
        return Err("This track has no beat grid to validate".into());
    }
    let json = serde_json::to_string(reviewed).map_err(|e| format!("Invalid beat grid: {e}"))?;
    // A track has one validation, found by its track: a second review changes
    // that row, and only the first one makes a row, under its own id. The
    // processor is the current grid's, the track's newest.
    sqlx::query(
        "INSERT INTO track_beat_validations
         (id, track_id, uid, track_hash, grid_json, processor_version, verdict, reason)
         SELECT ?, t.id, t.uid, t.track_hash, ?, b.processor_version, ?, ?
         FROM tracks t JOIN track_beats b ON b.id = (
             SELECT id FROM track_beats WHERE track_id = t.id
             ORDER BY created_at DESC, id DESC LIMIT 1)
         WHERE t.id = ?
         ON CONFLICT(track_id) DO UPDATE SET
         uid = excluded.uid, track_hash = excluded.track_hash,
         grid_json = excluded.grid_json, processor_version = excluded.processor_version,
         verdict = excluded.verdict, reason = excluded.reason",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(json)
    .bind(verdict)
    .bind(reason)
    .bind(track_id)
    .execute(access.connection())
    .await
    .map_err(|e| format!("Failed to save beat validation: {e}"))?;
    access.commit().await
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;

    /// A track has one validation, found by its track: a second review
    /// changes that row instead of making one, and the row's id is its own.
    #[tokio::test]
    async fn a_second_review_changes_the_tracks_one_validation() {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("validations.db"))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path)
                 VALUES ('reviewed', 'alice', 'audio-hash', '/audio.ogg');
             INSERT INTO track_beats
                 (id, track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset,
                  beats_per_bar, processor_version)
                 VALUES ('grid', 'reviewed', 'alice', '[0,0.5,1,1.5,2]', '[0,2]', 120.0, 0.0, 4, 7)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let grid = BeatGrid {
            beats: vec![0.0, 0.5, 1.0, 1.5, 2.0],
            downbeats: vec![0.0, 2.0],
            bpm: 120.0,
            downbeat_offset: 0.0,
            beats_per_bar: 4,
        };
        let rows = || async {
            sqlx::query_as::<_, (String, String, i64)>(
                "SELECT id, verdict, processor_version FROM track_beat_validations
                 WHERE track_id = 'reviewed'",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };

        set(
            &pool,
            "reviewed",
            &grid,
            BeatValidationVerdict::Correct,
            None,
        )
        .await
        .unwrap();
        let first = rows().await;
        assert_eq!(first.len(), 1);
        assert_ne!(first[0].0, "reviewed", "the id is its own, not its track's");
        assert_eq!(first[0].2, 7, "the grid's processor is recorded");

        set(
            &pool,
            "reviewed",
            &grid,
            BeatValidationVerdict::Incorrect,
            Some(BeatValidationReason::Tempo),
        )
        .await
        .unwrap();
        let second = rows().await;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, first[0].0, "the same row, changed");
        assert_eq!(second[0].1, "incorrect");
    }
}
