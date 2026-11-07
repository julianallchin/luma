//! Track Compositor
//!
//! Builds a [`Scene`] (one [`CompiledAnnotation`] per score row) for a
//! `(track, venue)` pair and installs it on the render engine. The Scene IS the
//! reusable compiled form — every annotation's pattern is lowered to an eval
//! [`Plan`] once, then `Scene::render` evaluates per-frame (cheap, seek-safe).

use std::path::Path;

use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::eval::Scene;
use crate::render_engine::{Built, RenderEngine, SceneTarget};
use crate::storage::StorageRoot;

/// Cancel compositing, clear the render engine's active scene, and unload audio.
/// Called when navigating away from the track editor.
///
/// Takes the *score* id, not the track id: the track is resolved under the
/// score's venue read authorization, so this must run before the score row is
/// deleted.
pub(crate) async fn leave_track(
    pool: &sqlx::SqlitePool,
    render_engine: &RenderEngine,
    host_audio: &crate::host_audio::HostAudioState,
    score_id: &str,
) -> Result<(), String> {
    score_scope(pool, score_id).await?;
    let update = render_engine.begin_scene_update(SceneTarget::Active);
    render_engine.finish_update(update, Built::Scene(crate::eval::Scene::default()));
    host_audio.unload();
    Ok(())
}

/// Open a score for reading and say which track it annotates.
///
/// The venue comes back inside the access ([`AuthorizedVenue::venue_id`]), so
/// this is the whole `(track, venue)` scope a score implies — resolved *under*
/// the score's own authorization rather than taken on trust from a caller.
async fn score_scope<'a>(
    pool: &'a sqlx::SqlitePool,
    score_id: &str,
) -> Result<(VenueAccess<'a, Read>, String), String> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id)).await?;
    let track_id: String = sqlx::query_scalar("SELECT track_id FROM scores WHERE id = ?")
        .bind(score_id)
        .fetch_one(access.connection())
        .await
        .map_err(|error| format!("Failed to resolve the score's track: {error}"))?;
    Ok((access, track_id))
}

/// Install **one score's** light show as the render engine's active scene.
///
/// The score is the subject, not the `(track, venue)` pair it sits on: a pair
/// carries as many scores as there are people who annotated it, and blending
/// them would light the rig with a document nobody is looking at. Which one is
/// on screen is the caller's fact. Building a candidate never changes another
/// render slot, and installation rejects an update superseded while compiling.
///
/// `graph_score` is the editor's working copy when it has one, and `None` for
/// a caller that is only *watching* the score, which then reads the score's own
/// rows.
pub(crate) async fn install_score_scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    render_engine: &RenderEngine,
    score_id: &str,
    graph_score: Option<luma_patterns::Score>,
) -> Result<(), String> {
    let update = render_engine.begin_scene_update(SceneTarget::Active);
    let scene = build_score_scene(pool, storage, resource_root, score_id, graph_score).await?;
    render_engine.finish_update(update, Built::Scene(scene));
    Ok(())
}

/// Resolve and compile one score, including a caller's optional working copy.
/// Rendering, performance decks and agent previews share this format boundary.
pub async fn build_score_scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    score_id: &str,
    graph_score: Option<luma_patterns::Score>,
) -> Result<Scene, String> {
    let (mut access, track_id) = score_scope(pool, score_id).await?;
    let score = match graph_score {
        Some(score) => score,
        None => {
            crate::database::local::scores::rows::load_score(access.connection(), score_id).await?
        }
    };
    crate::services::graph_scores::prepare_scene(
        &mut access,
        resource_root,
        storage,
        &track_id,
        &score,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::score_scope;

    #[tokio::test]
    async fn track_cleanup_derives_the_track_from_an_admitted_score() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE auth_write_admission (
                singleton INTEGER PRIMARY KEY,
                armed INTEGER NOT NULL,
                accepting INTEGER NOT NULL,
                maintenance INTEGER NOT NULL,
                active_uid TEXT
             );
             INSERT INTO auth_write_admission VALUES (1, 1, 1, 0, 'alice');
             CREATE TABLE venues (
                id TEXT PRIMARY KEY,
                uid TEXT,
                role TEXT NOT NULL DEFAULT 'owner'
             );
             CREATE TABLE venue_members (
                id TEXT PRIMARY KEY,
                uid TEXT NOT NULL,
                venue_id TEXT NOT NULL,
                role TEXT NOT NULL
             );
             CREATE TABLE scores (
                id TEXT PRIMARY KEY,
                venue_id TEXT NOT NULL,
                track_id TEXT NOT NULL
             );
             INSERT INTO venues (id, uid) VALUES ('venue', 'alice');
             INSERT INTO scores VALUES ('score', 'venue', 'track')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (access, track_id) = score_scope(&pool, "score").await.unwrap();
        assert_eq!(access.venue_id(), "venue");
        assert_eq!(track_id, "track");
        drop(access);

        sqlx::query("UPDATE auth_write_admission SET active_uid = 'bob'")
            .execute(&pool)
            .await
            .unwrap();
        let unauthorized = score_scope(&pool, "score").await.err().unwrap();
        assert_eq!(unauthorized, "Venue resource not found");
    }
}
