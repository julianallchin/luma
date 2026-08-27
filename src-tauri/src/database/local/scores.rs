use sqlx::SqlitePool;

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

/// One score's own clips, in timeline order.
///
/// [`get_scores_for_track`] deliberately spans every score on a
/// `(track, venue)` — that is what the live compositor blends. This is the
/// other question: what does *this score* contain.
pub async fn get_clips_of_score(
    access: &mut impl AuthorizedVenue,
    score_id: &str,
) -> Result<Vec<TrackScore>, String> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT track_scores.id, track_scores.uid, track_scores.score_id, track_scores.pattern_id, track_scores.start_time, track_scores.end_time, track_scores.z_index, track_scores.blend_mode, track_scores.args_json, track_scores.created_at, track_scores.updated_at
         FROM track_scores
         JOIN scores ON track_scores.score_id = scores.id
         WHERE track_scores.score_id = ? AND scores.venue_id = ?
         ORDER BY track_scores.start_time ASC, track_scores.z_index ASC",
    )
    .bind(score_id)
    .bind(access.venue_id().to_owned())
    .fetch_all(&mut *access.connection())
    .await
    .map_err(|e| format!("Failed to list this score's clips: {}", e))
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
    use crate::services::authored_documents::AuthoredDocuments;
    use crate::storage::StorageRoot;
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
        let (directory, pool) = test_pool().await;
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

        let authored = AuthoredDocuments::new(StorageRoot::from_path(
            directory.path().join("authored-storage"),
        ));
        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        assert!(authored
            .archive_score(&pool, Some("bob"), "score")
            .await
            .is_err());
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
        authored
            .archive_score(&pool, Some("alice"), "score")
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_threads WHERE id = 'thread'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
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
