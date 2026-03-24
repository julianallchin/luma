//! Tauri commands for venue operations

use tauri::State;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::venues as db;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
use crate::models::venues::Venue;

#[tauri::command]
pub async fn get_venue(db: State<'_, Db>, id: String) -> Result<Venue, String> {
    db::get_venue(&db.0, &id).await
}

#[tauri::command]
pub async fn list_venues(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
) -> Result<Vec<Venue>, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    match uid {
        Some(uid) => db::list_venues_for_user(&db.0, &uid).await,
        None => db::list_venues(&db.0).await,
    }
}

#[tauri::command]
pub async fn create_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    let uid = auth::get_current_user_id(&state_db.0).await?;
    db::create_venue(&db.0, name, description, uid).await
}

#[tauri::command]
pub async fn update_venue(
    db: State<'_, Db>,
    id: String,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    db::update_venue(&db.0, &id, name, description).await
}

#[tauri::command]
pub async fn delete_venue(db: State<'_, Db>, id: String) -> Result<(), String> {
    db::delete_venue(&db.0, &id).await
}

/// Generate (or return existing) share code for a venue. Owner only.
#[tauri::command]
pub async fn get_or_create_share_code(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: String,
) -> Result<String, String> {
    let current_uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;

    let venue = db::get_venue(&db.0, &venue_id).await?;

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
    db::set_share_code(&db.0, &venue_id, &code).await?;

    // Sync the share_code to Supabase
    let access_token = auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
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
    let access_token = auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;

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

    let current_uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;

    // Check if this user already has this venue locally (by cloud id)
    if let Ok(existing) = db::get_venue(&db.0, &venue_row.id).await {
        if existing.is_owner() {
            return Err("You already own this venue".to_string());
        }
        // Ensure membership record exists (idempotent)
        db::add_venue_membership(&db.0, &venue_row.id, &current_uid).await?;
        return Ok(existing);
    }

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
    )
    .await?;

    // Record membership for the current user
    db::add_venue_membership(&db.0, &venue_row.id, &current_uid).await?;

    // Pull venue fixtures and groups
    if let Err(e) =
        crate::services::cloud_pull::pull_venue_fixtures(&db.0, &client, &venue.id, &access_token)
            .await
    {
        eprintln!("[join_venue] Failed to pull fixtures: {}", e);
    }

    if let Err(e) =
        crate::services::cloud_pull::pull_venue_groups(&db.0, &client, &venue.id, &access_token)
            .await
    {
        eprintln!("[join_venue] Failed to pull groups: {}", e);
    }

    Ok(venue)
}

/// Leave a venue (remove membership, delete venue row only if no memberships remain)
#[tauri::command]
pub async fn leave_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: String,
) -> Result<(), String> {
    let venue = db::get_venue(&db.0, &venue_id).await?;

    if venue.is_owner() {
        return Err("Cannot leave a venue you own".to_string());
    }

    // Remove membership from Supabase
    let access_token = auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    let current_uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;

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

    // Remove local membership
    db::remove_venue_membership(&db.0, &venue_id, &current_uid).await?;

    // Delete venue row only if no memberships remain and it's not owned
    let remaining = db::count_venue_memberships(&db.0, &venue_id).await?;
    if remaining == 0 {
        db::delete_venue(&db.0, &venue_id).await?;
    }

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
