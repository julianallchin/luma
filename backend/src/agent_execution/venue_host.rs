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
use crate::agent_execution::spoken_groups::SpokenGroups;
use crate::agent_execution::track_host::TrackHost;
use crate::agent_execution::worker_process::{HostCallContext, HostCallError, HostCallHandler};
use crate::agent_execution::workspace::Workspace;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::eval::{Arena, Scope};
use crate::models::distribute::DistributeLayout;
use crate::models::universe::UniverseState;
use crate::services::groups;
use crate::services::stage_ops::{AimTarget, Draft, Filter, Stage, StageError};
use crate::stage_render::{self, Shot, VenueGeometry, MAX_DIMENSION};
use crate::sync::triggers::TurnAttribution;
use luma_scene::environment::{HazeAppearance, SkyParams, VenueEnvironment, VenueHaze};
use luma_scene::venue_tiles::TileMap;
use luma_scene::View;

/// One cell's venue capability table. Construct only from host-resolved scope.
pub struct VenueHost {
    runtime: Handle,
    pool: SqlitePool,
    /// The fixtures root; the mesh root is derived from it.
    resource_root: PathBuf,
    workspace: Arc<Workspace>,
    venue_id: String,
    /// Who this cell's writes are attributed to and the turn they belong to.
    attribution: TurnAttribution,
    /// The score that lights the room, when the thread has one. Without it the
    /// rig uses the venue's saved environment, with geometry and no score beams.
    lighting: Option<Arc<TrackHost>>,
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
        resource_root: PathBuf,
        workspace: Arc<Workspace>,
        venue_id: String,
        attribution: TurnAttribution,
        lighting: Option<Arc<TrackHost>>,
    ) -> Self {
        Self {
            runtime,
            pool,
            resource_root,
            workspace,
            venue_id,
            attribution,
            lighting,
            drafts: std::sync::Mutex::default(),
        }
    }

    async fn render(&self, request: RenderRequest) -> Result<Value, HostCallError> {
        if request.edit.is_some() && request.highlight.is_some() {
            return Err(HostCallError::new(
                "invalid_preview",
                "choose an edit preview or a full-brightness group highlight, not both",
            ));
        }
        if request.camera.is_some() && request.focus.is_some() {
            return Err(HostCallError::new(
                "invalid_camera",
                "choose camera= to place the camera or focus= to frame something, not both",
            ));
        }
        let view = View::from_str(&request.view)
            .map_err(|error| HostCallError::new("invalid_view", error.to_string()))?;
        let width = checked_dimension("width", request.width)?;
        let height = checked_dimension("height", request.height)?;
        let time = self.checked_time(request.t).await?;
        let lighting = dialed(request.house, request.sun)?;

        // One venue snapshot for the geometry *and* the selection: a mark on a
        // fixture that is not in the frame is worse than no mark at all.
        let mut access = self.read_access().await?;
        let mut geometry = VenueGeometry::read(&mut access, &self.resource_root)
            .await
            .map_err(|error| HostCallError::new("invalid_venue", error))?;
        // A frame-local light, written over the room's own after it was read
        // and never written back: `venue.environment` is the edit, this is the
        // exposure.
        if let Some(environment) = lighting {
            geometry.environment = environment;
        }
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
        let state = match &request.highlight {
            None => self.state_at(time, request.edit.as_ref()).await?,
            Some(targets) => Some(stage_render::highlight_members(
                &self.marked(&mut access, &geometry, targets).await?,
            )),
        };
        let lens = match (&request.camera, &request.focus) {
            (Some(camera), _) => Lens::Exact(camera.camera()?),
            (None, Some(targets)) => {
                Lens::Focus(view, self.marked(&mut access, &geometry, targets).await?)
            }
            (None, None) => Lens::View(view),
        };
        drop(access);

        self.shoot(
            geometry,
            Shoot {
                lens,
                time,
                state,
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
            lens,
            time,
            state,
            size: (width, height),
            bare,
        } = shot;
        let (mut scene, definitions) = geometry.scene();
        // The look the operator's stage uses, so a still is what they see:
        // the same metered exposure, curve and glare.
        if let Ok(settings) = crate::settings::load_settings(&self.pool).await {
            scene.render.look = luma_render::scene_desc::Look::stored(&settings.stage_look);
        }
        if bare {
            // A draft is looked at for its shape. The floor and its grid are
            // the *room's* furniture, and a component being previewed is not in
            // a room yet.
            scene.render.show_grid = false;
            scene.render.show_floor = false;
        }
        let booth = geometry.booth();
        let (camera, visible) = lens.camera(&scene, &definitions, booth, (width, height));
        let lasers = geometry.lasers.clone();
        let lights = geometry.lights();
        let meshes_root = crate::resources::meshes_root(Some(&self.resource_root));

        // The GPU render is a blocking submit-and-map. Running it on a blocking
        // thread is what keeps `supervise`'s cancellation poll alive across it.
        let png = tokio::task::spawn_blocking(move || {
            stage_render::render_png(
                scene,
                definitions,
                Shot {
                    aim: stage_render::Aim::Camera(camera),
                    booth,
                    state,
                    time,
                    size: (width, height),
                    lasers,
                    lights,
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
        let eye = camera.position();
        Ok(json!({
            "artifactRel": artifact_rel,
            "width": width,
            "height": height,
            "view": lens.name(),
            "t": time,
            "camera": {
                "eye": [eye.x, eye.y, eye.z],
                "target": [camera.target.x, camera.target.y, camera.target.z],
                "fov": camera.fov_y_deg,
                "looking": camera.describe(),
            },
            "visible": visible,
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
        let mut access = self.read_access().await?;
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

    /// The room's own light: read it, or move it.
    ///
    /// One verb for both directions because there is one value. Called with no
    /// arguments it reads; called with any it writes, and the write is venue
    /// truth — it lands on the venue record, so every later picture of this
    /// room, here or in the editor, is taken under it. The frame-local
    /// override lives on `venue.render` instead.
    ///
    /// The scalar picks the mode: `house` is an indoor room's rig, `sun` is the
    /// sky, and a room is lit by one or the other. `mode` alone switches
    /// without moving a dial — it keeps the scalar the room already had in that
    /// mode, or takes that mode's default the first time it is entered.
    async fn environment(&self, request: EnvironmentRequest) -> Result<Value, HostCallError> {
        let dial = dialed(request.house, request.sun)?;
        let mode = request.mode.as_deref().map(Mode::parse).transpose()?;
        if let (Some(mode), Some(dial)) = (mode, dial) {
            if mode != Mode::of(dial) {
                return Err(HostCallError::new(
                    "invalid_argument",
                    format!(
                        "{}= is the dial of an {} room, so it disagrees with mode={:?}",
                        Mode::of(dial).dial(),
                        Mode::of(dial).word(),
                        mode.word(),
                    ),
                ));
            }
        }

        // A pure read never opens a write transaction: an agent looking at the
        // room should not queue behind one that is building in it.
        if mode.is_none() && dial.is_none() {
            let mut access = self.read_access().await?;
            return Ok(described(self.current(&mut access).await?));
        }

        let mut access = self.write_access().await.map_err(|error| {
            HostCallError::new(
                "invalid_venue",
                format!("the venue is not available: {error}"),
            )
        })?;
        let current = self.current(&mut access).await?;
        let wanted = match (dial, mode) {
            (Some(dial), _) => dial,
            // Mode alone: the room keeps the dial it had, if it had one in this
            // mode, so switching out and back is not a way to lose a setting.
            (None, Some(Mode::Indoor)) => VenueEnvironment::indoor(match current {
                VenueEnvironment::Indoor { .. } => current.house_level(),
                VenueEnvironment::Outdoor { .. } => VenueEnvironment::default().house_level(),
            }),
            (None, Some(Mode::Outdoor)) => VenueEnvironment::outdoor(match current {
                VenueEnvironment::Outdoor { .. } => current.sun_elevation_deg(),
                VenueEnvironment::Indoor { .. } => SkyParams::DUSK.sun_elevation_deg,
            }),
            (None, None) => unreachable!("a read returned above"),
        };
        // This verb has no azimuth, cloud or floor dial, so an outdoor room
        // keeps the side its sun is on and the weather it has, and either
        // keeps its floor while the other kind of room does not offer it.
        let wanted = match current {
            VenueEnvironment::Outdoor { .. } => wanted
                .with_sun_azimuth(current.sun_azimuth_deg())
                .with_clouds(current.clouds()),
            VenueEnvironment::Indoor { .. } => wanted,
        }
        .with_floor(current.floor());
        crate::database::local::venues::set_environment(&mut access, wanted)
            .await
            .map_err(|error| HostCallError::new("internal", error))?;
        access
            .commit()
            .await
            .map_err(|error| HostCallError::new("internal", error))?;
        Ok(described(wanted))
    }

    /// One verb for both directions, like [`Self::environment`], and for the
    /// same reason: there is one value. Called with no arguments it reads;
    /// called with any it writes, and the write is venue truth — it lands on
    /// the venue record, so every later picture of this room is taken through
    /// it.
    ///
    /// Only the room's *look* is settable here. How many samples the march
    /// takes and at what resolution are the cost of drawing a frame, not a
    /// property of the room, so they are not on this verb and not on the
    /// record; see [`VenueHaze`].
    async fn haze(&self, request: HazeRequest) -> Result<Value, HostCallError> {
        // A pure read never opens a write transaction: an agent looking at the
        // room should not queue behind one that is building in it.
        if request.is_read() {
            let mut access = self.read_access().await?;
            return Ok(described_haze(self.current_haze(&mut access).await?));
        }

        let mut access = self.write_access().await.map_err(|error| {
            HostCallError::new(
                "invalid_venue",
                format!("the venue is not available: {error}"),
            )
        })?;
        let wanted = request.applied_to(self.current_haze(&mut access).await?);
        crate::database::local::venues::set_haze(&mut access, wanted)
            .await
            .map_err(|error| HostCallError::new("internal", error))?;
        access
            .commit()
            .await
            .map_err(|error| HostCallError::new("internal", error))?;
        Ok(described_haze(wanted))
    }

    /// This thread's venue, open for reading.
    /// `targets` resolved in the venue behind `access`: what a highlight
    /// lights and a focus frames. A name or an id that is not in the venue,
    /// or a cell past a fixture's last, is refused rather than skipped.
    async fn marked(
        &self,
        access: &mut VenueAccess<'_, Read>,
        geometry: &VenueGeometry,
        targets: &Targets,
    ) -> Result<Vec<stage_render::Marked>, HostCallError> {
        let invalid = |error: String| HostCallError::new("invalid_selection", error);
        let expression = match targets {
            Targets::Expression(expression) => expression,
            Targets::Members(members) => return marked_members(geometry, members).map_err(invalid),
        };
        let selection = SpokenGroups::read(&self.resource_root, access)
            .await
            .map_err(invalid)?
            .selection(expression)
            .map_err(invalid)?;
        let resolved = groups::resolve_selection(&self.resource_root, access, &selection)
            .await
            .map_err(invalid)?;
        Ok(resolved
            .into_iter()
            .map(|entry| stage_render::Marked {
                fixture: entry.fixture.id,
                heads: entry.heads,
            })
            .collect())
    }

    async fn read_access(&self) -> Result<VenueAccess<'_, Read>, HostCallError> {
        VenueAccess::<Read>::read(&self.pool, VenueResource::Venue(&self.venue_id))
            .await
            .map_err(|error| {
                HostCallError::new(
                    "invalid_venue",
                    format!("the venue is not available: {error}"),
                )
            })
    }

    /// This thread's venue, open for writing as the thread's actor.
    async fn write_access(&self) -> Result<VenueAccess<'_, Write>, String> {
        VenueAccess::<Write>::write_as(
            &self.pool,
            VenueResource::Venue(&self.venue_id),
            self.attribution.get(),
        )
        .await
    }

    /// The environment on the venue record right now.
    async fn current(
        &self,
        access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    ) -> Result<VenueEnvironment, HostCallError> {
        Ok(crate::database::local::venues::get_venue(access)
            .await
            .map_err(|error| {
                HostCallError::new(
                    "invalid_venue",
                    format!("the venue could not be read: {error}"),
                )
            })?
            .environment)
    }

    /// The haze on the venue record right now.
    async fn current_haze(
        &self,
        access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    ) -> Result<VenueHaze, HostCallError> {
        Ok(crate::database::local::venues::get_venue(access)
            .await
            .map_err(|error| {
                HostCallError::new(
                    "invalid_venue",
                    format!("the venue could not be read: {error}"),
                )
            })?
            .haze)
    }

    // -- the build verbs ------------------------------------------------

    /// This thread's venue, bound to the verbs.
    fn stage(&self) -> Stage<'_> {
        Stage::new(&self.pool, &self.resource_root, &self.venue_id)
            .attributed(self.attribution.get())
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
    fn fixture_library(&self, request: FixturesRequest) -> Result<Value, HostCallError> {
        let found = crate::services::fixtures::library(
            &self.resource_root,
            request.query.as_deref().unwrap_or_default(),
            request.limit.min(FIXTURE_PAGE_MAX),
        )
        .map_err(|error| HostCallError::new("internal", error))?;
        Ok(json!({ "fixtures": found }))
    }

    /// The tree as text — the channel a program reads after every mutation.
    async fn describe(&self, request: DescribeRequest) -> Result<Value, HostCallError> {
        let text = if request.detail {
            self.stage().describe().await?
        } else {
            self.stage().summary().await?
        };
        Ok(json!({ "text": text }))
    }

    /// Everything the solve left open: the dangling sockets.
    ///
    /// Live, not the cell's binding snapshot: a build script asks this *after*
    /// it has changed the rig, and a manifest assembled before the cell ran
    /// would answer about the room it walked into.
    async fn open(&self) -> Result<Value, HostCallError> {
        let venue = self.stage().resolved().await?;
        Ok(json!({ "dangling": venue.dangling }))
    }

    /// Saved fixture collections, read live after the cell's edits.
    async fn groups(&self) -> Result<Value, HostCallError> {
        let mut access = self.read_access().await?;
        let sources = groups::GroupSources::read(&self.resource_root, &mut access)
            .await
            .map_err(|error| {
                HostCallError::new(
                    "invalid_venue",
                    format!("the venue's groups could not be read: {error}"),
                )
            })?;
        let rows =
            crate::agent_execution::bindings::providers::venue::group_rows(&sources.hierarchy());
        Ok(json!({ "groups": rows }))
    }

    async fn group(&self, request: GroupRequest) -> Result<Value, HostCallError> {
        let mut access = self
            .write_access()
            .await
            .map_err(|error| HostCallError::new("invalid_group", error))?;
        let members: Vec<(String, i64)> = request
            .fixtures
            .iter()
            .map(|member| match member.rsplit_once(':') {
                Some((id, cell)) if cell.parse::<i64>().is_ok() => {
                    (id.to_string(), cell.parse().expect("checked"))
                }
                _ => (
                    member.clone(),
                    crate::database::local::groups::WHOLE_FIXTURE,
                ),
            })
            .collect();
        let group = groups::set_named_group(
            &self.resource_root,
            &mut access,
            &request.name,
            &members,
            request.replace,
        )
        .await
        .map_err(|error| HostCallError::new("invalid_group", error))?;
        access
            .commit()
            .await
            .map_err(|error| HostCallError::new("invalid_group", error))?;
        crate::database::local::venue_graph::rig_changed();
        let rows = crate::agent_execution::bindings::providers::venue::group_rows(&[group]);
        Ok(json!({ "group": rows.into_iter().next() }))
    }

    /// The patch, read now: what `luma.venue.fixtures` shows, after this
    /// cell's own edits.
    async fn fixtures(&self) -> Result<Value, HostCallError> {
        let mut access = self.read_access().await?;
        let invalid = |error: String| HostCallError::new("invalid_venue", error);
        let venue = crate::venue_graph::resolved(&mut access, &self.resource_root)
            .await
            .map_err(|error| invalid(error.to_string()))?;
        let rows = crate::database::local::fixtures::get_patched_fixtures(&mut access)
            .await
            .map_err(invalid)?;
        Ok(json!({
            "fixtures": crate::agent_execution::bindings::providers::venue::fixture_rows(
                &rows, &venue,
            ),
        }))
    }

    /// Set one fixture's DMX universe and address by hand, and pin it there.
    /// Either left out keeps where the fixture stands. Refused, with the
    /// reason, when it runs off the universe or lands on another fixture.
    async fn patch(&self, request: PatchRequest) -> Result<Value, HostCallError> {
        let invalid = |error: String| HostCallError::new("invalid_patch", error);
        let mut access = self.write_access().await.map_err(invalid)?;
        let rows = crate::database::local::fixtures::get_patched_fixtures(&mut access)
            .await
            .map_err(invalid)?;
        let row = rows
            .iter()
            .find(|row| row.id == request.fixture)
            .ok_or_else(|| invalid(format!("no fixture `{}` in this venue", request.fixture)))?;
        let universe = request
            .universe
            .unwrap_or(u16::try_from(row.universe).unwrap_or(1));
        let address = request
            .address
            .unwrap_or(u16::try_from(row.address).unwrap_or(1));
        crate::services::patch::set_address(&mut access, &request.fixture, universe, address)
            .await
            .map_err(|error| invalid(error.to_string()))?;
        access.commit().await.map_err(invalid)?;
        crate::database::local::venue_graph::rig_changed();
        Ok(json!({ "universe": universe, "address": address, "pinned": true }))
    }

    /// Delete a collection by name. A selection that named it keeps it listed
    /// and selects nothing there, so the answer says what selected it.
    async fn remove_group(&self, request: RemoveGroupRequest) -> Result<Value, HostCallError> {
        let invalid = |error: String| HostCallError::new("invalid_group", error);
        let mut access = self.write_access().await.map_err(invalid)?;
        let name = crate::models::groups::normalize_group_name(&request.name);
        let group = crate::database::local::groups::list_groups(&mut access)
            .await
            .map_err(invalid)?
            .into_iter()
            .find(|group| group.name.as_deref() == Some(name.as_str()))
            .ok_or_else(|| invalid(format!("no group named `{name}` in this venue")))?;
        let usage = crate::dispatch::handlers::group_references::usage(&mut access)
            .await
            .map_err(invalid)?
            .into_iter()
            .find(|usage| usage.group_id == group.id);
        crate::database::local::groups::delete_group(&mut access, &group.id)
            .await
            .map_err(invalid)?;
        access.commit().await.map_err(invalid)?;
        crate::database::local::venue_graph::rig_changed();
        Ok(json!({
            "name": name,
            "clips": usage.as_ref().map_or(0, |usage| usage.clips),
            "scores": usage.as_ref().map_or(0, |usage| usage.scores.len()),
            "perform": usage
                .as_ref()
                .map_or(0, |usage| usage.perform_patterns.len() + usage.perform_cells),
        }))
    }

    async fn generate_groups(&self) -> Result<Value, HostCallError> {
        let mut access = self
            .write_access()
            .await
            .map_err(|error| HostCallError::new("invalid_group", error))?;
        groups::snapshot_generated_groups(&self.resource_root, &mut access)
            .await
            .map_err(|error| HostCallError::new("invalid_group", error))?;
        access
            .commit()
            .await
            .map_err(|error| HostCallError::new("invalid_group", error))?;
        crate::database::local::venue_graph::rig_changed();
        self.groups().await
    }

    /// The window the venue's laser clips draw in; given one, set first.
    async fn laser_window(&self, request: LaserWindowRequest) -> Result<Value, HostCallError> {
        let invalid = |error: String| HostCallError::new("invalid_laser_window", error);
        if let LaserWindowRequest {
            turn: Some(turn),
            tilt: Some(tilt),
        } = request
        {
            let mut access = self.write_access().await.map_err(invalid)?;
            let window = crate::laser::Window { turn, tilt };
            crate::database::local::lasers::set_window(&mut access, Some(window))
                .await
                .map_err(invalid)?;
            crate::venue_graph::commit_graph(access)
                .await
                .map_err(invalid)?;
        } else if request.turn.is_some() || request.tilt.is_some() {
            return Err(invalid(
                "A laser window is set with both its turn and its tilt".into(),
            ));
        }
        let mut access = self.read_access().await?;
        let rig = crate::laser::LaserRig::load(&mut access, &self.resource_root)
            .await
            .map_err(|error| HostCallError::new("invalid_venue", error))?;
        Ok(json!({ "window": rig.window(), "set": rig.window.is_some() }))
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

    /// Delete nodes and everything under them, lights and their patch rows
    /// included — the same rule the page's context menu follows.
    ///
    /// A node that is not there is **satisfied**: removing a subtree removes
    /// everything under it, so a caller holding a list of ids from before is
    /// naming nodes that have already reached the state it asked for. Erroring
    /// on those would make "delete these six" a call nobody can write.
    async fn remove(&self, request: RemoveRequest) -> Result<Value, HostCallError> {
        for node_id in &request.node_ids {
            match self.stage().delete_subtree(node_id).await {
                Ok(_) => {}
                Err(StageError::Refused(message)) if message.contains("not in this venue") => {}
                Err(StageError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(json!({ "describe": self.stage().summary().await? }))
    }

    async fn params(&self, request: ParamsRequest) -> Result<Value, HostCallError> {
        let report = self
            .stage()
            .set_params(
                &request.node_id,
                request.params,
                request.label.as_deref(),
                crate::services::stage_ops::Frame::Plan,
            )
            .await?;
        self.placed(report).await
    }

    /// The face a caller means on a host: the one its vector points at, the
    /// one it named, or the host's only mounting face.
    async fn socket(
        &self,
        host: Option<&str>,
        face: Option<[f64; 3]>,
        named: Option<String>,
    ) -> Result<String, HostCallError> {
        Ok(match (face, named) {
            (Some(face), _) => self.stage().face_socket(host, face).await?,
            (None, Some(named)) => named,
            (None, None) => self.stage().default_face(host).await?,
        })
    }

    /// Give the room a size, or take it away, and paint its walls. A shade
    /// alone repaints the room it is.
    async fn room(&self, request: RoomRequest) -> Result<Value, HostCallError> {
        if request.size.is_some() || request.shade.is_none() {
            self.stage().set_room(request.size).await?;
        }
        if let Some(shade) = request.shade {
            self.stage().set_wall_shade(shade).await?;
        }
        Ok(json!({ "describe": self.stage().summary().await? }))
    }

    /// Move one light, keeping its id.
    async fn move_light(&self, request: MoveRequest) -> Result<Value, HostCallError> {
        let socket = match (&request.host_node_id, request.face, request.stay) {
            (None, None, true) => None,
            (host, face, _) => Some(self.socket(host.as_deref(), face, None).await?),
        };
        let moved = self
            .stage()
            .move_light(
                &request.node_id,
                socket
                    .as_deref()
                    .map(|socket| (request.host_node_id.as_deref(), socket)),
                request.layout,
            )
            .await?;
        let mut placed = self.placed(moved.report).await?;
        placed["rehung"] = json!(moved.rehung);
        Ok(placed)
    }

    /// The fixture constructor: place, name and patch a row in one
    /// transaction. A fit failure is a report, not an error.
    ///
    /// Nothing republishes the DMX output patch here: a cell has no live output,
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
            let socket = draft.face(supply, &host, face)?;
            // A point is put on the face now, in the draft's own directions:
            // the stamp may turn the piece, and the light turns with it.
            let layout = match request.layout.on_face() {
                Err(point) if point.trim != 0.0 => {
                    return Err(HostCallError::new(
                        "invalid_argument",
                        "a light in a draft has no height of its own until the stamp; \
                         lift it afterwards with v.trim(light, trim=...)",
                    ));
                }
                Err(point) => {
                    let (metres, across) =
                        draft.surface_point(supply, &host, &socket, [point.u, point.v])?;
                    DistributeLayout::At { metres, across }
                }
                Ok(_) => request.layout,
            };
            let row = draft.record(crate::services::stage_ops::PendingRow {
                host,
                face,
                fixture_path: request.fixture_path.clone(),
                mode_name: request.mode_name.clone(),
                count: request.count,
                layout,
                label_prefix: request.label_prefix.clone(),
                aim: None,
            });
            return Ok(json!({
                "report": {
                    "draftRow": row,
                    "fixtures": [],
                    "refusal": Value::Null,
                    "warnings": [
                        "recorded in the draft — the row is patched when the draft is stamped"
                    ],
                    "dangling": [],
                },
                "describe": draft.summary(supply),
            }));
        }
        let socket = Some(
            self.socket(
                request.host_node_id.as_deref(),
                request.face,
                request.host_socket.clone(),
            )
            .await?,
        );
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
            "describe": self.stage().summary().await?,
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
            return Ok(built_json(&built));
        }
        let built = self.stage().chain(&plan).await?;
        Ok(built_json(&built))
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
            ids: Some(vec![request.node_id.clone()]),
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
            "describe": self.stage().summary().await?,
        }))
    }

    // -- drafts ----------------------------------------------------------

    /// The geometry supply, resolved once per process.
    fn supply(&self) -> Result<&'static luma_scene::catalog::VenueSockets, HostCallError> {
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

    /// Point a row of lights a draft recorded. See
    /// [`crate::services::stage_ops::PendingRow::aim`].
    fn draft_aim(&self, request: DraftAimRequest) -> Result<Value, HostCallError> {
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
        let mut drafts = self.drafts();
        let draft = Self::draft_mut(&mut drafts, &request.draft_id)?;
        for row in &request.rows {
            draft.aim(
                *row,
                match target {
                    AimTarget::Direction(d) => AimTarget::Direction(d),
                    AimTarget::At(p) => AimTarget::At(p),
                },
            )?;
        }
        Ok(json!({ "aimed": request.rows.len() }))
    }

    /// Drop a piece and everything on it from a draft.
    fn draft_remove(&self, request: DraftNodesRequest) -> Result<Value, HostCallError> {
        let supply = self.supply()?;
        let mut drafts = self.drafts();
        let draft = Self::draft_mut(&mut drafts, &request.draft_id)?;
        for node_id in &request.node_ids {
            draft.remove(supply, node_id);
        }
        Ok(json!({ "text": draft.summary(supply) }))
    }

    /// Edit one of a draft's own nodes.
    fn draft_params(&self, request: DraftParamsRequest) -> Result<Value, HostCallError> {
        let supply = self.supply()?;
        let mut drafts = self.drafts();
        let draft = Self::draft_mut(&mut drafts, &request.draft_id)?;
        draft.set_params(supply, &request.node_id, &request.params)?;
        Ok(json!({ "text": draft.summary(supply) }))
    }

    fn draft_describe(&self, request: DraftRequest) -> Result<Value, HostCallError> {
        let drafts = self.drafts();
        let supply = self.supply()?;
        Ok(json!({ "text": Self::draft(&drafts, &request.draft_id)?.describe(supply) }))
    }

    /// A picture of the draft alone: the same render path, framed on nothing
    /// but what the component built.
    async fn draft_render(&self, request: DraftRenderRequest) -> Result<Value, HostCallError> {
        let view = View::from_str(&request.view)
            .map_err(|error| HostCallError::new("invalid_view", error.to_string()))?;
        let width = checked_dimension("width", request.width)?;
        let height = checked_dimension("height", request.height)?;
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
            // A draft is a component being looked at for its shape, not a room.
            // The house at full is the light that shows one.
            environment: luma_scene::environment::VenueEnvironment::default(),
            // Likewise: a shape is read off a clear picture of it.
            haze: luma_scene::environment::VenueHaze::default(),
            lasers: Default::default(),
        };
        self.shoot(
            geometry,
            Shoot {
                lens: Lens::View(view),
                time: 0.0,
                state: None,
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
            "describe": self.stage().summary().await?,
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
            "placement": placement_json(&report),
            "describe": placement_text(&report),
        }))
    }

    /// The evaluated universe at `time`, or `None` when this thread has no
    /// score to light the room with.
    async fn state_at(
        &self,
        time: f32,
        edit: Option<&Value>,
    ) -> Result<Option<UniverseState>, HostCallError> {
        let Some(track) = self.lighting.as_ref() else {
            if edit.is_some() {
                return Err(HostCallError::new(
                    "no_track",
                    "an edit preview needs a track in scope",
                ));
            }
            return Ok(None);
        };
        let scene = match edit {
            Some(edit) => {
                let score = track.named_score(edit.clone()).await?;
                track.prepare_score(&score).await?
            }
            None => track.saved_scene().await?,
        };
        let mut arena = Arena::default();
        Ok(scene
            .try_render(&[time], Scope::Composite, &mut arena)
            .map_err(|error| HostCallError::new("evaluation_error", error))?
            .into_iter()
            .next())
    }

    /// `t` clamped to the track's end; a non-finite or negative `t` is refused.
    async fn checked_time(&self, requested: f64) -> Result<f32, HostCallError> {
        if !requested.is_finite() || requested < 0.0 {
            return Err(HostCallError::new(
                "invalid_time",
                format!("t must be a finite number of seconds from 0, not {requested}"),
            ));
        }
        let end = match self.lighting.as_ref() {
            Some(track) => {
                crate::database::local::tracks::get_track_duration(&self.pool, track.track_id())
                    .await
                    .map_err(|error| HostCallError::new("internal", error))?
            }
            None => None,
        };
        // Past the end clamps to the end, as a score window does; the
        // response's `t` says where the frame was taken.
        Ok(end.map_or(requested, |end| requested.min(end)) as f32)
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
                    "venue.environment" => self.environment(decode(payload)?).await,
                    "venue.haze" => self.haze(decode(payload)?).await,
                    "venue.catalog" => self.catalog(),
                    "venue.fixture_library" => self.fixture_library(decode(payload)?),
                    "venue.describe" => self.describe(decode(payload)?).await,
                    "venue.open" => self.open().await,
                    "venue.groups" => self.groups().await,
                    "venue.group" => self.group(decode(payload)?).await,
                    "venue.fixtures" => self.fixtures().await,
                    "venue.patch" => self.patch(decode(payload)?).await,
                    "venue.remove_group" => self.remove_group(decode(payload)?).await,
                    "venue.generate_groups" => self.generate_groups().await,
                    "venue.laser_window" => self.laser_window(decode(payload)?).await,
                    "venue.reach" => self.reach(decode(payload)?).await,
                    "venue.place" => self.place(decode(payload)?).await,
                    "venue.attach" => self.attach(decode(payload)?).await,
                    "venue.extend" => self.extend(decode(payload)?).await,
                    "venue.duplicate" => self.duplicate(decode(payload)?).await,
                    "venue.remove" => self.remove(decode(payload)?).await,
                    "venue.params" => self.params(decode(payload)?).await,
                    "venue.distribute" => self.distribute(decode(payload)?).await,
                    "venue.move" => self.move_light(decode(payload)?).await,
                    "venue.room" => self.room(decode(payload)?).await,
                    "venue.chain" => self.chain(decode(payload)?).await,
                    "venue.query" => self.query(decode(payload)?).await,
                    "venue.extent" => self.extent(decode(payload)?).await,
                    "venue.tip" => self.tip(decode(payload)?).await,
                    "venue.aim" => self.aim_at(decode(payload)?).await,
                    "venue.stamp" => self.stamp(decode(payload)?).await,
                    "venue.draft.create" => self.draft_create(),
                    "venue.draft.discard" => self.draft_discard(decode(payload)?),
                    "venue.draft.describe" => self.draft_describe(decode(payload)?),
                    "venue.draft.aim" => self.draft_aim(decode(payload)?),
                    "venue.draft.remove" => self.draft_remove(decode(payload)?),
                    "venue.draft.params" => self.draft_params(decode(payload)?),
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

/// A frame side from 1 to [`MAX_DIMENSION`] pixels; any other value is refused.
fn checked_dimension(name: &str, value: u32) -> Result<u32, HostCallError> {
    if !(1..=MAX_DIMENSION).contains(&value) {
        return Err(HostCallError::new(
            "invalid_size",
            format!("{name} must be from 1 to {MAX_DIMENSION} pixels, not {value}"),
        ));
    }
    Ok(value)
}

/// Which of the two lighting models a call is talking about.
///
/// The words are [`VenueEnvironment::mode`]'s, parsed back — the record, the
/// verb and the editor's selector spell them once, in the renderer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Indoor,
    Outdoor,
}

impl Mode {
    fn parse(word: &str) -> Result<Self, HostCallError> {
        match word {
            "indoor" => Ok(Self::Indoor),
            "outdoor" => Ok(Self::Outdoor),
            other => Err(HostCallError::new(
                "invalid_argument",
                format!("mode is \"indoor\" or \"outdoor\", not {other:?}"),
            )),
        }
    }

    fn of(environment: VenueEnvironment) -> Self {
        match environment {
            VenueEnvironment::Indoor { .. } => Self::Indoor,
            VenueEnvironment::Outdoor { .. } => Self::Outdoor,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Indoor => "indoor",
            Self::Outdoor => "outdoor",
        }
    }

    /// The one dial this mode has, by the name the caller passes it under.
    fn dial(self) -> &'static str {
        match self {
            Self::Indoor => "house",
            Self::Outdoor => "sun",
        }
    }
}

/// One `house=` or `sun=` as an environment; `None` when neither was given.
///
/// The dial *is* the mode — a room is lit by its house rig or by the sky, so
/// naming a level names a room, and naming both names two rooms. A value that
/// is not finite or is outside its range is refused.
fn dialed(house: Option<f64>, sun: Option<f64>) -> Result<Option<VenueEnvironment>, HostCallError> {
    match (house, sun) {
        (Some(_), Some(_)) => Err(HostCallError::new(
            "invalid_argument",
            "a room is lit by its house rig or by the sky, not both: pass house= or sun=",
        )),
        (Some(level), None) => Ok(Some(VenueEnvironment::indoor(in_range(
            "house", level, 0.0, 1.0,
        )?))),
        (None, Some(deg)) => Ok(Some(VenueEnvironment::outdoor(in_range(
            "sun", deg, -90.0, 90.0,
        )?))),
        (None, None) => Ok(None),
    }
}

/// `value` when it is finite and inside `min..=max`; any other value is refused.
fn in_range(name: &str, value: f64, min: f64, max: f64) -> Result<f32, HostCallError> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(value as f32)
    } else {
        Err(HostCallError::new(
            "invalid_argument",
            format!("{name} must be a finite number from {min} to {max}, not {value}"),
        ))
    }
}

/// One environment as the wire carries it: the mode, and the dial that mode
/// has. The other mode's dial is `null` rather than zero — a room with no sun
/// in it does not have a sun at the horizon.
pub(crate) fn environment_record(environment: VenueEnvironment) -> Value {
    json!({
        "mode": environment.mode(),
        "house": matches!(environment, VenueEnvironment::Indoor { .. })
            .then(|| environment.house_level()),
        "sun": matches!(environment, VenueEnvironment::Outdoor { .. })
            .then(|| environment.sun_elevation_deg()),
    })
}

/// The same record with the sentence a reader actually wants beside it, so a
/// program that just moved the light can print what it did without formatting
/// two numbers itself.
fn described(environment: VenueEnvironment) -> Value {
    let mut value = environment_record(environment);
    value["describe"] = Value::String(match environment {
        VenueEnvironment::Indoor { .. } => {
            format!("indoor, house at {:.0}%", environment.house_level() * 100.0)
        }
        VenueEnvironment::Outdoor { .. } => {
            let deg = environment.sun_elevation_deg();
            let side = if deg < 0.0 { "below" } else { "above" };
            format!("outdoor, sun {:.1}\u{00b0} {side} the horizon", deg.abs())
        }
    });
    value
}

/// One haze as the wire carries it. Flat on purpose: the appearance dials are
/// nested on the record because they are one struct in the renderer, but to an
/// author they are seven dials on one room, and a nested object would be a
/// second shape to learn for no gain.
pub(crate) fn haze_record(haze: VenueHaze) -> Value {
    let haze = haze.sanitized();
    json!({
        "enabled": haze.enabled,
        "density": haze.density,
        "cloudiness": haze.appearance.cloudiness,
        "cloudSize": haze.appearance.cloud_size,
        "turbulence": haze.appearance.turbulence,
        "windSpeed": haze.appearance.wind_speed,
        "windDirection": haze.appearance.wind_direction,
    })
}

/// The same record with the sentence a reader actually wants beside it.
fn described_haze(haze: VenueHaze) -> Value {
    let haze = haze.sanitized();
    let mut value = haze_record(haze);
    value["describe"] = Value::String(if haze.enabled {
        format!(
            "haze density {:.2} (0 to {}), clouds {:.1} m, drifting {:.2} m/s",
            haze.density,
            VenueHaze::MAX_DENSITY,
            haze.appearance.cloud_size,
            haze.appearance.wind_speed,
        )
    } else {
        "no haze".to_string()
    });
    value
}

/// The venue's laser window, in degrees; empty to read it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaserWindowRequest {
    turn: Option<[f64; 2]>,
    tilt: Option<[f64; 2]>,
}

/// One fixture's DMX address, set by hand.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchRequest {
    fixture: String,
    universe: Option<u16>,
    address: Option<u16>,
}

/// Create a named collection, or explicitly replace its fixture membership.
#[derive(Deserialize)]
struct GroupRequest {
    name: String,
    fixtures: Vec<String>,
    #[serde(default)]
    replace: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveGroupRequest {
    name: String,
}

/// The room's own light. Every field absent is a read; any field present is a write.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentRequest {
    /// `"indoor"` or `"outdoor"`. Alone, it switches without moving a dial.
    mode: Option<String>,
    /// How far up the house rig is, 0 to 1. Implies an indoor room.
    /// Out of range is refused.
    house: Option<f64>,
    /// Degrees the sun stands above the horizon, -90 to 90. Implies an
    /// open-air room. Out of range is refused.
    sun: Option<f64>,
}

/// The room's atmosphere. Every field absent is a read; any field present is a
/// write, and a write is a *patch* — the fields not named keep the value the
/// room already had.
///
/// Patch rather than replace because the five appearance dials and the two
/// top-level ones are one value on the record but seven separate questions to
/// an author: "make it thicker" should not silently reset the wind.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HazeRequest {
    /// Whether the room has haze in it at all.
    enabled: Option<bool>,
    /// Mean density, 0 to 0.5. Values outside are clamped, not refused.
    density: Option<f64>,
    /// Density contrast, 0 (uniform) to 1 (pronounced clouds).
    cloudiness: Option<f64>,
    /// Characteristic cloud size in metres.
    cloud_size: Option<f64>,
    /// Strength of evolving deformation and fine wisps, 0 to 1.
    turbulence: Option<f64>,
    /// Horizontal drift in metres per second.
    wind_speed: Option<f64>,
    /// World heading of that drift, in degrees.
    wind_direction: Option<f64>,
}

impl HazeRequest {
    /// Whether this call names nothing and is therefore a read.
    fn is_read(&self) -> bool {
        self.enabled.is_none()
            && self.density.is_none()
            && self.cloudiness.is_none()
            && self.cloud_size.is_none()
            && self.turbulence.is_none()
            && self.wind_speed.is_none()
            && self.wind_direction.is_none()
    }

    /// This patch over the haze the room already had.
    ///
    /// Out-of-range and non-finite values are handled once, by
    /// [`VenueHaze::sanitized`] at the record boundary, rather than here: an
    /// author asking for density 9 means "as thick as it goes", and there is
    /// nothing for them to fix.
    fn applied_to(&self, current: VenueHaze) -> VenueHaze {
        let appearance = current.appearance;
        VenueHaze {
            enabled: self.enabled.unwrap_or(current.enabled),
            density: patched(self.density, current.density),
            appearance: HazeAppearance {
                cloudiness: patched(self.cloudiness, appearance.cloudiness),
                cloud_size: patched(self.cloud_size, appearance.cloud_size),
                turbulence: patched(self.turbulence, appearance.turbulence),
                wind_speed: patched(self.wind_speed, appearance.wind_speed),
                wind_direction: patched(self.wind_direction, appearance.wind_direction),
            },
        }
        .sanitized()
    }
}

/// One optional dial over the value it is replacing. A non-finite number has no
/// nearest legal answer and no JSON spelling, so it leaves the dial alone
/// rather than poisoning the record.
fn patched(value: Option<f64>, current: f32) -> f32 {
    match value {
        Some(value) if (value as f32).is_finite() => value as f32,
        _ => current,
    }
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

/// The nodes a `remove` names. A list, because a removal is a *goal state* and
/// the goal is usually a selection.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveRequest {
    node_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoomRequest {
    /// `[width, depth, height]` in metres; `None` is open floor.
    size: Option<[f64; 3]>,
    /// How light the walls and ceiling are painted, 0 to 1.
    shade: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveRequest {
    node_id: String,
    /// The host to hang it on; `None` is the room, or with `stay` the host
    /// the light already has.
    host_node_id: Option<String>,
    face: Option<[f64; 3]>,
    /// Keep the face it hangs on and move along it.
    #[serde(default)]
    stay: bool,
    layout: DistributeLayout,
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
    lens: Lens,
    time: f32,
    state: Option<UniverseState>,
    size: (u32, u32),
    /// Draw the piece and nothing else — no floor, no grid. What a draft is
    /// previewed through.
    bare: bool,
}

/// Where a render is seen from.
enum Lens {
    /// A named view, fitted to the room.
    View(View),
    /// A camera placed outright.
    Exact(luma_scene::camera::Camera),
    /// These fixtures, framed from the named view's side and turned past
    /// whatever stands in front of them.
    Focus(View, Vec<stage_render::Marked>),
}

impl Lens {
    /// The camera, and for a focus the share of what it frames that it sees.
    fn camera(
        &self,
        scene: &luma_render::scene_desc::Scene,
        definitions: &std::collections::BTreeMap<String, luma_scene::definition::Definition>,
        booth: Option<glam::Vec3>,
        size: (u32, u32),
    ) -> (luma_scene::camera::Camera, Option<f32>) {
        match self {
            Self::View(view) => (
                stage_render::Aim::View(*view).camera(scene, definitions, booth, size),
                None,
            ),
            Self::Exact(camera) => (*camera, None),
            Self::Focus(view, marked) => {
                let from = stage_render::Aim::View(*view).camera(scene, definitions, booth, size);
                let head = glam::Vec3::splat(luma_scene::Framing::HEAD_RADIUS);
                let targets: Vec<luma_scene::Aabb> = scene
                    .fixtures
                    .iter()
                    .filter(|f| marked.iter().any(|m| m.fixture == f.id))
                    .map(|f| {
                        let at = luma_scene::coords::world_from_data(glam::Vec3::from(f.pos));
                        luma_scene::Aabb::new(at - head, at + head)
                    })
                    .collect();
                let focus = luma_scene::focus::focus(
                    &targets,
                    &scene.obstacles(),
                    from.position() - from.target,
                    &stage_render::viewfinder(scene, size),
                );
                (focus.camera, Some(focus.visible))
            }
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::View(view) | Self::Focus(view, _) => view.name(),
            Self::Exact(_) => "camera",
        }
    }
}

/// A render's `highlight` or `focus`: a selection expression, or fixtures and
/// cells named outright (`"par_1"`, `"bar_2:3"`).
#[derive(Deserialize)]
#[serde(untagged)]
enum Targets {
    Expression(String),
    Members(Vec<String>),
}

/// Fixtures and cells by id (`"par_1"`, `"bar_2:3"`), one entry per fixture
/// in the order first named. A whole fixture takes in any of its cells.
fn marked_members(
    geometry: &VenueGeometry,
    members: &[String],
) -> Result<Vec<stage_render::Marked>, String> {
    let mut marked: Vec<stage_render::Marked> = Vec::new();
    for member in members {
        let (id, cell) = match member.rsplit_once(':') {
            Some((id, cell)) => match cell.parse::<usize>() {
                Ok(cell) => (id, Some(cell)),
                Err(_) => (member.as_str(), None),
            },
            None => (member.as_str(), None),
        };
        let Some(fixture) = geometry.fixtures.iter().find(|f| f.id == id) else {
            return Err(format!("no fixture `{id}` in this venue"));
        };
        if let Some(cell) = cell {
            let cells = geometry.cells(fixture);
            if cell >= cells {
                return Err(format!(
                    "`{id}` has {cells} cell{}, numbered from 0; there is no cell {cell}",
                    if cells == 1 { "" } else { "s" }
                ));
            }
        }
        let entry = match marked.iter_mut().find(|m| m.fixture == id) {
            Some(entry) => entry,
            None => {
                marked.push(stage_render::Marked {
                    fixture: id.to_string(),
                    heads: Some(Vec::new()),
                });
                marked.last_mut().expect("just pushed")
            }
        };
        match (cell, &mut entry.heads) {
            (None, heads) => *heads = None,
            (Some(cell), Some(heads)) if !heads.contains(&cell) => heads.push(cell),
            _ => {}
        }
    }
    Ok(marked)
}

/// A camera placed outright, in facade metres.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CameraRequest {
    eye: [f32; 3],
    target: [f32; 3],
    fov: Option<f32>,
}

impl CameraRequest {
    fn camera(&self) -> Result<luma_scene::camera::Camera, HostCallError> {
        let fov = self.fov.unwrap_or(stage_render::FOV_Y_DEG);
        if !(5.0..=120.0).contains(&fov) {
            return Err(HostCallError::new(
                "invalid_camera",
                format!("fov is the vertical field of view, 5 to 120 degrees, not {fov}"),
            ));
        }
        let (eye, target) = (glam::Vec3::from(self.eye), glam::Vec3::from(self.target));
        if !eye.is_finite() || !target.is_finite() || eye.distance(target) < 0.05 {
            return Err(HostCallError::new(
                "invalid_camera",
                "eye and target must be finite points at least 5 cm apart",
            ));
        }
        Ok(luma_scene::camera::Camera::looking_from(eye, target, fov))
    }
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
    /// The nodes named outright. Absent narrows nothing; **present and empty**
    /// is a selection of nothing, and answers with nothing.
    #[serde(default)]
    ids: Option<Vec<String>>,
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

/// A recorded row of lights, and where it is asked to point.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftAimRequest {
    draft_id: String,
    /// Indexes handed back by the draft's own `distribute`.
    rows: Vec<usize>,
    direction: Option<[f64; 3]>,
    at: Option<[f64; 3]>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftNodesRequest {
    draft_id: String,
    node_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftParamsRequest {
    draft_id: String,
    node_id: String,
    params: std::collections::BTreeMap<String, f64>,
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
fn placement_json(report: &crate::models::venue_graph::PlacementReport) -> Value {
    json!({
        "nodeId": report.node_id,
        "parentId": report.parent_id, "warnings": report.warnings,
        "dangling": report.dangling, "constraints": report.constraints,
    })
}

fn placement_text(report: &crate::models::venue_graph::PlacementReport) -> String {
    let mut text = format!(
        "{} placed on {}\n",
        report.node_id,
        report.parent_id.as_deref().unwrap_or("no host")
    );
    for warning in &report.warnings {
        text.push_str(&format!("warning: {warning}\n"));
    }
    for constraint in &report.constraints {
        text.push_str(&format!(
            "constraint: {} -> {}: {}\n",
            constraint.node_id, constraint.target_node, constraint.status
        ));
    }
    text
}

fn built_json(built: &crate::services::stage_ops::Built) -> Value {
    json!({
        "node": built.node_id,
        "at": built.at.at,
        "z": built.at.z,
        "size": built.at.size,
        "tip": built.tip.as_ref().map(tip_json),
        "announce": built.announce,
        "placement": placement_json(&built.report),
        "describe": format!("{}at={:.2?}, z={:.2}, size={:.2?}\n", placement_text(&built.report), built.at.at, built.at.z, built.at.size),
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
    view.json()
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
struct DescribeRequest {
    #[serde(default)]
    detail: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenderRequest {
    view: String,
    t: f64,
    width: u32,
    height: u32,
    /// A selection expression, or fixtures and cells by id. Present, its
    /// heads are the only thing lit, and the score at `t` is not drawn at all.
    /// Absent, the score lights the room.
    highlight: Option<Targets>,
    /// A camera placed outright.
    camera: Option<CameraRequest>,
    /// Frame these, from `view`'s side.
    focus: Option<Targets>,
    /// A private candidate score, evaluated by the same compositor as apply.
    /// Omitted for the saved score. Never persisted or sent to the live viewport.
    edit: Option<Value>,
    /// Light this one frame with the house at `house`, whatever the room is.
    ///
    /// A camera setting, not an edit: it is written over the environment the
    /// venue was read with and never written back, so the room is unchanged
    /// the moment the shutter closes. `venue.environment` is the edit.
    house: Option<f64>,
    /// Light this one frame with the sun `sun` degrees up, whatever the room
    /// is. The same camera setting as [`Self::house`], and exclusive with it —
    /// one frame has one sky.
    sun: Option<f64>,
}
