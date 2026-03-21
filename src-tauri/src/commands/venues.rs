//! Tauri commands for venue operations

use tauri::State;

use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::local::venues as db;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
use crate::models::venues::Venue;

const SUPABASE_URL: &str = "https://smuuycypmsutwrkpctws.supabase.co";
const SUPABASE_ANON_KEY: &str = "sb_publishable_V8JRQkGliRYDAiGghjUrmQ_w8fpfjRb";

#[tauri::command]
pub async fn get_venue(db: State<'_, Db>, id: i64) -> Result<Venue, String> {
    db::get_venue(&db.0, id).await
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
    id: i64,
    name: String,
    description: Option<String>,
) -> Result<Venue, String> {
    db::update_venue(&db.0, id, name, description).await
}

#[tauri::command]
pub async fn delete_venue(db: State<'_, Db>, id: i64) -> Result<(), String> {
    db::delete_venue(&db.0, id).await
}

/// Generate (or return existing) share code for a venue. Owner only.
#[tauri::command]
pub async fn get_or_create_share_code(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: i64,
) -> Result<String, String> {
    let current_uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;

    let venue = db::get_venue(&db.0, venue_id).await?;

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
    db::set_share_code(&db.0, venue_id, &code).await?;

    // Sync the share_code to Supabase if the venue has been synced
    if let Some(remote_id_str) = &venue.remote_id {
        if let Ok(remote_id) = remote_id_str.parse::<i64>() {
            let access_token = auth::get_current_access_token(&state_db.0)
                .await?
                .ok_or_else(|| "Not authenticated".to_string())?;
            let client =
                SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

            #[derive(serde::Serialize)]
            struct ShareCodePayload<'a> {
                share_code: &'a str,
            }

            let _ = client
                .update(
                    "venues",
                    remote_id,
                    &ShareCodePayload { share_code: &code },
                    &access_token,
                )
                .await;
        }
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

    // Check if we already have this venue locally
    if let Some(existing) = db::get_venue_by_remote_id(&db.0, &venue_row.id.to_string()).await? {
        return Ok(existing);
    }

    // Insert locally as a member
    let venue = db::insert_joined_venue(
        &db.0,
        venue_row.id,
        &venue_row.uid,
        &venue_row.name,
        venue_row.description.as_deref(),
        venue_row.share_code.as_deref(),
    )
    .await?;

    // Pull venue fixtures
    if let Err(e) = crate::services::cloud_pull::pull_venue_fixtures(
        &db.0,
        &client,
        venue_row.id,
        venue.id,
        &access_token,
    )
    .await
    {
        eprintln!("[join_venue] Failed to pull fixtures: {}", e);
    }

    Ok(venue)
}

/// Leave a venue (remove membership and delete local data)
#[tauri::command]
pub async fn leave_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: i64,
) -> Result<(), String> {
    let venue = db::get_venue(&db.0, venue_id).await?;

    if venue.role != "member" {
        return Err("Cannot leave a venue you own".to_string());
    }

    // Remove membership from Supabase
    if let Some(remote_id_str) = &venue.remote_id {
        let access_token = auth::get_current_access_token(&state_db.0)
            .await?
            .ok_or_else(|| "Not authenticated".to_string())?;
        let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

        let current_uid = auth::get_current_user_id(&state_db.0)
            .await?
            .ok_or_else(|| "Not authenticated".to_string())?;

        let _ = client
            .delete_by_filter(
                "venue_members",
                &format!("venue_id=eq.{}&user_id=eq.{}", remote_id_str, current_uid),
                &access_token,
            )
            .await;
    }

    // Delete locally (cascades to fixtures, groups, scores, etc.)
    db::delete_venue(&db.0, venue_id).await
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
    id: i64,
    uid: String,
    name: String,
    description: Option<String>,
    share_code: Option<String>,
}

/// Params for join_venue_by_code RPC
#[derive(serde::Serialize)]
struct JoinByCodeParams<'a> {
    code: &'a str,
}
