//! Validation and relational projection for authored lighting timelines.
//!
//! Python drafts send a complete semantic candidate, never persistence-owned
//! fields. Read-only checks bind candidates to a host-resolved score scope.
//! Mutations enter through `AuthoredDocuments`, which owns revision history and the
//! transaction, then calls this module's in-transaction projector. New row
//! identities, timestamps, and sync ownership always come from Rust/SQLite.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use ts_rs::TS;
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

/// One authored pattern placement, stripped of database/sync bookkeeping.
///
/// Persisted clips use their UUID. A draft-only clip uses a reserved `new:*`
/// id until apply; the result maps that id to the UUID allocated by Rust.
#[derive(TS, Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackClip {
    pub id: String,
    pub pattern_id: String,
    pub start_time: f64,
    pub end_time: f64,
    #[ts(type = "number")]
    pub z_index: i64,
    pub blend_mode: BlendMode,
    #[ts(type = "Record<string, unknown>")]
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
#[derive(TS, Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackEditResult {
    pub revision: String,
    pub clips: Vec<TrackClip>,
    pub id_map: BTreeMap<String, String>,
    /// Stable ID allocated by the partial-create adapter, when this result
    /// originated from that operation.
    pub created_clip_id: Option<String>,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// False only for a response-loss retry whose original operation commit
    /// remains in history but a newer edit is now the live projection.
    pub applied_to_current_projection: bool,
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

#[derive(Clone, Copy)]
enum ScopeOwnership<'a> {
    /// Read-only previews may inspect a coherent exact score scope regardless
    /// of who owns it.
    Any,
    /// Mutations and durable history are pinned to the current principal;
    /// `None` means the signed-out principal and therefore requires SQL NULL.
    Principal(Option<&'a str>),
}

#[derive(Clone, Copy)]
enum NewIdentityPolicy<'a> {
    /// Model/UI candidates use `new:*` correlation IDs; Rust allocates UUIDs.
    DraftIds,
    /// An authored workspace may combine new `new:*` correlations with stable IDs that
    /// already occur in that workspace's reachable revision history.
    WorkspaceLineage(&'a BTreeSet<String>),
    /// A validated authored revision restores its durable clip UUIDs verbatim.
    StableIds(TrackProjectionIdentity<'a>),
}

/// Why an unknown stable UUID is allowed to enter a score projection. Human
/// imports may add only IDs allocated by the host during that compile. Trees
/// already accepted into this document's revision lineage may restore deleted
/// historical IDs.
#[derive(Clone, Copy)]
pub(crate) enum TrackProjectionIdentity<'a> {
    ExistingOnly,
    HostAllocated(&'a BTreeSet<String>),
    Allowed {
        lineage_ids: &'a BTreeSet<String>,
        host_allocated_ids: &'a BTreeSet<String>,
    },
    TrustedRevision,
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
#[cfg(test)]
pub async fn load_track_document(
    pool: &SqlitePool,
    scope: &TrackEditScope,
) -> Result<TrackDocument, TrackEditError> {
    let track_scope = TrackScope::from(scope);
    load_track_document_for_principal(pool, &track_scope, Some(&scope.user_id)).await
}

/// Load the document for the exact authenticated (or signed-out) principal.
/// This is the durable-history counterpart to the public read-only preview,
/// which intentionally has no ownership requirement.
pub async fn load_track_document_for_principal(
    pool: &SqlitePool,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
) -> Result<TrackDocument, TrackEditError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to begin track read: {e}")))?;
    let document = load_track_document_for_connection(&mut tx, scope, owner_user_id).await?;
    tx.commit()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to finish track read: {e}")))?;
    Ok(document)
}

pub(crate) async fn load_track_document_for_connection(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
) -> Result<TrackDocument, TrackEditError> {
    load_and_validate_scope(connection, scope, ScopeOwnership::Principal(owner_user_id)).await?;
    let scores = load_scores(connection, &scope.score_id).await?;
    Ok(TrackDocument {
        revision: track_revision(&scores),
        clips: sorted_clips(scores.iter().map(TrackClip::from).collect()),
    })
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

pub(crate) async fn check_track_candidate_for_connection(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    plan: TrackEditPlan,
) -> Result<TrackEditCheck, TrackEditError> {
    check_track_candidate_on_connection(connection, scope, ScopeOwnership::Any, plan).await
}

async fn check_track_candidate_as(
    pool: &SqlitePool,
    scope: &TrackScope,
    required_user_id: Option<&str>,
    plan: TrackEditPlan,
) -> Result<TrackEditCheck, TrackEditError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to begin track check: {e}")))?;
    let ownership = required_user_id
        .map(|owner| ScopeOwnership::Principal(Some(owner)))
        .unwrap_or(ScopeOwnership::Any);
    let checked = check_track_candidate_on_connection(&mut tx, scope, ownership, plan).await?;
    tx.commit()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to finish track check: {e}")))?;
    Ok(checked)
}

async fn check_track_candidate_on_connection(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    ownership: ScopeOwnership<'_>,
    plan: TrackEditPlan,
) -> Result<TrackEditCheck, TrackEditError> {
    validate_candidate_envelope(&plan.candidate)?;
    load_and_validate_scope(connection, scope, ownership).await?;
    let current = load_scores(connection, &scope.score_id).await?;
    assert_current_revision(&plan.base_revision, &current)?;
    let min_duration = minimum_duration(connection, &scope.track_id).await?;
    let track_duration = track_duration(connection, &scope.track_id).await?;
    validate_candidate_ids(
        connection,
        &scope.score_id,
        &current,
        &plan.candidate,
        NewIdentityPolicy::DraftIds,
    )
    .await?;
    validate_candidate(
        connection,
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
    Ok(checked)
}

/// Test harness for exercising the in-transaction projector in isolation.
#[cfg(test)]
async fn apply_track_edit(
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
    let result = apply_track_candidate_in_transaction(
        &mut tx,
        &track_scope,
        ScopeOwnership::Principal(Some(&scope.user_id)),
        plan,
        NewIdentityPolicy::DraftIds,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| TrackEditError::storage(format!("failed to commit track edit: {e}")))?;
    Ok(result)
}

/// Project a validated authored score revision while preserving its stable clip UUIDs.
/// The caller owns the SQLite transaction so the score rows and revision head
/// advance atomically.
pub(crate) async fn apply_track_projection_in_transaction(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
    plan: TrackEditPlan,
    identity: TrackProjectionIdentity<'_>,
) -> Result<TrackEditResult, TrackEditError> {
    validate_candidate_envelope(&plan.candidate)?;
    apply_track_candidate_in_transaction(
        connection,
        scope,
        ScopeOwnership::Principal(owner_user_id),
        plan,
        NewIdentityPolicy::StableIds(identity),
    )
    .await
}

/// Validate a complete score candidate at an authored-workspace boundary without
/// projecting it. Unlike a UI draft check, stable IDs are authorized by the
/// caller's explicit provenance policy rather than accepted merely for being
/// UUID-shaped.
pub(crate) async fn check_track_projection_candidate(
    pool: &SqlitePool,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
    candidate: &[TrackClip],
    identity: TrackProjectionIdentity<'_>,
) -> Result<(), TrackEditError> {
    validate_candidate_envelope(candidate)?;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| TrackEditError::storage(format!("begin authored score check: {error}")))?;
    load_and_validate_scope(
        &mut transaction,
        scope,
        ScopeOwnership::Principal(owner_user_id),
    )
    .await?;
    let current = load_scores(&mut transaction, &scope.score_id).await?;
    let min_duration = minimum_duration(&mut transaction, &scope.track_id).await?;
    let duration = track_duration(&mut transaction, &scope.track_id).await?;
    validate_candidate_ids(
        &mut transaction,
        &scope.score_id,
        &current,
        candidate,
        NewIdentityPolicy::StableIds(identity),
    )
    .await?;
    validate_candidate(
        &mut transaction,
        &current,
        candidate,
        min_duration,
        duration,
    )
    .await?;
    transaction.commit().await.map_err(|error| {
        TrackEditError::storage(format!("finish authored score check: {error}"))
    })?;
    Ok(())
}

/// Validate model/human-authored workspace source without allocating durable
/// row identities or mutating SQLite. Missing IDs arrive as reserved `new:*`
/// correlations, while stable IDs must already occur in the exact workspace
/// revision lineage. This lets a canonical workspace remain checkable after its
/// newly allocated IDs have been committed without accepting caller-invented
/// UUIDs.
pub(crate) async fn check_track_workspace_candidate(
    pool: &SqlitePool,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
    current: &[TrackClip],
    candidate: &[TrackClip],
    lineage_ids: &BTreeSet<String>,
) -> Result<(), TrackEditError> {
    validate_candidate_envelope(candidate)?;
    let current = detached_track_scores(scope, owner_user_id, current);
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| TrackEditError::storage(format!("begin score draft check: {error}")))?;
    load_and_validate_scope(
        &mut transaction,
        scope,
        ScopeOwnership::Principal(owner_user_id),
    )
    .await?;
    let min_duration = minimum_duration(&mut transaction, &scope.track_id).await?;
    let duration = track_duration(&mut transaction, &scope.track_id).await?;
    validate_candidate_ids(
        &mut transaction,
        &scope.score_id,
        &current,
        candidate,
        NewIdentityPolicy::WorkspaceLineage(lineage_ids),
    )
    .await?;
    validate_candidate(
        &mut transaction,
        &current,
        candidate,
        min_duration,
        duration,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| TrackEditError::storage(format!("finish score draft check: {error}")))?;
    Ok(())
}

/// Validate a detached authored candidate against the exact document it was
/// based on. Unlike live projection checks, identity ownership must not drift
/// with concurrent changes to the main score.
pub(crate) async fn check_track_detached_candidate(
    pool: &SqlitePool,
    scope: &TrackScope,
    owner_user_id: Option<&str>,
    current: &[TrackClip],
    candidate: &[TrackClip],
    identity: TrackProjectionIdentity<'_>,
) -> Result<(), TrackEditError> {
    validate_candidate_envelope(candidate)?;
    let current = detached_track_scores(scope, owner_user_id, current);
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| TrackEditError::storage(format!("begin score draft check: {error}")))?;
    load_and_validate_scope(
        &mut transaction,
        scope,
        ScopeOwnership::Principal(owner_user_id),
    )
    .await?;
    let min_duration = minimum_duration(&mut transaction, &scope.track_id).await?;
    let duration = track_duration(&mut transaction, &scope.track_id).await?;
    validate_candidate_ids(
        &mut transaction,
        &scope.score_id,
        &current,
        candidate,
        NewIdentityPolicy::StableIds(identity),
    )
    .await?;
    validate_candidate(
        &mut transaction,
        &current,
        candidate,
        min_duration,
        duration,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| TrackEditError::storage(format!("finish score draft check: {error}")))?;
    Ok(())
}

fn detached_track_scores(
    scope: &TrackScope,
    owner_user_id: Option<&str>,
    current: &[TrackClip],
) -> Vec<TrackScore> {
    current
        .iter()
        .map(|clip| TrackScore {
            id: clip.id.clone(),
            uid: owner_user_id.map(str::to_owned),
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

async fn apply_track_candidate_in_transaction(
    connection: &mut SqliteConnection,
    scope: &TrackScope,
    ownership: ScopeOwnership<'_>,
    plan: TrackEditPlan,
    identity_policy: NewIdentityPolicy<'_>,
) -> Result<TrackEditResult, TrackEditError> {
    let owner = load_and_validate_scope(connection, scope, ownership).await?;
    let current = load_scores(connection, &scope.score_id).await?;
    assert_current_revision(&plan.base_revision, &current)?;

    let min_duration = minimum_duration(connection, &scope.track_id).await?;
    let track_duration = track_duration(connection, &scope.track_id).await?;
    validate_candidate_ids(
        connection,
        &scope.score_id,
        &current,
        &plan.candidate,
        identity_policy,
    )
    .await?;
    validate_candidate(
        connection,
        &current,
        &plan.candidate,
        min_duration,
        track_duration,
    )
    .await?;
    let (candidate, id_map) = materialize_candidate(&current, plan.candidate, identity_policy);

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
            .execute(&mut *connection)
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
        .bind(clip.blend_mode.name())
        .bind(clip.args.to_string())
        .execute(&mut *connection)
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
        .bind(clip.blend_mode.name())
        .bind(clip.args.to_string())
        .bind(&clip.id)
        .bind(&scope.score_id)
        .execute(&mut *connection)
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

    let stored = load_scores(connection, &scope.score_id).await?;
    let result = TrackEditResult {
        revision: track_revision(&stored),
        clips: sorted_clips(stored.iter().map(TrackClip::from).collect()),
        id_map,
        created_clip_id: None,
        added: additions.len(),
        updated: updates.len(),
        removed: removed_ids.len(),
        applied_to_current_projection: true,
    };
    Ok(result)
}

/// Test harness for the persistence-shaped snapshot adapter. Production uses
/// `AuthoredDocuments::replace_track_scores_for_scope` so history and SQLite move
/// together.
#[cfg(test)]
async fn replace_track_scores_from_snapshot(
    pool: &SqlitePool,
    scope: &TrackEditScope,
    base: Vec<TrackScore>,
    candidate: Vec<TrackScore>,
) -> Result<TrackEditResult, TrackEditError> {
    let replacement = plan_track_snapshot_replacement(&scope.score_id, &base, &candidate)?;
    let result = apply_track_edit(pool, scope, replacement.plan).await?;
    Ok(remap_track_snapshot_result(
        result,
        replacement.client_ids_by_draft,
    ))
}

pub(crate) struct TrackSnapshotReplacement {
    pub plan: TrackEditPlan,
    pub client_ids_by_draft: BTreeMap<String, String>,
}

/// Convert persistence-shaped UI snapshots into an authority-safe semantic
/// plan without touching SQLite. Candidate IDs absent from the exact base are
/// correlation values, not row authority, and are rewritten to draft IDs.
pub(crate) fn plan_track_snapshot_replacement(
    score_id: &str,
    base: &[TrackScore],
    candidate: &[TrackScore],
) -> Result<TrackSnapshotReplacement, TrackEditError> {
    let base_ids = validate_snapshot_rows(score_id, base, "base snapshot")?;
    validate_snapshot_rows(score_id, candidate, "candidate")?;
    let mut client_ids_by_draft = BTreeMap::new();
    let candidate = candidate
        .iter()
        .enumerate()
        .map(|(index, score)| {
            let mut clip = TrackClip::from(score);
            if !base_ids.contains(score.id.as_str()) {
                let draft_id = format!("new:ui-{index}");
                client_ids_by_draft.insert(draft_id.clone(), score.id.clone());
                clip.id = draft_id;
            }
            clip
        })
        .collect();
    Ok(TrackSnapshotReplacement {
        plan: TrackEditPlan {
            base_revision: track_revision(base),
            candidate,
        },
        client_ids_by_draft,
    })
}

pub(crate) fn remap_track_snapshot_result(
    mut result: TrackEditResult,
    mut client_ids_by_draft: BTreeMap<String, String>,
) -> TrackEditResult {
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
    result
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
    ownership: ScopeOwnership<'_>,
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
    let owns_scope = match ownership {
        ScopeOwnership::Any => true,
        ScopeOwnership::Principal(expected) => owner.uid.as_deref() == expected,
    };
    if !owns_scope {
        return Err(TrackEditError::scope(format!(
            "score {} is not owned by the current principal",
            scope.score_id
        )));
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
    identity_policy: NewIdentityPolicy<'_>,
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

        if !current_ids.contains(clip.id.as_str()) {
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
            if let Some(owner) = owner {
                return Err(TrackEditError::scope(format!(
                    "clip {} belongs to score {}, not {}",
                    clip.id, owner, score_id
                )));
            }
            match identity_policy {
                NewIdentityPolicy::DraftIds if !valid_draft_id(&clip.id) => {
                    return Err(TrackEditError::invalid(format!(
                        "unknown clip id {}; new clips must use a new:* draft id",
                        clip.id
                    )));
                }
                NewIdentityPolicy::WorkspaceLineage(lineage_ids) => {
                    if !valid_draft_id(&clip.id) {
                        if Uuid::parse_str(&clip.id).is_err() {
                            return Err(TrackEditError::invalid(format!(
                                "authored score contains invalid stable clip id {}",
                                clip.id
                            )));
                        }
                        if !lineage_ids.contains(&clip.id) {
                            return Err(TrackEditError::invalid(format!(
                                "score source supplied unknown clip id {}; omit the id to let Luma allocate it",
                                clip.id
                            )));
                        }
                    }
                }
                NewIdentityPolicy::StableIds(identity) => {
                    if valid_draft_id(&clip.id) || Uuid::parse_str(&clip.id).is_err() {
                        return Err(TrackEditError::invalid(format!(
                            "authored score contains invalid stable clip id {}",
                            clip.id
                        )));
                    }
                    let allowed = match identity {
                        TrackProjectionIdentity::ExistingOnly => false,
                        TrackProjectionIdentity::HostAllocated(allowed) => {
                            allowed.contains(&clip.id)
                        }
                        TrackProjectionIdentity::Allowed {
                            lineage_ids,
                            host_allocated_ids,
                        } => {
                            lineage_ids.contains(&clip.id) || host_allocated_ids.contains(&clip.id)
                        }
                        TrackProjectionIdentity::TrustedRevision => true,
                    };
                    if !allowed {
                        return Err(TrackEditError::invalid(format!(
                            "score source supplied unknown clip id {}; omit the id to let Luma allocate it",
                            clip.id
                        )));
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn materialize_candidate(
    current: &[TrackScore],
    candidate: Vec<TrackClip>,
    identity_policy: NewIdentityPolicy<'_>,
) -> (Vec<TrackClip>, BTreeMap<String, String>) {
    let current_ids: HashSet<&str> = current.iter().map(|score| score.id.as_str()).collect();
    let mut id_map = BTreeMap::new();
    let materialized = candidate
        .into_iter()
        .map(|mut clip| {
            if !current_ids.contains(clip.id.as_str())
                && matches!(identity_policy, NewIdentityPolicy::DraftIds)
            {
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

/// Check a candidate against the rules a stored clip must obey: finite times
/// inside the track, a minimum duration, an object for args, and a pattern
/// that exists.
///
/// Two clips overlapping in time on one layer is **not** one of those rules.
/// The timeline is a stack of layers the compositor blends in `z_index` order
/// and resolves ties by array order; a lane is not a monophonic voice, so an
/// editor that lets a clip be dragged across its neighbour is expressing what
/// the model already allows. Rejecting it here refused an edit the canvas had
/// already painted and the user had already seen land.
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
            let preserves_legacy = current_by_id.get(clip.id.as_str()).is_some_and(|existing| {
                crate::canonical_json::equivalent(&existing.args, &clip.args)
            });
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

pub(crate) fn is_valid_track_draft_id(id: &str) -> bool {
    valid_draft_id(id)
}

/// Validate caller correlation identities before any UUID allocation. This is
/// intentionally pure so a duplicate or malformed draft cannot consume IDs or
/// partially alter an authored workspace.
pub(crate) fn validate_track_draft_envelope(candidate: &[TrackClip]) -> Result<(), TrackEditError> {
    validate_candidate_envelope(candidate)?;
    let mut seen = HashSet::with_capacity(candidate.len());
    for clip in candidate {
        if !seen.insert(clip.id.as_str()) {
            return Err(TrackEditError::invalid(format!(
                "clip id {} appears more than once",
                clip.id
            )));
        }
        if clip.id.starts_with("new:") && !valid_draft_id(&clip.id) {
            return Err(TrackEditError::invalid(format!(
                "invalid draft clip id {}",
                clip.id
            )));
        }
    }
    Ok(())
}

fn same_semantics(existing: &TrackScore, candidate: &TrackClip) -> bool {
    track_clips_semantically_equal(&TrackClip::from(existing), candidate)
}

pub(crate) fn track_clips_semantically_equal(left: &TrackClip, right: &TrackClip) -> bool {
    left.id == right.id
        && left.pattern_id == right.pattern_id
        && left.start_time.to_bits() == right.start_time.to_bits()
        && left.end_time.to_bits() == right.end_time.to_bits()
        && left.z_index == right.z_index
        && left.blend_mode == right.blend_mode
        && crate::canonical_json::equivalent(&left.args, &right.args)
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

pub(crate) fn revision_for_clips(clips: &[TrackClip]) -> String {
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
        hash_string(&mut hasher, clip.blend_mode.name());
        hash_string(&mut hasher, &crate::canonical_json::to_string(&clip.args));
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
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

    /// Two clips may share a layer and a span: the compositor blends by
    /// `z_index` and breaks ties by order, and the editors let a clip be
    /// dragged across its neighbour. A guard here refused edits the user had
    /// already watched land.
    #[tokio::test]
    async fn clips_may_overlap_on_one_layer() {
        let (_directory, pool) = test_pool().await;
        insert_score(&pool, &score("a", 0.0, 2.0, 0), 1, "synced").await;
        insert_score(&pool, &score("b", 4.0, 6.0, 0), 1, "synced").await;
        let document = load_track_document(&pool, &scope()).await.unwrap();

        // Slide `b` back over `a`, and land a third clip inside both.
        let mut candidate = document.clips.clone();
        let moved = candidate
            .iter_mut()
            .find(|existing| existing.id == "b")
            .unwrap();
        moved.start_time = 1.0;
        moved.end_time = 3.0;
        candidate.push(clip("new:collision", 1.5, 2.5, 0));

        let applied = apply_track_edit(
            &pool,
            &scope(),
            TrackEditPlan {
                base_revision: document.revision,
                candidate,
            },
        )
        .await
        .unwrap();
        assert_eq!((applied.added, applied.updated, applied.removed), (1, 1, 0));

        let reloaded = load_track_document(&pool, &scope()).await.unwrap();
        assert_eq!(reloaded.clips.len(), 3);
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
