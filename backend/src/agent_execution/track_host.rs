//! Host capabilities behind `luma.track.edit()`.
//!
//! Python owns the ergonomic draft object. This adapter owns authority: its
//! score/track/venue/user scope is captured from the durable thread and auth
//! state, every candidate goes through the transaction service, and previews
//! use the production compositor. The worker protocol remains domain-free.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::runtime::Handle;

use crate::agent_execution::bindings::manifest::{AxisSpec, DType, Provenance, TensorRef};
use crate::agent_execution::cell_host::{call_limit, decode, supervise};
use crate::agent_execution::worker_process::{HostCallContext, HostCallError, HostCallHandler};
use crate::agent_execution::workspace::Workspace;
use crate::eval::context::resolve_primitive_ids;
use crate::eval::{Arena, Scope};
use crate::models::universe::UniverseState;
use crate::storage::StorageRoot;

/// The score a cell may read, captured by the host from the durable thread.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackScope {
    pub score_id: String,
    pub track_id: String,
    pub venue_id: String,
}

/// The same identity plus the principal allowed to write it. Deliberately
/// separate from a candidate: Python may describe a score, but it may never
/// choose which score that candidate replaces.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEditScope {
    pub score_id: String,
    pub track_id: String,
    pub venue_id: String,
    /// Authenticated user captured from the app database's active admission.
    pub user_id: String,
}

impl From<&TrackEditScope> for TrackScope {
    fn from(scope: &TrackEditScope) -> Self {
        Self {
            score_id: scope.score_id.clone(),
            track_id: scope.track_id.clone(),
            venue_id: scope.venue_id.clone(),
        }
    }
}

mod score;

const SAMPLES_PER_BEAT: f64 = 16.0;
const FALLBACK_SAMPLES_PER_SECOND: f64 = 32.0;
const MIN_SAMPLES: usize = 2;
const MAX_SAMPLES: usize = 2048;

/// One cell's track capability table. Construct only from host-resolved scope.
pub struct TrackHost {
    runtime: Handle,
    pool: SqlitePool,
    storage: StorageRoot,
    resource_root: PathBuf,
    workspace: Arc<Workspace>,
    scope: TrackScope,
    edit_scope: Option<TrackEditScope>,
    /// The draft this thread writes into, for a subagent. `None` means the
    /// thread edits the live score.
    draft_id: Option<String>,
}

impl TrackHost {
    pub fn new(
        runtime: Handle,
        pool: SqlitePool,
        storage: StorageRoot,
        resource_root: PathBuf,
        workspace: Arc<Workspace>,
        scope: TrackScope,
        edit_scope: Option<TrackEditScope>,
        draft_id: Option<String>,
    ) -> Self {
        Self {
            runtime,
            pool,
            storage,
            resource_root,
            workspace,
            scope,
            edit_scope,
            draft_id,
        }
    }

    async fn render_scene(
        &self,
        scene: crate::eval::Scene,
        start: f64,
        end: f64,
    ) -> Result<Value, HostCallError> {
        let resolved = resolve_primitive_ids(
            &self.pool,
            &self.scope.venue_id,
            &self.resource_root,
            &[],
            &[],
            &HashMap::new(),
            None,
        )
        .await;
        let light_ids: Vec<String> = resolved.into_iter().map(|(id, _)| id).collect();
        if light_ids.is_empty() {
            return Err(HostCallError::new(
                "invalid_venue",
                "the selected venue has no patched lights",
            ));
        }

        let beat_grid = crate::compositor::load_beat_grid(&self.pool, &self.scope.track_id)
            .await
            .map_err(|message| HostCallError::new("internal", message))?;
        let requested_times = sample_times(
            start,
            end,
            beat_grid.as_ref().map(|grid| f64::from(grid.bpm)),
        );
        let render_times: Vec<f32> = requested_times.iter().map(|time| *time as f32).collect();
        // The evaluator is f32; publish the exact coordinates it received,
        // rather than nearby f64 values that only existed before conversion.
        let times: Vec<f64> = render_times.iter().map(|time| f64::from(*time)).collect();
        let mut arena = Arena::default();
        let frames = scene.render(&render_times, Scope::Composite, &mut arena);
        let values = rgb_light_tensor(&frames, &light_ids);

        let descriptor = {
            let store = self.workspace.store();
            let mut store = store.lock().await;
            store.write_raw_f32(&values)
        }
        .map_err(|error| {
            HostCallError::new(
                "internal",
                format!("failed to store preview tensor: {error}"),
            )
        })?;
        let tensor = TensorRef::new(
            descriptor.id.clone(),
            DType::F32,
            vec![light_ids.len(), times.len(), 3],
            vec![
                AxisSpec::labels("light", light_ids),
                AxisSpec::coordinates("time", times, Some("s".into())),
                AxisSpec::labels("channel", vec!["r".into(), "g".into(), "b".into()]),
            ],
            Provenance::new("track_candidate_compositor")
                .with_note("production Scene composite; normalized RGB multiplied by dimmer"),
        );

        // ArtifactDescriptor uses its id as the manifest map key and therefore
        // skips it during ordinary serialization. A host-call response has no
        // enclosing map, so include the id explicitly for Python to register in
        // the namespace's existing ArtifactStore.
        let mut artifact = serde_json::to_value(&descriptor)
            .map_err(|error| HostCallError::new("internal", error.to_string()))?;
        artifact
            .as_object_mut()
            .expect("ArtifactDescriptor serializes as an object")
            .insert("id".into(), json!(descriptor.id.as_str()));
        let mut tensor = serde_json::to_value(tensor)
            .map_err(|error| HostCallError::new("internal", error.to_string()))?;
        tensor
            .as_object_mut()
            .expect("TensorRef serializes as an object")
            .insert("$kind".into(), json!("tensor"));

        Ok(json!({ "artifact": artifact, "tensor": tensor }))
    }
}

impl HostCallHandler for TrackHost {
    fn handle(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        // This handler is invoked from Workspace's blocking execution task.
        // Keeping the protocol synchronous makes Python methods ordinary while
        // Tokio still owns every database/compositor operation.
        context.check()?;
        let limit = call_limit(context)?;
        self.runtime.block_on(async {
            if matches!(
                method,
                "track.score_check"
                    | "track.score_upgrade"
                    | "track.graph_instance"
                    | "track.score_apply"
                    | "track.score_render"
                    | "track.graph_edit"
                    | "track.graph_customize"
                    | "track.score_independent"
            ) {
                return self.score_call(method, payload, context).await;
            }
            let _ = (payload, limit);
            Err(HostCallError::new(
                "unknown_method",
                format!("unknown track host method {method:?}"),
            ))
        })
    }
}

async fn validate_window(
    pool: &SqlitePool,
    track_id: &str,
    start: f64,
    end: f64,
) -> Result<(), HostCallError> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err(HostCallError::new(
            "invalid_window",
            "render window must be finite, non-negative, and have end > start",
        ));
    }
    if start > f64::from(f32::MAX) || end > f64::from(f32::MAX) {
        return Err(HostCallError::new(
            "invalid_window",
            "render window is outside the compositor's time range",
        ));
    }
    let duration = crate::database::local::tracks::get_track_duration(pool, track_id)
        .await
        .map_err(|message| HostCallError::new("internal", message))?;
    if duration.is_some_and(|duration| end > duration + 1e-6) {
        return Err(HostCallError::new(
            "invalid_window",
            format!("render window ends after the track ({end:.3}s)"),
        ));
    }
    Ok(())
}

fn sample_times(start: f64, end: f64, bpm: Option<f64>) -> Vec<f64> {
    let rate = bpm
        .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
        .map(|bpm| bpm / 60.0 * SAMPLES_PER_BEAT)
        .unwrap_or(FALLBACK_SAMPLES_PER_SECOND);
    let count = (((end - start) * rate).ceil() as usize).clamp(MIN_SAMPLES, MAX_SAMPLES);
    let step = (end - start) / count as f64;
    (0..count)
        .map(|index| start + index as f64 * step)
        .collect()
}

/// Row-major `[light, time, rgb]`, using one concept of light: RGB already
/// darkened by dimmer. Missing primitives are black.
fn rgb_light_tensor(frames: &[UniverseState], light_ids: &[String]) -> Vec<f32> {
    let mut values = Vec::with_capacity(light_ids.len() * frames.len() * 3);
    for light_id in light_ids {
        for frame in frames {
            if let Some(state) = frame.primitives.get(light_id) {
                values.extend(state.color.map(|channel| channel * state.dimmer));
            } else {
                values.extend([0.0; 3]);
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::universe::PrimitiveState;

    #[test]
    fn render_sampling_is_half_open() {
        let times = sample_times(10.0, 11.0, Some(120.0));
        assert_eq!(times.len(), 32);
        assert_eq!(times[0], 10.0);
        assert!(times.iter().all(|time| *time < 11.0));
        assert_eq!(times[31], 10.96875);
    }

    #[test]
    fn tensor_is_light_major_rgb_times_dimmer() {
        let mut first = UniverseState::default();
        first.primitives.insert(
            "a".into(),
            PrimitiveState {
                dimmer: 0.5,
                color: [1.0, 0.4, 0.2],
                strobe: 0.0,
                position: [0.0, 0.0],
                speed: 1.0,
            },
        );
        let second = UniverseState::default();
        assert_eq!(
            rgb_light_tensor(&[first, second], &["a".into(), "b".into()]),
            vec![0.5, 0.2, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,]
        );
    }
}
