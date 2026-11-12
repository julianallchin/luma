//! Track Compositor
//!
//! Builds a [`Scene`] (one [`CompiledAnnotation`] per score row) for a
//! `(track, venue)` pair and installs it on the render engine. The Scene IS the
//! reusable compiled form — every annotation's pattern is lowered to an eval
//! [`Plan`] once, then `Scene::render` evaluates per-frame (cheap, seek-safe).

use std::path::Path;

use crate::controls::ControlBanks;
use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::eval::Scene;
use crate::render_engine::{Built, RenderEngine};
use crate::storage::StorageRoot;

/// Stop showing a score: drop its scene from the render engine, and any
/// composite of it still compiling. Called when the score is closed.
///
/// A no-op unless `score_id` is the score the engine was last asked to show,
/// so it is safe after the score's row is gone and after another score opened.
/// The closed editor's audio is its session's to release.
pub(crate) fn leave_track(render_engine: &RenderEngine, score_id: &str) {
    render_engine.leave_score(score_id);
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
    controls: &ControlBanks,
    render_engine: &RenderEngine,
    score_id: &str,
    graph_score: Option<luma_patterns::Score>,
) -> Result<(), String> {
    let update = render_engine.begin_score_update(score_id);
    let scene = build_score_scene(
        pool,
        storage,
        resource_root,
        controls,
        score_id,
        graph_score,
    )
    .await?;
    render_engine.finish_update(update, Built::Scene(scene));
    Ok(())
}

/// Resolve and compile one score, including a caller's optional working copy.
/// Rendering, performance decks and agent previews share this format boundary.
/// Its faders and buttons read the venue's bank of `controls`.
pub async fn build_score_scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    controls: &ControlBanks,
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
        controls,
        crate::database::local::tracks::Grid::of_score(&track_id, score_id),
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
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venues (id, uid, name) VALUES ('venue', 'alice', 'Venue');
             INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('score', 'alice', 'track', 'venue')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (access, track_id) = score_scope(&pool, "score").await.unwrap();
        assert_eq!(access.venue_id(), "venue");
        assert_eq!(track_id, "track");
        drop(access);

        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        let unauthorized = score_scope(&pool, "score").await.err().unwrap();
        assert_eq!(unauthorized, "Venue resource not found");
    }
}
