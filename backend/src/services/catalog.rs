//! Creating and deleting the rows a person owns: patterns, their default
//! implementation, and scores.
//!
//! Every id here is derived from the caller's `request_id`, so a retried
//! request finds the row it made the first time instead of making a second one.
//! That is the whole of the idempotency story now — there is no operation
//! ledger to consult, because the row either exists or it does not.

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::database::local::auth::principal_key;
use crate::database::local::patterns as patterns_db;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, VenueResource, Write};
use crate::models::node_graph::Graph;
use crate::models::patterns::{ForkPatternInput, ForkPatternResult, PatternSummary};
use crate::models::scores::Score;
use crate::services::graph_documents::exact_graph_json;

/// Create a pattern and its single, empty implementation.
pub async fn create_pattern(
    pool: &SqlitePool,
    principal: Option<&str>,
    request_id: &str,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    create_pattern_with_graph(pool, principal, request_id, name, description, None, None).await
}

/// Create a pattern whose implementation starts from `graph`, optionally local
/// to one score.
pub async fn create_pattern_with_graph(
    pool: &SqlitePool,
    principal: Option<&str>,
    request_id: &str,
    name: String,
    description: Option<String>,
    graph: Option<Graph>,
    score_id: Option<&str>,
) -> Result<PatternSummary, String> {
    let request_id = request_uuid(request_id)?;
    let key = principal_key(principal);
    let pattern_id = derived_id(&key, "pattern", &request_id, "subject");
    let implementation_id = derived_id(&key, "pattern", &request_id, "implementation");
    if let Some(pattern) = patterns_db::optional_pattern(pool, &pattern_id).await? {
        return Ok(pattern);
    }
    let graph = graph.unwrap_or(Graph {
        nodes: Vec::new(),
        edges: Vec::new(),
        args: Vec::new(),
    });
    crate::services::graph_documents::canonicalize_graph(&graph)
        .map_err(|error| error.to_string())?;
    let graph_json = exact_graph_json(&graph).map_err(|error| error.to_string())?;
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("begin pattern creation: {error}"))?;
    sqlx::query("INSERT INTO patterns (id, uid, name, description, score_id) VALUES (?, ?, ?, ?, ?)")
        .bind(&pattern_id)
        .bind(principal)
        .bind(&name)
        .bind(&description)
        .bind(score_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("insert pattern: {error}"))?;
    insert_implementation(&mut transaction, &implementation_id, principal, &pattern_id, &graph_json)
        .await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("commit pattern creation: {error}"))?;
    patterns_db::get_pattern_pool(pool, &pattern_id).await
}

/// Copy a pattern's graph into a new pattern of the caller's own.
pub async fn fork_pattern(
    pool: &SqlitePool,
    principal: Option<&str>,
    input: ForkPatternInput,
) -> Result<ForkPatternResult, String> {
    let request_id = request_uuid(&input.request_id)?;
    let key = principal_key(principal);
    let pattern_id = derived_id(&key, "pattern_fork", &request_id, "subject");
    let implementation_id = derived_id(&key, "pattern_fork", &request_id, "implementation");
    if let Some(pattern) = patterns_db::optional_pattern(pool, &pattern_id).await? {
        return Ok(ForkPatternResult {
            pattern,
            implementation_id,
        });
    }
    let source = patterns_db::get_pattern_pool(pool, &input.source_pattern_id).await?;
    let document = crate::services::graph_documents::load_visible_graph_document(
        pool,
        &input.source_pattern_id,
        None,
        Some(&input.source_implementation_id),
    )
    .await
    .map_err(|error| error.to_string())?;
    let graph_json = exact_graph_json(&document.graph).map_err(|error| error.to_string())?;
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("begin pattern fork: {error}"))?;
    sqlx::query(
        "INSERT INTO patterns (id, uid, name, description, forked_from_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&pattern_id)
    .bind(principal)
    .bind(format!("{}_fork", source.name))
    .bind(&source.description)
    .bind(&input.source_pattern_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("insert forked pattern: {error}"))?;
    insert_implementation(&mut transaction, &implementation_id, principal, &pattern_id, &graph_json)
        .await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("commit pattern fork: {error}"))?;
    Ok(ForkPatternResult {
        pattern: patterns_db::get_pattern_pool(pool, &pattern_id).await?,
        implementation_id,
    })
}

/// Delete a pattern the caller owns. Its implementations go with it.
pub async fn delete_pattern(
    pool: &SqlitePool,
    principal: Option<&str>,
    id: &str,
) -> Result<(), String> {
    let owner: Option<Option<String>> = sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|error| format!("read pattern owner: {error}"))?;
    let Some(owner) = owner else {
        return Err(format!("pattern {id} does not exist"));
    };
    if owner.as_deref() != principal {
        return Err("you can only delete your own patterns".into());
    }
    sqlx::query("DELETE FROM patterns WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|error| format!("delete pattern: {error}"))?;
    Ok(())
}

/// Create a score on a `(track, venue)` pair. A pair carries as many scores as
/// there are people who annotated it, so this always makes a new one.
pub async fn create_score(
    pool: &SqlitePool,
    request_id: &str,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
) -> Result<Score, String> {
    insert_score(pool, request_id, track_id, venue_id, name, false).await
}

/// Give the track durable membership in the venue: an existing score for the
/// pair is returned rather than joined by a second one.
pub async fn ensure_venue_score(
    pool: &SqlitePool,
    request_id: &str,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
) -> Result<Score, String> {
    insert_score(pool, request_id, track_id, venue_id, name, true).await
}

async fn insert_score(
    pool: &SqlitePool,
    request_id: &str,
    track_id: &str,
    venue_id: &str,
    name: Option<&str>,
    reuse_existing: bool,
) -> Result<Score, String> {
    let request_id = request_uuid(request_id)?;
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id)).await?;
    let owner = access.principal().map(str::to_owned);
    let score_id = derived_id(&principal_key(owner.as_deref()), "score", &request_id, "subject");
    let existing: Option<String> = if reuse_existing {
        sqlx::query_scalar(
            "SELECT id FROM scores WHERE (track_id = ? AND venue_id = ?) OR id = ?
             ORDER BY id = ? DESC, created_at, id LIMIT 1",
        )
        .bind(track_id)
        .bind(venue_id)
        .bind(&score_id)
        .bind(&score_id)
        .fetch_optional(access.connection())
        .await
        .map_err(|error| format!("find an existing score: {error}"))?
    } else {
        sqlx::query_scalar("SELECT id FROM scores WHERE id = ?")
            .bind(&score_id)
            .fetch_optional(access.connection())
            .await
            .map_err(|error| format!("find an existing score: {error}"))?
    };
    if let Some(existing) = existing {
        return crate::database::local::scores::get_score(&mut access, &existing).await;
    }
    let visible: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM auth_visible_tracks WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(access.connection())
            .await
            .map_err(|error| format!("authorize the score's track: {error}"))?;
    if visible.is_none() {
        return Err("track does not exist".into());
    }
    sqlx::query("INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES (?, ?, ?, ?, ?)")
        .bind(&score_id)
        .bind(owner.as_deref())
        .bind(track_id)
        .bind(venue_id)
        .bind(name)
        .execute(access.connection())
        .await
        .map_err(|error| format!("insert score: {error}"))?;
    let score = crate::database::local::scores::get_score(&mut access, &score_id).await?;
    access.commit().await?;
    Ok(score)
}

/// Delete a score. Its clips, definitions and drafts cascade; the agent threads
/// that worked on it do not — a conversation outlives its subject.
pub async fn delete_score(pool: &SqlitePool, score_id: &str) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(pool, VenueResource::Score(score_id)).await?;
    sqlx::query("DELETE FROM scores WHERE id = ?")
        .bind(score_id)
        .execute(access.connection())
        .await
        .map_err(|error| format!("delete score: {error}"))?;
    access.commit().await
}

async fn insert_implementation(
    connection: &mut sqlx::SqliteConnection,
    id: &str,
    principal: Option<&str>,
    pattern_id: &str,
    graph_json: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO implementations (id, uid, pattern_id, name, graph_json)
         VALUES (?, ?, ?, NULL, ?)",
    )
    .bind(id)
    .bind(principal)
    .bind(pattern_id)
    .bind(graph_json)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("insert pattern implementation: {error}"))?;
    Ok(())
}

fn request_uuid(request_id: &str) -> Result<String, String> {
    Uuid::parse_str(request_id)
        .map(|id| id.to_string())
        .map_err(|_| "creation request_id must be a UUID".to_string())
}

/// The same `(principal, kind, request, role)` always names the same row, so a
/// retry is a lookup rather than a second creation.
fn derived_id(principal_key: &str, kind: &str, request_id: &str, role: &str) -> String {
    let mut hash = Sha256::new();
    for field in [
        "luma.creation-id.v1",
        principal_key,
        kind,
        request_id,
        role,
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
