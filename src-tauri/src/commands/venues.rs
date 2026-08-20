//! Tauri commands for venue operations

use tauri::State;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as db;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
use crate::models::venues::Venue;

#[tauri::command]
pub async fn get_venue(db: State<'_, Db>, id: String) -> Result<Venue, String> {
    let mut access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&id)).await?;
    db::get_venue(&mut access).await
}

#[tauri::command]
pub async fn create_venue(
    db: State<'_, Db>,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    db::create_venue(&db.0, name, description).await
}

#[tauri::command]
pub async fn update_venue(
    db: State<'_, Db>,
    id: String,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&id)).await?;
    let venue = db::update_venue(&mut access, name, description).await?;
    access.commit().await?;
    Ok(venue)
}

#[tauri::command]
pub async fn delete_venue(db: State<'_, Db>, id: String) -> Result<(), String> {
    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&id)).await?;
    db::delete_venue(&mut access).await?;
    access.commit().await
}

/// Generate (or return existing) share code for a venue. Owner only.
#[tauri::command]
pub async fn get_or_create_share_code(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: String,
) -> Result<String, String> {
    let auth = auth::get_current_auth(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let current_uid = auth.principal.user_id;
    let access_token = auth.access_token;

    let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = db::get_venue(&mut access).await?;

    // Only the owner can generate a share code
    if venue.uid.as_deref() != Some(&current_uid) {
        return Err("Only the venue owner can generate a share code".to_string());
    }

    // Return existing code if already generated
    if let Some(code) = &venue.share_code {
        return Ok(code.clone());
    }

    // Generate a new 8-char base62 code
    let code = generate_share_code();
    db::set_share_code(&mut access, &code).await?;
    access.commit().await?;

    // Sync the share_code to Supabase
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    #[derive(serde::Serialize)]
    struct ShareCodePayload<'a> {
        share_code: &'a str,
    }

    if let Err(e) = client
        .update(
            "venues",
            &venue_id,
            &ShareCodePayload { share_code: &code },
            &access_token,
        )
        .await
    {
        eprintln!(
            "[get_or_create_share_code] Failed to sync share_code to cloud: {}",
            e
        );
    }

    Ok(code)
}

/// Join a venue by share code. Creates a local venue with role='member'.
#[tauri::command]
pub async fn join_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    code: String,
) -> Result<Venue, String> {
    let auth = auth::get_current_auth(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let access_token = auth.access_token;
    let current_uid = auth.principal.user_id;

    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    // Call the Supabase RPC to join by share code
    let venue_row = client
        .rpc::<RemoteVenueRow>(
            "join_venue_by_code",
            &JoinByCodeParams { code: &code },
            &access_token,
        )
        .await
        .map_err(|e| format!("Failed to join venue: {}", e))?;

    // Get the venue owner's uid from the RPC response
    let owner_uid = venue_row
        .uid
        .as_deref()
        .ok_or_else(|| "Venue has no owner uid".to_string())?;

    // Insert locally as a member — uid is the OWNER's uid (not the joiner's)
    // The cloud UUID becomes the local id directly
    let venue = db::insert_joined_venue(
        &db.0,
        &venue_row.id,
        owner_uid,
        &venue_row.name,
        venue_row.description.as_deref(),
        None,
        &current_uid,
    )
    .await?;

    // Fixtures and groups are pulled by the sync_full call the frontend
    // triggers after join — no need for a separate pull here.

    Ok(venue)
}

/// Leave a venue (remove membership, delete venue row only if no memberships remain)
#[tauri::command]
pub async fn leave_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: String,
) -> Result<(), String> {
    let mut read_access = VenueAccess::<Read>::read(&db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = db::get_venue(&mut read_access).await?;

    if venue.is_owner() {
        return Err("Cannot leave a venue you own".to_string());
    }
    drop(read_access);

    // Remove membership from Supabase
    let auth = auth::get_current_auth(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let access_token = auth.access_token;
    let current_uid = auth.principal.user_id;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    if let Err(e) = client
        .delete_by_filter(
            "venue_members",
            &format!("venue_id=eq.{}&user_id=eq.{}", venue_id, current_uid),
            &access_token,
        )
        .await
    {
        eprintln!("[leave_venue] Failed to remove cloud membership: {}", e);
    }

    db::remove_current_venue_membership(&db.0, &venue_id, &current_uid).await?;

    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// Generate an 8-character base62 share code (a-z, A-Z, 0-9)
fn generate_share_code() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

/// Venue row returned from Supabase RPC
#[derive(serde::Deserialize)]
struct RemoteVenueRow {
    id: String,
    uid: Option<String>,
    name: String,
    description: Option<String>,
}

/// Params for join_venue_by_code RPC
#[derive(serde::Serialize)]
struct JoinByCodeParams<'a> {
    code: &'a str,
}
