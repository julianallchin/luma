//! Venues: the local library's rows, plus the two cloud round-trips
//! (share-code publish, membership join/leave) that keep them reachable from
//! another machine.

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource, Write};
use crate::database::local::venues as venues_db;
use crate::database::remote::common::SupabaseClient;
use crate::dispatch::{AppServices, CommandError};
use crate::models::venues::Venue;

/// Every venue in the local library, owned and joined alike. Read-only and
/// unscoped — the per-venue authorization gate (`VenueAccess`) guards the
/// *contents* of a venue, not its existence in this list.
pub async fn list_venues(services: &AppServices) -> Result<Vec<Venue>, CommandError> {
    Ok(venues_db::list_venues(&services.db.0).await?)
}

/// Note the argument is `id`, not `venue_id` — the wire contract, inconsistent
/// with the rest of the surface but load-bearing.
pub async fn get_venue(services: &AppServices, id: String) -> Result<Venue, CommandError> {
    let mut access = VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&id)).await?;
    Ok(venues_db::get_venue(&mut access).await?)
}

/// The one venue write that does not open a `VenueAccess`: there is no venue
/// yet to take a lease on.
pub async fn create_venue(
    services: &AppServices,
    name: String,
    description: Option<String>,
) -> Result<Venue, CommandError> {
    Ok(venues_db::create_venue(&services.db.0, name, description).await?)
}

/// Full replace, not a patch — an empty `name` blanks the venue's name.
pub async fn update_venue(
    services: &AppServices,
    id: String,
    name: String,
    description: Option<String>,
) -> Result<Venue, CommandError> {
    let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&id)).await?;
    let venue = venues_db::update_venue(&mut access, name, description).await?;
    access.commit().await?;
    Ok(venue)
}

/// Owner-side delete. No cloud call — contrast [`leave_venue`].
pub async fn delete_venue(services: &AppServices, id: String) -> Result<(), CommandError> {
    let mut access = VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&id)).await?;
    venues_db::delete_venue(&mut access).await?;
    Ok(access.commit().await?)
}

/// Generate (or return existing) share code for a venue. Owner only.
///
/// Idempotent: an existing code is returned without touching the cloud. The
/// cloud publish of a *new* code is best-effort, so a local code can exist that
/// the cloud never learned about — [`join_venue`] would reject it.
pub async fn get_or_create_share_code(
    services: &AppServices,
    venue_id: String,
) -> Result<String, CommandError> {
    let auth = current_auth(services).await?;

    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    let venue = venues_db::get_venue(&mut access).await?;

    if venue.uid.as_deref() != Some(auth.principal.user_id.as_str()) {
        return Err(CommandError::Unauthorized(
            "Only the venue owner can generate a share code".to_string(),
        ));
    }

    if let Some(code) = &venue.share_code {
        return Ok(code.clone());
    }

    let code = generate_share_code();
    venues_db::set_share_code(&mut access, &code).await?;
    access.commit().await?;

    #[derive(serde::Serialize)]
    struct ShareCodePayload<'a> {
        share_code: &'a str,
    }

    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    if let Err(e) = client
        .update(
            "venues",
            &venue_id,
            &ShareCodePayload { share_code: &code },
            &auth.access_token,
        )
        .await
    {
        eprintln!("[get_or_create_share_code] Failed to sync share_code to cloud: {e}");
    }

    Ok(code)
}

/// Join a venue by share code. Creates a local venue with `role='member'`.
///
/// Fixtures, groups and tracks are not pulled here — the frontend fires
/// `sync_full` after joining.
pub async fn join_venue(services: &AppServices, code: String) -> Result<Venue, CommandError> {
    let auth = current_auth(services).await?;

    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let venue_row = client
        .rpc::<RemoteVenueRow>(
            "join_venue_by_code",
            &JoinByCodeParams { code: &code },
            &auth.access_token,
        )
        .await
        .map_err(|e| CommandError::Internal(format!("Failed to join venue: {e}")))?;

    let owner_uid = venue_row
        .uid
        .as_deref()
        .ok_or_else(|| CommandError::Internal("Venue has no owner uid".to_string()))?;

    // `uid` is the OWNER's, not the joiner's — `is_owner()` is decided by
    // `role`, and the cloud UUID becomes the local id directly.
    Ok(venues_db::insert_joined_venue(
        &services.db.0,
        &venue_row.id,
        owner_uid,
        &venue_row.name,
        venue_row.description.as_deref(),
        None,
        &auth.principal.user_id,
    )
    .await?)
}

/// Leave a venue: remove the membership locally and in the cloud. The venue row
/// survives if other memberships remain.
///
/// Ordering is load-bearing — ownership check, drop the read transaction, then
/// the network call, so no transaction is held across IO. The cloud delete is
/// best-effort, so a local-left/cloud-still-member state is tolerated.
pub async fn leave_venue(services: &AppServices, venue_id: String) -> Result<(), CommandError> {
    let pool = &services.db.0;
    let mut read_access = VenueAccess::<Read>::read(pool, VenueResource::Venue(&venue_id)).await?;
    let venue = venues_db::get_venue(&mut read_access).await?;
    if venue.is_owner() {
        return Err(CommandError::Invalid(
            "Cannot leave a venue you own".to_string(),
        ));
    }
    drop(read_access);

    let auth = current_auth(services).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    if let Err(e) = client
        .delete_by_filter(
            "venue_members",
            &format!(
                "venue_id=eq.{}&user_id=eq.{}",
                venue_id, auth.principal.user_id
            ),
            &auth.access_token,
        )
        .await
    {
        eprintln!("[leave_venue] Failed to remove cloud membership: {e}");
    }

    venues_db::remove_current_venue_membership(pool, &venue_id, &auth.principal.user_id).await?;

    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// The verified session, including its access token.
///
/// A third notion of "current user" alongside `admitted_principal()` and
/// `session_user_id()` — and the only one of the three that yields a token, so
/// the four cloud-touching venue commands cannot use either accessor. It also
/// means `fixture_principal` does not override it: a headless fixture has no
/// Supabase token to speak with.
async fn current_auth(services: &AppServices) -> Result<auth::VerifiedAuth, CommandError> {
    auth::get_current_auth(&services.state_db.0)
        .await?
        .ok_or_else(|| CommandError::Unauthorized("Not authenticated".to_string()))
}

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
