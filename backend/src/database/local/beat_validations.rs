//! Human-reviewed beat grids, independent of replaceable analysis artifacts.
use sqlx::SqlitePool;

use super::track_access::{Operate, Read, VisibleTrackAccess};
use crate::models::node_graph::BeatGrid;
use crate::models::tracks::{BeatValidation, BeatValidationReason, BeatValidationVerdict};

pub async fn get(pool: &SqlitePool, track_id: &str) -> Result<Option<BeatValidation>, String> {
    let mut access = VisibleTrackAccess::<Read>::read(pool, track_id).await?;
    let principal = access.principal().map(str::to_owned);
    let row: Option<(String, BeatValidationVerdict, Option<BeatValidationReason>)> =
        sqlx::query_as(
            "SELECT v.grid_json, v.verdict, v.reason FROM track_beat_validations v
         JOIN tracks t ON t.id = v.track_id AND t.track_hash = v.track_hash
         WHERE v.track_id = ? AND v.uid IS ? AND v.verdict != 'unreviewed'",
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
    let principal = access.principal().map(str::to_owned);
    let owns: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE id = ? AND uid IS ?)")
            .bind(track_id)
            .bind(&principal)
            .fetch_one(access.connection())
            .await
            .map_err(|e| format!("Failed to check track owner: {e}"))?;
    if !owns {
        return Err("Only the track owner can validate its beat grid".into());
    }
    let current =
        crate::services::tracks::get_track_beats_for_connection(access.connection(), track_id)
            .await?
            .ok_or("This track has no beat grid to validate")?;
    if &current != reviewed {
        return Err("The beat grid changed. Reopen the track before validating it.".into());
    }
    if current.beats.is_empty() || current.downbeats.is_empty() {
        return Err("This track has no beat grid to validate".into());
    }
    let json = serde_json::to_string(reviewed).map_err(|e| format!("Invalid beat grid: {e}"))?;
    sqlx::query(
        "INSERT INTO track_beat_validations
         (track_id, uid, track_hash, grid_json, processor_version, verdict, reason)
         SELECT t.id, t.uid, t.track_hash, ?, b.processor_version, ?, ?
         FROM tracks t JOIN track_beats b ON b.track_id = t.id WHERE t.id = ?
         ON CONFLICT(track_id) DO UPDATE SET
         uid = excluded.uid, track_hash = excluded.track_hash,
         grid_json = excluded.grid_json, processor_version = excluded.processor_version,
         verdict = excluded.verdict, reason = excluded.reason",
    )
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
    #[tokio::test]
    async fn migration_preserves_approvals_and_undos() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql("CREATE TABLE tracks (id TEXT PRIMARY KEY); INSERT INTO tracks VALUES ('yes'), ('undo');")
            .execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/20260909000000_track_beat_validations.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO track_beat_validations (track_id, track_hash, grid_json, processor_version, approved) VALUES ('yes', 'hash', '{}', 7, 1), ('undo', 'hash', '{}', 7, 0);")
            .execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/20260909010000_beat_validation_verdicts.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as("SELECT track_id, verdict, reason, processor_version FROM track_beat_validations ORDER BY track_id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(
            rows,
            vec![
                ("undo".into(), "unreviewed".into(), None, 7),
                ("yes".into(), "correct".into(), None, 7)
            ]
        );
        assert!(sqlx::query(
            "UPDATE track_beat_validations SET reason = 'drift' WHERE track_id = 'yes'"
        )
        .execute(&pool)
        .await
        .is_err());
        sqlx::query("UPDATE track_beat_validations SET verdict = 'incorrect', reason = 'offset' WHERE track_id = 'yes'").execute(&pool).await.unwrap();
        let version: i64 =
            sqlx::query_scalar("SELECT version FROM track_beat_validations WHERE track_id = 'yes'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(version, 2);
    }
}
