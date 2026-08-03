use std::collections::{BTreeSet, HashSet};

use sqlx::{FromRow, SqliteConnection, SqlitePool};

use crate::services::score_dsl::decode_canonical_track_document;

use super::{
    canonicalize_graph, clips_to_canonical_document, graph_files, graph_from_files, graph_revision,
    load_score_pattern_names, merge_document_trivia, merge_document_trivia_later_wins,
    merge_graphs, merge_track_documents, parse_score_document, pattern_names_from_document,
    require_exact_paths, revision_for_clips, serialize_canonical, serialize_track, utf8_file,
    AuthoredDocument, AuthoredDocuments, AuthoredDocumentsError, AuthoredMergeConflict,
    AuthoredMergeConflictKind, AuthoredMergePathSegment, AuthoredMergeValue, AuthoredSnapshot,
    DocumentScope, FileMap, GraphDocument, GraphDocumentError, GraphScope, GraphValidationIssue,
    ResolvedScope, Result, RevisionId, TrackDocument, TrackScope, GRAPH_PATH, LAYOUT_PATH,
    SCORE_PATH,
};

impl AuthoredDocuments {
    pub(super) async fn snapshot_from_connection(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        prior_files: Option<&FileMap>,
    ) -> Result<AuthoredSnapshot> {
        let document = match &scope.document {
            DocumentScope::Track(track_scope) => AuthoredDocument::Track(
                load_track_document_for_connection(
                    connection,
                    track_scope,
                    scope.owner_user_id.as_deref(),
                )
                .await?,
            ),
            DocumentScope::Pattern(graph_scope) => AuthoredDocument::Graph(
                load_graph_document_for_connection(connection, graph_scope).await?,
            ),
        };
        let files = self
            .files_for_document_on_connection(connection, scope, &document, prior_files)
            .await?;
        Ok(AuthoredSnapshot { files, document })
    }

    pub(super) async fn snapshot_from_revision(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        revision_id: &RevisionId,
    ) -> Result<AuthoredSnapshot> {
        let (_, files) = self
            .store
            .read_revision(connection, &scope.document_id, revision_id)
            .await?;
        let document = self.decode_files(scope, &files)?;
        Ok(AuthoredSnapshot { files, document })
    }

    pub(super) fn decode_files(
        &self,
        scope: &ResolvedScope,
        files: &FileMap,
    ) -> Result<AuthoredDocument> {
        match &scope.document {
            DocumentScope::Track(_) => {
                require_exact_paths(files, &[SCORE_PATH])?;
                let source = utf8_file(files, SCORE_PATH)?;
                let (document, _) = decode_canonical_track_document(source)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                Ok(AuthoredDocument::Track(document))
            }
            DocumentScope::Pattern(graph_scope) => {
                require_exact_paths(files, &[GRAPH_PATH, LAYOUT_PATH])?;
                let graph = graph_from_files(
                    utf8_file(files, GRAPH_PATH)?,
                    utf8_file(files, LAYOUT_PATH)?,
                )?;
                Ok(AuthoredDocument::Graph(GraphDocument {
                    implementation_id: graph_scope.implementation_id.clone(),
                    revision: graph_revision(&graph)?,
                    graph,
                }))
            }
        }
    }

    pub(super) async fn files_for_document(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        document: &AuthoredDocument,
        prior_files: Option<&FileMap>,
    ) -> Result<FileMap> {
        match (&scope.document, document) {
            (DocumentScope::Track(_), AuthoredDocument::Track(track)) => {
                if let Some(reused) = reusable_track_files(track, prior_files) {
                    return Ok(reused);
                }
                let pattern_names = load_score_pattern_names(pool)
                    .await
                    .map_err(AuthoredDocumentsError::Storage)?;
                serialize_track_files(track, &pattern_names, prior_files)
            }
            (DocumentScope::Pattern(_), AuthoredDocument::Graph(graph)) => {
                graph_files(&graph.graph)
            }
            _ => Err(AuthoredDocumentsError::Storage(
                "cannot serialize mixed authored document kinds".into(),
            )),
        }
    }

    pub(super) async fn files_for_document_on_connection(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        document: &AuthoredDocument,
        prior_files: Option<&FileMap>,
    ) -> Result<FileMap> {
        match (&scope.document, document) {
            (DocumentScope::Track(_), AuthoredDocument::Track(track)) => {
                if let Some(reused) = reusable_track_files(track, prior_files) {
                    return Ok(reused);
                }
                let rows: Vec<(String, String)> =
                    sqlx::query_as("SELECT id, name FROM patterns ORDER BY id")
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(|error| {
                            AuthoredDocumentsError::Storage(format!(
                                "load score pattern labels: {error}"
                            ))
                        })?;
                let pattern_names = rows.into_iter().collect();
                serialize_track_files(track, &pattern_names, prior_files)
            }
            (DocumentScope::Pattern(_), AuthoredDocument::Graph(graph)) => {
                graph_files(&graph.graph)
            }
            _ => Err(AuthoredDocumentsError::Storage(
                "cannot serialize mixed authored document kinds".into(),
            )),
        }
    }

    /// Strict semantic merge used by agent turns and subagent workspaces.
    /// A conflict returns typed data and never publishes a partial result.
    pub(super) async fn merge_snapshots(
        &self,
        pool: &SqlitePool,
        scope: &ResolvedScope,
        base: &AuthoredSnapshot,
        ours: &AuthoredSnapshot,
        theirs: &AuthoredSnapshot,
    ) -> Result<std::result::Result<(AuthoredDocument, FileMap), Vec<AuthoredMergeConflict>>> {
        match (&base.document, &ours.document, &theirs.document) {
            (
                AuthoredDocument::Track(base_track),
                AuthoredDocument::Track(ours_track),
                AuthoredDocument::Track(theirs_track),
            ) => {
                let semantic = match merge_track_documents(base_track, ours_track, theirs_track)
                    .into_result()
                {
                    Ok(merged) => merged,
                    Err(conflicts) => {
                        return Ok(Err(conflicts.into_iter().map(Into::into).collect()));
                    }
                };
                let base_ast = parse_score_document(utf8_file(&base.files, SCORE_PATH)?)?;
                let ours_ast = parse_score_document(utf8_file(&ours.files, SCORE_PATH)?)?;
                let theirs_ast = parse_score_document(utf8_file(&theirs.files, SCORE_PATH)?)?;
                let mut pattern_names = pattern_names_from_document(&base_ast);
                pattern_names.extend(pattern_names_from_document(&theirs_ast));
                pattern_names.extend(pattern_names_from_document(&ours_ast));
                let semantic_ast = clips_to_canonical_document(&semantic.clips, &pattern_names)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let merged_ast =
                    match merge_document_trivia(&base_ast, &ours_ast, &theirs_ast, semantic_ast)
                        .into_result()
                    {
                        Ok(document) => document,
                        Err(conflicts) => {
                            return Ok(Err(conflicts.into_iter().map(Into::into).collect()));
                        }
                    };
                let source = serialize_canonical(&merged_ast)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                let (document, _) = decode_canonical_track_document(&source)
                    .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
                Ok(Ok((
                    AuthoredDocument::Track(document),
                    FileMap::from([(SCORE_PATH.to_owned(), source.into_bytes())]),
                )))
            }
            (
                AuthoredDocument::Graph(base_graph),
                AuthoredDocument::Graph(ours_graph),
                AuthoredDocument::Graph(theirs_graph),
            ) => match merge_graphs(&base_graph.graph, &ours_graph.graph, &theirs_graph.graph)
                .into_result()
            {
                Ok(graph) => {
                    let graph = match canonicalize_graph(&graph) {
                        Ok(graph) => graph,
                        Err(GraphDocumentError::Invalid { issues }) => {
                            return Ok(Err(graph_validation_conflicts(issues)));
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let implementation_id = match &scope.document {
                        DocumentScope::Pattern(graph_scope) => {
                            graph_scope.implementation_id.clone()
                        }
                        DocumentScope::Track(_) => {
                            return Err(AuthoredDocumentsError::Storage(
                                "graph merge requested for a score document".into(),
                            ));
                        }
                    };
                    let document = AuthoredDocument::Graph(GraphDocument {
                        implementation_id,
                        revision: graph_revision(&graph)?,
                        graph,
                    });
                    let files = self
                        .files_for_document(pool, scope, &document, None)
                        .await?;
                    Ok(Ok((document, files)))
                }
                Err(conflicts) => Ok(Err(conflicts.into_iter().map(Into::into).collect())),
            },
            _ => Err(AuthoredDocumentsError::Storage(
                "authored history contains mixed document kinds".into(),
            )),
        }
    }

    /// Compose score comments for server-ordered convergence using the same
    /// stable identities as strict agent merges. Independent comments survive;
    /// an overlapping comment field takes the later proposal. Invalid trivia
    /// addressing is returned so the integration layer can choose the complete
    /// proposal file as its terminal fallback.
    pub(super) fn merge_track_files_later_wins(
        &self,
        base: &AuthoredSnapshot,
        current: &AuthoredSnapshot,
        proposal: &AuthoredSnapshot,
        semantic: &TrackDocument,
    ) -> Result<FileMap> {
        let base_ast = parse_score_document(utf8_file(&base.files, SCORE_PATH)?)?;
        let current_ast = parse_score_document(utf8_file(&current.files, SCORE_PATH)?)?;
        let proposal_ast = parse_score_document(utf8_file(&proposal.files, SCORE_PATH)?)?;
        let mut pattern_names = pattern_names_from_document(&base_ast);
        pattern_names.extend(pattern_names_from_document(&current_ast));
        pattern_names.extend(pattern_names_from_document(&proposal_ast));
        let semantic_ast = clips_to_canonical_document(&semantic.clips, &pattern_names)
            .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
        let merged_ast =
            merge_document_trivia_later_wins(&base_ast, &current_ast, &proposal_ast, semantic_ast)
                .map_err(|conflicts| {
                    AuthoredDocumentsError::Invalid(format!(
                        "cannot converge canonical score comments: {} invalid trivia location(s)",
                        conflicts.len()
                    ))
                })?;
        let source = serialize_canonical(&merged_ast)
            .map_err(|error| AuthoredDocumentsError::Invalid(error.to_string()))?;
        Ok(FileMap::from([(
            SCORE_PATH.to_owned(),
            source.into_bytes(),
        )]))
    }

    pub(super) async fn track_lineage_ids(
        &self,
        connection: &mut SqliteConnection,
        scope: &ResolvedScope,
        start: &RevisionId,
        needed: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        if needed.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut pending = vec![start.clone()];
        let mut visited = HashSet::new();
        let mut ids = BTreeSet::new();
        while let Some(revision_id) = pending.pop() {
            if !visited.insert(revision_id.clone()) {
                continue;
            }
            let (revision, files) = self
                .store
                .read_revision(connection, &scope.document_id, &revision_id)
                .await?;
            match self.decode_files(scope, &files)? {
                AuthoredDocument::Track(document) => {
                    for clip in document.clips {
                        if needed.contains(&clip.id) {
                            ids.insert(clip.id);
                        }
                    }
                }
                AuthoredDocument::Graph(_) => {
                    return Err(AuthoredDocumentsError::Storage(
                        "graph revision found in score lineage".into(),
                    ));
                }
            }
            if ids.len() == needed.len() {
                break;
            }
            pending.extend(revision.parents);
        }
        Ok(ids)
    }
}

fn reusable_track_files(track: &TrackDocument, prior_files: Option<&FileMap>) -> Option<FileMap> {
    let prior = prior_files?;
    let source = prior
        .get(SCORE_PATH)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())?;
    let (parsed, _) = decode_canonical_track_document(source).ok()?;
    (parsed.revision == track.revision).then(|| prior.clone())
}

fn serialize_track_files(
    track: &TrackDocument,
    pattern_names: &super::PatternNames,
    prior_files: Option<&FileMap>,
) -> Result<FileMap> {
    let prior_source = prior_files
        .and_then(|files| files.get(SCORE_PATH))
        .and_then(|bytes| std::str::from_utf8(bytes).ok());
    let source = serialize_track(track, pattern_names, prior_source)?;
    Ok(FileMap::from([(
        SCORE_PATH.to_owned(),
        source.into_bytes(),
    )]))
}

fn graph_validation_conflicts(issues: Vec<GraphValidationIssue>) -> Vec<AuthoredMergeConflict> {
    issues
        .into_iter()
        .map(|issue| AuthoredMergeConflict {
            path: vec![AuthoredMergePathSegment::Field(issue.path)],
            kind: AuthoredMergeConflictKind::InvalidInput,
            base: AuthoredMergeValue::Missing,
            ours: AuthoredMergeValue::Missing,
            theirs: AuthoredMergeValue::Missing,
            detail: Some(issue.message),
        })
        .collect()
}

pub(super) async fn load_track_document_for_connection(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner: Option<&str>,
) -> Result<TrackDocument> {
    #[derive(FromRow)]
    struct Owner {
        track_id: String,
        venue_id: Option<String>,
        uid: Option<String>,
    }
    let found =
        sqlx::query_as::<_, Owner>("SELECT track_id, venue_id, uid FROM scores WHERE id = ?")
            .bind(&scope.score_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| AuthoredDocumentsError::Storage(format!("load score scope: {error}")))?
            .ok_or_else(|| AuthoredDocumentsError::Scope("score does not exist".into()))?;
    if found.track_id != scope.track_id
        || found.venue_id.as_deref() != Some(scope.venue_id.as_str())
        || found.uid.as_deref() != owner
    {
        return Err(AuthoredDocumentsError::Scope(
            "score does not belong to the exact current authored scope".into(),
        ));
    }
    let rows = sqlx::query_as::<_, crate::models::scores::TrackScore>(
        "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index,
                blend_mode, args_json, created_at, updated_at
         FROM track_scores WHERE score_id = ? ORDER BY start_time, z_index, id",
    )
    .bind(&scope.score_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load score clips: {error}")))?;
    let clips = rows.iter().map(Into::into).collect::<Vec<_>>();
    Ok(TrackDocument {
        revision: revision_for_clips(&clips),
        clips,
    })
}

pub(super) async fn load_graph_document_for_connection(
    connection: &mut SqliteConnection,
    scope: &GraphScope,
) -> Result<GraphDocument> {
    let owns = match scope.owner_user_id.as_deref() {
        Some(owner) => {
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ? AND uid = ?")
                .bind(&scope.pattern_id)
                .bind(owner)
                .fetch_optional(&mut *connection)
                .await
        }
        None => {
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM patterns WHERE id = ? AND uid IS NULL")
                .bind(&scope.pattern_id)
                .fetch_optional(&mut *connection)
                .await
        }
    }
    .map_err(|error| AuthoredDocumentsError::Storage(format!("authorize pattern: {error}")))?
    .is_some();
    if !owns {
        return Err(AuthoredDocumentsError::Scope(
            "pattern does not belong to the current principal".into(),
        ));
    }
    let implementation: (Option<String>, String) = sqlx::query_as(
        "SELECT uid, graph_json FROM implementations WHERE id = ? AND pattern_id = ?",
    )
    .bind(&scope.implementation_id)
    .bind(&scope.pattern_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AuthoredDocumentsError::Storage(format!("load pattern graph: {error}")))?
    .ok_or_else(|| {
        AuthoredDocumentsError::Scope(format!(
            "implementation {} does not belong to pattern {}",
            scope.implementation_id, scope.pattern_id
        ))
    })?;
    if implementation.0.as_deref() != scope.owner_user_id.as_deref() {
        return Err(AuthoredDocumentsError::Scope(
            "implementation does not belong to the current principal".into(),
        ));
    }
    let graph = serde_json::from_str(&implementation.1).map_err(|error| {
        AuthoredDocumentsError::Storage(format!("stored pattern graph is corrupt: {error}"))
    })?;
    Ok(GraphDocument {
        implementation_id: scope.implementation_id.clone(),
        revision: graph_revision(&graph)?,
        graph,
    })
}
