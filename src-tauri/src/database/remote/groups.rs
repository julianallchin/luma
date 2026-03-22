// Remote CRUD operations for fixture_groups and fixture_group_members

use super::common::{SupabaseClient, SyncError};
use serde::Serialize;

#[derive(Serialize)]
struct GroupPayload<'a> {
    id: &'a str,
    uid: &'a str,
    venue_id: &'a str,
    name: Option<&'a str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    movement_config: Option<&'a str>,
    display_order: i64,
}

#[derive(Serialize)]
struct GroupMemberPayload<'a> {
    fixture_id: &'a str,
    group_id: &'a str,
    display_order: i64,
}

/// Upsert a fixture group to Supabase.
/// Uses ON CONFLICT(venue_id, name) so duplicate syncs are idempotent.
pub async fn upsert_group(
    client: &SupabaseClient,
    id: &str,
    uid: &str,
    venue_id: &str,
    name: Option<&str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    movement_config: Option<&str>,
    display_order: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    let payload = GroupPayload {
        id,
        uid,
        venue_id,
        name,
        axis_lr,
        axis_fb,
        axis_ab,
        movement_config,
        display_order,
    };

    client
        .upsert_no_return("fixture_groups", &payload, "id", access_token)
        .await
}

/// Sync group members: upsert all members for this group (idempotent).
/// Does not delete cloud members that are absent locally.
pub async fn sync_group_members(
    client: &SupabaseClient,
    group_id: &str,
    members: &[(String, i64)], // (fixture_id UUID, display_order)
    access_token: &str,
) -> Result<(), SyncError> {
    if members.is_empty() {
        return Ok(());
    }

    let payloads: Vec<GroupMemberPayload> = members
        .iter()
        .map(|(fixture_id, display_order)| GroupMemberPayload {
            fixture_id,
            group_id,
            display_order: *display_order,
        })
        .collect();

    client
        .upsert_batch_no_return(
            "fixture_group_members",
            &payloads,
            "fixture_id,group_id",
            access_token,
        )
        .await
}
