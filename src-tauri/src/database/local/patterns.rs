use sqlx::SqliteConnection;

use crate::models::patterns::PatternSummary;

const PATTERN_SUMMARY_SELECT: &str =
    "SELECT pattern.id, pattern.uid, pattern.name, pattern.description,
            pattern.category_name, pattern.created_at, pattern.updated_at,
            pattern.is_verified, pattern.author_name, pattern.forked_from_id
     FROM patterns pattern
     JOIN auth_visible_patterns visible ON visible.pattern_id = pattern.id";

/// Core: fetch a pattern summary
pub async fn get_pattern_pool(pool: &sqlx::SqlitePool, id: &str) -> Result<PatternSummary, String> {
    let row = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} WHERE pattern.id = ?",
        PATTERN_SUMMARY_SELECT
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to fetch pattern: {}\n", e))?;

    Ok(row)
}

/// Core: list patterns
pub async fn list_patterns_pool(pool: &sqlx::SqlitePool) -> Result<Vec<PatternSummary>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to open pattern list: {error}"))?;
    list_patterns_for_connection(&mut connection).await
}

pub(crate) async fn list_patterns_for_connection(
    connection: &mut SqliteConnection,
) -> Result<Vec<PatternSummary>, String> {
    let rows = sqlx::query_as::<_, PatternSummary>(sqlx::AssertSqlSafe(format!(
        "{} ORDER BY pattern.updated_at DESC",
        PATTERN_SUMMARY_SELECT
    )))
    .fetch_all(connection)
    .await
    .map_err(|e| format!("Failed to query patterns: {}\n", e))?;

    Ok(rows)
}

/// Core: update pattern name and description
pub async fn update_pattern_pool(
    pool: &sqlx::SqlitePool,
    id: &str,
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

/// Core: set pattern category by name
pub async fn set_pattern_category_pool(
    pool: &sqlx::SqlitePool,
    pattern_id: &str,
    category_name: Option<&str>,
) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET category_name = ? WHERE id = ?")
        .bind(category_name)
        .bind(pattern_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern category: {}\n", e))?;

    Ok(())
}

/// Delete the catalog half of an authored pattern archive transaction.
///
/// `AuthoredDocuments` must transition every graph projection ledger for this
/// pattern to `archived` in this same transaction before calling this
/// function. The pattern may own arbitrary graph content; only external uses
/// and durable conversations prevent intentional archival.
pub(crate) async fn delete_unused_pattern_for_authored_archive(
    connection: &mut SqliteConnection,
    id: &str,
    owner_user_id: Option<&str>,
) -> Result<(), String> {
    let uid: Option<Option<String>> = sqlx::query_scalar("SELECT uid FROM patterns WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|e| format!("Failed to authorize pattern deletion: {e}"))?;
    let Some(uid) = uid else {
        return Err(format!("Pattern {id} not found"));
    };
    if uid.as_deref() != owner_user_id {
        return Err(format!("Pattern {id} not found"));
    }
    let clips: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_scores WHERE pattern_id = ?")
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| format!("Failed to inspect pattern usage: {e}"))?;
    if clips != 0 {
        return Err(
            "Pattern is used by authored score clips; remove those clips through score history first"
                .into(),
        );
    }
    let threads: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_threads
         WHERE subject_kind = 'pattern' AND subject_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|e| format!("Failed to inspect pattern conversations: {e}"))?;
    if threads != 0 {
        return Err(
            "Pattern still owns durable conversations; delete those conversations first".into(),
        );
    }
    let cues: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cues WHERE pattern_id = ?")
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| format!("Failed to inspect pattern cue usage: {e}"))?;
    if cues != 0 {
        return Err("Pattern is used by venue cues; remove those cues first".into());
    }
    let overrides: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM venue_implementation_overrides WHERE pattern_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|e| format!("Failed to inspect pattern venue routing: {e}"))?;
    if overrides != 0 {
        return Err(
            "Pattern is used by venue implementation routing; remove that routing first".into(),
        );
    }

    sqlx::query("DELETE FROM implementations WHERE pattern_id = ?")
        .bind(id)
        .execute(&mut *connection)
        .await
        .map_err(|e| format!("Failed to delete implementations: {}", e))?;

    let deleted = sqlx::query("DELETE FROM patterns WHERE id = ?")
        .bind(id)
        .execute(&mut *connection)
        .await
        .map_err(|e| format!("Failed to delete pattern: {}", e))?;
    if deleted.rows_affected() != 1 {
        return Err(format!("Pattern {id} not found"));
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Community / sharing support
// -----------------------------------------------------------------------------

/// Set verified state
pub async fn set_verified(pool: &sqlx::SqlitePool, id: &str, verified: bool) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET is_verified = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(verified)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern verified state: {}", e))?;
    Ok(())
}

/// Set author_name
pub async fn set_author_name(pool: &sqlx::SqlitePool, id: &str, name: &str) -> Result<(), String> {
    sqlx::query("UPDATE patterns SET author_name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to set pattern author_name: {}", e))?;
    Ok(())
}
