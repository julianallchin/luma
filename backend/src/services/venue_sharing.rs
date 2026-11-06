//! Inviting someone into a venue, and seeing who is in it.
//!
//! The owner's side is ordinary venue writes: the share code is a column on
//! `venues` and access is a row in `venue_members`, both written locally under
//! the owner's write lease and carried up by the upload queue. Postgres
//! row-level security agrees: only the owner may update the venue row, and the
//! owner may delete anyone's membership.
//!
//! The joiner's side cannot be a local write. Their device has never seen the
//! venue, and the server will not let an ordinary client insert a membership
//! into a venue it cannot read, so redeeming a code is the `security definer`
//! function `public.join_venue`. The membership, the venue and everything in
//! it then arrive by download, like any other rows.

use std::time::{Duration, Instant};

use rand::Rng;
use sqlx::SqlitePool;

use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::database::local::venues as venues_db;
use crate::database::remote::postgrest::Postgrest;
use crate::dispatch::CommandError;
use crate::models::venues::{
    display_share_code, normalize_share_code, Venue, VenueMember, SHARE_CODE_ALPHABET,
    SHARE_CODE_LEN,
};
use crate::sync::error::SyncError;

/// How long a join waits for the venue it redeemed to download. Generous: the
/// alternative is telling someone the join failed when it did not.
pub const ARRIVAL: Duration = Duration::from_secs(30);

/// Why a server did not redeem a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No venue has this code — it was never issued, or it was revoked.
    UnknownCode,
    /// The server could not be reached.
    Offline(String),
    /// There is no session to redeem it with.
    SignedOut(String),
    /// Anything else the server said.
    Failed(String),
}

/// Where share codes are redeemed.
///
/// A seam rather than a direct call so a process with no server — a test, the
/// UI harness — can stand in a [`LocalShareCodes`] and still exercise the
/// whole join, download wait included.
#[async_trait::async_trait]
pub trait ShareCodeServer: Send + Sync {
    /// Admit the signed-in account to the venue holding `code`, returning its
    /// id. An owner redeeming their own code is admitted already and gets the
    /// id back with nothing written.
    async fn redeem(&self, code: &str) -> Result<String, Refusal>;
}

/// The production server: `public.join_venue` over PostgREST, as whoever the
/// state database says is signed in.
pub struct SupabaseShareCodes {
    postgrest: Postgrest,
    state_pool: SqlitePool,
}

impl SupabaseShareCodes {
    #[must_use]
    pub fn new(state_pool: SqlitePool) -> Self {
        Self {
            postgrest: Postgrest::from_config(),
            state_pool,
        }
    }
}

#[async_trait::async_trait]
impl ShareCodeServer for SupabaseShareCodes {
    async fn redeem(&self, code: &str) -> Result<String, Refusal> {
        let token = crate::database::local::auth::get_current_access_token(&self.state_pool)
            .await
            .map_err(|error| match error {
                crate::database::local::auth::AuthError::SessionRevoked => {
                    Refusal::SignedOut(error.to_string())
                }
                // Refreshing an expired token is the first network call a
                // join makes, so an unreachable auth server is "offline" too.
                crate::database::local::auth::AuthError::Other(message) => {
                    Refusal::Offline(message)
                }
            })?
            .ok_or_else(|| Refusal::SignedOut("no signed-in session".into()))?;
        redeem_with_token(&self.postgrest, &token, code).await
    }
}

/// `public.join_venue(code)` with an access token in hand.
///
/// # Errors
///
/// The server's answer as a [`Refusal`]: the function raises `no venue for
/// share code` for a code no venue holds.
pub async fn redeem_with_token(
    postgrest: &Postgrest,
    token: &str,
    code: &str,
) -> Result<String, Refusal> {
    postgrest
        .rpc::<String>("join_venue", &serde_json::json!({ "code": code }), token)
        .await
        .map_err(refusal)
}

fn refusal(error: SyncError) -> Refusal {
    match error {
        SyncError::Network(message) => Refusal::Offline(message),
        SyncError::Api { message, .. } if message.contains("no venue for share code") => {
            Refusal::UnknownCode
        }
        SyncError::Api {
            status: 401,
            message,
        } => Refusal::SignedOut(message),
        SyncError::Api { status, .. } if status >= 500 || status == 408 || status == 429 => {
            Refusal::Offline(error.to_string())
        }
        _ => Refusal::Failed(error.to_string()),
    }
}

/// A server whose database is this one: a code resolves against the venues
/// on this device, and the membership it grants is written as a download
/// would write it. For the UI harness and tests, which have no server; the
/// fixture seeds the venue another account owns, code and all.
pub struct LocalShareCodes {
    pool: SqlitePool,
}

impl LocalShareCodes {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ShareCodeServer for LocalShareCodes {
    async fn redeem(&self, code: &str) -> Result<String, Refusal> {
        let failed = |error: sqlx::Error| Refusal::Failed(error.to_string());
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let principal: Option<String> =
            sqlx::query_scalar("SELECT active_uid FROM auth_write_admission WHERE singleton = 1")
                .fetch_optional(&mut *transaction)
                .await
                .map_err(failed)?
                .flatten();
        let principal =
            principal.ok_or_else(|| Refusal::SignedOut("no signed-in session".into()))?;
        let venue: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT id, uid FROM venues WHERE share_code = ?")
                .bind(code)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(failed)?;
        let (venue, owner) = venue.ok_or(Refusal::UnknownCode)?;
        if owner.as_deref() == Some(principal.as_str()) {
            return Ok(venue);
        }
        crate::database::local::write_admission::enter_remote_writes(&mut transaction)
            .await
            .map_err(Refusal::Failed)?;
        sqlx::query(
            "INSERT INTO venue_members (id, uid, venue_id, role) VALUES (?, ?, ?, 'member')
             ON CONFLICT DO NOTHING",
        )
        .bind(format!("{venue}:{principal}"))
        .bind(&principal)
        .bind(&venue)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        crate::database::local::write_admission::leave_remote_writes(&mut transaction)
            .await
            .map_err(Refusal::Failed)?;
        transaction.commit().await.map_err(failed)?;
        Ok(venue)
    }
}

/// A fresh code from [`SHARE_CODE_ALPHABET`].
fn generate_share_code() -> String {
    let mut rng = rand::thread_rng();
    (0..SHARE_CODE_LEN)
        .map(|_| char::from(SHARE_CODE_ALPHABET[rng.gen_range(0..SHARE_CODE_ALPHABET.len())]))
        .collect()
}

/// Give the venue a new share code, replacing the one it had: the old code
/// stops working and nobody who already joined loses access.
///
/// Owner only. The code reaches the server with the next upload, so a code
/// minted offline cannot be redeemed until this device reconnects.
///
/// # Errors
///
/// If the caller does not own the venue, or the write fails.
pub async fn rotate_share_code(pool: &SqlitePool, venue_id: &str) -> Result<String, CommandError> {
    // A collision is one in 10¹¹ per venue on the device; three draws is
    // belt and braces, not a loop that expects to run.
    for _ in 0..3 {
        let code = generate_share_code();
        let mut access = owner_write(pool, venue_id).await?;
        match venues_db::set_share_code(&mut access, Some(&code)).await {
            Ok(()) => {
                access.commit().await?;
                return Ok(code);
            }
            Err(error) if error == venues_db::SHARE_CODE_TAKEN => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(CommandError::Internal(
        "Could not find an unused share code".into(),
    ))
}

/// Clear the venue's share code. Nobody can join with the old one; everyone
/// who already joined keeps access until the owner removes them.
///
/// # Errors
///
/// If the caller does not own the venue, or the write fails.
pub async fn revoke_share_code(pool: &SqlitePool, venue_id: &str) -> Result<(), CommandError> {
    let mut access = owner_write(pool, venue_id).await?;
    venues_db::set_share_code(&mut access, None).await?;
    Ok(access.commit().await?)
}

/// Who can open the venue: its owner, then everyone who joined.
///
/// Owner only, as on the server: a member's device downloads every
/// membership row of a venue it can open, but the server would show a member
/// only their own, and this list is the owner's to manage.
///
/// # Errors
///
/// [`CommandError::Unauthorized`] for anyone but the owner.
pub async fn members(pool: &SqlitePool, venue_id: &str) -> Result<Vec<VenueMember>, CommandError> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;
    if !venues_db::get_venue(&mut access).await?.is_owner() {
        return Err(not_owner());
    }
    Ok(venues_db::venue_members(&mut access).await?)
}

/// Take `uid`'s access away. Owner only, and never the owner's own.
///
/// # Errors
///
/// [`CommandError::NotFound`] when `uid` has no membership to remove.
pub async fn remove_member(
    pool: &SqlitePool,
    venue_id: &str,
    uid: &str,
) -> Result<(), CommandError> {
    let mut access = owner_write(pool, venue_id).await?;
    if access.principal() == Some(uid) {
        return Err(CommandError::Invalid(
            "The owner's access cannot be removed".into(),
        ));
    }
    if !venues_db::remove_venue_member(&mut access, uid).await? {
        return Err(CommandError::NotFound(
            "They no longer have access to this venue".into(),
        ));
    }
    Ok(access.commit().await?)
}

/// Join the venue whose share code `input` is, and return it once its rows
/// are on this device.
///
/// `input` is whatever was typed: [`normalize_share_code`] forgives case,
/// spaces and the hyphen. A code for a venue this account can already open is
/// refused before any request — a member's device holds the venue's code, so
/// this works offline too.
///
/// # Errors
///
/// Prose a person can act on, by kind: [`CommandError::Invalid`] for
/// something that is not a code or a venue already open to this account,
/// [`CommandError::NotFound`] for a code no venue holds,
/// [`CommandError::Unauthorized`] without a session, and
/// [`CommandError::Internal`] when the server cannot be reached or the venue
/// does not arrive within `arrival`.
pub async fn join(
    pool: &SqlitePool,
    server: &dyn ShareCodeServer,
    input: &str,
    arrival: Duration,
) -> Result<Venue, CommandError> {
    let code = normalize_share_code(input).ok_or_else(|| {
        CommandError::Invalid(
            "That isn't an invite code. Codes are eight letters and numbers, like K7M4-Q9TX."
                .into(),
        )
    })?;
    let before = venues_db::list_venues(pool).await?;
    if let Some(venue) = before
        .iter()
        .find(|venue| venue.share_code.as_deref() == Some(code.as_str()))
    {
        return Err(already_open(venue));
    }

    let venue_id = server
        .redeem(&code)
        .await
        .map_err(|refusal| match refusal {
            Refusal::UnknownCode => CommandError::NotFound(format!(
                "No venue uses {}. It may have been revoked; ask whoever shared it for a new one.",
                display_share_code(&code)
            )),
            Refusal::Offline(_) => CommandError::Internal(
                "Can't reach Luma's servers. Check your connection and try again.".into(),
            ),
            Refusal::SignedOut(_) => {
                CommandError::Unauthorized("Sign in again to join a venue.".into())
            }
            Refusal::Failed(message) => CommandError::Internal(format!("Couldn't join: {message}")),
        })?;
    // The code was changed since this device last synced, but the account was
    // already in: nothing new to open.
    if let Some(venue) = before.iter().find(|venue| venue.id == venue_id) {
        return Err(already_open(venue));
    }

    let deadline = Instant::now() + arrival;
    loop {
        if let Some(venue) = arrived(pool, &venue_id).await {
            return Ok(venue);
        }
        if Instant::now() >= deadline {
            return Err(CommandError::Internal(
                "You're in. The venue is still downloading and will appear in your list shortly."
                    .into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The venue, once this device can open it and its root is down with it — a
/// venue with no root is one whose rows are still arriving.
async fn arrived(pool: &SqlitePool, venue_id: &str) -> Option<Venue> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id))
        .await
        .ok()?;
    let venue = venues_db::get_venue(&mut access).await.ok()?;
    let rooted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM venue_nodes WHERE id = ?)")
        .bind(crate::venue_graph::root_id(venue_id))
        .fetch_one(&mut *access.connection())
        .await
        .ok()?;
    rooted.then_some(venue)
}

fn already_open(venue: &Venue) -> CommandError {
    CommandError::Invalid(if venue.is_owner() {
        format!("That's the invite code for {}, which is yours.", venue.name)
    } else {
        format!("You already have access to {}.", venue.name)
    })
}

async fn owner_write<'a>(
    pool: &'a SqlitePool,
    venue_id: &str,
) -> Result<VenueAccess<'a, Write>, CommandError> {
    // The guard answers "not found" for a venue this account cannot write,
    // which for a venue on the list means someone else owns it.
    VenueAccess::<Write>::write(pool, VenueResource::Venue(venue_id))
        .await
        .map_err(|error| {
            if error == "Venue resource not found" {
                not_owner()
            } else {
                error.into()
            }
        })
}

fn not_owner() -> CommandError {
    CommandError::Unauthorized("Only the venue's owner can manage who has access".into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;
    use crate::database::local::auth::arm_write_admission;

    const VENUE: &str = "shared";

    /// One database, three accounts taking turns at it: Alice owns the venue,
    /// Bob and Carol are strangers until a code says otherwise.
    /// [`LocalShareCodes`] plays the server against the same rows, so what a
    /// code resolves to is exactly what Alice wrote.
    async fn library() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("sharing.db"))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        arm_write_admission(&pool, Some("alice")).await.unwrap();
        sqlx::query(
            "INSERT INTO venues (id, uid, name) VALUES ('shared', 'alice', 'Warehouse');
             INSERT INTO venue_nodes (id, uid, venue_id, kind)
                 VALUES ('shared:venue', 'alice', 'shared', 'venue')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    async fn as_user(pool: &SqlitePool, user: &str) {
        arm_write_admission(pool, Some(user)).await.unwrap();
    }

    async fn stored_code(pool: &SqlitePool) -> Option<String> {
        sqlx::query_scalar("SELECT share_code FROM venues WHERE id = ?")
            .bind(VENUE)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn can_open(pool: &SqlitePool) -> bool {
        venues_db::list_venues(pool)
            .await
            .unwrap()
            .iter()
            .any(|venue| venue.id == VENUE)
    }

    async fn join_now(pool: &SqlitePool, code: &str) -> Result<Venue, CommandError> {
        join(
            pool,
            &LocalShareCodes::new(pool.clone()),
            code,
            Duration::ZERO,
        )
        .await
    }

    #[tokio::test]
    async fn the_owner_mints_replaces_and_revokes_the_code() {
        let (_directory, pool) = library().await;

        let first = rotate_share_code(&pool, VENUE).await.unwrap();
        assert_eq!(
            normalize_share_code(&first).as_deref(),
            Some(first.as_str())
        );
        assert_eq!(stored_code(&pool).await.as_deref(), Some(first.as_str()));

        let second = rotate_share_code(&pool, VENUE).await.unwrap();
        assert_ne!(first, second, "rotating kept the old code");
        assert_eq!(stored_code(&pool).await.as_deref(), Some(second.as_str()));

        revoke_share_code(&pool, VENUE).await.unwrap();
        assert_eq!(stored_code(&pool).await, None);
    }

    #[tokio::test]
    async fn only_the_owner_touches_the_code() {
        let (_directory, pool) = library().await;
        let code = rotate_share_code(&pool, VENUE).await.unwrap();
        as_user(&pool, "bob").await;
        join_now(&pool, &code).await.unwrap();

        for refused in [
            rotate_share_code(&pool, VENUE).await.map(drop),
            revoke_share_code(&pool, VENUE).await,
        ] {
            assert!(
                matches!(refused, Err(CommandError::Unauthorized(_))),
                "a member changed the code: {refused:?}"
            );
        }
        assert_eq!(stored_code(&pool).await.as_deref(), Some(code.as_str()));
    }

    #[tokio::test]
    async fn a_code_admits_someone_until_it_is_revoked() {
        let (_directory, pool) = library().await;
        let code = rotate_share_code(&pool, VENUE).await.unwrap();

        // Typed the way people type it.
        as_user(&pool, "bob").await;
        assert!(!can_open(&pool).await);
        let joined = join_now(&pool, &display_share_code(&code).to_lowercase())
            .await
            .unwrap();
        assert_eq!(joined.id, VENUE);
        assert!(joined.is_member());
        assert!(can_open(&pool).await);

        as_user(&pool, "alice").await;
        revoke_share_code(&pool, VENUE).await.unwrap();

        // The revoked code opens nothing for the next person...
        as_user(&pool, "carol").await;
        let refused = join_now(&pool, &code).await.unwrap_err();
        assert!(
            matches!(&refused, CommandError::NotFound(message) if message.contains("revoked")),
            "a revoked code still joined: {refused:?}"
        );
        assert!(!can_open(&pool).await);

        // ...and takes nothing from the person already in.
        as_user(&pool, "bob").await;
        assert!(can_open(&pool).await);
    }

    #[tokio::test]
    async fn a_new_code_retires_the_old_one() {
        let (_directory, pool) = library().await;
        let old = rotate_share_code(&pool, VENUE).await.unwrap();
        let new = rotate_share_code(&pool, VENUE).await.unwrap();

        as_user(&pool, "carol").await;
        assert!(matches!(
            join_now(&pool, &old).await,
            Err(CommandError::NotFound(_))
        ));
        assert_eq!(join_now(&pool, &new).await.unwrap().id, VENUE);
    }

    #[tokio::test]
    async fn a_join_refuses_what_is_not_a_code_and_what_is_already_open() {
        let (_directory, pool) = library().await;
        let code = rotate_share_code(&pool, VENUE).await.unwrap();

        let refused = join_now(&pool, "hello").await.unwrap_err();
        assert!(matches!(refused, CommandError::Invalid(_)), "{refused:?}");

        let refused = join_now(&pool, &code).await.unwrap_err();
        assert_eq!(
            refused.to_string(),
            "That's the invite code for Warehouse, which is yours."
        );

        as_user(&pool, "bob").await;
        join_now(&pool, &code).await.unwrap();
        let refused = join_now(&pool, &code).await.unwrap_err();
        assert_eq!(refused.to_string(), "You already have access to Warehouse.");
    }

    #[tokio::test]
    async fn the_owner_sees_everyone_and_removes_a_member() {
        let (_directory, pool) = library().await;
        let code = rotate_share_code(&pool, VENUE).await.unwrap();
        for user in ["bob", "carol"] {
            as_user(&pool, user).await;
            join_now(&pool, &code).await.unwrap();
        }

        // A member's device holds every membership row, but the list is not
        // theirs to read.
        assert!(matches!(
            members(&pool, VENUE).await,
            Err(CommandError::Unauthorized(_))
        ));
        assert!(matches!(
            remove_member(&pool, VENUE, "carol").await,
            Err(CommandError::Unauthorized(_))
        ));

        as_user(&pool, "alice").await;
        let listed = members(&pool, VENUE).await.unwrap();
        let shape: Vec<_> = listed
            .iter()
            .map(|member| (member.uid.as_str(), member.role.as_str(), member.is_you))
            .collect();
        assert_eq!(
            shape,
            [
                ("alice", "owner", true),
                ("bob", "member", false),
                ("carol", "member", false)
            ]
        );

        remove_member(&pool, VENUE, "bob").await.unwrap();
        assert!(matches!(
            remove_member(&pool, VENUE, "bob").await,
            Err(CommandError::NotFound(_))
        ));
        assert!(matches!(
            remove_member(&pool, VENUE, "alice").await,
            Err(CommandError::Invalid(_))
        ));
        let left: Vec<_> = members(&pool, VENUE)
            .await
            .unwrap()
            .into_iter()
            .map(|member| member.uid)
            .collect();
        assert_eq!(left, ["alice", "carol"]);

        as_user(&pool, "bob").await;
        assert!(
            !can_open(&pool).await,
            "a removed member still opens the venue"
        );
    }

    /// Redeems at once and lets the rows land a moment later, as a download
    /// would.
    struct Downloading(Arc<LocalShareCodes>, Duration);

    #[async_trait::async_trait]
    impl ShareCodeServer for Downloading {
        async fn redeem(&self, code: &str) -> Result<String, Refusal> {
            let (server, after, code) = (Arc::clone(&self.0), self.1, code.to_owned());
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                server.redeem(&code).await.unwrap();
            });
            Ok(VENUE.to_owned())
        }
    }

    #[tokio::test]
    async fn a_join_answers_once_the_venue_has_downloaded() {
        let (_directory, pool) = library().await;
        let code = rotate_share_code(&pool, VENUE).await.unwrap();
        as_user(&pool, "bob").await;
        let server = Arc::new(LocalShareCodes::new(pool.clone()));

        let slow = Downloading(Arc::clone(&server), Duration::from_secs(5));
        let refused = join(&pool, &slow, &code, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("still downloading"),
            "{refused:?}"
        );

        let prompt = Downloading(server, Duration::from_millis(300));
        let joined = join(&pool, &prompt, &code, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(joined.id, VENUE);
    }

    struct Down;

    #[async_trait::async_trait]
    impl ShareCodeServer for Down {
        async fn redeem(&self, _code: &str) -> Result<String, Refusal> {
            Err(refusal(SyncError::Network(
                "error sending request: connection refused".into(),
            )))
        }
    }

    #[tokio::test]
    async fn an_unreachable_server_reads_as_offline() {
        let (_directory, pool) = library().await;
        as_user(&pool, "bob").await;
        let refused = join(&pool, &Down, "K7M4-Q9TX", Duration::ZERO)
            .await
            .unwrap_err();
        assert!(
            matches!(&refused, CommandError::Internal(message) if message.starts_with("Can't reach")),
            "{refused:?}"
        );
    }

    #[test]
    fn the_server_s_answers_become_refusals() {
        let unknown = SyncError::Api {
            status: 400,
            message: r#"{"code":"P0001","message":"no venue for share code","details":null}"#
                .into(),
        };
        assert_eq!(refusal(unknown), Refusal::UnknownCode);
        assert!(matches!(
            refusal(SyncError::Api {
                status: 401,
                message: "JWT expired".into()
            }),
            Refusal::SignedOut(_)
        ));
        assert!(matches!(
            refusal(SyncError::Api {
                status: 503,
                message: String::new()
            }),
            Refusal::Offline(_)
        ));
        assert!(matches!(
            refusal(SyncError::Api {
                status: 403,
                message: "denied".into()
            }),
            Refusal::Failed(_)
        ));
    }
}
