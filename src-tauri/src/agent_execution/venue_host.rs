//! Host capabilities behind `luma.venue.*`: the camera, the two text channels,
//! and the build verbs.
//!
//! The venue every call acts on comes from the durable thread's scope — Python
//! names no venue id and cannot point at another room.
//!
//! # The verbs are not implemented here
//!
//! Every mutating call is one line into [`crate::services::stage_ops`], which
//! is the same module the gpui stage page reaches through
//! `dispatch::handlers::stage`. This file is the *boundary*: it decodes the
//! payload, refuses what the scope forbids, and hands back the resolver's own
//! [`crate::models::venue_graph::PlacementReport`] with a fresh `describe()`
//! beside it, because a program that just changed the rig should be able to
//! read its own effect without a second call.
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
use crate::models::distribute::DistributeLayout;
use crate::models::selection::Selection;
use crate::models::universe::UniverseState;
use crate::services::groups;
use crate::services::stage_ops::{AimTarget, Draft, Filter, Stage, StageError};
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
    /// The scratch graphs open in this cell, by id.
    ///
    /// In memory and cell-scoped on purpose: a draft is a *component function
    /// being run somewhere that is not the venue yet*, so it has no rows, no
    /// undo entry and nothing to clean up if the cell dies. Stamping is the
    /// only moment it becomes a venue's business.
    drafts: std::sync::Mutex<std::collections::HashMap<String, Draft>>,
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
            drafts: std::sync::Mutex::default(),
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

        self.shoot(
            geometry,
            Shoot {
                view,
                time,
                state,
                aim_arrows: request.aim_arrows,
                size: (width, height),
                bare: false,
            },
        )
        .await
    }

    /// One frame of whatever geometry it is handed.
    ///
    /// Shared by the venue camera and the draft camera so a preview cannot come
    /// out looking like a different renderer: a draft is the *same* room math
    /// with nothing in it but the draft.
    async fn shoot(&self, geometry: VenueGeometry, shot: Shoot) -> Result<Value, HostCallError> {
        let Shoot {
            view,
            time,
            state,
            aim_arrows,
            size: (width, height),
            bare,
        } = shot;
        let (mut scene, definitions) = geometry.scene();
        scene.aim_arrows = aim_arrows;
        if bare {
            // A draft is looked at for its shape. The floor and its grid are
            // the *room's* furniture, and a component being previewed is not in
            // a room yet.
            scene.render.show_grid = false;
            scene.render.show_floor = false;
        }
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

    // -- the build verbs ------------------------------------------------

    /// This thread's venue, bound to the verbs.
    fn stage(&self) -> Stage<'_> {
        Stage::new(&self.pool, &self.resource_root, &self.venue_id)
    }

    /// The placeable vocabulary. A read of the catalog, not of the venue, so it
    /// answers on an empty room — which is exactly when an agent needs it.
    fn catalog(&self) -> Result<Value, HostCallError> {
        let catalog = crate::services::stage_ops::catalog(&self.resource_root)?;
        Ok(json!({ "catalog": catalog }))
    }

    /// The fixtures a `distribute` can name, matched by the same rule the patch
    /// page's search field uses.
    ///
    /// A read of the library, not of the venue: the vocabulary half of
    /// `catalog()`, which answers for structure and cannot answer for lights.
    fn fixtures(&self, request: FixturesRequest) -> Result<Value, HostCallError> {
        let found = crate::services::fixtures::library(
            &self.resource_root,
            request.query.as_deref().unwrap_or_default(),
            request.limit.min(FIXTURE_PAGE_MAX),
        )
        .map_err(|error| HostCallError::new("internal", error))?;
        Ok(json!({ "fixtures": found }))
    }

    /// The tree as text — the channel a program reads after every mutation.
    async fn describe(&self) -> Result<Value, HostCallError> {
        Ok(json!({ "text": self.stage().describe().await? }))
    }

    /// Everything the solve left open: unplaced branches and dangling sockets.
    ///
    /// Live, not the cell's binding snapshot: a build script asks this *after*
    /// it has changed the rig, and a manifest assembled before the cell ran
    /// would answer about the room it walked into.
    async fn open(&self) -> Result<Value, HostCallError> {
        let venue = self.stage().resolved().await?;
        Ok(json!({ "unplaced": venue.unplaced, "dangling": venue.dangling }))
    }

    async fn reach(&self, request: ReachRequest) -> Result<Value, HostCallError> {
        let reach = self
            .stage()
            .reach(&request.node_id, &request.socket)
            .await?;
        Ok(json!({ "reach": reach }))
    }

    async fn place(&self, request: PlaceRequest) -> Result<Value, HostCallError> {
        let report = self
            .stage()
            .place_free(
                &request.kind,
                request.catalog_ref.as_deref(),
                request.label.as_deref(),
                request.surface_node_id.as_deref(),
                request.surface_socket.as_deref(),
                request.my_socket.as_deref(),
                request.u,
                request.v,
                request.yaw,
                request.trim,
                request.params,
            )
            .await?;
        self.placed(report).await
    }

    async fn attach(&self, request: AttachRequest) -> Result<Value, HostCallError> {
        let report = self
            .stage()
            .attach(
                &request.kind,
                request.catalog_ref.as_deref(),
                request.label.as_deref(),
                &request.parent_id,
                request.my_socket.as_deref(),
                &request.their_socket,
                request.yaw,
                request.params,
            )
            .await?;
        self.placed(report).await
    }

    async fn extend(&self, request: ExtendRequest) -> Result<Value, HostCallError> {
        let report = self
            .stage()
            .extend(&request.node_id, &request.socket, request.length_m)
            .await?;
        self.placed(report).await
    }

    async fn duplicate(&self, request: DuplicateRequest) -> Result<Value, HostCallError> {
        let report = self
            .stage()
            .duplicate(
                &request.node_id,
                &request.parent_id,
                &request.their_socket,
                request.flip,
            )
            .await?;
        self.placed(report).await
    }

    async fn detach(&self, request: NodeRequest) -> Result<Value, HostCallError> {
        let report = self.stage().detach(&request.node_id).await?;
        self.placed(report).await
    }

    /// Delete a node and everything structural under it. Its fixtures are
    /// trayed, not deleted — the same rule the page's context menu follows.
    async fn remove(&self, request: NodeRequest) -> Result<Value, HostCallError> {
        self.stage().delete_subtree(&request.node_id).await?;
        Ok(json!({ "describe": self.stage().describe().await? }))
    }

    async fn params(&self, request: ParamsRequest) -> Result<Value, HostCallError> {
        let report = self
            .stage()
            .set_params(&request.node_id, request.params, request.label.as_deref())
            .await?;
        self.placed(report).await
    }

    /// The one fixture constructor: place, name, group and patch a row in one
    /// transaction. A fit failure is a report, not an error.
    ///
    /// Nothing republishes the Art-Net patch here: a cell has no live output,
    /// and the desktop app re-reads the patch when the venue reloads.
    async fn distribute(&self, request: DistributeRequest) -> Result<Value, HostCallError> {
        // A draft has no patch, so a row it asks for is *recorded* and laid at
        // the stamp — the first moment there is a universe to address it in.
        if let Some(draft_id) = request.draft_id.as_deref() {
            let face = request.face.ok_or_else(|| {
                HostCallError::new(
                    "invalid_argument",
                    "a row in a draft is hung on a face vector",
                )
            })?;
            let host = request.host_node_id.clone().ok_or_else(|| {
                HostCallError::new(
                    "invalid_argument",
                    "a row in a draft hangs on a piece the draft built",
                )
            })?;
            let supply = self.supply()?;
            let mut drafts = self.drafts();
            let draft = Self::draft_mut(&mut drafts, draft_id)?;
            // Refuse a face the host does not have *now*, rather than at the
            // stamp: the caller is standing in front of the piece it just
            // built, and that is where the fix is cheapest.
            draft.face(supply, &host, face)?;
            draft.record(crate::services::stage_ops::PendingRow {
                host,
                face,
                fixture_path: request.fixture_path.clone(),
                mode_name: request.mode_name.clone(),
                count: request.count,
                layout: request.layout,
                label_prefix: request.label_prefix.clone(),
            });
            return Ok(json!({
                "report": {
                    "fixtures": [],
                    "refusal": Value::Null,
                    "warnings": [
                        "recorded in the draft — the row is patched when the draft is stamped"
                    ],
                    "dangling": [],
                    "unplaced": [],
                },
                "describe": draft.describe(),
            }));
        }
        let socket = match (request.face, request.host_socket.clone()) {
            (Some(face), _) => Some(
                self.stage()
                    .face_socket(request.host_node_id.as_deref(), face)
                    .await?,
            ),
            (None, named) => named,
        };
        let distributed = self
            .stage()
            .distribute(
                request.host_node_id.as_deref(),
                socket.as_deref(),
                &request.fixture_path,
                &request.mode_name,
                request.count,
                request.layout,
                request.label_prefix.as_deref(),
            )
            .await?;
        Ok(json!({
            "report": distributed.report,
            "describe": self.stage().describe().await?,
        }))
    }

    // -- the authoring surface -------------------------------------------

    /// One `place` or `add` — against the venue, or against a draft.
    ///
    /// The two paths differ only in where the rows land, which is why the
    /// compiler is one function: a draft that built differently from the venue
    /// would make a preview a lie.
    async fn chain(&self, request: ChainRequest) -> Result<Value, HostCallError> {
        let plan = request.compile()?;
        if let Some(draft_id) = request.draft_id.as_deref() {
            let supply = self.supply()?;
            let mut drafts = self.drafts();
            let draft = Self::draft_mut(&mut drafts, draft_id)?;
            let built = draft.chain(supply, &plan)?;
            let text = draft.describe();
            return Ok(built_json(&built, &text));
        }
        let built = self.stage().chain(&plan).await?;
        let text = self.stage().describe().await?;
        Ok(built_json(&built, &text))
    }

    /// Every node a filter names, in the facade frame.
    async fn query(&self, request: QueryRequest) -> Result<Value, HostCallError> {
        let filter = request.filter();
        let nodes = match request.draft_id.as_deref() {
            Some(id) => {
                let supply = self.supply()?;
                let drafts = self.drafts();
                Self::draft(&drafts, id)?.nodes(supply, &filter)
            }
            None => self.stage().nodes(&filter).await?,
        };
        Ok(json!({ "nodes": nodes.iter().map(node_json).collect::<Vec<_>>() }))
    }

    /// The span and centre of everything a filter names — the "is it centred"
    /// check in one call.
    async fn extent(&self, request: QueryRequest) -> Result<Value, HostCallError> {
        let filter = request.filter();
        let extent = match request.draft_id.as_deref() {
            Some(id) => {
                let supply = self.supply()?;
                let drafts = self.drafts();
                Self::draft(&drafts, id)?.extent(supply, &filter)
            }
            None => self.stage().extent(&filter).await?,
        };
        Ok(json!({ "extent": extent.map(extent_json) }))
    }

    /// A cursor on an existing node's free end.
    ///
    /// The node's own row comes back with it, because a cursor *is* a node
    /// handle: handing back the end without the piece would make the caller ask
    /// twice for one thing.
    async fn tip(&self, request: TipRequest) -> Result<Value, HostCallError> {
        let end = request.end;
        let filter = Filter {
            ids: vec![request.node_id.clone()],
            ..Filter::default()
        };
        if let Some(id) = request.draft_id.as_deref() {
            let supply = self.supply()?;
            let drafts = self.drafts();
            let draft = Self::draft(&drafts, id)?;
            let tip = draft.tip(supply, &request.node_id, end)?;
            let node = draft.nodes(supply, &filter);
            return Ok(json!({
                "tip": tip_json(&tip),
                "node": node.first().map(node_json),
            }));
        }
        let tip = self.stage().tip(&request.node_id, end).await?;
        let node = self.stage().nodes(&filter).await?;
        Ok(json!({
            "tip": tip_json(&tip),
            "node": node.first().map(node_json),
        }))
    }

    /// Point heads: along a direction, or at a point.
    async fn aim_at(&self, request: AimRequest) -> Result<Value, HostCallError> {
        let target = match (request.direction, request.at) {
            (Some(direction), None) => AimTarget::Direction(direction),
            (None, Some(at)) => AimTarget::At(at),
            _ => {
                return Err(HostCallError::new(
                    "invalid_argument",
                    "aim takes a direction= or an at=, and exactly one of them",
                ))
            }
        };
        let aimed = self.stage().aim(&request.nodes, &target).await?;
        Ok(json!({
            "aimed": aimed,
            "describe": self.stage().describe().await?,
        }))
    }

    // -- drafts ----------------------------------------------------------

    /// The geometry supply, resolved once per process.
    fn supply(&self) -> Result<&'static luma_render::catalog::VenueSockets, HostCallError> {
        crate::venue_graph::sockets(&self.resource_root)
            .map_err(|error| HostCallError::new("internal", error))
    }

    fn drafts(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Draft>> {
        // A poisoned lock means a previous call panicked mid-edit; the drafts
        // are still readable and a cell that cannot preview is worse than one
        // that previews a graph a panic left alone.
        self.drafts.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn draft<'d>(
        drafts: &'d std::collections::HashMap<String, Draft>,
        id: &str,
    ) -> Result<&'d Draft, HostCallError> {
        drafts.get(id).ok_or_else(|| {
            HostCallError::new("not_found", format!("no draft `{id}` is open in this cell"))
        })
    }

    fn draft_mut<'d>(
        drafts: &'d mut std::collections::HashMap<String, Draft>,
        id: &str,
    ) -> Result<&'d mut Draft, HostCallError> {
        drafts.get_mut(id).ok_or_else(|| {
            HostCallError::new("not_found", format!("no draft `{id}` is open in this cell"))
        })
    }

    fn draft_create(&self) -> Result<Value, HostCallError> {
        let supply = self.supply()?;
        let id = format!("draft-{}", uuid::Uuid::new_v4());
        self.drafts().insert(id.clone(), Draft::new(supply));
        Ok(json!({ "draftId": id }))
    }

    fn draft_discard(&self, request: DraftRequest) -> Result<Value, HostCallError> {
        self.drafts().remove(&request.draft_id);
        Ok(json!({}))
    }

    fn draft_describe(&self, request: DraftRequest) -> Result<Value, HostCallError> {
        let drafts = self.drafts();
        Ok(json!({ "text": Self::draft(&drafts, &request.draft_id)?.describe() }))
    }

    /// A picture of the draft alone: the same render path, framed on nothing
    /// but what the component built.
    async fn draft_render(&self, request: DraftRenderRequest) -> Result<Value, HostCallError> {
        let view = View::from_str(&request.view)
            .map_err(|error| HostCallError::new("invalid_view", error.to_string()))?;
        let width = clamp_dimension("width", request.width)?;
        let height = clamp_dimension("height", request.height)?;
        let venue = {
            let drafts = self.drafts();
            let draft = Self::draft(&drafts, &request.draft_id)?;
            if draft.is_empty() {
                return Err(HostCallError::new(
                    "invalid_venue",
                    "this draft is empty, so there is nothing to render",
                ));
            }
            draft.solved().clone()
        };
        // No patch and no definitions: a draft holds structure, and the lights
        // it recorded are laid at the stamp, which is the first moment there is
        // a universe to address them in.
        let geometry = VenueGeometry {
            fixtures: Vec::new(),
            venue,
            definitions: std::collections::HashMap::new(),
        };
        self.shoot(
            geometry,
            Shoot {
                view,
                time: 0.0,
                state: None,
                aim_arrows: false,
                size: (width, height),
                bare: true,
            },
        )
        .await
    }

    /// Copy a draft into the venue.
    async fn stamp(&self, request: StampRequest) -> Result<Value, HostCallError> {
        let nodes = {
            let drafts = self.drafts();
            let draft = Self::draft(&drafts, &request.draft_id)?;
            self.stage()
                .stamp(draft, request.at, request.yaw, request.trim)
                .await?
        };
        Ok(json!({
            "nodes": nodes,
            "describe": self.stage().describe().await?,
        }))
    }

    /// The resolver's own placement report, with the tree it produced beside
    /// it. Two channels, one solve apiece, so a program never has to ask twice
    /// what its own call did.
    async fn placed(
        &self,
        report: crate::models::venue_graph::PlacementReport,
    ) -> Result<Value, HostCallError> {
        Ok(json!({
            "placement": report,
            "describe": self.stage().describe().await?,
        }))
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
                    "venue.catalog" => self.catalog(),
                    "venue.fixtures" => self.fixtures(decode(payload)?),
                    "venue.describe" => self.describe().await,
                    "venue.open" => self.open().await,
                    "venue.reach" => self.reach(decode(payload)?).await,
                    "venue.place" => self.place(decode(payload)?).await,
                    "venue.attach" => self.attach(decode(payload)?).await,
                    "venue.extend" => self.extend(decode(payload)?).await,
                    "venue.duplicate" => self.duplicate(decode(payload)?).await,
                    "venue.detach" => self.detach(decode(payload)?).await,
                    "venue.remove" => self.remove(decode(payload)?).await,
                    "venue.params" => self.params(decode(payload)?).await,
                    "venue.distribute" => self.distribute(decode(payload)?).await,
                    "venue.chain" => self.chain(decode(payload)?).await,
                    "venue.query" => self.query(decode(payload)?).await,
                    "venue.extent" => self.extent(decode(payload)?).await,
                    "venue.tip" => self.tip(decode(payload)?).await,
                    "venue.aim" => self.aim_at(decode(payload)?).await,
                    "venue.stamp" => self.stamp(decode(payload)?).await,
                    "venue.draft.create" => self.draft_create(),
                    "venue.draft.discard" => self.draft_discard(decode(payload)?),
                    "venue.draft.describe" => self.draft_describe(decode(payload)?),
                    "venue.draft.render" => self.draft_render(decode(payload)?).await,
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

/// How many library rows one call will parse and return, whatever it asks for.
///
/// A page, not the library: the answer is read by a model, and a thousand
/// fixtures is a context window rather than an answer.
const FIXTURE_PAGE_MAX: usize = 100;

/// A library search. `query` absent is every fixture, first page.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixturesRequest {
    query: Option<String>,
    limit: usize,
}

/// `cell_m` is the only dial: everything else about the map is a function of
/// the venue, and the size of a tile is the one thing a reader trades detail
/// against width for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TilesRequest {
    cell_m: f64,
}

/// One node, named. `detach` and `remove` take nothing else.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeRequest {
    node_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReachRequest {
    node_id: String,
    socket: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceRequest {
    kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
    /// The surface to seat on; absent is the venue's own floor.
    surface_node_id: Option<String>,
    surface_socket: Option<String>,
    /// Absent lets the piece's own underside decide — see
    /// `stage_ops::Stage::seat_socket`.
    my_socket: Option<String>,
    u: f64,
    v: f64,
    yaw: f64,
    trim: f64,
    params: std::collections::BTreeMap<String, f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachRequest {
    kind: String,
    catalog_ref: Option<String>,
    label: Option<String>,
    parent_id: String,
    /// Absent lets the catalog decide which of the piece's sockets mates the
    /// host's — the same predicate the snap search scores with.
    my_socket: Option<String>,
    their_socket: String,
    yaw: f64,
    params: std::collections::BTreeMap<String, f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtendRequest {
    node_id: String,
    socket: String,
    /// Absent means "to whatever the ray found".
    length_m: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateRequest {
    node_id: String,
    parent_id: String,
    their_socket: String,
    flip: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParamsRequest {
    node_id: String,
    params: std::collections::BTreeMap<String, f64>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DistributeRequest {
    draft_id: Option<String>,
    host_node_id: Option<String>,
    /// The face as a **vector**, which is the vocabulary the build surface
    /// states intent in. Beam is the mount normal, so this is also where the
    /// row points at rest.
    face: Option<[f64; 3]>,
    /// The face by name — the older, lower layer, still how the gpui page and
    /// an existing script spell it.
    host_socket: Option<String>,
    fixture_path: String,
    mode_name: String,
    count: usize,
    layout: DistributeLayout,
    label_prefix: Option<String>,
}

/// What one frame is of, beside the geometry itself.
struct Shoot {
    view: View,
    time: f32,
    state: Option<UniverseState>,
    aim_arrows: bool,
    size: (u32, u32),
    /// Draw the piece and nothing else — no floor, no grid. What a draft is
    /// previewed through.
    bare: bool,
}

/// One `place` or `add`, as the wire carries it.
///
/// One shape for both verbs, because they build the same piece: `from` present
/// is an `add` growing off a tip, absent is a `place` anchored by its footprint
/// centre. Everything else — the direction, the joint, the length — is common,
/// and splitting them would put the quantization in two places.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainRequest {
    /// The scratch graph to build in, or the venue itself.
    draft_id: Option<String>,
    piece: String,
    /// A cursor handed back by an earlier call, round-tripped verbatim — which
    /// is how the caller names an end without ever naming a socket.
    from: Option<WireTip>,
    at: Option<[f64; 2]>,
    on: Option<String>,
    face: Option<[f64; 3]>,
    direction: Option<[f64; 3]>,
    axis: Option<[f64; 3]>,
    angle: Option<f64>,
    length: Option<f64>,
    to: Option<String>,
    #[serde(default)]
    trim: f64,
    label: Option<String>,
}

/// A cursor on one free end, as the wire carries it.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WireTip {
    node: String,
    socket: String,
    direction: [f64; 3],
    at: [f64; 3],
}

impl ChainRequest {
    fn compile(&self) -> Result<luma_scene::build::Request, HostCallError> {
        Ok(luma_scene::build::Request {
            piece: self.piece.clone(),
            from: self.from.as_ref().map(|tip| luma_scene::build::Tip {
                node: tip.node.clone(),
                socket: tip.socket.clone(),
                direction: tip.direction,
                at: tip.at,
            }),
            at: self.at,
            on: self.on.clone(),
            face: self.face,
            direction: self.direction,
            axis: self.axis,
            angle: self.angle,
            length: self.length,
            to: self.to.clone(),
            trim: self.trim,
            label: self.label.clone(),
        })
    }
}

/// Which nodes a read is about. The empty filter is everything placed.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct QueryRequest {
    draft_id: Option<String>,
    #[serde(default)]
    ids: Vec<String>,
    kind: Option<String>,
    label: Option<String>,
    on: Option<String>,
    region: Option<[f64; 4]>,
}

impl QueryRequest {
    fn filter(&self) -> Filter {
        Filter {
            ids: self.ids.clone(),
            kind: self.kind.clone(),
            label: self.label.clone(),
            on: self.on.clone(),
            region: self.region,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TipRequest {
    draft_id: Option<String>,
    node_id: String,
    /// The direction the wanted end faces. Absent picks the only free end, or
    /// refuses listing them.
    end: Option<[f64; 3]>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AimRequest {
    nodes: Vec<String>,
    direction: Option<[f64; 3]>,
    at: Option<[f64; 3]>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftRequest {
    draft_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftRenderRequest {
    draft_id: String,
    view: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StampRequest {
    draft_id: String,
    at: [f64; 2],
    /// Radians about world up, as every angle on this boundary is.
    #[serde(default)]
    yaw: f64,
    #[serde(default)]
    trim: f64,
}

/// One chain op's answer: what was built, where it landed, and the end the next
/// op grows from.
fn built_json(built: &crate::services::stage_ops::Built, describe: &str) -> Value {
    json!({
        "node": built.node_id,
        "at": built.at.at,
        "z": built.at.z,
        "size": built.at.size,
        "tip": built.tip.as_ref().map(tip_json),
        "announce": built.announce,
        "placement": built.report,
        "describe": describe,
    })
}

fn tip_json(tip: &luma_scene::build::Tip) -> Value {
    json!({
        "node": tip.node,
        "socket": tip.socket,
        "direction": tip.direction,
        "at": tip.at,
    })
}

fn node_json(view: &crate::services::stage_ops::NodeView) -> Value {
    json!({
        "id": view.id,
        "kind": view.kind,
        "catalogRef": view.catalog_ref,
        "short": view.short,
        "label": view.label,
        "host": view.host,
        "at": view.at.at,
        "z": view.at.z,
        "size": view.at.size,
        "face": view.face,
        "tips": view.tips.iter().map(tip_json).collect::<Vec<_>>(),
    })
}

fn extent_json(extent: luma_scene::build::Extent) -> Value {
    json!({
        "count": extent.count,
        "min": extent.min,
        "max": extent.max,
        "centre": extent.centre,
        "size": extent.size,
    })
}

/// The one lowering of a stage refusal into a host-call error.
///
/// `refused` is its own code because it is the design's hard error and the
/// message is the fix: the Python facade raises `luma.VenueRefused` on it, and
/// on nothing else.
impl From<StageError> for HostCallError {
    fn from(error: StageError) -> Self {
        let message = error.to_string();
        match error {
            StageError::Refused(_) => HostCallError::new("refused", message),
            StageError::NotFound(_) => HostCallError::new("not_found", message),
            StageError::Internal(_) => HostCallError::new("internal", message),
        }
    }
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
