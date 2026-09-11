//! The track editor's two compositing commands: install a track's scene on the
//! render engine, and tear it down again.

use crate::compositor;
use crate::dispatch::{AppServices, CommandError};

/// Compile **one score** into a scene and install it as the render engine's
/// active scene.
///
/// Addressed by score, like [`leave_track`]: a `(track, venue)` pair carries
/// as many scores as there are people who annotated it, and the rig shows the
/// one that is open, not a blend of all of them.
///
/// `graph_score` is the editor's working copy when it has one; omitted means
/// "use the score's own rows".
pub async fn composite_track(
    services: &AppServices,
    score_id: String,
    graph_score: Option<luma_patterns::Score>,
) -> Result<(), CommandError> {
    compositor::install_score_scene(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &services.render_engine,
        &score_id,
        graph_score,
    )
    .await?;
    Ok(())
}

/// Leave the track editor: abort any in-flight composite, drop the active
/// scene, unload host audio, and evict the track's stems.
///
/// Takes the *score* id — the track is resolved under the score's venue
/// authorization, so this must be called before the score row is deleted.
pub async fn leave_track(services: &AppServices, score_id: String) -> Result<(), CommandError> {
    compositor::leave_track(
        &services.db.0,
        &services.render_engine,
        &services.host_audio,
        &services.stem_cache,
        &score_id,
    )
    .await?;
    Ok(())
}
