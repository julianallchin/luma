// Remote CRUD operations for fixture_groups and fixture_group_members

use super::common::{SupabaseClient, SyncError};
use serde::Serialize;

#[derive(Serialize)]
struct GroupPayload<'a> {
    uid: &'a str,
    venue_id: i64,
    name: Option<&'a str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    movement_config: Option<&'a str>,
    display_order: i64,
}

#[derive(Serialize)]
struct GroupMemberPayload {
    fixture_id: i64,
    group_id: i64,
    display_order: i64,
}

/// Upsert a fixture group to Supabase
pub async fn upsert_group(
    client: &SupabaseClient,
    remote_id: Option<&str>,
    uid: &str,
    venue_remote_id: i64,
    name: Option<&str>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
    movement_config: Option<&str>,
    display_order: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let payload = GroupPayload {
        uid,
        venue_id: venue_remote_id,
        name,
        axis_lr,
        axis_fb,
        axis_ab,
        movement_config,
        display_order,
    };

    match remote_id {
        None => {
            client
                .insert("fixture_groups", &payload, access_token)
                .await
        }
        Some(rid_str) => {
            let rid = rid_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid group remote_id: {}", rid_str))
            })?;
            client
                .update("fixture_groups", rid, &payload, access_token)
                .await?;
            Ok(rid)
        }
    }
}

/// Sync group members: delete all for this group, re-insert
pub async fn sync_group_members(
    client: &SupabaseClient,
    group_remote_id: i64,
    members: &[(i64, i64)], // (fixture_remote_id, display_order)
    access_token: &str,
) -> Result<(), SyncError> {
    // Delete existing members for this group
    client
        .delete_by_filter(
            "fixture_group_members",
            &format!("group_id=eq.{}", group_remote_id),
            access_token,
        )
        .await?;

    if members.is_empty() {
        return Ok(());
    }

    // Insert new members
    let payloads: Vec<GroupMemberPayload> = members
        .iter()
        .map(|(fixture_remote_id, display_order)| GroupMemberPayload {
            fixture_id: *fixture_remote_id,
            group_id: group_remote_id,
            display_order: *display_order,
        })
        .collect();

    client
        .insert_batch("fixture_group_members", &payloads, access_token)
        .await?;

    Ok(())
}
