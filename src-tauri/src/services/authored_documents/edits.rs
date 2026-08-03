use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::operations::OperationSpec;
use super::{
    check_track_projection_candidate, compile_import_track_document, file_snapshot_id, graph_files,
    is_valid_track_draft_id, load_score_dsl_context, load_unscoped_graph_document_for_connection,
    normalized_creation_request_id, operation_request_fingerprint, plan_track_snapshot_replacement,
    remap_track_snapshot_result, revision_for_clips, serialize_track,
    track_clips_semantically_equal, validate_token, validate_track_draft_envelope,
    AppliedAuthoredState, AppliedAuthoredTrackEdit, AuthoredDocument, AuthoredDocuments,
    AuthoredDocumentsError, BlendMode, CreateTrackScoreInput, DeleteTrackScoreInput, FileMap,
    ForkPatternInput, ForkPatternResult, Graph, GraphDocument, MainState, PatternSummary,
    ResolvedScope, Result, TrackClip, TrackDocument, TrackEditError, TrackEditPlan,
    TrackEditResult, TrackProjectionAuthority, TrackProjectionIdentity, TrackScope, TrackScore,
    UpdateTrackScoreInput, PATTERN_SUMMARY_COLUMNS, SCORE_PATH,
};

impl AuthoredDocuments {
    pub async fn apply_graph_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        pattern_id: &str,
        implementation_id: &str,
        operation_id: &str,
        graph: Graph,
        expected_revision: &str,
        subject: &str,
    ) -> Result<AppliedAuthoredState> {
        validate_token(operation_id, "graph edit operation id")?;
        let scope = ResolvedScope::pattern(principal, pattern_id, implementation_id)?;
        let files = graph_files(&graph)?;
        let candidate = self.decode_files(&scope, &files)?;
        let fingerprint = operation_request_fingerprint(
            "graph_edit",
            &[
                pattern_id,
                implementation_id,
                expected_revision,
                &file_snapshot_id(&files),
            ],
        );
        let _guard = self.document_guard(&scope.document_id).await;
        let main = self.load_current_locked(pool, &scope).await?;
        Ok(self
            .apply_candidate_locked(
                pool,
                &scope,
                &main.head,
                expected_revision,
                files,
                candidate,
                TrackProjectionAuthority::ExistingOnly,
                OperationSpec {
                    kind: "graph_edit",
                    id: operation_id,
                    fingerprint: &fingerprint,
                    result_json: None,
                },
                subject,
                None,
                None,
                None,
                None,
            )
            .await?
            .state)
    }

    pub async fn apply_score_source_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        operation_id: &str,
        source: &str,
        expected_revision: &str,
        subject: &str,
    ) -> Result<AppliedAuthoredState> {
        validate_token(operation_id, "score edit operation id")?;
        let fingerprint = operation_request_fingerprint(
            "score_dsl_import",
            &[
                &track_scope.score_id,
                &track_scope.track_id,
                &track_scope.venue_id,
                expected_revision,
                source,
            ],
        );
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.document_guard(&scope.document_id).await;
        let main = self.load_current_locked(pool, &scope).await?;
        let context = load_score_dsl_context(pool, scope.track_scope().expect("track scope"))
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        let imported = compile_import_track_document(source, &context, true)
            .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
        let current = require_track(&main)?;
        let mut result =
            track_edit_result(&current.clips, &imported.document.clips, BTreeMap::new());
        let result_json = serde_json::to_string(&result).map_err(|error| {
            AuthoredDocumentsError::Storage(format!("encode score import result: {error}"))
        })?;
        let applied = self
            .apply_candidate_locked(
                pool,
                &scope,
                &main.head,
                expected_revision,
                FileMap::from([(
                    SCORE_PATH.to_owned(),
                    imported.canonical_source.into_bytes(),
                )]),
                AuthoredDocument::Track(imported.document),
                TrackProjectionAuthority::HostAllocated(imported.host_allocated_ids),
                OperationSpec {
                    kind: "score_edit",
                    id: operation_id,
                    fingerprint: &fingerprint,
                    result_json: Some(&result_json),
                },
                subject,
                None,
                None,
                None,
                None,
            )
            .await?;
        if let Some(edit) = applied.track_edit {
            result.revision = edit.revision;
            result.clips = edit.clips;
        }
        Ok(applied.state)
    }

    pub(crate) async fn replay_track_edit_for_thread(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        expected_track_scope: &TrackScope,
        operation_id: &str,
        request_fingerprint: &str,
    ) -> Result<Option<AppliedAuthoredTrackEdit>> {
        validate_token(operation_id, "Python score edit operation id")?;
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        if scope.track_scope() != Some(expected_track_scope) {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread does not own the requested track score scope".into(),
            ));
        }
        let Some(outcome) = self
            .operation_outcome(pool, &scope, "score_edit", operation_id)
            .await?
        else {
            return Ok(None);
        };
        if outcome.request_fingerprint != request_fingerprint {
            return Err(AuthoredDocumentsError::Invalid(
                "operation id was already used with different input".into(),
            ));
        }
        let main = self.load_current_locked(pool, &scope).await?;
        let current = require_track(&main)?;
        let mut edit: TrackEditResult =
            serde_json::from_str(outcome.result_json.as_deref().ok_or_else(|| {
                AuthoredDocumentsError::Storage(
                    "score edit operation is missing its durable result".into(),
                )
            })?)
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!("decode score edit result: {error}"))
            })?;
        let result_revision = outcome.result_revision_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage("score edit has no result revision".into())
        })?;
        let changed = track_edit_changed(&edit);
        edit.revision = current.revision.clone();
        edit.clips = current.clips.clone();
        edit.applied_to_current_projection = result_revision == main.head;
        Ok(Some(AppliedAuthoredTrackEdit {
            authored: AppliedAuthoredState {
                document_id: scope.document_id.to_string(),
                revision_id: result_revision.to_string(),
                changed,
                document: main.document.projected(),
            },
            edit,
        }))
    }

    pub async fn apply_track_edit_for_thread(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        thread_id: &str,
        expected_track_scope: &TrackScope,
        operation_id: &str,
        request_fingerprint: &str,
        plan: TrackEditPlan,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        validate_token(operation_id, "Python score edit operation id")?;
        let (_thread, scope, _guard) = self.lock_active_thread(pool, principal, thread_id).await?;
        if scope.track_scope() != Some(expected_track_scope) {
            return Err(AuthoredDocumentsError::Scope(
                "agent thread does not own the requested track score scope".into(),
            ));
        }
        let main = self.load_current_locked(pool, &scope).await?;
        self.apply_track_plan_locked(
            pool,
            &scope,
            main,
            plan,
            operation_id,
            request_fingerprint,
            BTreeMap::new(),
            None,
            subject,
        )
        .await
    }

    pub async fn replace_track_scores_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        base: &[TrackScore],
        candidate: &[TrackScore],
        operation_id: &str,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        validate_token(operation_id, "score edit operation id")?;
        let fingerprint = score_edit_request_fingerprint(
            "replace",
            &track_scope,
            &serde_json::json!({ "base": base, "candidate": candidate }),
        )?;
        let replacement = plan_track_snapshot_replacement(&track_scope.score_id, base, candidate)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.document_guard(&scope.document_id).await;
        let main = self.load_current_locked(pool, &scope).await?;
        self.apply_track_plan_locked(
            pool,
            &scope,
            main,
            replacement.plan,
            operation_id,
            &fingerprint,
            replacement.client_ids_by_draft,
            None,
            subject,
        )
        .await
    }

    pub async fn create_track_score_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        payload: CreateTrackScoreInput,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        assert_track_payload_scope(&track_scope, &payload.score_id, &payload.track_id)?;
        let request_id = normalized_creation_request_id(&payload.request_id)?;
        let blend_mode = payload.blend_mode.unwrap_or(BlendMode::Replace);
        let args = payload
            .args
            .clone()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        let fingerprint =
            track_clip_creation_fingerprint(&track_scope, &payload, &blend_mode, &args)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.document_guard(&scope.document_id).await;
        let main = self.load_current_locked(pool, &scope).await?;
        if let Some(replayed) = self
            .replay_track_operation(pool, &scope, &main, &request_id, &fingerprint)
            .await?
        {
            return Ok(replayed);
        }
        let stable_id = deterministic_track_clip_id(&scope.principal_key, &request_id);
        let mut candidate = require_track(&main)?.clips.clone();
        candidate.push(TrackClip {
            id: stable_id.clone(),
            pattern_id: payload.pattern_id,
            start_time: payload.start_time,
            end_time: payload.end_time,
            z_index: payload.z_index,
            blend_mode,
            args,
        });
        let base_revision = main.document.revision().to_owned();
        self.apply_track_plan_locked(
            pool,
            &scope,
            main,
            TrackEditPlan {
                base_revision,
                candidate,
            },
            &request_id,
            &fingerprint,
            BTreeMap::new(),
            Some(stable_id),
            subject,
        )
        .await
    }

    pub async fn update_track_score_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        payload: UpdateTrackScoreInput,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        assert_track_payload_scope(&track_scope, &payload.score_id, &payload.track_id)?;
        validate_token(&payload.operation_id, "score edit operation id")?;
        let fingerprint = score_edit_request_fingerprint("update", &track_scope, &payload)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.document_guard(&scope.document_id).await;
        let main = self.load_current_locked(pool, &scope).await?;
        let mut candidate = require_track(&main)?.clips.clone();
        let clip = candidate
            .iter_mut()
            .find(|clip| clip.id == payload.id)
            .ok_or_else(|| {
                AuthoredDocumentsError::Scope(format!(
                    "clip {} does not belong to this score",
                    payload.id
                ))
            })?;
        if let Some(value) = payload.start_time {
            clip.start_time = value;
        }
        if let Some(value) = payload.end_time {
            clip.end_time = value;
        }
        if let Some(value) = payload.z_index {
            clip.z_index = value;
        }
        if let Some(value) = payload.blend_mode {
            clip.blend_mode = value;
        }
        if let Some(value) = payload.args {
            clip.args = value;
        }
        let base_revision = main.document.revision().to_owned();
        self.apply_track_plan_locked(
            pool,
            &scope,
            main,
            TrackEditPlan {
                base_revision,
                candidate,
            },
            &payload.operation_id,
            &fingerprint,
            BTreeMap::new(),
            None,
            subject,
        )
        .await
    }

    pub async fn delete_track_score_for_scope(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        track_scope: TrackScope,
        payload: DeleteTrackScoreInput,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        assert_track_payload_scope(&track_scope, &payload.score_id, &payload.track_id)?;
        validate_token(&payload.operation_id, "score edit operation id")?;
        let fingerprint = score_edit_request_fingerprint("delete", &track_scope, &payload)?;
        let scope = ResolvedScope::track(principal, track_scope)?;
        let _guard = self.document_guard(&scope.document_id).await;
        let main = self.load_current_locked(pool, &scope).await?;
        let mut candidate = require_track(&main)?.clips.clone();
        let before = candidate.len();
        candidate.retain(|clip| clip.id != payload.id);
        if candidate.len() == before {
            return Err(AuthoredDocumentsError::Scope(format!(
                "clip {} does not belong to this score",
                payload.id
            )));
        }
        let base_revision = main.document.revision().to_owned();
        self.apply_track_plan_locked(
            pool,
            &scope,
            main,
            TrackEditPlan {
                base_revision,
                candidate,
            },
            &payload.operation_id,
            &fingerprint,
            BTreeMap::new(),
            None,
            subject,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_track_plan_locked(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: MainState,
        plan: TrackEditPlan,
        operation_id: &str,
        fingerprint: &str,
        client_ids_by_draft: BTreeMap<String, String>,
        created_clip_id: Option<String>,
        subject: &str,
    ) -> Result<AppliedAuthoredTrackEdit> {
        if let Some(replayed) = self
            .replay_track_operation(pool, scope, &main, operation_id, fingerprint)
            .await?
        {
            return Ok(replayed);
        }
        let current = require_track(&main)?.clone();
        if plan.base_revision != current.revision {
            return Err(AuthoredDocumentsError::Track(TrackEditError::Conflict {
                expected_revision: plan.base_revision,
                current_revision: current.revision,
            }));
        }
        validate_track_draft_envelope(&plan.candidate)?;
        let current_ids: HashSet<&str> =
            current.clips.iter().map(|clip| clip.id.as_str()).collect();
        let mut host_allocated_ids = BTreeSet::new();
        let mut id_map = BTreeMap::new();
        let mut candidate = plan.candidate;
        for clip in &mut candidate {
            if current_ids.contains(clip.id.as_str()) {
                continue;
            }
            if is_valid_track_draft_id(&clip.id) {
                let draft_id = std::mem::take(&mut clip.id);
                let stable_id = Uuid::new_v4().to_string();
                host_allocated_ids.insert(stable_id.clone());
                id_map.insert(draft_id, stable_id.clone());
                clip.id = stable_id;
            }
        }
        if let Some(created) = &created_clip_id {
            host_allocated_ids.insert(created.clone());
        }
        let requested_lineage_ids: BTreeSet<String> = candidate
            .iter()
            .filter(|clip| {
                !current_ids.contains(clip.id.as_str()) && !host_allocated_ids.contains(&clip.id)
            })
            .map(|clip| clip.id.clone())
            .collect();
        let mut connection = pool.acquire().await.map_err(|error| {
            AuthoredDocumentsError::Storage(format!("open score lineage: {error}"))
        })?;
        let lineage_ids = self
            .track_lineage_ids(&mut connection, scope, &main.head, &requested_lineage_ids)
            .await?;
        drop(connection);
        sort_track_clips(&mut candidate);
        check_track_projection_candidate(
            pool,
            scope.track_scope().expect("track scope"),
            scope.owner_user_id.as_deref(),
            &candidate,
            TrackProjectionIdentity::Allowed {
                lineage_ids: &lineage_ids,
                host_allocated_ids: &host_allocated_ids,
            },
        )
        .await?;
        let document = TrackDocument {
            revision: revision_for_clips(&candidate),
            clips: candidate,
        };
        let pattern_names = super::load_score_pattern_names(pool)
            .await
            .map_err(AuthoredDocumentsError::Storage)?;
        let source = serialize_track(
            &document,
            &pattern_names,
            Some(super::utf8_file(&main.files, SCORE_PATH)?),
        )?;
        let mut result = track_edit_result(&current.clips, &document.clips, id_map);
        result.created_clip_id = created_clip_id;
        result = remap_track_snapshot_result(result, client_ids_by_draft);
        let result_json = serde_json::to_string(&result).map_err(|error| {
            AuthoredDocumentsError::Storage(format!("encode score edit result: {error}"))
        })?;
        let application = self
            .apply_candidate_locked(
                pool,
                scope,
                &main.head,
                &plan.base_revision,
                FileMap::from([(SCORE_PATH.to_owned(), source.into_bytes())]),
                AuthoredDocument::Track(document),
                TrackProjectionAuthority::Allowed {
                    lineage_ids,
                    host_allocated_ids,
                },
                OperationSpec {
                    kind: "score_edit",
                    id: operation_id,
                    fingerprint,
                    result_json: Some(&result_json),
                },
                subject,
                None,
                scope.thread_id.as_deref(),
                None,
                None,
            )
            .await?;
        if let Some(projected) = application.track_edit {
            result.revision = projected.revision;
            result.clips = projected.clips;
        }
        Ok(AppliedAuthoredTrackEdit {
            authored: application.state,
            edit: result,
        })
    }

    async fn replay_track_operation(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        main: &MainState,
        operation_id: &str,
        fingerprint: &str,
    ) -> Result<Option<AppliedAuthoredTrackEdit>> {
        let Some(outcome) = self
            .operation_outcome(pool, scope, "score_edit", operation_id)
            .await?
        else {
            return Ok(None);
        };
        if outcome.request_fingerprint != fingerprint {
            return Err(AuthoredDocumentsError::Invalid(
                "score edit operation id was already used with different input".into(),
            ));
        }
        if outcome.status != "committed" {
            return Err(AuthoredDocumentsError::Invalid(
                "score edit previously completed with structured conflicts".into(),
            ));
        }
        let result_revision = outcome.result_revision_id.ok_or_else(|| {
            AuthoredDocumentsError::Storage("score edit has no result revision".into())
        })?;
        let current = require_track(main)?;
        let mut edit: TrackEditResult =
            serde_json::from_str(outcome.result_json.as_deref().ok_or_else(|| {
                AuthoredDocumentsError::Storage("score edit is missing its result".into())
            })?)
            .map_err(|error| {
                AuthoredDocumentsError::Storage(format!("decode score edit result: {error}"))
            })?;
        let changed = track_edit_changed(&edit);
        edit.revision = current.revision.clone();
        edit.clips = current.clips.clone();
        edit.applied_to_current_projection = result_revision == main.head;
        Ok(Some(AppliedAuthoredTrackEdit {
            authored: AppliedAuthoredState {
                document_id: scope.document_id.to_string(),
                revision_id: result_revision.to_string(),
                changed,
                document: main.document.projected(),
            },
            edit,
        }))
    }

    pub async fn fork_pattern(
        &self,
        pool: &SqlitePool,
        principal: Option<&str>,
        input: ForkPatternInput,
    ) -> Result<ForkPatternResult> {
        validate_token(&input.request_id, "pattern fork request id")?;
        let source = load_pattern_fork_source(
            pool,
            &input.source_pattern_id,
            &input.source_implementation_id,
        )
        .await?;
        self.create_pattern_fork(pool, principal, input, source)
            .await
    }
}

pub(super) struct PatternForkSource {
    pub pattern: PatternSummary,
    pub graph: GraphDocument,
}

pub(super) async fn load_pattern_fork_source(
    pool: &SqlitePool,
    pattern_id: &str,
    implementation_id: &str,
) -> Result<PatternForkSource> {
    let mut transaction = pool.begin().await.map_err(|error| {
        AuthoredDocumentsError::Storage(format!("begin pattern fork snapshot: {error}"))
    })?;
    let pattern = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "SELECT {PATTERN_SUMMARY_COLUMNS}
         FROM patterns
         JOIN auth_visible_patterns visible ON visible.pattern_id = patterns.id
         WHERE patterns.id = ?"
    )))
    .bind(pattern_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load fork source: {error}")))?
    .ok_or_else(|| AuthoredDocumentsError::Scope("fork source pattern does not exist".into()))?;
    let graph = load_unscoped_graph_document_for_connection(
        &mut transaction,
        pattern_id,
        implementation_id,
    )
    .await?;
    transaction.commit().await.map_err(|error| {
        AuthoredDocumentsError::Storage(format!("finish pattern fork snapshot: {error}"))
    })?;
    Ok(PatternForkSource { pattern, graph })
}

fn require_track(main: &MainState) -> Result<&TrackDocument> {
    match &main.document {
        AuthoredDocument::Track(document) => Ok(document),
        AuthoredDocument::Graph(_) => Err(AuthoredDocumentsError::Storage(
            "score scope resolved to graph history".into(),
        )),
    }
}

fn assert_track_payload_scope(scope: &TrackScope, score_id: &str, track_id: &str) -> Result<()> {
    if scope.score_id == score_id && scope.track_id == track_id {
        Ok(())
    } else {
        Err(AuthoredDocumentsError::Scope(
            "track mutation payload does not match its trusted score scope".into(),
        ))
    }
}

fn track_clip_creation_fingerprint(
    scope: &TrackScope,
    payload: &CreateTrackScoreInput,
    blend_mode: &BlendMode,
    args: &serde_json::Value,
) -> Result<String> {
    let start_time = format!("{:016x}", payload.start_time.to_bits());
    let end_time = format!("{:016x}", payload.end_time.to_bits());
    let z_index = payload.z_index.to_string();
    let blend_mode = serde_json::to_string(blend_mode).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode track clip blend mode: {error}"))
    })?;
    let args = crate::canonical_json::to_string(args);
    Ok(operation_request_fingerprint(
        "create_track_clip",
        &[
            &scope.score_id,
            &scope.track_id,
            &scope.venue_id,
            &payload.pattern_id,
            &start_time,
            &end_time,
            &z_index,
            &blend_mode,
            &args,
        ],
    ))
}

fn score_edit_request_fingerprint(
    edit_kind: &str,
    scope: &TrackScope,
    payload: &impl Serialize,
) -> Result<String> {
    let mut value = serde_json::to_value(payload).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("encode score edit fingerprint: {error}"))
    })?;
    if let serde_json::Value::Object(fields) = &mut value {
        fields.remove("operationId");
    }
    let canonical = crate::canonical_json::to_string(&serde_json::json!({
        "kind": edit_kind,
        "scoreId": scope.score_id,
        "trackId": scope.track_id,
        "venueId": scope.venue_id,
        "payload": value,
    }));
    Ok(operation_request_fingerprint("score_edit", &[&canonical]))
}

fn deterministic_track_clip_id(principal_key: &str, request_id: &str) -> String {
    super::deterministic_creation_id(principal_key, "track_clip", request_id, "subject")
}

fn sort_track_clips(clips: &mut [TrackClip]) {
    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
}

fn track_edit_result(
    current: &[TrackClip],
    candidate: &[TrackClip],
    id_map: BTreeMap<String, String>,
) -> TrackEditResult {
    let current_by_id: HashMap<&str, &TrackClip> = current
        .iter()
        .map(|clip| (clip.id.as_str(), clip))
        .collect();
    let candidate_ids: HashSet<&str> = candidate.iter().map(|clip| clip.id.as_str()).collect();
    let added = candidate
        .iter()
        .filter(|clip| !current_by_id.contains_key(clip.id.as_str()))
        .count();
    let updated = candidate
        .iter()
        .filter(|clip| {
            current_by_id
                .get(clip.id.as_str())
                .is_some_and(|current| !track_clips_semantically_equal(current, clip))
        })
        .count();
    let removed = current
        .iter()
        .filter(|clip| !candidate_ids.contains(clip.id.as_str()))
        .count();
    TrackEditResult {
        revision: revision_for_clips(candidate),
        clips: candidate.to_vec(),
        id_map,
        created_clip_id: None,
        added,
        updated,
        removed,
        applied_to_current_projection: true,
    }
}

fn track_edit_changed(result: &TrackEditResult) -> bool {
    result.added != 0 || result.updated != 0 || result.removed != 0
}
