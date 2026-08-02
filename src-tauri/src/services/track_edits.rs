//! Atomic, revision-checked edits of a track's authored lighting timeline.
//!
//! Python drafts send a complete semantic candidate, never persistence-owned
//! fields. The service binds that candidate to a host-resolved score scope,
//! compares an opaque content revision under `BEGIN IMMEDIATE`, validates the
//! result, and writes only the rows whose meaning changed. New row identities,
//! timestamps and sync ownership always come from Rust/SQLite.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::models::node_graph::BlendMode;
use crate::models::scores::TrackScore;

const REVISION_DOMAIN: &[u8] = b"luma.track.v1\0";
const DRAFT_ID_PREFIX: &str = "new:";
const MAX_TRACK_CLIPS: usize = 2048;
const MAX_CANDIDATE_JSON_BYTES: usize = 6 * 1024 * 1024;

/// The immutable authored-track identity captured from a durable thread.
/// Read and render capabilities need this exact scope but do not require
/// ownership; mutation adds the authenticated owner in [`TrackEditScope`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackScope {
    pub score_id: String,
    pub track_id: String,
    pub venue_id: String,
}

/// The score identity captured by the host from the durable agent thread.
///
/// This is deliberately separate from [`TrackEditPlan`]: Python may describe
/// a candidate, but it may never choose which score that candidate mutates.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEditScope {
    pub score_id: String,
    pub track_id: String,
    pub venue_id: String,
    /// Authenticated user captured by the host from StateDb.
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

/// One authored pattern placement, stripped of database/sync bookkeeping.
///
/// Persisted clips use their UUID. A draft-only clip uses a reserved `new:*`
/// id until apply; the result maps that id to the UUID allocated by Rust.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackClip {
    pub id: String,
    pub pattern_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub z_index: i64,
    pub blend_mode: BlendMode,
    pub args: Value,
}

impl From<&TrackScore> for TrackClip {
    fn from(score: &TrackScore) -> Self {
        Self {
            id: score.id.clone(),
            pattern_id: score.pattern_id.clone(),
            start_time: score.start_time,
            end_time: score.end_time,
            z_index: score.z_index,
            blend_mode: score.blend_mode,
            args: score.args.clone(),
        }
    }
}

/// The current semantic track document and the CAS token for editing it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDocument {
    pub revision: String,
    pub clips: Vec<TrackClip>,
}

/// A complete candidate based on one previously observed document revision.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEditPlan {
    pub base_revision: String,
    pub candidate: Vec<TrackClip>,
}

/// An authoritative, non-mutating check result. Draft ids are intentionally
/// preserved so the same candidate can be rendered before it is applied.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEditCheck {
    pub base_revision: String,
    pub candidate: Vec<TrackClip>,
}

/// The authoritative document after a successful apply.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEditResult {
    pub revision: String,
    pub clips: Vec<TrackClip>,
    pub id_map: BTreeMap<String, String>,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

/// Structured failures let the Python bridge distinguish a stale draft from a
/// malformed candidate without scraping prose.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrackEditError {
    Conflict {
        #[serde(rename = "expectedRevision")]
        expected_revision: String,
        #[serde(rename = "currentRevision")]
        current_revision: String,
    },
    Invalid {
        message: String,
    },
    Scope {
        message: String,
    },
    Storage {
        message: String,
    },
}

impl TrackEditError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }

    fn scope(message: impl Into<String>) -> Self {
        Self::Scope {
            message: message.into(),
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::Storage {
            message: message.into(),
        }
    }
}

impl fmt::Display for TrackEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                expected_revision,
                current_revision,
            } => write!(
                f,
                "track changed while this edit was open (expected {expected_revision}, current {current_revision})"
            ),
            Self::Invalid { message } | Self::Scope { message } | Self::Storage { message } => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for TrackEditError {}

#[derive(Debug, FromRow)]
struct ScoreOwner {
    track_id: String,
    venue_id: Option<String>,
    uid: Option<String>,
}

/// Stable semantic revision of a persisted track document.
///
/// Row order, JSON object-key order, timestamps, UIDs and sync columns do not
/// affect it. Floating-point boundaries are hashed by their exact IEEE bits.
pub fn track_revision(scores: &[TrackScore]) -> String {
    let clips: Vec<TrackClip> = scores.iter().map(TrackClip::from).collect();
    revision_for_clips(&clips)
}

/// Load a coherent document for a host-resolved scope.
pub async fn load_track_document(
    pool: &SqlitePool,
    scope: &TrackEditScope,
) -> Result<TrackDocument, TrackEditError> {
    let track_scope = TrackScope::from(scope);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to begin track read: {e}")))?;
    load_and_validate_scope(&mut tx, &track_scope, Some(&scope.user_id)).await?;
    let scores = load_scores(&mut tx, &scope.score_id).await?;
    let document = TrackDocument {
        revision: track_revision(&scores),
        clips: sorted_clips(scores.iter().map(TrackClip::from).collect()),
    };
    tx.commit()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to finish track read: {e}")))?;
    Ok(document)
}

/// Validate a candidate against the current authoritative document without
/// allocating UUIDs or writing rows. Apply repeats the same checks inside its
/// exclusive transaction; this method is for `check()` and candidate renders,
/// not a substitute for apply's CAS.
pub async fn check_track_edit(
    pool: &SqlitePool,
    scope: &TrackEditScope,
    plan: TrackEditPlan,
) -> Result<TrackEditCheck, TrackEditError> {
    check_track_candidate_as(pool, &TrackScope::from(scope), Some(&scope.user_id), plan).await
}

/// Validate a candidate for an exact track/venue/score scope without requiring
/// ownership. This powers read-only compositor previews; it never writes.
pub async fn check_track_candidate(
    pool: &SqlitePool,
    scope: &TrackScope,
    plan: TrackEditPlan,
) -> Result<TrackEditCheck, TrackEditError> {
    check_track_candidate_as(pool, scope, None, plan).await
}

async fn check_track_candidate_as(
    pool: &SqlitePool,
    scope: &TrackScope,
    required_user_id: Option<&str>,
    plan: TrackEditPlan,
) -> Result<TrackEditCheck, TrackEditError> {
    validate_candidate_envelope(&plan.candidate)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to begin track check: {e}")))?;
    load_and_validate_scope(&mut tx, scope, required_user_id).await?;
    let current = load_scores(&mut tx, &scope.score_id).await?;
    assert_current_revision(&plan.base_revision, &current)?;
    let min_duration = minimum_duration(&mut tx, &scope.track_id).await?;
    let track_duration = track_duration(&mut tx, &scope.track_id).await?;
    validate_candidate_ids(&mut tx, &scope.score_id, &current, &plan.candidate).await?;
    validate_candidate(
        &mut tx,
        &current,
        &plan.candidate,
        min_duration,
        track_duration,
    )
    .await?;

    let checked = TrackEditCheck {
        base_revision: plan.base_revision,
        candidate: sorted_clips(plan.candidate),
    };
    tx.commit()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to finish track check: {e}")))?;
    Ok(checked)
}

/// Atomically apply a complete semantic candidate with compare-and-swap.
pub async fn apply_track_edit(
    pool: &SqlitePool,
    scope: &TrackEditScope,
    plan: TrackEditPlan,
) -> Result<TrackEditResult, TrackEditError> {
    // Reject pathological input before taking SQLite's write reservation.
    validate_candidate_envelope(&plan.candidate)?;
    // The write reservation is acquired before the current revision is read.
    // Once the comparison succeeds no other writer can invalidate it before
    // this transaction commits.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to begin track edit: {e}")))?;

    let track_scope = TrackScope::from(scope);
    let owner = load_and_validate_scope(&mut tx, &track_scope, Some(&scope.user_id)).await?;
    let current = load_scores(&mut tx, &scope.score_id).await?;
    assert_current_revision(&plan.base_revision, &current)?;

    let min_duration = minimum_duration(&mut tx, &scope.track_id).await?;
    let track_duration = track_duration(&mut tx, &scope.track_id).await?;
    validate_candidate_ids(&mut tx, &scope.score_id, &current, &plan.candidate).await?;
    validate_candidate(
        &mut tx,
        &current,
        &plan.candidate,
        min_duration,
        track_duration,
    )
    .await?;
    let (candidate, id_map) = materialize_candidate(&current, plan.candidate);

    let current_by_id: HashMap<&str, &TrackScore> = current
        .iter()
        .map(|score| (score.id.as_str(), score))
        .collect();
    let candidate_by_id: HashMap<&str, &TrackClip> = candidate
        .iter()
        .map(|clip| (clip.id.as_str(), clip))
        .collect();

    let removed_ids: Vec<&str> = current
        .iter()
        .filter(|score| !candidate_by_id.contains_key(score.id.as_str()))
        .map(|score| score.id.as_str())
        .collect();
    let additions: Vec<&TrackClip> = candidate
        .iter()
        .filter(|clip| !current_by_id.contains_key(clip.id.as_str()))
        .collect();
    let updates: Vec<&TrackClip> = candidate
        .iter()
        .filter(|clip| {
            current_by_id
                .get(clip.id.as_str())
                .is_some_and(|existing| !same_semantics(existing, clip))
        })
        .collect();

    for id in &removed_ids {
        let deleted = sqlx::query("DELETE FROM track_scores WHERE id = ? AND score_id = ?")
            .bind(id)
            .bind(&scope.score_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| TrackEditError::storage(format!("failed to delete clip {id}: {e}")))?
            .rows_affected();
        if deleted != 1 {
            return Err(TrackEditError::storage(format!(
                "clip {id} disappeared during an exclusive track edit"
            )));
        }
    }

    for clip in &additions {
        sqlx::query(
            "INSERT INTO track_scores
             (id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&clip.id)
        .bind(&owner.uid)
        .bind(&scope.score_id)
        .bind(&clip.pattern_id)
        .bind(clip.start_time)
        .bind(clip.end_time)
        .bind(clip.z_index)
        .bind(blend_mode_name(clip.blend_mode))
        .bind(clip.args.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to add clip {}: {e}", clip.id)))?;
    }

    for clip in &updates {
        let changed = sqlx::query(
            "UPDATE track_scores
             SET pattern_id = ?, start_time = ?, end_time = ?, z_index = ?,
                 blend_mode = ?, args_json = ?
             WHERE id = ? AND score_id = ?",
        )
        .bind(&clip.pattern_id)
        .bind(clip.start_time)
        .bind(clip.end_time)
        .bind(clip.z_index)
        .bind(blend_mode_name(clip.blend_mode))
        .bind(clip.args.to_string())
        .bind(&clip.id)
        .bind(&scope.score_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to update clip {}: {e}", clip.id)))?
        .rows_affected();
        if changed != 1 {
            return Err(TrackEditError::storage(format!(
                "clip {} disappeared during an exclusive track edit",
                clip.id
            )));
        }
    }

    let stored = load_scores(&mut tx, &scope.score_id).await?;
    let result = TrackEditResult {
        revision: track_revision(&stored),
        clips: sorted_clips(stored.iter().map(TrackClip::from).collect()),
        id_map,
        added: additions.len(),
        updated: updates.len(),
        removed: removed_ids.len(),
    };
    tx.commit()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to commit track edit: {e}")))?;
    Ok(result)
}

/// Apply a complete document produced by a trusted UI workflow (DSL import,
/// generation, undo/redo) against the exact semantic snapshot it observed.
///
/// The UI may carry persistence-shaped `TrackScore` rows because that is its
/// display model, but none of their ownership, timestamp, or new-row identity
/// fields are trusted. Existing IDs are retained only when they appeared in the
/// base snapshot; every genuinely new row is rewritten to a draft ID and
/// materialized by [`apply_track_edit`]. This keeps every full-document writer
/// on the same scope checks, revision CAS, validation, and diff transaction as
/// Python.
pub async fn replace_track_scores_from_snapshot(
    pool: &SqlitePool,
    scope: &TrackEditScope,
    base: Vec<TrackScore>,
    candidate: Vec<TrackScore>,
) -> Result<TrackEditResult, TrackEditError> {
    let base_ids = validate_snapshot_rows(&scope.score_id, &base, "base snapshot")?;
    validate_snapshot_rows(&scope.score_id, &candidate, "candidate")?;

    let base_revision = track_revision(&base);
    let mut client_ids_by_draft = BTreeMap::new();
    let candidate = candidate
        .iter()
        .enumerate()
        .map(|(index, score)| {
            let mut clip = TrackClip::from(score);
            if !base_ids.contains(score.id.as_str()) {
                // Client-created UUIDs and restored persistence fields are not
                // row authority. The canonical transaction allocates identity.
                let draft_id = format!("new:ui-{index}");
                client_ids_by_draft.insert(draft_id.clone(), score.id.clone());
                clip.id = draft_id;
            }
            clip
        })
        .collect();

    let mut result = apply_track_edit(
        pool,
        scope,
        TrackEditPlan {
            base_revision,
            candidate,
        },
    )
    .await?;
    result.id_map = result
        .id_map
        .into_iter()
        .map(|(draft_id, stored_id)| {
            (
                client_ids_by_draft.remove(&draft_id).unwrap_or(draft_id),
                stored_id,
            )
        })
        .collect();
    Ok(result)
}

fn validate_snapshot_rows<'a>(
    score_id: &str,
    rows: &'a [TrackScore],
    label: &str,
) -> Result<HashSet<&'a str>, TrackEditError> {
    let mut ids = HashSet::with_capacity(rows.len());
    for row in rows {
        if row.score_id != score_id {
            return Err(TrackEditError::scope(format!(
                "{} clip {} belongs to score {}, not {}",
                label, row.id, row.score_id, score_id
            )));
        }
        if row.id.is_empty() {
            return Err(TrackEditError::invalid(format!(
                "{label} contains an empty clip id"
            )));
        }
        if !ids.insert(row.id.as_str()) {
            return Err(TrackEditError::invalid(format!(
                "{label} contains clip {} more than once",
                row.id
            )));
        }
    }
    Ok(ids)
}

fn validate_candidate_envelope(candidate: &[TrackClip]) -> Result<(), TrackEditError> {
    if candidate.len() > MAX_TRACK_CLIPS {
        return Err(TrackEditError::invalid(format!(
            "a track may contain at most {MAX_TRACK_CLIPS} clips per candidate"
        )));
    }
    let encoded_bytes = serde_json::to_vec(candidate)
        .map_err(|error| TrackEditError::invalid(format!("candidate is not valid JSON: {error}")))?
        .len();
    if encoded_bytes > MAX_CANDIDATE_JSON_BYTES {
        return Err(TrackEditError::invalid(format!(
            "candidate is too large ({encoded_bytes} bytes; maximum is {MAX_CANDIDATE_JSON_BYTES})"
        )));
    }
    Ok(())
}

async fn load_and_validate_scope(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    required_user_id: Option<&str>,
) -> Result<ScoreOwner, TrackEditError> {
    let owner =
        sqlx::query_as::<_, ScoreOwner>("SELECT track_id, venue_id, uid FROM scores WHERE id = ?")
            .bind(&scope.score_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|e| TrackEditError::storage(format!("failed to resolve score scope: {e}")))?
            .ok_or_else(|| {
                TrackEditError::scope(format!("score {} does not exist", scope.score_id))
            })?;

    if owner.track_id != scope.track_id {
        return Err(TrackEditError::scope(format!(
            "score {} belongs to track {}, not {}",
            scope.score_id, owner.track_id, scope.track_id
        )));
    }
    if owner.venue_id.as_deref() != Some(scope.venue_id.as_str()) {
        return Err(TrackEditError::scope(format!(
            "score {} belongs to venue {}, not {}",
            scope.score_id,
            owner.venue_id.as_deref().unwrap_or("(none)"),
            scope.venue_id
        )));
    }
    if let Some(user_id) = required_user_id {
        if owner.uid.as_deref() != Some(user_id) {
            return Err(TrackEditError::scope(format!(
                "score {} is not owned by the current user",
                scope.score_id
            )));
        }
    }
    Ok(owner)
}

async fn load_scores(
    connection: &mut SqliteConnection,
    score_id: &str,
) -> Result<Vec<TrackScore>, TrackEditError> {
    sqlx::query_as::<_, TrackScore>(
        "SELECT id, uid, score_id, pattern_id, start_time, end_time, z_index,
                blend_mode, args_json, created_at, updated_at
         FROM track_scores
         WHERE score_id = ?
         ORDER BY start_time, z_index, id",
    )
    .bind(score_id)
    .fetch_all(connection)
    .await
    .map_err(|e| TrackEditError::storage(format!("failed to load track clips: {e}")))
}

async fn minimum_duration(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<f64, TrackEditError> {
    let row: Option<(Option<f64>, Option<i64>)> =
        sqlx::query_as("SELECT bpm, beats_per_bar FROM track_beats WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(connection)
            .await
            .map_err(|e| TrackEditError::storage(format!("failed to load track timing: {e}")))?;
    let (bpm, beats_per_bar) = row
        .and_then(|(bpm, beats)| bpm.zip(beats))
        .filter(|(bpm, beats)| bpm.is_finite() && *bpm > 0.0 && *beats > 0)
        .unwrap_or((120.0, 4));
    Ok(((beats_per_bar as f64 / bpm) * 60.0) / 32.0)
}

async fn track_duration(
    connection: &mut SqliteConnection,
    track_id: &str,
) -> Result<Option<f64>, TrackEditError> {
    let duration: Option<f64> =
        sqlx::query_scalar("SELECT duration_seconds FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(connection)
            .await
            .map_err(|e| TrackEditError::storage(format!("failed to load track duration: {e}")))?
            .flatten();
    Ok(duration.filter(|duration| duration.is_finite() && *duration > 0.0))
}

fn assert_current_revision(
    expected_revision: &str,
    current: &[TrackScore],
) -> Result<(), TrackEditError> {
    let current_revision = track_revision(current);
    if expected_revision == current_revision {
        Ok(())
    } else {
        Err(TrackEditError::Conflict {
            expected_revision: expected_revision.to_string(),
            current_revision,
        })
    }
}

async fn validate_candidate_ids(
    connection: &mut SqliteConnection,
    score_id: &str,
    current: &[TrackScore],
    candidate: &[TrackClip],
) -> Result<(), TrackEditError> {
    let current_ids: HashSet<&str> = current.iter().map(|score| score.id.as_str()).collect();
    let mut seen = HashSet::with_capacity(candidate.len());

    for clip in candidate {
        if clip.id.is_empty() {
            return Err(TrackEditError::invalid("clip id cannot be empty"));
        }
        if !seen.insert(clip.id.clone()) {
            return Err(TrackEditError::invalid(format!(
                "clip id {} appears more than once",
                clip.id
            )));
        }

        if !current_ids.contains(clip.id.as_str()) && !valid_draft_id(&clip.id) {
            let owner: Option<String> =
                sqlx::query_scalar("SELECT score_id FROM track_scores WHERE id = ?")
                    .bind(&clip.id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(|e| {
                        TrackEditError::storage(format!(
                            "failed to validate clip {} ownership: {e}",
                            clip.id
                        ))
                    })?;
            return Err(match owner {
                Some(owner) => TrackEditError::scope(format!(
                    "clip {} belongs to score {}, not {}",
                    clip.id, owner, score_id
                )),
                None => TrackEditError::invalid(format!(
                    "unknown clip id {}; new clips must use a new:* draft id",
                    clip.id
                )),
            });
        }
    }

    Ok(())
}

fn materialize_candidate(
    current: &[TrackScore],
    candidate: Vec<TrackClip>,
) -> (Vec<TrackClip>, BTreeMap<String, String>) {
    let current_ids: HashSet<&str> = current.iter().map(|score| score.id.as_str()).collect();
    let mut id_map = BTreeMap::new();
    let materialized = candidate
        .into_iter()
        .map(|mut clip| {
            if !current_ids.contains(clip.id.as_str()) {
                let draft_id = clip.id;
                let id = Uuid::new_v4().to_string();
                id_map.insert(draft_id, id.clone());
                clip.id = id;
            }
            clip
        })
        .collect();
    (materialized, id_map)
}

async fn validate_candidate(
    connection: &mut SqliteConnection,
    current: &[TrackScore],
    candidate: &[TrackClip],
    min_duration: f64,
    track_duration: Option<f64>,
) -> Result<(), TrackEditError> {
    let current_by_id: HashMap<&str, &TrackScore> = current
        .iter()
        .map(|score| (score.id.as_str(), score))
        .collect();

    let mut pattern_ids = BTreeSet::new();
    for clip in candidate {
        if !clip.start_time.is_finite() || !clip.end_time.is_finite() {
            return Err(TrackEditError::invalid(format!(
                "clip {} times must be finite",
                clip.id
            )));
        }
        let duration = clip.end_time - clip.start_time;
        if duration < min_duration {
            return Err(TrackEditError::invalid(format!(
                "clip {} is too short ({duration:.4}s); minimum is {min_duration:.4}s",
                clip.id
            )));
        }
        let preserves_existing_times =
            current_by_id.get(clip.id.as_str()).is_some_and(|existing| {
                existing.start_time.to_bits() == clip.start_time.to_bits()
                    && existing.end_time.to_bits() == clip.end_time.to_bits()
            });
        if !preserves_existing_times && clip.start_time < 0.0 {
            return Err(TrackEditError::invalid(format!(
                "clip {} starts before the track",
                clip.id
            )));
        }
        if !preserves_existing_times
            && track_duration.is_some_and(|track_duration| clip.end_time > track_duration + 1e-6)
        {
            return Err(TrackEditError::invalid(format!(
                "clip {} ends after the track",
                clip.id
            )));
        }
        if !clip.args.is_object() {
            let preserves_legacy = current_by_id
                .get(clip.id.as_str())
                .is_some_and(|existing| existing.args == clip.args);
            if !preserves_legacy {
                return Err(TrackEditError::invalid(format!(
                    "clip {} args must be an object",
                    clip.id
                )));
            }
        }
        pattern_ids.insert(clip.pattern_id.as_str());
    }

    for pattern_id in pattern_ids {
        let exists: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM patterns WHERE id = ?)")
            .bind(pattern_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|e| {
                TrackEditError::storage(format!("failed to validate pattern {pattern_id}: {e}"))
            })?;
        if exists == 0 {
            return Err(TrackEditError::invalid(format!(
                "pattern {pattern_id} does not exist"
            )));
        }
    }

    let before = overlap_pairs(
        &current
            .iter()
            .map(TrackClip::from)
            .collect::<Vec<TrackClip>>(),
    );
    let after = overlap_pairs(candidate);
    if let Some((left, right)) = after.difference(&before).next() {
        return Err(TrackEditError::invalid(format!(
            "clips {left} and {right} would overlap on the same layer"
        )));
    }

    Ok(())
}

fn valid_draft_id(id: &str) -> bool {
    let Some(token) = id.strip_prefix(DRAFT_ID_PREFIX) else {
        return false;
    };
    !token.is_empty()
        && token.len() <= 128
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn overlap_pairs(clips: &[TrackClip]) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for (index, left) in clips.iter().enumerate() {
        for right in &clips[index + 1..] {
            if left.z_index != right.z_index {
                continue;
            }
            if left.start_time < right.end_time && right.start_time < left.end_time {
                let pair = if left.id <= right.id {
                    (left.id.clone(), right.id.clone())
                } else {
                    (right.id.clone(), left.id.clone())
                };
                pairs.insert(pair);
            }
        }
    }
    pairs
}

fn same_semantics(existing: &TrackScore, candidate: &TrackClip) -> bool {
    existing.id == candidate.id
        && existing.pattern_id == candidate.pattern_id
        && existing.start_time.to_bits() == candidate.start_time.to_bits()
        && existing.end_time.to_bits() == candidate.end_time.to_bits()
        && existing.z_index == candidate.z_index
        && existing.blend_mode == candidate.blend_mode
        && existing.args == candidate.args
}

fn sorted_clips(mut clips: Vec<TrackClip>) -> Vec<TrackClip> {
    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
    clips
}

fn revision_for_clips(clips: &[TrackClip]) -> String {
    let mut ordered: Vec<&TrackClip> = clips.iter().collect();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));

    let mut hasher = Sha256::new();
    hasher.update(REVISION_DOMAIN);
    for clip in ordered {
        hash_string(&mut hasher, &clip.id);
        hash_string(&mut hasher, &clip.pattern_id);
        hasher.update(clip.start_time.to_bits().to_le_bytes());
        hasher.update(clip.end_time.to_bits().to_le_bytes());
        hasher.update(clip.z_index.to_le_bytes());
        hash_string(&mut hasher, blend_mode_name(clip.blend_mode));
        hash_string(&mut hasher, &canonical_json(&clip.args));
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let body: Vec<String> = sorted
                .into_iter()
                .map(|(key, value)| {
                    format!("{}:{}", Value::String(key.clone()), canonical_json(value))
                })
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", body.join(","))
        }
        scalar => scalar.to_string(),
    }
}

fn blend_mode_name(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Replace => "replace",
        BlendMode::Add => "add",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Max => "max",
        BlendMode::Min => "min",
        BlendMode::Lighten => "lighten",
        BlendMode::Value => "value",
        BlendMode::Subtract => "subtract",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{
        apply_track_edit, check_track_edit, load_track_document,
        replace_track_scores_from_snapshot, track_revision, validate_candidate_envelope, TrackClip,
        TrackEditError, TrackEditPlan, TrackEditScope,
    };
    use crate::models::node_graph::BlendMode;
    use crate::models::scores::TrackScore;

    const SCORE: &str = "score";
    const TRACK: &str = "track";
    const VENUE: &str = "venue";
    const USER: &str = "user";
    const PATTERN: &str = "pattern";

    fn scope() -> TrackEditScope {
        TrackEditScope {
            score_id: SCORE.to_string(),
            track_id: TRACK.to_string(),
            venue_id: VENUE.to_string(),
            user_id: USER.to_string(),
        }
    }

    fn score(id: &str, start_time: f64, end_time: f64, z_index: i64) -> TrackScore {
        TrackScore {
            id: id.to_string(),
            uid: Some(USER.to_string()),
            score_id: SCORE.to_string(),
            pattern_id: PATTERN.to_string(),
            start_time,
            end_time,
            z_index,
            blend_mode: BlendMode::Replace,
            args: json!({"intensity": 1}),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn clip(id: &str, start_time: f64, end_time: f64, z_index: i64) -> TrackClip {
        TrackClip::from(&score(id, start_time, end_time, z_index))
    }

    async fn test_pool() -> (TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(directory.path().join("track-edits.sqlite"))
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();

        sqlx::query(
            "CREATE TABLE scores (
                id TEXT PRIMARY KEY,
                uid TEXT,
                track_id TEXT NOT NULL,
                venue_id TEXT,
                name TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                version INTEGER NOT NULL DEFAULT 1,
                synced_at TEXT
            );
            CREATE TABLE patterns (id TEXT PRIMARY KEY);
            CREATE TABLE tracks (
                id TEXT PRIMARY KEY,
                duration_seconds REAL
            );
            CREATE TABLE track_beats (
                track_id TEXT PRIMARY KEY,
                bpm REAL NOT NULL,
                beats_per_bar INTEGER NOT NULL
            );
            CREATE TABLE track_scores (
                id TEXT PRIMARY KEY,
                uid TEXT,
                score_id TEXT NOT NULL,
                pattern_id TEXT NOT NULL,
                start_time REAL NOT NULL,
                end_time REAL NOT NULL,
                z_index INTEGER NOT NULL DEFAULT 0,
                blend_mode TEXT NOT NULL DEFAULT 'replace',
                args_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                version INTEGER NOT NULL DEFAULT 1,
                synced_at TEXT,
                FOREIGN KEY (score_id) REFERENCES scores(id),
                FOREIGN KEY (pattern_id) REFERENCES patterns(id)
            );
            CREATE TRIGGER track_scores_updated_at
            AFTER UPDATE ON track_scores FOR EACH ROW
            WHEN OLD.version = NEW.version
            BEGIN
                UPDATE track_scores
                SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                    version = OLD.version + 1
                WHERE id = OLD.id;
            END;",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id) VALUES
                ('score', 'user', 'track', 'venue'),
                ('other-score', 'other-user', 'other-track', 'other-venue');
             INSERT INTO patterns (id) VALUES ('pattern'), ('other-pattern');
             INSERT INTO tracks (id, duration_seconds) VALUES
                ('track', 120.0),
                ('other-track', 120.0);
             INSERT INTO track_beats (track_id, bpm, beats_per_bar)
             VALUES ('track', 120.0, 4);",
        )
        .execute(&pool)
        .await
        .unwrap();

        (directory, pool)
    }

    async fn insert_score(pool: &SqlitePool, score: &TrackScore, version: i64, synced_at: &str) {
        sqlx::query(
            "INSERT INTO track_scores
             (id, uid, score_id, pattern_id, start_time, end_time, z_index,
              blend_mode, args_json, created_at, updated_at, version, synced_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&score.id)
        .bind(&score.uid)
        .bind(&score.score_id)
        .bind(&score.pattern_id)
        .bind(score.start_time)
        .bind(score.end_time)
        .bind(score.z_index)
        .bind("replace")
        .bind(score.args.to_string())
        .bind(&score.created_at)
        .bind(&score.updated_at)
        .bind(version)
        .bind(synced_at)
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn revision_is_semantic_and_deterministic() {
        let first = score("a", 0.0, 1.0, 0);
        let mut second = score("b", 1.0, 2.0, 1);
        second.args = serde_json::from_str(r#"{"outer":{"z":2,"a":1},"x":3}"#).unwrap();

        let mut same_first = first.clone();
        same_first.created_at = "different".to_string();
        same_first.updated_at = "different".to_string();
        same_first.uid = Some("different".to_string());
        let mut same_second = second.clone();
        same_second.args = serde_json::from_str(r#"{"x":3,"outer":{"a":1,"z":2}}"#).unwrap();

        let expected = track_revision(&[first, second]);
        let mut semantically_same = vec![same_second, same_first];
        assert_eq!(expected, track_revision(&semantically_same));
        assert!(expected.starts_with("sha256:"));

        semantically_same[0].z_index += 1;
        assert_ne!(expected, track_revision(&semantically_same));
    }

    #[test]
    fn pathological_candidate_is_rejected_before_a_transaction() {
        let clips = (0..=super::MAX_TRACK_CLIPS)
            .map(|index| clip(&format!("new:{index}"), index as f64, index as f64 + 1.0, 0))
            .collect::<Vec<_>>();
        let error = validate_candidate_envelope(&clips).unwrap_err();
        assert!(error.to_string().contains("at most"));
    }

    #[tokio::test]
    async fn check_preserves_draft_ids_without_writing() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();
        let mut candidate = document.clips;
        candidate.push(clip("new:1", 1.0, 2.0, 0));

        let checked = check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision,
                candidate,
            },
        )
        .await
        .unwrap();

        assert!(checked.candidate.iter().any(|clip| clip.id == "new:1"));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_scores")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn apply_allocates_ids_and_preserves_unchanged_sync_identity() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 7, "synced-a").await;
        insert_score(&pool, &score("b", 1.0, 2.0, 0), 3, "synced-b").await;
        insert_score(&pool, &score("c", 3.0, 4.0, 0), 2, "synced-c").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();
        let mut candidate = document.clips;
        candidate.retain(|clip| clip.id != "c");
        candidate
            .iter_mut()
            .find(|clip| clip.id == "b")
            .unwrap()
            .args = json!({"intensity": 0.5});
        candidate.push(clip("new:accent", 2.0, 3.0, 0));

        let result = apply_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision,
                candidate,
            },
        )
        .await
        .unwrap();

        assert_eq!((result.added, result.updated, result.removed), (1, 1, 1));
        let allocated = result.id_map.get("new:accent").unwrap();
        assert!(Uuid::parse_str(allocated).is_ok());
        assert!(result.clips.iter().any(|clip| clip.id == *allocated));

        let unchanged: (String, String, i64, Option<String>) = sqlx::query_as(
            "SELECT created_at, updated_at, version, synced_at
             FROM track_scores WHERE id = 'a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            unchanged,
            (
                "2026-01-01T00:00:00Z".to_string(),
                "2026-01-01T00:00:00Z".to_string(),
                7,
                Some("synced-a".to_string())
            )
        );

        let updated: (i64, Option<String>) =
            sqlx::query_as("SELECT version, synced_at FROM track_scores WHERE id = 'b'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(updated, (4, Some("synced-b".to_string())));
        let new_uid: Option<String> =
            sqlx::query_scalar("SELECT uid FROM track_scores WHERE id = ?")
                .bind(allocated)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(new_uid.as_deref(), Some(USER));
        let removed_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM track_scores WHERE id = 'c'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(removed_count, 0);

        let reloaded = load_track_document(&pool, &scope()).await.unwrap();
        assert_eq!(result.revision, reloaded.revision);
        assert_eq!(result.clips, reloaded.clips);
    }

    #[tokio::test]
    async fn stale_plan_conflicts_without_deleting_or_overwriting() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 1, "synced").await;
        insert_score(&pool, &score("b", 1.0, 2.0, 0), 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();

        sqlx::query("UPDATE track_scores SET z_index = 4 WHERE id = 'b'")
            .execute(&pool)
            .await
            .unwrap();
        let error = apply_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision,
                candidate: vec![document.clips[0].clone()],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TrackEditError::Conflict { .. }));

        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT id, z_index FROM track_scores ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![("a".to_string(), 0), ("b".to_string(), 4)]);
    }

    #[tokio::test]
    async fn ui_replacement_uses_the_exact_base_snapshot_for_cas() {
        let (_directory, pool) = test_pool().await;
        let first = score("a", 0.0, 1.0, 0);
        let second = score("b", 1.0, 2.0, 0);
        insert_score(&pool, &first, 1, "synced").await;
        insert_score(&pool, &second, 1, "synced").await;
        let base = vec![first, second];

        sqlx::query("UPDATE track_scores SET z_index = 7 WHERE id = 'b'")
            .execute(&pool)
            .await
            .unwrap();
        let error = replace_track_scores_from_snapshot(
            &pool,
            &scope(),
            base.clone(),
            vec![base[0].clone()],
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TrackEditError::Conflict { .. }));

        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT id, z_index FROM track_scores ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![("a".to_string(), 0), ("b".to_string(), 7)]);
    }

    #[tokio::test]
    async fn ui_replacement_does_not_trust_new_identity_ownership_or_timestamps() {
        let (_directory, pool) = test_pool().await;
        let existing = score("a", 0.0, 1.0, 0);
        insert_score(&pool, &existing, 1, "synced").await;
        let mut client_created = score("client-chosen-id", 1.0, 2.0, 0);
        client_created.uid = Some("attacker".to_string());
        client_created.created_at = "1900-01-01T00:00:00Z".to_string();
        client_created.updated_at = "1900-01-01T00:00:00Z".to_string();

        let result = replace_track_scores_from_snapshot(
            &pool,
            &scope(),
            vec![existing.clone()],
            vec![existing, client_created],
        )
        .await
        .unwrap();
        let stored_id = result.id_map.get("client-chosen-id").unwrap();
        assert_ne!(stored_id, "client-chosen-id");
        assert!(Uuid::parse_str(stored_id).is_ok());

        let stored: (Option<String>, String, String) =
            sqlx::query_as("SELECT uid, created_at, updated_at FROM track_scores WHERE id = ?")
                .bind(stored_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored.0.as_deref(), Some(USER));
        assert_ne!(stored.1, "1900-01-01T00:00:00Z");
        assert_ne!(stored.2, "1900-01-01T00:00:00Z");
        let client_id_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM track_scores WHERE id = 'client-chosen-id'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(client_id_count, 0);
    }

    #[tokio::test]
    async fn existing_overlap_is_preserved_but_new_overlap_is_rejected() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 2.0, 0), 1, "synced").await;
        insert_score(&pool, &score("b", 1.0, 3.0, 0), 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();

        let no_op = apply_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision.clone(),
                candidate: document.clips.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!((no_op.added, no_op.updated, no_op.removed), (0, 0, 0));

        let mut candidate = document.clips;
        candidate.push(clip("new:collision", 1.5, 2.5, 0));
        let error = check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: no_op.revision,
                candidate,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TrackEditError::Invalid { .. }));
        assert!(error.to_string().contains("overlap on the same layer"));
    }

    #[tokio::test]
    async fn invalid_candidate_rolls_back_every_change() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 5, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();
        let mut changed = document.clips[0].clone();
        changed.z_index = 8;
        let mut invalid = clip("new:bad", 1.0, 2.0, 0);
        invalid.pattern_id = "missing-pattern".to_string();

        let error = apply_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision,
                candidate: vec![changed, invalid],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TrackEditError::Invalid { .. }));

        let state: (i64, i64) =
            sqlx::query_as("SELECT z_index, version FROM track_scores WHERE id = 'a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(state, (0, 5));
    }

    #[tokio::test]
    async fn ids_and_durations_are_authoritatively_validated() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 1, "synced").await;
        let mut foreign = score("foreign", 2.0, 3.0, 0);
        foreign.score_id = "other-score".to_string();
        foreign.uid = Some("other-user".to_string());
        insert_score(&pool, &foreign, 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();

        let duplicate = check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision.clone(),
                candidate: vec![document.clips[0].clone(), document.clips[0].clone()],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(duplicate, TrackEditError::Invalid { .. }));

        let unknown = check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision.clone(),
                candidate: vec![clip("not-a-draft", 1.0, 2.0, 0)],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(unknown, TrackEditError::Invalid { .. }));

        let foreign_error = check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision.clone(),
                candidate: vec![clip("foreign", 2.0, 3.0, 0)],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(foreign_error, TrackEditError::Scope { .. }));

        let too_short = check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision,
                candidate: vec![clip("new:short", 1.0, 1.01, 0)],
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(too_short, TrackEditError::Invalid { .. }));
    }

    #[tokio::test]
    async fn new_ranges_must_stay_inside_the_track_but_legacy_times_are_lossless() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("legacy", -1.0, 1.0, 0), 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();

        let mut legacy_update = document.clips[0].clone();
        legacy_update.z_index = 1;
        check_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision.clone(),
                candidate: vec![legacy_update],
            },
        )
        .await
        .unwrap();

        for invalid in [
            clip("new:before", -1.0, 1.0, 2),
            clip("new:after", 119.5, 120.5, 2),
        ] {
            let error = check_track_edit(
                &pool,
                &scope(),
                TrackEditPlan {
                    base_revision: document.revision.clone(),
                    candidate: vec![document.clips[0].clone(), invalid],
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(error, TrackEditError::Invalid { .. }));
        }
    }

    #[tokio::test]
    async fn scope_requires_exact_track_venue_and_owner() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 1, "synced").await;

        for wrong in [
            TrackEditScope {
                track_id: "wrong".to_string(),
                ..scope()
            },
            TrackEditScope {
                venue_id: "wrong".to_string(),
                ..scope()
            },
            TrackEditScope {
                user_id: "wrong".to_string(),
                ..scope()
            },
        ] {
            let error = load_track_document(&pool, &wrong).await.unwrap_err();
            assert!(matches!(error, TrackEditError::Scope { .. }));
        }
    }

    #[tokio::test]
    async fn two_concurrent_plans_from_one_revision_have_one_winner() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 1.0, 0), 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();
        let mut first_candidate = document.clips.clone();
        first_candidate[0].z_index = 1;
        let mut second_candidate = document.clips;
        second_candidate[0].z_index = 2;

        let edit_scope = scope();
        let first = apply_track_edit(
            &pool,
            &edit_scope,
            TrackEditPlan {
                base_revision: document.revision.clone(),
                candidate: first_candidate,
            },
        );
        let second = apply_track_edit(
            &pool,
            &edit_scope,
            TrackEditPlan {
                base_revision: document.revision,
                candidate: second_candidate,
            },
        );
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.is_ok() as u8 + second.is_ok() as u8, 1);
        let loser = if first.is_err() {
            first.unwrap_err()
        } else {
            second.unwrap_err()
        };
        assert!(matches!(loser, TrackEditError::Conflict { .. }));
        let stored_z: i64 = sqlx::query_scalar("SELECT z_index FROM track_scores WHERE id = 'a'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(matches!(stored_z, 1 | 2));
    }
}
