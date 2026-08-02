use sqlx::{SqliteConnection, SqlitePool};

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::scores::{Score, ScoreSummary, TrackScore};

/// List all track_scores for a (track, venue) pair
pub async fn get_scores_for_track(
    access: &mut impl AuthorizedVenue,
    track_id: &str,
) -> Result<Vec<TrackScore>, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT track_scores.id, track_scores.uid, track_scores.score_id, track_scores.pattern_id, track_scores.start_time, track_scores.end_time, track_scores.z_index, track_scores.blend_mode, track_scores.args_json, track_scores.created_at, track_scores.updated_at
         FROM track_scores
         JOIN scores ON track_scores.score_id = scores.id
         WHERE scores.track_id = ? AND scores.venue_id = ?
         ORDER BY track_scores.start_time ASC, track_scores.z_index ASC",
    )
    .bind(track_id)
    .bind(access.venue_id().to_owned())
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to list track_scores: {}", e))
}

/// Return the venue_id of the most-recently-updated score for a track that has
/// at least one annotation, if any. Used by previews that only receive a track_id.
pub async fn get_accessible_venue_for_track(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<String>, String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT s.venue_id
         FROM scores s
         JOIN track_scores ts ON ts.score_id = s.id
         JOIN venues venue ON venue.id = s.venue_id
         CROSS JOIN auth_write_admission admission
         WHERE s.track_id = ?
           AND admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
           AND (
                (admission.active_uid IS NULL
                 AND venue.uid IS NULL AND venue.role != 'member')
                OR
                (admission.active_uid IS NOT NULL AND (
                    venue.uid = admission.active_uid
                    OR EXISTS(
                        SELECT 1 FROM venue_memberships membership
                        WHERE membership.venue_id = venue.id
                          AND membership.user_id = admission.active_uid
                          AND membership.role = 'member'
                    )
                ))
           )
         GROUP BY s.id
         ORDER BY s.updated_at DESC
         LIMIT 1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to resolve venue for track: {}", e))?;
    Ok(row.map(|r| r.0))
}

/// Delete the relational projection half of an authored score archive.
///
/// `AuthoredDocuments` must transition the score's projection ledger to
/// `archived` in this same transaction before calling this function. A
/// database trigger enforces that ordering even for accidental direct SQL.
/// Clips are projection data already retained by that Git commit, so they are
/// removed here with the catalog. Durable conversations remain an explicit
/// prerequisite because deleting a score must never silently destroy chats.
pub(crate) async fn delete_score_projection_for_authored_archive(
    connection: &mut SqliteConnection,
    id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let uid: Option<Option<String>> = sqlx::query_scalar("SELECT uid FROM scores WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|e| format!("Failed to authorize score deletion: {e}"))?;
    let Some(uid) = uid else {
        return Err(format!("Score {id} not found"));
    };
    if uid.as_deref() != owner_user_id {
        return Err(format!("Score {id} not found"));
    }
    let threads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_threads WHERE score_id = ?")
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| format!("Failed to inspect score conversations: {e}"))?;
    if threads != 0 {
        return Err(
            "Score still owns durable conversations; delete those conversations first".into(),
        );
    }
    sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
        .bind(id)
        .execute(&mut *connection)
        .await
        .map_err(|e| format!("Failed to delete archived score clips: {e}"))?;
    let result = sqlx::query("DELETE FROM scores WHERE id = ?")
        .bind(id)
        .execute(&mut *connection)
        .await
        .map_err(|e| format!("Failed to delete score: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(format!("Score {} not found", id));
    }

    Ok(())
}

/// List scores for a track inside the guard's one admitted venue.
pub async fn list_scores_for_track(
    access: &mut impl AuthorizedVenue,
    track_id: &str,
) -> Result<Vec<ScoreSummary>, String> {
    const ONE_VENUE: &str = "SELECT s.id, s.uid, s.venue_id, s.name,
                COUNT(ts.id) AS annotation_count,
                s.created_at, s.updated_at
         FROM scores s
         LEFT JOIN track_scores ts ON ts.score_id = s.id
         WHERE s.track_id = ? AND s.venue_id = ?
         GROUP BY s.id
         ORDER BY s.updated_at DESC";
    sqlx::query_as::<_, ScoreSummary>(ONE_VENUE)
        .bind(track_id)
        .bind(access.venue_id().to_owned())
        .fetch_all(&mut *access.connection())
        .await
        .map_err(|e| format!("Failed to list scores for track: {}", e))
}

/// Cross-venue score picker view filtered in one statement by the current app
/// admission. It never returns sealed rows from another principal.
pub async fn list_accessible_scores_for_track(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Vec<ScoreSummary>, String> {
    sqlx::query_as::<_, ScoreSummary>(
        "SELECT score.id, score.uid, score.venue_id, score.name,
                COUNT(clip.id) AS annotation_count,
                score.created_at, score.updated_at
         FROM scores score
         JOIN venues venue ON venue.id = score.venue_id
         LEFT JOIN track_scores clip ON clip.score_id = score.id
         CROSS JOIN auth_write_admission admission
         WHERE score.track_id = ?
           AND admission.singleton = 1
           AND admission.armed = 1
           AND admission.accepting = 1
           AND admission.maintenance = 0
           AND (
                (admission.active_uid IS NULL
                 AND venue.uid IS NULL AND venue.role != 'member')
                OR
                (admission.active_uid IS NOT NULL AND (
                    venue.uid = admission.active_uid
                    OR EXISTS(
                        SELECT 1 FROM venue_memberships membership
                        WHERE membership.venue_id = venue.id
                          AND membership.user_id = admission.active_uid
                          AND membership.role = 'member'
                    )
                ))
           )
         GROUP BY score.id
         ORDER BY score.updated_at DESC",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("Failed to list accessible scores for track: {error}"))
}

/// Fetch a score by ID
pub async fn get_score(access: &mut impl AuthorizedVenue, id: &str) -> Result<Score, String> {
    sqlx::query_as::<_, Score>(
        "SELECT id, uid, track_id, venue_id, name, created_at, updated_at
         FROM scores WHERE id = ? AND venue_id = ?",
    )
    .bind(id)
    .bind(access.venue_id().to_owned())
    .fetch_one(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to fetch score: {}", e))
}

/// List all track_scores for a given score_id
pub async fn list_track_scores_for_score(
    access: &mut impl AuthorizedVenue,
    score_id: &str,
) -> Result<Vec<TrackScore>, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at
         FROM track_scores
         WHERE score_id = ?
           AND EXISTS(
               SELECT 1 FROM scores score
               WHERE score.id = track_scores.score_id AND score.venue_id = ?
           )
         ORDER BY start_time ASC, z_index ASC",
    )
    .bind(score_id)
    .bind(access.venue_id().to_owned())
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to list track_scores for score {}: {}", score_id, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("luma-test.db");
        let migrate_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .expect("migration pool");
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .expect("migrations");
        migrate_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database_path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .expect("pool");
        (directory, pool)
    }

    #[tokio::test]
    async fn archived_score_projection_deletes_clips_but_never_durable_threads() {
        let (_directory, pool) = test_pool().await;
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, file_path)
             VALUES ('track', 'alice', 'hash', '/track')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Venue')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'Pattern')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name)
             VALUES ('score', 'alice', 'track', 'venue', 'Score')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track_scores
             (id, uid, score_id, pattern_id, start_time, end_time, z_index, args_json)
             VALUES ('clip', 'alice', ?, 'pattern', 0, 1, 0, '{}')",
        )
        .bind("score")
        .execute(&pool)
        .await
        .unwrap();

        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(delete_score_projection_for_authored_archive(
            &mut transaction,
            "score",
            Some("bob")
        )
        .await
        .is_err());
        transaction.rollback().await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO agent_threads
             (id, owner_user_id, agent_kind, subject_kind, subject_id, venue_id, score_id)
             VALUES ('thread', 'alice', 'track_copilot', 'track', 'track', 'venue', ?)",
        )
        .bind("score")
        .execute(&pool)
        .await
        .unwrap();
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let thread_error =
            delete_score_projection_for_authored_archive(&mut transaction, "score", Some("alice"))
                .await
                .unwrap_err();
        transaction.rollback().await.unwrap();
        assert!(thread_error.contains("durable conversations"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores WHERE id = 'clip'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );

        sqlx::query("DELETE FROM agent_threads WHERE id = 'thread'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO authored_state_projections
             (repository_id, document_kind, principal_key, subject_id, track_id, venue_id,
              score_id, projected_commit, materialization_state)
             VALUES ('score-repository', 'track_score', 'signed-in:alice', 'track',
                     'track', 'venue', 'score', ?, 'archived')",
        )
        .bind("a".repeat(40))
        .execute(&pool)
        .await
        .unwrap();
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        delete_score_projection_for_authored_archive(&mut transaction, "score", Some("alice"))
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scores WHERE id = ?")
                .bind("score")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_scores WHERE id = 'clip'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }
}
