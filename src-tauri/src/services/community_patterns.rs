use crate::database::local::patterns as local_patterns;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::implementations as remote_implementations;
use crate::database::remote::patterns as remote_patterns;
use sqlx::SqlitePool;

pub struct PullStats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

/// Pull the current user's own patterns from Supabase into local SQLite.
///
/// Only adds patterns that don't already exist locally (by remote_id).
/// Local is authoritative — existing patterns are not overwritten.
/// Removes local own patterns whose remote_id is no longer in the cloud (deleted on another device).
pub async fn pull_own_patterns(
    pool: &SqlitePool,
    client: &SupabaseClient,
    access_token: &str,
    current_user_uid: &str,
) -> Result<PullStats, String> {
    let own = remote_patterns::fetch_own_patterns(client, current_user_uid, access_token)
        .await
        .map_err(|e| format!("Failed to fetch own patterns: {}", e))?;

    let mut added = 0usize;
    let active_remote_ids: Vec<String> = own.iter().map(|p| p.id.to_string()).collect();

    for pat in &own {
        let remote_id_str = pat.id.to_string();

        // Skip if already exists locally
        let exists: bool =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns WHERE remote_id = ?")
                .bind(&remote_id_str)
                .fetch_one(pool)
                .await
                .unwrap_or(0)
                > 0;

        if exists {
            continue;
        }

        // Upsert the pattern locally
        let local_id = local_patterns::upsert_community_pattern(
            pool,
            &remote_id_str,
            &pat.uid,
            &pat.name,
            pat.description.as_deref(),
            pat.author_name.as_deref(),
            pat.is_published,
            &pat.created_at,
            &pat.updated_at,
        )
        .await?;

        added += 1;

        // Fetch and upsert the implementation by pattern_id
        match remote_implementations::fetch_implementation_by_pattern(client, pat.id, access_token)
            .await
        {
            Ok(Some(impl_row)) => {
                let impl_remote_id_str = impl_row.id.to_string();
                local_patterns::upsert_community_implementation(
                    pool,
                    &impl_remote_id_str,
                    &impl_row.uid,
                    local_id,
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

    // Delete local own patterns that no longer exist in the cloud
    let removed =
        local_patterns::delete_stale_own_patterns(pool, current_user_uid, &active_remote_ids)
            .await?;

    Ok(PullStats {
        added,
        updated: 0,
        removed: removed as usize,
    })
}

/// Pull all published community patterns from Supabase into local SQLite.
///
/// Skips patterns owned by the current user (those are already local).
/// For each community pattern, also fetches and upserts the default implementation.
/// Removes stale community patterns that are no longer published.
pub async fn pull_community_patterns(
    pool: &SqlitePool,
    client: &SupabaseClient,
    access_token: &str,
    current_user_uid: &str,
) -> Result<PullStats, String> {
    // 1. Fetch all published patterns from Supabase
    let published = remote_patterns::fetch_published_patterns(client, access_token)
        .await
        .map_err(|e| format!("Failed to fetch published patterns: {}", e))?;

    // 2. Filter out own patterns
    let community: Vec<_> = published
        .iter()
        .filter(|p| p.uid != current_user_uid)
        .collect();

    let mut added = 0usize;
    let mut updated = 0usize;
    let active_remote_ids: Vec<String> = community.iter().map(|p| p.id.to_string()).collect();

    // 3. For each community pattern: upsert into local SQLite
    for pat in &community {
        let remote_id_str = pat.id.to_string();

        // Check if this pattern already exists locally
        let existed: bool =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns WHERE remote_id = ?")
                .bind(&remote_id_str)
                .fetch_one(pool)
                .await
                .unwrap_or(0)
                > 0;

        let local_id = local_patterns::upsert_community_pattern(
            pool,
            &remote_id_str,
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

        // 4. Fetch and upsert the implementation by pattern_id
        match remote_implementations::fetch_implementation_by_pattern(client, pat.id, access_token)
            .await
        {
            Ok(Some(impl_row)) => {
                let impl_remote_id_str = impl_row.id.to_string();
                local_patterns::upsert_community_implementation(
                    pool,
                    &impl_remote_id_str,
                    &impl_row.uid,
                    local_id,
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

    // 5. Delete stale community patterns not in fetched set
    let removed =
        local_patterns::delete_stale_community_patterns(pool, current_user_uid, &active_remote_ids)
            .await?;

    Ok(PullStats {
        added,
        updated,
        removed: removed as usize,
    })
}
