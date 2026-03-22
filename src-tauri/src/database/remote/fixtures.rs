// Remote CRUD operations for fixtures table

use super::common::{SupabaseClient, SyncError};
use crate::models::fixtures::PatchedFixture;
use serde::Serialize;

/// Payload for upserting a fixture to Supabase
#[derive(Serialize)]
struct FixturePayload<'a> {
    id: &'a str,
    uid: &'a str,
    venue_id: &'a str,
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

/// Upsert a fixture in Supabase (idempotent).
///
/// The local UUID is sent as the cloud `id`. Uses ON CONFLICT(id) upsert.
/// The venue_id is taken directly from the fixture (already a UUID).
pub async fn upsert_fixture(
    client: &SupabaseClient,
    fixture: &PatchedFixture,
    access_token: &str,
) -> Result<(), SyncError> {
    let uid = fixture
        .uid
        .as_ref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))?;

    let payload = FixturePayload {
        id: &fixture.id,
        uid,
        venue_id: &fixture.venue_id,
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

    client
        .upsert_no_return("fixtures", &payload, "id", access_token)
        .await
}

/// Delete a fixture from Supabase
pub async fn delete_fixture(
    client: &SupabaseClient,
    id: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete("fixtures", id, access_token).await
}
