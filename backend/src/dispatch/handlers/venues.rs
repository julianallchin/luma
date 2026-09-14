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
use luma_render::scene_desc::{VenueEnvironment, VenueHaze};

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

/// Join a venue by its share code.
///
/// The membership is written on the server: an ordinary client may not insert
/// a row into a venue it cannot yet see, so `public.join_venue` is a `security
/// definer` function. Nothing is written locally — the membership, the venue
/// and everything in it arrive by download, which is also what makes a retried
/// join harmless. This waits for the venue row so the caller has something to
/// open.
pub async fn join_venue(services: &AppServices, code: String) -> Result<Venue, CommandError> {
    let venue_id = crate::sync::connector::join_venue(&services.state_db.0, &code)
        .await
        .map_err(CommandError::Internal)?;
    let deadline = std::time::Instant::now() + JOIN_DOWNLOAD_TIMEOUT;
    loop {
        if let Ok(mut access) =
            VenueAccess::<Read>::read(&services.db.0, VenueResource::Venue(&venue_id)).await
        {
            if let Ok(venue) = venues_db::get_venue(&mut access).await {
                return Ok(venue);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(CommandError::Internal(format!(
                "joined {venue_id}, but it has not arrived on this device yet"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// How long a join waits for the venue it just joined to download. Generous:
/// the alternative is telling the user the join failed when it did not.
const JOIN_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Leave a venue: delete the local membership row. The upload carries the
/// delete to the server, and the venue's rows leave on the next checkpoint
/// because they are no longer in this account's bucket.
///
/// Ordering is load-bearing — ownership check, drop the read transaction, then
/// the write.
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

    let principal = services.require_session().await?;
    venues_db::remove_current_venue_membership(pool, &venue_id, &principal).await?;
    Ok(())
}

/// What kind of room this venue is, and how far up its one dial is.
///
/// Venue truth at the tier of the name, and synced with the venue since the
/// 20260905 migration: take the write lease, write the column, commit — the
/// ordinary venue dirtiness trigger carries the edit up. One write for both
/// modes because the value is one closed enum; see
/// [`venues_db::set_environment`].
pub async fn set_venue_environment(
    services: &AppServices,
    venue_id: String,
    environment: VenueEnvironment,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    venues_db::set_environment(&mut access, environment).await?;
    Ok(access.commit().await?)
}

/// How hazy this venue is, and what its haze looks like.
///
/// Venue truth beside the environment, and synced the same way: write lease,
/// column, commit. One write for the whole look; see [`venues_db::set_haze`].
pub async fn set_venue_haze(
    services: &AppServices,
    venue_id: String,
    haze: VenueHaze,
) -> Result<(), CommandError> {
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&venue_id)).await?;
    venues_db::set_haze(&mut access, haze).await?;
    Ok(access.commit().await?)
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
