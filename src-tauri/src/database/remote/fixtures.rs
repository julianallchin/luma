// Remote CRUD operations for fixtures table

use super::common::{SupabaseClient, SyncError};
use crate::fixtures::models::PatchedFixture;
use serde::Serialize;

/// Payload for upserting a fixture to Supabase
#[derive(Serialize)]
struct FixturePayload<'a> {
    uid: &'a str,
    venue_id: i64, // Cloud venue ID (from venue's remote_id)
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: &'a str,
    model: &'a str,
    mode_name: &'a str,
    fixture_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
}

/// Insert or update a fixture in Supabase
///
/// If the fixture has no remote_id, performs an INSERT and returns the generated cloud ID.
/// If the fixture has a remote_id, performs an UPDATE using that ID.
///
/// Returns the cloud ID (either newly generated or existing remote_id).
///
/// # Arguments
/// * `client` - Supabase client
/// * `fixture` - The fixture to sync
/// * `venue_remote_id` - The cloud ID of the venue (from venue's remote_id)
/// * `access_token` - User's access token
///
/// # FK Resolution
/// The venue must be synced first to get its remote_id, which is used as venue_id in the cloud.
pub async fn upsert_fixture(
    client: &SupabaseClient,
    fixture: &PatchedFixture,
    venue_remote_id: i64,
    access_token: &str,
) -> Result<i64, SyncError> {
    let uid = fixture
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = FixturePayload {
        uid,
        venue_id: venue_remote_id,
        universe: fixture.universe,
        address: fixture.address,
        num_channels: fixture.num_channels,
        manufacturer: &fixture.manufacturer,
        model: &fixture.model,
        mode_name: &fixture.mode_name,
        fixture_path: &fixture.fixture_path,
        label: fixture.label.as_deref(),
        pos_x: fixture.pos_x,
        pos_y: fixture.pos_y,
        pos_z: fixture.pos_z,
        rot_x: fixture.rot_x,
        rot_y: fixture.rot_y,
        rot_z: fixture.rot_z,
    };

    match &fixture.remote_id {
        None => {
            // INSERT: Cloud generates new ID
            client.insert("fixtures", &payload, access_token).await
        }
        Some(remote_id_str) => {
            // UPDATE: Use existing cloud ID
            let remote_id = remote_id_str.parse::<i64>().map_err(|_| {
                SyncError::ParseError(format!("Invalid remote_id: {}", remote_id_str))
            })?;

            client
                .update("fixtures", remote_id, &payload, access_token)
                .await?;
            Ok(remote_id)
        }
    }
}

/// Delete a fixture from Supabase
///
/// Requires the fixture to have a remote_id (must be synced first).
pub async fn delete_fixture(
    client: &SupabaseClient,
    remote_id: i64,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("fixtures", remote_id, access_token).await
}
