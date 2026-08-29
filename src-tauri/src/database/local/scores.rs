use sqlx::SqlitePool;

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::scores::{Score, ScoreSummary, TrackScore};

/// Every clip of every score on a `(track, venue)` pair, blended together.
///
/// Rarely what a caller means: a pair carries as many scores as there are
/// people who annotated it. [`get_clips_of_score`] is the one that names a
/// document; this one is for callers that have matched a *track* and have no
/// score to name.
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
/// What the compositor installs ([`crate::compositor::install_score_scene`])
/// and what a recording captures: one document, not the pair's union.
///
/// [`get_scores_for_track`] is the other question — every clip on a
/// `(track, venue)`, which now only a perform deck asks, having matched a
/// track and no score.
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

/// The one order a score listing takes: newest-created first.
///
/// `datetime()` rather than the raw column because the two ways a score row is
/// born spell the same instant differently — a local insert takes SQLite's
/// `CURRENT_TIMESTAMP` (`2026-08-29 12:00:00`) while a pulled one carries the
/// remote's RFC 3339 (`2026-08-29T12:00:00Z`) — and `'T' > ' '` in text order,
/// so raw comparison files every synced row above every locally made one
/// whatever the clock says. Parsing both to one canonical form is what makes
/// the comparison mean what it reads as.
///
/// The tie-break is `rowid`, because `CURRENT_TIMESTAMP` only resolves to the
/// second and two scores minted in one are otherwise unordered. `rowid` is
/// this database's insert order — the one thing that still says which of the
/// two it learned about last — where the uuid `id` would sort at random. It
/// is the same key [`score_ordinal`] ranks by, so the list is exactly the
/// ordinal ladder read upside down: `#3` can never appear below `#2`.
///
/// Ordering lives here and nowhere else: a client that re-sorts a listing is a
/// second definition of "newest", and the two drift the moment one of them is
/// tuned.
macro_rules! newest_first {
    () => {
        "ORDER BY datetime(score.created_at) DESC, score.rowid DESC"
    };
}

/// The seam's display handle within its venue — `#1` is the oldest score.
///
/// Shares [`newest_first`]'s parse of `created_at` for the same reason, and
/// its `rowid` tie-break so the two rankings are reverses of one another
/// rather than two nearly-equal opinions.
macro_rules! score_ordinal {
    () => {
        "ROW_NUMBER() OVER (
                    PARTITION BY score.venue_id
                    ORDER BY datetime(score.created_at), score.rowid
                ) AS ordinal"
    };
}

/// Return the venue_id of the newest score for a track that has
/// at least one annotation, if any. Used by previews that only receive a track_id.
pub async fn get_accessible_venue_for_track(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<String>, String> {
    // Aliased `score` so the tie-break below is the listings' `newest_first!`
    // and not a second opinion about which score is the newest.
    let row: Option<(String,)> = sqlx::query_as(concat!(
        "SELECT score.venue_id
         FROM scores score
         JOIN track_scores clip ON clip.score_id = score.id
         JOIN venues venue ON venue.id = score.venue_id
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
         ",
        newest_first!(),
        " LIMIT 1"
    ))
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to resolve venue for track: {}", e))?;
    Ok(row.map(|r| r.0))
}

/// The provenance columns every score listing carries: who wrote the newest
/// revision of the score's authored document, when, and how many there are.
///
/// Correlated subqueries rather than a join, because both listings already
/// group by score to count clips and a second one-to-many join would multiply
/// that count. The join key is `authored_documents.score_id`: the document id
/// is a hash of its scope, so nothing outside the authored-state service ever
/// has to re-derive one to find a score's history.
///
/// Assumes the listing's score is aliased `score`. A macro rather than a
/// `const` so each listing stays one `concat!`-ed literal: the statements
/// carry no runtime input, and keeping them `&'static str` is what lets sqlx
/// take them without an assertion that they are safe to run.
macro_rules! provenance {
    () => {
        concat!(
            "(SELECT revision.actor
                  FROM authored_revisions revision
                  JOIN authored_documents document
                    ON document.document_id = revision.document_id
                 WHERE document.score_id = score.id
                 ORDER BY revision.authored_at DESC, revision.revision_id DESC
                 LIMIT 1) AS last_actor,
                (SELECT revision.authored_at
                  FROM authored_revisions revision
                  JOIN authored_documents document
                    ON document.document_id = revision.document_id
                 WHERE document.score_id = score.id
                 ORDER BY revision.authored_at DESC, revision.revision_id DESC
                 LIMIT 1) AS last_authored_at,
                (SELECT COUNT(*)
                  FROM authored_revisions revision
                  JOIN authored_documents document
                    ON document.document_id = revision.document_id
                 WHERE document.score_id = score.id) AS revision_count,
                (SELECT SUM(usage.cost_usd) FROM agent_thread_usage usage
                 WHERE usage.thread_id IN (",
            authoring_threads!(),
            ")) AS cost_usd,
                (SELECT COALESCE(SUM(usage.input_tokens + usage.output_tokens
                                     + usage.cache_creation_tokens
                                     + usage.cache_read_tokens), 0)
                   FROM agent_thread_usage usage
                 WHERE usage.thread_id IN (",
            authoring_threads!(),
            ")) AS total_tokens"
        )
    };
}

/// Every agent thread that wrote a revision of this score's authored document,
/// once each.
///
/// The `DISTINCT` is the whole point: a run writes many revisions and its cost
/// is recorded once, so joining revisions to costs directly would multiply one
/// run's price by how much it wrote. Spelled apart from [`provenance`] because
/// both of that macro's cost columns need it and a second copy would be a
/// second chance to forget the `DISTINCT`.
macro_rules! authoring_threads {
    () => {
        "SELECT DISTINCT revision.thread_id
                    FROM authored_revisions revision
                    JOIN authored_documents document
                      ON document.document_id = revision.document_id
                   WHERE document.score_id = score.id
                     AND revision.thread_id IS NOT NULL"
    };
}

/// List scores for a track inside the guard's one admitted venue.
pub async fn list_scores_for_track(
    access: &mut impl AuthorizedVenue,
    track_id: &str,
) -> Result<Vec<ScoreSummary>, String> {
    // The venue join is LEFT so it decorates without filtering: this query's
    // result set is settled by the guard above it, not by the name lookup.
    const ONE_VENUE: &str = concat!(
        "SELECT score.id, score.uid, score.venue_id, venue.name AS venue_name, score.name,
                ",
        score_ordinal!(),
        ",
                COUNT(clip.id) AS annotation_count,
                ",
        provenance!(),
        ",
                score.created_at, score.updated_at
         FROM scores score
         LEFT JOIN venues venue ON venue.id = score.venue_id
         LEFT JOIN track_scores clip ON clip.score_id = score.id
         WHERE score.track_id = ? AND score.venue_id = ?
         GROUP BY score.id
         ",
        newest_first!()
    );
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
    const ACCESSIBLE: &str = concat!(
        "SELECT score.id, score.uid, score.venue_id, venue.name AS venue_name, score.name,
                ",
        score_ordinal!(),
        ",
                COUNT(clip.id) AS annotation_count,
                ",
        provenance!(),
        ",
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
         ",
        newest_first!()
    );
    sqlx::query_as::<_, ScoreSummary>(ACCESSIBLE)
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

    /// Newest-created leads, and it is the *instant* that decides — not the
    /// text.
    ///
    /// The regression: `updated_at` used to be the key, and the two ways a
    /// timestamp is written disagree lexically. A local insert takes
    /// SQLite's `CURRENT_TIMESTAMP` (`2026-01-01 00:00:00`); the trigger that
    /// bumps `updated_at`, and a row pulled from the remote, both use
    /// RFC 3339 (`2026-01-01T00:00:00Z`). `'T' > ' '`, so *any* edited or
    /// synced score sorted above *every* freshly made one and a new score
    /// never appeared at the top of the sidebar.
    #[tokio::test]
    async fn a_new_score_leads_the_listing_however_its_timestamp_is_spelled() {
        let (_directory, pool) = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, uid, file_path, title, track_hash) VALUES ('track', 'alice', '/t.wav', 'T', 'hash')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Venue')")
            .execute(&pool)
            .await
            .unwrap();
        // `oldest` is written the remote's way and later *edited*, so under
        // the old ordering it held the top slot for good; `newest` is a plain
        // local insert, the shape every score made in the app has. `tie` is
        // minted in the same second as `newest` — the double-click case, which
        // only `rowid` can order.
        for (id, name, created) in [
            ("oldest", "Oldest", "2026-01-01T00:00:00Z"),
            ("middle", "Middle", "2026-01-02 00:00:00"),
            ("newest", "Newest", "2026-01-03 00:00:00"),
            ("tie", "Tie", "2026-01-03 00:00:00"),
        ] {
            sqlx::query(
                "INSERT INTO scores (id, uid, track_id, venue_id, name, created_at, updated_at)
                 VALUES (?, 'alice', 'track', 'venue', ?, ?, ?)",
            )
            .bind(id)
            .bind(name)
            .bind(created)
            .bind(created)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("UPDATE scores SET updated_at = '2026-06-01T00:00:00Z' WHERE id = 'oldest'")
            .execute(&pool)
            .await
            .unwrap();

        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        let listed = list_accessible_scores_for_track(&pool, "track")
            .await
            .expect("the listing");

        let order: Vec<&str> = listed.iter().map(|score| score.id.as_str()).collect();
        assert_eq!(order, ["tie", "newest", "middle", "oldest"]);
        // The list is the ordinal ladder upside down — `#3` can never sit
        // below `#2`, whichever way the two rows spell their timestamps.
        let ordinals: Vec<i64> = listed.iter().map(|score| score.ordinal).collect();
        assert_eq!(ordinals, [4, 3, 2, 1]);
    }
}
