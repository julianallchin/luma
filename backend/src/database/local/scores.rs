use sqlx::SqlitePool;

use crate::database::local::venue_access::AuthorizedVenue;
use crate::models::scores::{Score, ScoreSummary};

pub mod rows;

/// The one order a score listing takes: newest-created first.
///
/// `datetime()` rather than the raw column because the two ways a score row is
/// born spell the same instant differently — a local insert takes SQLite's
/// `CURRENT_TIMESTAMP` (`2026-08-29 12:00:00`) while a row written by the
/// current schema carries RFC 3339 (`2026-08-29T12:00:00Z`) — and `'T' > ' '`
/// in text order, so raw comparison files every new-format row above every
/// older one whatever the clock says. Parsing both to one canonical form is
/// what makes the comparison mean what it reads as.
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

/// Every venue this admission may read, spelled once. The admission view
/// [`auth_venue_access`] says the same thing for triggers; a listing needs it
/// inline because it filters rather than aborts.
macro_rules! admitted_venue {
    () => {
        "admission.singleton = 1
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
                        SELECT 1 FROM venue_members membership
                        WHERE membership.venue_id = venue.id
                          AND membership.uid = admission.active_uid
                    )
                ))
           )"
    };
}

/// Return the venue_id of the newest score for a track that has at least one
/// clip, if any. Used by previews that only receive a track_id.
pub async fn get_accessible_venue_for_track(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Option<String>, String> {
    // Aliased `score` so the tie-break below is the listings' `newest_first!`
    // and not a second opinion about which score is the newest.
    let row: Option<(String,)> = sqlx::query_as(concat!(
        "SELECT score.venue_id
         FROM scores score
         JOIN venues venue ON venue.id = score.venue_id
         CROSS JOIN auth_write_admission admission
         WHERE score.track_id = ?
           AND EXISTS(SELECT 1 FROM clips WHERE clips.score_id = score.id)
           AND ",
        admitted_venue!(),
        " ",
        newest_first!(),
        " LIMIT 1"
    ))
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to resolve venue for track: {}", e))?;
    Ok(row.map(|r| r.0))
}

/// The provenance columns every score listing carries: who last wrote the
/// score, when, and what its agent threads have cost.
///
/// Authorship comes from the change log rather than the score row's own
/// `updated_at`, because the row moves for reasons that are not authorship.
/// `save_score` touches the score row exactly when something changed, so the
/// newest `changes` entry naming this score *is* the last edit, and its
/// `actor` is who made it.
///
/// Correlated subqueries rather than joins, because a one-to-many join would
/// multiply the counts the listing already groups for.
///
/// Assumes the listing's score is aliased `score`. A macro rather than a
/// `const` so each listing stays one `concat!`-ed literal: the statements
/// carry no runtime input, and keeping them `&'static str` is what lets sqlx
/// take them without an assertion that they are safe to run.
macro_rules! provenance {
    () => {
        "(SELECT change.actor FROM changes change
                 WHERE change.table_name = 'scores' AND change.row_id = score.id
                 ORDER BY change.at DESC LIMIT 1) AS last_actor,
                (SELECT change.at FROM changes change
                 WHERE change.table_name = 'scores' AND change.row_id = score.id
                 ORDER BY change.at DESC LIMIT 1) AS last_authored_at,
                (SELECT SUM(usage.cost_usd)
                   FROM agent_thread_usage usage
                   JOIN agent_threads thread ON thread.id = usage.thread_id
                  WHERE thread.score_id = score.id) AS cost_usd,
                (SELECT COALESCE(SUM(usage.input_tokens + usage.output_tokens
                                     + usage.cache_creation_tokens
                                     + usage.cache_read_tokens), 0)
                   FROM agent_thread_usage usage
                   JOIN agent_threads thread ON thread.id = usage.thread_id
                  WHERE thread.score_id = score.id) AS total_tokens"
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
                (SELECT COUNT(*) FROM clips WHERE clips.score_id = score.id) AS annotation_count,
                ",
        provenance!(),
        ",
                score.created_at, score.updated_at
         FROM scores score
         LEFT JOIN venues venue ON venue.id = score.venue_id
         WHERE score.track_id = ? AND score.venue_id = ?
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
                (SELECT COUNT(*) FROM clips WHERE clips.score_id = score.id) AS annotation_count,
                ",
        provenance!(),
        ",
                score.created_at, score.updated_at
         FROM scores score
         JOIN venues venue ON venue.id = score.venue_id
         CROSS JOIN auth_write_admission admission
         WHERE score.track_id = ?
           AND ",
        admitted_venue!(),
        " ",
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    pub(crate) async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
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

    /// Newest-created leads, and it is the *instant* that decides — not the
    /// text.
    ///
    /// The regression: `updated_at` used to be the key, and the two ways a
    /// timestamp is written disagree lexically. A local insert takes
    /// SQLite's `CURRENT_TIMESTAMP` (`2026-01-01 00:00:00`); the trigger that
    /// bumps `updated_at` uses RFC 3339 (`2026-01-01T00:00:00Z`). `'T' > ' '`,
    /// so *any* edited score sorted above *every* freshly made one and a new
    /// score never appeared at the top of the sidebar.
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
        // `oldest` is written the new way and later *edited*, so under the old
        // ordering it held the top slot for good; `newest` is a plain local
        // insert. `tie` is minted in the same second as `newest` — the
        // double-click case, which only `rowid` can order.
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
