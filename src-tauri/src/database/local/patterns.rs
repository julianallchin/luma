use crate::models::node_graph::{Graph, PatternArgDef};
use crate::models::patterns::PatternSummary;

/// Core: fetch a pattern summary
pub async fn get_pattern_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.remote_id, p.uid, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at, p.is_published, p.author_name, p.forked_from_remote_id
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern: {}\n", e))?;

    Ok(row)
}

/// Core: list patterns
pub async fn list_patterns_pool(pool: &sqlx::SqlitePool) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(
        "SELECT p.id, p.remote_id, p.uid, p.name, p.description, p.category_id, c.name as category_name, p.created_at, p.updated_at, p.is_published, p.author_name, p.forked_from_remote_id
         FROM patterns p
         LEFT JOIN pattern_categories c ON p.category_id = c.id
         ORDER BY p.updated_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to query patterns: {}\n", e))?;

    Ok(rows)
}

/// Core: create a pattern
pub async fn create_pattern_pool(
    pool: &sqlx::SqlitePool,
    name: String,
    description: Option<String>,
    uid: Option<String>,
) -> Result<PatternSummary, String> {
    // remote_id starts as NULL - populated after successful cloud sync with Supabase's BIGINT id
    let id = sqlx::query("INSERT INTO patterns (name, description, uid) VALUES (?, ?, ?)")
        .bind(&name)
        .bind(&description)
        .bind(&uid)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create pattern: {}\n", e))?
        .last_insert_rowid();

    get_pattern_pool(pool, id).await
}

/// Core: update pattern name and description
pub async fn update_pattern_pool(
    pool: &sqlx::SqlitePool,
    id: i64,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, String> {
    sqlx::query("UPDATE patterns SET name = ?, description = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(&name)
        .bind(&description)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update pattern: {}\n", e))?;

    get_pattern_pool(pool, id).await
}

/// Core: set pattern category
pub async fn set_pattern_category_pool(
    pool: &sqlx::SqlitePool,
    pattern_id: i64,
    category_id: Option<i64>,
) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET category_id = ? WHERE id = ?")
        .bind(category_id)
        .bind(pattern_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern category: {}\n", e))?;

    Ok(())
}

/// Core: fetch a pattern graph
pub async fn get_pattern_graph_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<String, String> {
    let result: Option<String> = sqlx::query_scalar(
        "SELECT graph_json FROM implementations WHERE pattern_id = ? ORDER BY id LIMIT 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern graph: {}\n", e))?;

    Ok(result.unwrap_or_else(|| "{\"nodes\":[],\"edges\":[],\"args\":[]}".to_string()))
}

/// Core: fetch pattern arg defs
pub async fn get_pattern_args_pool(
    pool: &sqlx::SqlitePool,
    id: i64,
) -> Result<Vec<PatternArgDef>, String> {
    let graph_json = get_pattern_graph_pool(pool, id).await?;
    let graph: Graph = serde_json::from_str(&graph_json).unwrap_or(Graph {
        nodes: vec![],
        edges: vec![],
        args: vec![],
    });
    Ok(graph.args)
}

/// Core: save pattern graph. Derives uid from the parent pattern row.
pub async fn save_pattern_graph_pool(
    pool: &sqlx::SqlitePool,
    id: i64,
    graph_json: String,
) -> Result<(), String> {
    let uid: Option<String> = sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to fetch pattern: {}\n", e))?
        .ok_or_else(|| format!("Pattern {} not found", id))?;

    // Try to update existing implementation, otherwise insert new one
    let updated = sqlx::query(
        "UPDATE implementations SET graph_json = ?, uid = COALESCE(uid, ?) WHERE pattern_id = ?",
    )
    .bind(&graph_json)
    .bind(&uid)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update pattern graph: {}\n", e))?;

    if updated.rows_affected() == 0 {
        sqlx::query("INSERT INTO implementations (pattern_id, uid, graph_json) VALUES (?, ?, ?)")
            .bind(id)
            .bind(&uid)
            .bind(&graph_json)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to create implementation: {}\n", e))?;
    }

    Ok(())
}

/// Core: delete a pattern and its implementations
pub async fn delete_pattern_pool(pool: &sqlx::SqlitePool, id: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM implementations WHERE pattern_id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete implementations: {}", e))?;

    sqlx::query("DELETE FROM patterns WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete pattern: {}", e))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Sync support
// -----------------------------------------------------------------------------

/// Set remote_id after syncing to cloud
pub async fn set_remote_id(pool: &sqlx::SqlitePool, id: i64, remote_id: i64) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET remote_id = ? WHERE id = ?")
        .bind(remote_id.to_string())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern remote_id: {}", e))?;
    Ok(())
}

/// Clear remote_id (e.g., after deleting from cloud)
pub async fn clear_remote_id(pool: &sqlx::SqlitePool, id: i64) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET remote_id = NULL WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to clear pattern remote_id: {}", e))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Community / sharing support
// -----------------------------------------------------------------------------

/// Set published state
pub async fn set_published(
    pool: &sqlx::SqlitePool,
    id: i64,
    published: bool,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE patterns SET is_published = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(published)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to set pattern published state: {}", e))?;
    Ok(())
}

/// Set author_name
pub async fn set_author_name(pool: &sqlx::SqlitePool, id: i64, name: &str) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET author_name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern author_name: {}", e))?;
    Ok(())
}

/// Upsert a community pattern (pulled from cloud). Keyed by remote_id.
pub async fn upsert_community_pattern(
    pool: &sqlx::SqlitePool,
    remote_id: &str,
    uid: &str,
    name: &str,
    description: Option<&str>,
    author_name: Option<&str>,
    is_published: bool,
    created_at: &str,
    updated_at: &str,
) -> Result<i64, String> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO patterns (remote_id, uid, name, description, author_name, is_published, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(remote_id) DO UPDATE SET
           name = excluded.name,
           description = excluded.description,
           author_name = excluded.author_name,
           is_published = excluded.is_published,
           updated_at = excluded.updated_at
         RETURNING id",
    )
    .bind(remote_id)
    .bind(uid)
    .bind(name)
    .bind(description)
    .bind(author_name)
    .bind(is_published)
    .bind(created_at)
    .bind(updated_at)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to upsert community pattern: {}", e))?;

    Ok(id)
}

/// Upsert a community implementation. Keyed by remote_id.
pub async fn upsert_community_implementation(
    pool: &sqlx::SqlitePool,
    remote_id: &str,
    uid: &str,
    pattern_local_id: i64,
    name: Option<&str>,
    graph_json: &str,
) -> Result<i64, String> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO implementations (remote_id, uid, pattern_id, name, graph_json)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(remote_id) DO UPDATE SET
           name = excluded.name,
           graph_json = excluded.graph_json
         RETURNING id",
    )
    .bind(remote_id)
    .bind(uid)
    .bind(pattern_local_id)
    .bind(name)
    .bind(graph_json)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to upsert community implementation: {}", e))?;

    Ok(id)
}

/// Delete community patterns not in the given set of remote_ids
pub async fn delete_stale_community_patterns(
    pool: &sqlx::SqlitePool,
    current_user_uid: &str,
    active_remote_ids: &[String],
) -> Result<u64, String> {
    if active_remote_ids.is_empty() {
        // Delete ALL community patterns (none are active)
        let result = sqlx::query("DELETE FROM patterns WHERE uid != ? AND remote_id IS NOT NULL")
            .bind(current_user_uid)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete stale community patterns: {}", e))?;
        return Ok(result.rows_affected());
    }

    // Build placeholders for IN clause
    let placeholders: Vec<String> = active_remote_ids.iter().map(|_| "?".to_string()).collect();
    let in_clause = placeholders.join(", ");
    let sql = format!(
        "DELETE FROM patterns WHERE uid != ? AND remote_id IS NOT NULL AND remote_id NOT IN ({})",
        in_clause
    );

    let mut query = sqlx::query(&sql).bind(current_user_uid);
    for rid in active_remote_ids {
        query = query.bind(rid);
    }

    let result = query
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete stale community patterns: {}", e))?;

    Ok(result.rows_affected())
}

/// Delete own patterns that exist locally (with a remote_id) but are no longer in the cloud.
/// This handles the case where a pattern was deleted on another device.
pub async fn delete_stale_own_patterns(
    pool: &sqlx::SqlitePool,
    current_user_uid: &str,
    active_remote_ids: &[String],
) -> Result<u64, String> {
    if active_remote_ids.is_empty() {
        // No patterns in cloud — delete all own patterns that have a remote_id
        let result = sqlx::query("DELETE FROM patterns WHERE uid = ? AND remote_id IS NOT NULL")
            .bind(current_user_uid)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete stale own patterns: {}", e))?;
        return Ok(result.rows_affected());
    }

    let placeholders: Vec<String> = active_remote_ids.iter().map(|_| "?".to_string()).collect();
    let in_clause = placeholders.join(", ");
    let sql = format!(
        "DELETE FROM patterns WHERE uid = ? AND remote_id IS NOT NULL AND remote_id NOT IN ({})",
        in_clause
    );

    let mut query = sqlx::query(&sql).bind(current_user_uid);
    for rid in active_remote_ids {
        query = query.bind(rid);
    }

    let result = query
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to delete stale own patterns: {}", e))?;

    Ok(result.rows_affected())
}
