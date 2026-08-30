//! Host capabilities behind `luma.venue.render()`.
//!
//! A picture of the room is a *read*: this handler never mutates anything, and
//! every input it takes from Python is a camera dial. The venue it draws, and
//! the score that lights it, come from the durable thread's scope — Python
//! cannot point the camera at another room.
//!
//! The output is an ordinary workspace figure. It is written under `outputs/`
//! and handed back as a workspace-relative path, so the existing post-cell path
//! (`services::agent_execution::project`) registers it as a generated artifact
//! and puts it in the model-facing `figures` list exactly like a matplotlib
//! figure. There is one figure mechanism, not two.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::runtime::Handle;

use crate::agent_execution::cell_host::{call_limit, decode, supervise};
use crate::agent_execution::worker_process::{HostCallContext, HostCallError, HostCallHandler};
use crate::agent_execution::workspace::Workspace;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::eval::{Arena, Scope};
use crate::models::selection::Selection;
use crate::models::universe::UniverseState;
use crate::services::groups;
use crate::services::track_edits::TrackScope;
use crate::stage_render::{self, Shot, VenueGeometry, MAX_DIMENSION};
use crate::storage::StorageRoot;
use luma_render::venue_tiles::TileMap;
use luma_scene::View;

/// One cell's venue capability table. Construct only from host-resolved scope.
pub struct VenueHost {
    runtime: Handle,
    pool: SqlitePool,
    storage: StorageRoot,
    /// The fixtures root; the mesh root is derived from it.
    resource_root: PathBuf,
    workspace: Arc<Workspace>,
    venue_id: String,
    /// The score that lights the room, when the thread has one. Without it the
    /// rig is drawn on the editor's work light alone — geometry, no beams.
    lighting: Option<TrackScope>,
}

impl VenueHost {
    pub fn new(
        runtime: Handle,
        pool: SqlitePool,
        storage: StorageRoot,
        resource_root: PathBuf,
        workspace: Arc<Workspace>,
        venue_id: String,
        lighting: Option<TrackScope>,
    ) -> Self {
        Self {
            runtime,
            pool,
            storage,
            resource_root,
            workspace,
            venue_id,
            lighting,
        }
    }

    async fn render(&self, request: RenderRequest) -> Result<Value, HostCallError> {
        let view = View::from_str(&request.view)
            .map_err(|error| HostCallError::new("invalid_view", error.to_string()))?;
        let width = clamp_dimension("width", request.width)?;
        let height = clamp_dimension("height", request.height)?;
        let time = self.clamp_time(request.t).await?;

        // One venue snapshot for the geometry *and* the selection: a mark on a
        // fixture that is not in the frame is worse than no mark at all.
        let mut access =
            VenueAccess::<Read>::read(&self.pool, VenueResource::Venue(&self.venue_id))
                .await
                .map_err(|error| {
                    HostCallError::new(
                        "invalid_venue",
                        format!("the venue is not available: {error}"),
                    )
                })?;
        let geometry = VenueGeometry::read(&mut access, &self.resource_root)
            .await
            .map_err(|error| HostCallError::new("invalid_venue", error))?;
        if geometry.is_empty() {
            return Err(HostCallError::new(
                "invalid_venue",
                "this venue has no patched fixtures and no stage pieces, so there is \
                 nothing to render",
            ));
        }

        // A highlight *replaces* the lighting: the question it answers is
        // "which heads are these?", and a score playing over the answer is
        // only noise on top of it.
        let state = match request.highlight.as_deref() {
            None => self.state_at(time).await?,
            Some(expression) => Some(stage_render::highlight_state(
                &groups::resolve_selection_expression_with_path(
                    &self.resource_root,
                    &mut access,
                    &Selection::new(expression),
                    // A highlight is a picture of one answer, so the random
                    // selectors have to give the same answer twice.
                    0,
                )
                .await
                .map_err(|error| HostCallError::new("invalid_selection", error))?,
            )),
        };
        drop(access);

        let (mut scene, definitions) = geometry.scene();
        scene.aim_arrows = request.aim_arrows;
        let booth = geometry.booth();
        let meshes_root = stage_render::meshes_root(Some(&self.resource_root));

        // The GPU render is a blocking submit-and-map. Running it on a blocking
        // thread is what keeps `supervise`'s cancellation poll alive across it.
        let png = tokio::task::spawn_blocking(move || {
            stage_render::render_png(
                scene,
                definitions,
                Shot {
                    view,
                    booth,
                    state,
                    time,
                    size: (width, height),
                },
                meshes_root,
            )
        })
        .await
        .map_err(|error| {
            HostCallError::new("internal", format!("the render task failed: {error}"))
        })?
        .map_err(|error| HostCallError::new("render_failed", error))?;

        let artifact_rel = self.write_figure(&png).await?;
        Ok(json!({
            "artifactRel": artifact_rel,
            "width": width,
            "height": height,
            "view": view.name(),
            "t": time,
        }))
    }

    /// The Gauntlet view: the room as a top-down text map.
    ///
    /// A read of the same solve `render` draws, in the channel the design doc
    /// ranks above a picture — it says where every piece is in something a
    /// diff can localise, at a thousandth of the tokens.
    async fn tiles(&self, request: TilesRequest) -> Result<Value, HostCallError> {
        if !request.cell_m.is_finite() {
            return Err(HostCallError::new(
                "invalid_argument",
                "cell_m must be a finite number of metres",
            ));
        }
        let mut access =
            VenueAccess::<Read>::read(&self.pool, VenueResource::Venue(&self.venue_id))
                .await
                .map_err(|error| {
                    HostCallError::new(
                        "invalid_venue",
                        format!("the venue is not available: {error}"),
                    )
                })?;
        let map = crate::venue_graph::tiles(
            &mut access,
            &self.resource_root,
            TileMap {
                cell_m: request.cell_m,
                ..TileMap::default()
            },
        )
        .await
        .map_err(|error| HostCallError::new("invalid_venue", error))?;
        Ok(json!({ "map": map }))
    }

    /// The evaluated universe at `time`, or `None` when this thread has no
    /// score to light the room with.
    async fn state_at(&self, time: f32) -> Result<Option<UniverseState>, HostCallError> {
        let Some(scope) = self.lighting.as_ref() else {
            return Ok(None);
        };
        let scores =
            crate::compositor::scores_for_track(&self.pool, &scope.venue_id, &scope.track_id)
                .await
                .map_err(|error| HostCallError::new("internal", error))?;
        if scores.is_empty() {
            return Ok(None);
        }
        let scene = crate::compositor::build_scene_strict(
            &self.pool,
            &self.pool,
            &self.storage,
            &self.resource_root,
            &scope.track_id,
            &scope.venue_id,
            &scores,
        )
        .await
        .map_err(|message| HostCallError::new("compile_error", message))?;
        let mut arena = Arena::default();
        Ok(scene
            .render(&[time], Scope::Composite, &mut arena)
            .into_iter()
            .next())
    }

    /// `t` inside the track's own span, so a camera can never be asked for a
    /// moment the score does not have.
    async fn clamp_time(&self, requested: f64) -> Result<f32, HostCallError> {
        if !requested.is_finite() {
            return Err(HostCallError::new("invalid_time", "t must be finite"));
        }
        let end = match self.lighting.as_ref() {
            Some(scope) => {
                crate::database::local::tracks::get_track_duration(&self.pool, &scope.track_id)
                    .await
                    .map_err(|error| HostCallError::new("internal", error))?
            }
            None => None,
        };
        let clamped = requested.max(0.0).min(end.unwrap_or(f64::from(f32::MAX)));
        Ok(clamped as f32)
    }

    /// Put the PNG in the workspace's output area and return its
    /// workspace-relative path.
    async fn write_figure(&self, png: &[u8]) -> Result<String, HostCallError> {
        let outputs = {
            let store = self.workspace.store();
            let store = store.lock().await;
            store.outputs_dir()
        };
        tokio::fs::create_dir_all(&outputs)
            .await
            .map_err(|error| HostCallError::new("internal", format!("outputs/: {error}")))?;
        let name = format!("stage-{}.png", uuid::Uuid::new_v4());
        tokio::fs::write(outputs.join(&name), png)
            .await
            .map_err(|error| {
                HostCallError::new("internal", format!("could not write {name}: {error}"))
            })?;
        Ok(format!("outputs/{name}"))
    }
}

impl HostCallHandler for VenueHost {
    fn handle(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        context.check()?;
        let limit = call_limit(context)?;
        self.runtime.block_on(supervise(
            async {
                match method {
                    "venue.render" => self.render(decode(payload)?).await,
                    "venue.tiles" => self.tiles(decode(payload)?).await,
                    _ => Err(HostCallError::new(
                        "unknown_method",
                        format!("unknown venue host method {method:?}"),
                    )),
                }
            },
            context,
            limit,
        ))
    }
}

fn clamp_dimension(name: &str, value: u32) -> Result<u32, HostCallError> {
    if value == 0 {
        return Err(HostCallError::new(
            "invalid_size",
            format!("{name} must be at least 1 pixel"),
        ));
    }
    Ok(value.min(MAX_DIMENSION))
}

/// `cell_m` is the only dial: everything else about the map is a function of
/// the venue, and the size of a tile is the one thing a reader trades detail
/// against width for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TilesRequest {
    cell_m: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderRequest {
    view: String,
    t: f64,
    width: u32,
    height: u32,
    /// A selection expression. Present, its heads are the only thing lit, and
    /// the score at `t` is not drawn at all. Absent, the score lights the room.
    highlight: Option<String>,
    /// Draw each fixture's rest aim as an arrow. Defaulted on the Python side
    /// rather than here, so the answer to "on or off by default" has one home;
    /// this channel is a verification channel, and a picture that does not say
    /// which way the heads point does not verify a patch.
    aim_arrows: bool,
}
