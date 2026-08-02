//! Host capabilities behind `luma.track.edit()`.
//!
//! Python owns the ergonomic draft object. This adapter owns authority: its
//! score/track/venue/user scope is captured from the durable thread and auth
//! state, every candidate goes through the transaction service, and previews
//! use the production compositor. The worker protocol remains domain-free.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::runtime::Handle;

use crate::agent_execution::bindings::manifest::{AxisSpec, DType, Provenance, TensorRef};
use crate::agent_execution::worker_process::{HostCallContext, HostCallError, HostCallHandler};
use crate::agent_execution::workspace::Workspace;
use crate::compositor::build_scene_strict;
use crate::eval::context::resolve_primitive_ids;
use crate::eval::{Arena, Scope};
use crate::models::scores::TrackScore;
use crate::models::universe::UniverseState;
use crate::services::authored_documents::{AuthoredDocuments, AuthoredDocumentsError};
use crate::services::track_edits::{
    check_track_candidate, check_track_edit, TrackClip, TrackEditCheck, TrackEditError,
    TrackEditPlan, TrackEditScope, TrackScope,
};
use crate::storage::StorageRoot;

const SAMPLES_PER_BEAT: f64 = 16.0;
const FALLBACK_SAMPLES_PER_SECOND: f64 = 32.0;
const MIN_SAMPLES: usize = 2;
const MAX_SAMPLES: usize = 2048;
const MAX_HOST_CALL_DURATION: Duration = Duration::from_secs(30);
const HOST_CANCEL_POLL: Duration = Duration::from_millis(25);

/// One cell's track capability table. Construct only from host-resolved scope.
pub struct TrackHost {
    runtime: Handle,
    pool: SqlitePool,
    storage: StorageRoot,
    resource_root: PathBuf,
    workspace: Arc<Workspace>,
    authored: AuthoredDocuments,
    thread_id: String,
    scope: TrackScope,
    edit_scope: Option<TrackEditScope>,
}

impl TrackHost {
    pub fn new(
        runtime: Handle,
        pool: SqlitePool,
        storage: StorageRoot,
        resource_root: PathBuf,
        workspace: Arc<Workspace>,
        authored: AuthoredDocuments,
        thread_id: String,
        scope: TrackScope,
        edit_scope: Option<TrackEditScope>,
    ) -> Self {
        Self {
            runtime,
            pool,
            storage,
            resource_root,
            workspace,
            authored,
            thread_id,
            scope,
            edit_scope,
        }
    }

    async fn check(&self, plan: TrackEditPlan) -> Result<TrackEditCheck, HostCallError> {
        let edit_scope = self
            .edit_scope
            .as_ref()
            .ok_or_else(|| HostCallError::new("forbidden", "this authored track is read-only"))?;
        let checked = check_track_edit(&self.pool, edit_scope, plan)
            .await
            .map_err(edit_error)?;
        self.compile_all(&checked.candidate).await?;
        Ok(checked)
    }

    async fn compile_all(&self, clips: &[TrackClip]) -> Result<(), HostCallError> {
        let scores = as_track_scores(&self.scope, clips);
        build_scene_strict(
            &self.pool,
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.scope.track_id,
            &self.scope.venue_id,
            &scores,
        )
        .await
        .map(|_| ())
        .map_err(|message| HostCallError::new("compile_error", message))
    }

    async fn render(&self, request: TrackRenderRequest) -> Result<Value, HostCallError> {
        validate_window(
            &self.pool,
            &self.scope.track_id,
            request.start_time,
            request.end_time,
        )
        .await?;

        let checked = check_track_candidate(
            &self.pool,
            &self.scope,
            TrackEditPlan {
                base_revision: request.base_revision,
                candidate: request.candidate,
            },
        )
        .await
        .map_err(edit_error)?;

        // Keep full clip spans: span-relative patterns must retain their
        // original phase when a window begins in the middle of a clip.
        let visible: Vec<TrackClip> = checked
            .candidate
            .into_iter()
            .filter(|clip| clip.end_time > request.start_time && clip.start_time < request.end_time)
            .collect();
        let scores = as_track_scores(&self.scope, &visible);
        let scene = build_scene_strict(
            &self.pool,
            &self.pool,
            &self.storage,
            &self.resource_root,
            &self.scope.track_id,
            &self.scope.venue_id,
            &scores,
        )
        .await
        .map_err(|message| HostCallError::new("compile_error", message))?;

        let resolved = resolve_primitive_ids(
            &self.pool,
            &self.scope.venue_id,
            &self.resource_root,
            &[],
            &[],
            &HashMap::new(),
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
            request.start_time,
            request.end_time,
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
            Provenance::new("track_candidate_compositor").with_note(
                "production Scene composite; normalized linear RGB multiplied by dimmer",
            ),
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

    async fn commit(
        &self,
        plan: TrackEditPlan,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Value, HostCallError> {
        let edit_scope = self
            .edit_scope
            .as_ref()
            .ok_or_else(|| HostCallError::new("forbidden", "this authored track is read-only"))?;
        let result = self
            .authored
            .apply_track_edit_for_thread(
                &self.pool,
                Some(&edit_scope.user_id),
                &self.thread_id,
                &self.scope,
                operation_id,
                request_fingerprint,
                plan,
                "Apply track agent edit",
            )
            .await
            .map_err(authored_error)?;
        serde_json::to_value(result.edit)
            .map_err(|error| HostCallError::new("internal", error.to_string()))
    }

    async fn replay(
        &self,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Option<Value>, HostCallError> {
        let edit_scope = self
            .edit_scope
            .as_ref()
            .ok_or_else(|| HostCallError::new("forbidden", "this authored track is read-only"))?;
        self.authored
            .replay_track_edit_for_thread(
                &self.pool,
                Some(&edit_scope.user_id),
                &self.thread_id,
                &self.scope,
                operation_id,
                request_fingerprint,
            )
            .await
            .map(|replayed| {
                replayed
                    .map(|result| serde_json::to_value(result.edit))
                    .transpose()
            })
            .map_err(authored_error)?
            .map_err(|error| HostCallError::new("internal", error.to_string()))
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
        let limit = context
            .remaining()
            .ok_or_else(|| HostCallError::new("timeout", "the cell deadline has expired"))?
            .min(MAX_HOST_CALL_DURATION);
        self.runtime.block_on(async {
            if method == "track.apply" {
                let plan: TrackEditPlan = decode(payload)?;
                let edit_scope = self.edit_scope.as_ref().ok_or_else(|| {
                    HostCallError::new("forbidden", "this authored track is read-only")
                })?;
                let operation_scope = context.operation_scope().ok_or_else(|| {
                    HostCallError::new(
                        "internal",
                        "editable Python cell has no durable operation scope",
                    )
                })?;
                let request_fingerprint = apply_request_fingerprint(edit_scope, &plan)?;
                let operation_id =
                    apply_operation_id(operation_scope.operation_namespace(), &request_fingerprint);
                if let Some(replayed) = supervise(
                    self.replay(&operation_id, &request_fingerprint),
                    context,
                    limit,
                )
                .await?
                {
                    return Ok(replayed);
                }
                // Compilation and read-only validation remain cancellable. Once
                // they pass, atomically choose between Stop and the write, then
                // await COMMIT to an authoritative result. Dropping a SQLx
                // commit future is commit-ambiguous and is never allowed here.
                supervise(self.check(plan.clone()), context, limit).await?;
                context.begin_irreversible()?;
                return self.commit(plan, &operation_id, &request_fingerprint).await;
            }

            supervise(
                async {
                    match method {
                        "track.check" => {
                            let plan = decode(payload)?;
                            let checked = self.check(plan).await?;
                            serde_json::to_value(checked)
                                .map_err(|error| HostCallError::new("internal", error.to_string()))
                        }
                        "track.render" => self.render(decode(payload)?).await,
                        _ => Err(HostCallError::new(
                            "unknown_method",
                            format!("unknown track host method {method:?}"),
                        )),
                    }
                },
                context,
                limit,
            )
            .await
        })
    }
}

fn apply_operation_id(operation_namespace: &str, request_fingerprint: &str) -> String {
    format!(
        "python-{:x}",
        scoped_apply_digest(
            b"luma.python-track-apply-operation.v1",
            &[operation_namespace, request_fingerprint],
        )
    )
}

fn apply_request_fingerprint(
    scope: &TrackEditScope,
    plan: &TrackEditPlan,
) -> Result<String, HostCallError> {
    #[derive(Serialize)]
    struct Request<'a> {
        scope: &'a TrackEditScope,
        plan: &'a TrackEditPlan,
    }

    let value = serde_json::to_value(Request { scope, plan })
        .map_err(|error| HostCallError::new("invalid_request", error.to_string()))?;
    let canonical = crate::canonical_json::to_string(&value);
    Ok(format!(
        "sha256:{:x}",
        scoped_apply_digest(b"luma.python-track-apply-request.v1", &[&canonical])
    ))
}

fn scoped_apply_digest(domain: &[u8], values: &[&str]) -> impl std::fmt::LowerHex {
    let mut hash = Sha256::new();
    hash.update(domain);
    for value in values {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize()
}

async fn supervise<T>(
    operation: impl Future<Output = Result<T, HostCallError>>,
    context: &HostCallContext,
    limit: Duration,
) -> Result<T, HostCallError> {
    tokio::pin!(operation);
    let timeout = tokio::time::sleep(limit);
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            biased;
            result = &mut operation => break result,
            _ = &mut timeout => {
                break Err(HostCallError::new(
                    "timeout",
                    format!("track host call exceeded {:.0}s", limit.as_secs_f64()),
                ));
            }
            _ = tokio::time::sleep(HOST_CANCEL_POLL) => {
                if context.is_cancelled() {
                    break Err(HostCallError::new(
                        "cancelled",
                        "the track host call was cancelled",
                    ));
                }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackRenderRequest {
    base_revision: String,
    candidate: Vec<TrackClip>,
    start_time: f64,
    end_time: f64,
}

fn decode<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T, HostCallError> {
    serde_json::from_value(payload)
        .map_err(|error| HostCallError::new("invalid_request", error.to_string()))
}

fn edit_error(error: TrackEditError) -> HostCallError {
    let code = match &error {
        TrackEditError::Conflict { .. } => "conflict",
        TrackEditError::Invalid { .. } => "invalid_edit",
        TrackEditError::Scope { .. } => "forbidden",
        TrackEditError::Storage { .. } => "internal",
    };
    HostCallError::new(code, error.to_string())
}

fn authored_error(error: AuthoredDocumentsError) -> HostCallError {
    match error {
        AuthoredDocumentsError::Track(error) => edit_error(error),
        AuthoredDocumentsError::Invalid(message) => HostCallError::new("invalid_edit", message),
        AuthoredDocumentsError::Scope(message) => HostCallError::new("forbidden", message),
        error => HostCallError::new("internal", error.to_string()),
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

fn as_track_scores(scope: &TrackScope, clips: &[TrackClip]) -> Vec<TrackScore> {
    clips
        .iter()
        .map(|clip| TrackScore {
            id: clip.id.clone(),
            uid: None,
            score_id: scope.score_id.clone(),
            pattern_id: clip.pattern_id.clone(),
            start_time: clip.start_time,
            end_time: clip.end_time,
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
            args: clip.args.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        })
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
