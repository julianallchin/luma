//! The canonical score document owns both graph definitions and clip instances.
//! Reads and projection writes share the authored-history transaction; NULL in
//! the score row preserves an existing, not-yet-migrated score.
use crate::services::track_edits::TrackScope;
use luma_patterns::{standard_library, Score};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

pub(crate) mod merge;

/// Authoring consumers keep an unmigrated score explicit. Graphs and legacy
/// rows are never combined into a fictitious partial document.
#[derive(Clone, Debug)]
pub enum ScoreDocument {
    Legacy(crate::services::track_edits::TrackDocument),
    Graph(GraphScoreDocument),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphScoreEdit {
    pub base_revision: String,
    pub candidate: Score,
}

pub(crate) struct GraphScoreOperation<'a> {
    pub thread_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub scope: &'a TrackScope,
    pub id: &'a str,
    pub fingerprint: &'a str,
}

pub(crate) async fn read_score_document(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    scope: &TrackScope,
) -> Result<ScoreDocument, String> {
    let metadata = crate::database::local::scores::get_score(access, &scope.score_id).await?;
    if let Some(document) = load(access.connection(), scope, metadata.uid.as_deref()).await? {
        return Ok(ScoreDocument::Graph(document));
    }
    let rows = crate::database::local::scores::get_clips_of_score(access, &scope.score_id).await?;
    Ok(ScoreDocument::Legacy(
        crate::services::track_edits::TrackDocument {
            revision: crate::services::track_edits::track_revision(&rows),
            clips: rows
                .iter()
                .map(crate::services::track_edits::TrackClip::from)
                .collect(),
        },
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphScoreDocument {
    pub revision: String,
    pub score: Score,
}
impl GraphScoreDocument {
    pub fn new(score: Score) -> Result<Self, String> {
        score
            .validate(&standard_library())
            .map_err(|error| error.to_string())?;
        let source = source(&score)?;
        let mut hash = Sha256::new();
        hash.update(b"luma.graph-score.v2\0");
        hash.update(source.as_bytes());
        Ok(Self {
            revision: format!("sha256:{:x}", hash.finalize()),
            score,
        })
    }

    pub fn from_source(source: &str) -> Result<Self, String> {
        if source.len() > 6 * 1024 * 1024 {
            return Err("score document exceeds 6 MiB".into());
        }
        let score = serde_json::from_str(source)
            .map_err(|error| format!("invalid score document: {error}"))?;
        Self::new(score)
    }

    pub fn source(&self) -> Result<String, String> {
        source(&self.score)
    }
}

/// Rebase a native draft onto edits made elsewhere in the same open score.
/// Uses the authored-history merge rules, including indivisible typed values.
pub fn merge_working_copy(base: &Score, current: &Score, draft: &Score) -> Result<Score, String> {
    let base = GraphScoreDocument::new(base.clone())?;
    let current = GraphScoreDocument::new(current.clone())?;
    let draft = GraphScoreDocument::new(draft.clone())?;
    merge::strict(&base,&current,&draft).map(|document|document.score).map_err(|_|"Another edit changed the same graph. Your draft is still open; undo the overlapping change to continue.".into())
}

fn source(score: &Score) -> Result<String, String> {
    let value = serde_json::to_value(score).map_err(|error| error.to_string())?;
    let source = format!("{}\n", crate::canonical_json::to_string(&value));
    if source.len() > 6 * 1024 * 1024 {
        return Err("score document exceeds 6 MiB".into());
    }
    Ok(source)
}

/// Exact owner, track and venue checks take place on the same connection as
/// the read. Callers acquire read/write admission before entering this layer.
pub(crate) async fn load(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner: Option<&str>,
) -> Result<Option<GraphScoreDocument>, String> {
    let found: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT track_id, venue_id, uid, graph_document_json FROM scores WHERE id = ?",
    )
    .bind(&scope.score_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| error.to_string())?;
    let Some((track, venue, uid, source)) = found else {
        return Err("score does not exist".into());
    };
    if track != scope.track_id || venue != scope.venue_id || uid.as_deref() != owner {
        return Err("score does not belong to the exact authored scope".into());
    }
    source
        .as_deref()
        .map(GraphScoreDocument::from_source)
        .transpose()
}

/// Called only by the authored projector, after admission and its head CAS.
/// The source and the removal of old clip projections are one transaction.
pub(crate) async fn project(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner: Option<&str>,
    candidate: &GraphScoreDocument,
) -> Result<(), String> {
    // Revalidate the public deserialized boundary rather than trusting a
    // caller-supplied revision or a prior preview's validation.
    let checked = GraphScoreDocument::new(candidate.score.clone())?;
    if checked.revision != candidate.revision {
        return Err("score revision does not match its content".into());
    }
    load(connection, scope, owner).await?;
    let updated = sqlx::query("UPDATE scores SET graph_document_json = ? WHERE id = ? AND track_id = ? AND venue_id = ? AND uid IS ?")
        .bind(checked.source()?).bind(&scope.score_id).bind(&scope.track_id).bind(&scope.venue_id).bind(owner)
        .execute(&mut *connection).await.map_err(|error| error.to_string())?.rows_affected();
    if updated != 1 {
        return Err("score disappeared during authored projection".into());
    }
    sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
        .bind(&scope.score_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Compile the canonical score directly into the existing compositor's plans.
/// Geometry, group selections and the beat grid come from one authorized read.
pub(crate) async fn prepare_scene(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
) -> Result<crate::eval::Scene, String> {
    score
        .validate(&standard_library())
        .map_err(|error| error.to_string())?;
    if score.clips.is_empty() {
        return Ok(crate::eval::Scene::default());
    }
    let grid =
        crate::services::tracks::get_track_beats_for_connection(access.connection(), track_id)
            .await?
            .ok_or("analyze the track before placing effects on its musical grid")?;
    let clock = grid.timeline().map_err(|error| error.to_string())?;
    let library = score
        .library(&standard_library())
        .map_err(|error| error.to_string())?;
    let mut compiled = Vec::new();
    let mut prepared_clips = Vec::new();
    let mut requests = Vec::new();
    let mut domains = std::collections::BTreeMap::new();
    for (id, clip) in &score.clips {
        let key = (
            serde_json::to_string(&clip.selection).map_err(|error| error.to_string())?,
            clip.seed,
        );
        let cells = if let Some(cells) = domains.get(&key) {
            Vec::<luma_patterns::Cell>::clone(cells)
        } else {
            let cells = super::composable_patterns::resolve_cells(
                access,
                fixtures_root,
                std::slice::from_ref(&clip.selection),
                clip.seed,
            )
            .await?;
            domains.insert(key, cells.clone());
            cells
        };
        if cells.is_empty() {
            continue;
        }
        let prepared = luma_patterns::PreparedGraph::new(
            &library,
            &clip.graph,
            &clip.inputs,
            luma_patterns::Frame {
                cells: &cells,
                features: None,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )
        .map_err(|e| format!("clip {id}: {e}"))?;
        for request in prepared.feature_requests() {
            if !requests.contains(request) {
                requests.push(request.clone());
            }
        }
        prepared_clips.push((id, clip, cells, prepared));
    }
    let features = if requests.is_empty() {
        None
    } else {
        Some(
            crate::eval::track_features::prepare(
                access,
                storage,
                track_id,
                clock.clone(),
                &requests,
            )
            .await?,
        )
    };
    for (id, clip, cells, prepared) in prepared_clips {
        let prepared = match &features {
            Some(features) => prepared
                .with_features(features.clone())
                .map_err(|e| format!("clip {id}: {e}"))?,
            None => prepared,
        };
        let output = library.definitions[&clip.graph]
            .lighting_output()
            .ok_or("clip graph must produce fixture output")?;
        let plan =
            crate::eval::lighting::compile_clip(clip, clock.clone(), cells, prepared, output)
                .map_err(|error| format!("clip {id}: {error}"))?;
        compiled.push(crate::eval::CompiledAnnotation {
            span: plan.ctx.span,
            plan: std::sync::Arc::new(plan),
            z_index: clip.z_index,
            blend_mode: clip.blend_mode,
        });
    }
    Ok(crate::eval::Scene::new(compiled))
}

pub(crate) async fn preview_clip(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    fixtures_root: &std::path::Path,
    storage: &crate::storage::StorageRoot,
    track_id: &str,
    score: &Score,
    clip_id: &str,
) -> Result<crate::models::patterns::AnnotationPreview, String> {
    let clip = score
        .clips
        .get(clip_id)
        .ok_or_else(|| format!("unknown clip {clip_id}"))?;
    let width = (clip.duration * 16.0).ceil().clamp(8.0, 512.0) as usize;
    let mut single = score.clone();
    single.clips.retain(|id, _| id == clip_id);
    let scene = prepare_scene(access, fixtures_root, storage, track_id, &single).await?;
    let mut frames = Vec::new();
    let mut span = (0.0, 0.0);
    if let Some(annotation) = scene.annotations.first() {
        if (annotation.plan.n as usize).saturating_mul(width) > 1_000_000 {
            return Err("clip preview exceeds one million head samples".into());
        }
        span = annotation.span;
        let times: Vec<_> = (0..width)
            .map(|index| span.0 + (span.1 - span.0) * index as f32 / width as f32)
            .collect();
        frames = scene.render(
            &times,
            crate::eval::Scope::Single(0),
            &mut crate::eval::Arena::default(),
        );
    }
    Ok(crate::annotation_preview::render_preview(
        clip_id.to_owned(),
        &frames,
        None,
        span.0,
        span.1,
    ))
}
