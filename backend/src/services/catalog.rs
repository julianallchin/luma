//! Creating and deleting the scores a person owns.

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::database::local::scores::{get_score, rows};
use crate::database::local::tracks::current_grid_id;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, VenueResource, Write};
use crate::models::scores::Score;
use crate::sync::triggers::Attribution;

/// Create a score on a `(track, venue)` pair. A pair carries as many scores as
/// there are people who annotated it, so this always makes a new one.
/// `attribution` says who made it when that is not the person alone: an MCP
/// client opening a session mints its score before it has a thread.
pub async fn create_score(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
    attribution: Attribution<'_>,
) -> Result<Score, String> {
    insert_score(pool, track_id, venue_id, name, attribution, false).await
}

/// Give the track durable membership in the venue: an existing score for the
/// pair is returned rather than joined by a second one.
pub async fn ensure_venue_score(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
    attribution: Attribution<'_>,
) -> Result<Score, String> {
    insert_score(pool, track_id, venue_id, name, attribution, true).await
}

async fn insert_score(
    pool: &SqlitePool,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
    attribution: Attribution<'_>,
    reuse_existing: bool,
) -> Result<Score, String> {
    let mut access =
        VenueAccess::<Write>::write_as(pool, VenueResource::Venue(venue_id), attribution).await?;
    let owner = access.principal().to_owned();
    if reuse_existing {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM scores WHERE track_id = ? AND venue_id = ?
             ORDER BY created_at, id LIMIT 1",
        )
        .bind(track_id)
        .bind(venue_id)
        .fetch_optional(access.connection())
        .await
        .map_err(|error| format!("find an existing score: {error}"))?;
        if let Some(existing) = existing {
            return get_score(&mut access, &existing).await;
        }
    }
    let visible: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM auth_visible_tracks WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(access.connection())
            .await
            .map_err(|error| format!("authorize the score's track: {error}"))?;
    if visible.is_none() {
        return Err("track does not exist".into());
    }
    // Written against the track's current grid, and read on it from then
    // on: a later analysis of the song does not move this score's clips.
    let grid_id = current_grid_id(access.connection(), track_id).await?;
    let score_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name, grid_id)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&score_id)
    .bind(&owner)
    .bind(track_id)
    .bind(venue_id)
    .bind(name)
    .bind(grid_id)
    .execute(access.connection())
    .await
    .map_err(|error| format!("insert score: {error}"))?;
    let score = get_score(&mut access, &score_id).await?;
    access.commit().await?;
    Ok(score)
}

/// Copy `score_id` — the row, the beat grid it names and every clip on it —
/// into a new score of the same track in the same venue, named `name`, in one
/// transaction. The copy is the caller's, as a score they created would be,
/// whoever owns the source: copying is how someone else's score is edited.
pub async fn duplicate_score(
    pool: &SqlitePool,
    score_id: &str,
    name: &str,
) -> Result<Score, String> {
    let mut access =
        VenueAccess::<Write>::write(pool, VenueResource::BesideScore(score_id)).await?;
    let owner = access.principal().to_owned();
    let copy_id = Uuid::new_v4().to_string();
    let source = get_score(&mut access, score_id).await?;
    // The copied clips are in the beats of the source's grid, so the copy
    // names the same one. It names its source too: the record would
    // otherwise read the copy's clips as work somebody did again.
    sqlx::query(
        "INSERT INTO scores (id, uid, track_id, venue_id, name, grid_id, copied_from_score_id)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&copy_id)
    .bind(&owner)
    .bind(&source.track_id)
    .bind(&source.venue_id)
    .bind(name)
    .bind(&source.grid_id)
    .bind(score_id)
    .execute(access.connection())
    .await
    .map_err(|error| format!("insert the copy: {error}"))?;
    // A clip id names one row in the library, so the copy's clips are new
    // rows with new ids. Their labels are per score and come along.
    // Where two clips overlap in one z_index the greater id paints on top;
    // that order is written into z_index first, as new ids would lose it.
    let mut source_clips = rows::load_score(access.connection(), score_id).await?;
    source_clips.separate_tracks();
    let clips = luma_patterns::Score {
        clips: source_clips
            .clips
            .into_values()
            .map(|clip| (Uuid::new_v4().to_string(), clip))
            .collect(),
    };
    rows::save_score(access.connection(), &copy_id, &clips).await?;
    let copy = get_score(&mut access, &copy_id).await?;
    access.commit().await?;
    Ok(copy)
}

/// Delete a score. Its clips, definitions and drafts cascade; the agent threads
/// that worked on it do not — a conversation outlives its subject.
pub async fn delete_score(pool: &SqlitePool, score_id: &str) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(score_id)).await?;
    sqlx::query("DELETE FROM scores WHERE id = ?")
        .bind(score_id)
        .execute(access.connection())
        .await
        .map_err(|error| format!("delete score: {error}"))?;
    access.commit().await
}
