//! Creating and deleting the scores a person owns.
//!
//! Every id here is derived from the caller's `request_id`, so a retried
//! request finds the row it made the first time instead of making a second one.
//! There is no operation ledger: the row either exists or it does not.

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::database::local::auth::principal_key;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, VenueResource, Write};
use crate::models::scores::Score;

/// Create a score on a `(track, venue)` pair. A pair carries as many scores as
/// there are people who annotated it, so this always makes a new one.
pub async fn create_score(
    pool: &SqlitePool,
    request_id: &str,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
) -> Result<Score, String> {
    insert_score(pool, request_id, track_id, venue_id, name, false).await
}

/// Give the track durable membership in the venue: an existing score for the
/// pair is returned rather than joined by a second one.
pub async fn ensure_venue_score(
    pool: &SqlitePool,
    request_id: &str,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
) -> Result<Score, String> {
    insert_score(pool, request_id, track_id, venue_id, name, true).await
}

async fn insert_score(
    pool: &SqlitePool,
    request_id: &str,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
    reuse_existing: bool,
) -> Result<Score, String> {
    let request_id = request_uuid(request_id)?;
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id)).await?;
    let owner = access.principal().map(str::to_owned);
    let score_id = derived_id(
        &principal_key(owner.as_deref()),
        "score",
        &request_id,
        "subject",
    );
    let existing: Option<String> = if reuse_existing {
        sqlx::query_scalar(
            "SELECT id FROM scores WHERE (track_id = ? AND venue_id = ?) OR id = ?
             ORDER BY id = ? DESC, created_at, id LIMIT 1",
        )
        .bind(track_id)
        .bind(venue_id)
        .bind(&score_id)
        .bind(&score_id)
        .fetch_optional(access.connection())
        .await
        .map_err(|error| format!("find an existing score: {error}"))?
    } else {
        sqlx::query_scalar("SELECT id FROM scores WHERE id = ?")
            .bind(&score_id)
            .fetch_optional(access.connection())
            .await
            .map_err(|error| format!("find an existing score: {error}"))?
    };
    if let Some(existing) = existing {
        return crate::database::local::scores::get_score(&mut access, &existing).await;
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
    sqlx::query("INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES (?, ?, ?, ?, ?)")
        .bind(&score_id)
        .bind(owner.as_deref())
        .bind(track_id)
        .bind(venue_id)
        .bind(name)
        .execute(access.connection())
        .await
        .map_err(|error| format!("insert score: {error}"))?;
    let score = crate::database::local::scores::get_score(&mut access, &score_id).await?;
    access.commit().await?;
    Ok(score)
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

fn request_uuid(request_id: &str) -> Result<String, String> {
    Uuid::parse_str(request_id)
        .map(|id| id.to_string())
        .map_err(|_| "creation request_id must be a UUID".to_string())
}

/// The same `(principal, kind, request, role)` always names the same row, so a
/// retry is a lookup rather than a second creation.
fn derived_id(principal_key: &str, kind: &str, request_id: &str, role: &str) -> String {
    let mut hash = Sha256::new();
    for field in ["luma.creation-id.v1", principal_key, kind, request_id, role] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
