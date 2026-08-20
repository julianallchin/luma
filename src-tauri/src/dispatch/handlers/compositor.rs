//! The track editor's two compositing commands: install a track's scene on the
//! render engine, and tear it down again.

use crate::compositor::{self, LiveAnnotation};
use crate::dispatch::{AppServices, CommandError};

/// Compile every annotation on `(track_id, venue_id)` into a scene and install
/// it as the render engine's active scene.
///
/// `annotations` is load-bearing: omitted means "use the persisted score rows",
/// while an empty list is an authoritative empty document that clears the
/// scene. The editor always sends the live list, whose args run ahead of the
/// database mid-drag.
///
/// `_skip_cache` is dead — the plan cache is keyed by an input signature, so a
/// changed annotation already recompiles. It stays on the wire until the
/// frontend stops sending it.
pub async fn composite_track(
    services: &AppServices,
    track_id: String,
    venue_id: String,
    annotations: Option<Vec<LiveAnnotation>>,
    _skip_cache: Option<bool>,
) -> Result<(), CommandError> {
    compositor::install_track_scene(
        &services.db.0,
        &services.storage,
        &services.fixtures_root,
        &services.render_engine,
        &track_id,
        &venue_id,
        annotations,
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
