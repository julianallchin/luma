use crate::database::local::patterns as local_patterns;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::queries as remote_queries;
use sqlx::SqlitePool;

pub struct PullStats {
    pub added: usize,
    pub updated: usize,
}

/// Pull the current user's own patterns from Supabase into local SQLite.
///
/// Purely additive: inserts new patterns, updates existing ones. Never deletes.
/// Local is authoritative -- if a pattern exists locally, it stays.
pub async fn pull_own_patterns(
    pool: &SqlitePool,
    client: &SupabaseClient,
    access_token: &str,
    current_user_uid: &str,
) -> Result<PullStats, String> {
    let own = remote_queries::fetch_own_patterns(client, current_user_uid, access_token)
        .await
        .map_err(|e| format!("Failed to fetch own patterns: {}", e))?;

    let mut added = 0usize;
    let mut updated = 0usize;

    for pat in &own {
        let existed: bool =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns WHERE id = ?")
                .bind(&pat.id)
                .fetch_one(pool)
                .await
                .unwrap_or(0)
                > 0;

        let local_id = local_patterns::upsert_community_pattern(
            pool,
            &pat.id,
            &pat.uid,
            &pat.name,
            pat.description.as_deref(),
            pat.author_name.as_deref(),
            pat.is_published,
            &pat.created_at,
            &pat.updated_at,
        )
        .await?;

        if existed {
            updated += 1;
        } else {
            added += 1;
        }

        // Upsert implementation (updates graph_json if changed)
        match remote_queries::fetch_implementation_by_pattern(client, &pat.id, access_token).await {
            Ok(Some(impl_row)) => {
                local_patterns::upsert_community_implementation(
                    pool,
                    &impl_row.id,
                    &impl_row.uid,
                    &local_id,
                    impl_row.name.as_deref(),
                    &impl_row.graph_json,
                )
                .await?;
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "[pull_own_patterns] Failed to fetch implementation for pattern {}: {}",
                    pat.id, e
                );
            }
        }
    }

    Ok(PullStats { added, updated })
}

/// Pull all published community patterns from Supabase into local SQLite.
///
/// Purely additive: inserts new patterns, updates existing ones. Never deletes.
/// Skips patterns owned by the current user (those come from pull_own_patterns).
pub async fn pull_community_patterns(
    pool: &SqlitePool,
    client: &SupabaseClient,
    access_token: &str,
    current_user_uid: &str,
) -> Result<PullStats, String> {
    let published = remote_queries::fetch_published_patterns(client, access_token)
        .await
        .map_err(|e| format!("Failed to fetch published patterns: {}", e))?;

    let community: Vec<_> = published
        .iter()
        .filter(|p| p.uid != current_user_uid)
        .collect();

    let mut added = 0usize;
    let mut updated = 0usize;

    for pat in &community {
        let existed: bool =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns WHERE id = ?")
                .bind(&pat.id)
                .fetch_one(pool)
                .await
                .unwrap_or(0)
                > 0;

        let local_id = local_patterns::upsert_community_pattern(
            pool,
            &pat.id,
            &pat.uid,
            &pat.name,
            pat.description.as_deref(),
            pat.author_name.as_deref(),
            pat.is_published,
            &pat.created_at,
            &pat.updated_at,
        )
        .await?;

        if existed {
            updated += 1;
        } else {
            added += 1;
        }

        // Upsert implementation
        match remote_queries::fetch_implementation_by_pattern(client, &pat.id, access_token).await {
            Ok(Some(impl_row)) => {
                local_patterns::upsert_community_implementation(
                    pool,
                    &impl_row.id,
                    &impl_row.uid,
                    &local_id,
                    impl_row.name.as_deref(),
                    &impl_row.graph_json,
                )
                .await?;
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "[community_patterns] Failed to fetch implementation for pattern {}: {}",
                    pat.id, e
                );
            }
        }
    }

    Ok(PullStats { added, updated })
}
