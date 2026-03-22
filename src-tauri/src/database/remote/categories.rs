// Remote CRUD operations for pattern_categories table

use super::common::{SupabaseClient, SyncError};
use crate::models::patterns::PatternCategory;
use serde::Serialize;

/// Payload for upserting a pattern category to Supabase
#[derive(Serialize)]
struct PatternCategoryPayload<'a> {
    id: &'a str,
    uid: &'a str,
    name: &'a str,
}

/// Upsert a pattern category in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(id) upsert.
pub async fn upsert_category(
    client: &SupabaseClient,
    category: &PatternCategory,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = category
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = PatternCategoryPayload {
        id: &category.id,
        uid,
        name: &category.name,
    };

    client
        .upsert_no_return("pattern_categories", &payload, "id", access_token)
        .await
}

/// Delete a pattern category from Supabase
pub async fn delete_category(
    client: &SupabaseClient,
    id: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("pattern_categories", id, access_token).await
}
