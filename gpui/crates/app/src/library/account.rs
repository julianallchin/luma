//! Who is signed in: the cached account, signing in and out, and the cloud
//! session and sync status that hang off it.

use std::future::Future;
use std::sync::Arc;

use luma_lib::database::local::auth::{AuthAccount, SUPABASE_SESSION_KEY};
use luma_lib::dispatch::handlers;

use super::{refused, Library, LibraryError};

/// Who the app database admitted, as the app names them.
///
/// Two halves that are not interchangeable: [`Self::id`] is the proven
/// principal every row is stamped with, [`Self::email`] is the address the
/// session was obtained with and exists only to put a name on screen. A
/// session whose blob carries no address is still a perfectly good identity —
/// hence [`Self::label`], which never shows the id.
#[derive(Clone, Debug)]
pub struct Account {
    pub id: String,
    pub email: Option<String>,
}

impl From<AuthAccount> for Account {
    fn from(account: AuthAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
        }
    }
}

impl Account {
    /// What to call this account in front of a person.
    ///
    /// The id is deliberately *not* the fallback. A uuid is not a name — it
    /// tells the reader nothing they can act on, and reads as a bug in a row
    /// that is supposed to say who they are. "Signed in" is the honest answer
    /// to a session that proved someone without saying which address it was
    /// issued to.
    #[must_use]
    pub fn label(&self) -> &str {
        match self.email.as_deref() {
            Some(email) if !email.is_empty() => email,
            _ => "Signed in",
        }
    }
}

impl Library {
    /// Why the stored session could not be verified at launch, if there was
    /// one to verify.
    ///
    /// Distinct from `user_id().is_none()`, and the distinction is the whole
    /// point: a library nobody has signed into is a guest library, which is a
    /// working library and owes the user nothing. A library holding
    /// credentials that no longer prove anyone has lost something the user
    /// had, and only signing in again gets it back — so that is the one state
    /// that raises the sign-in gate unasked.
    pub fn lapsed(&self) -> Option<&str> {
        self.lapsed.as_deref()
    }

    /// The signed-in principal, or `None` for the guest namespace.
    pub fn user_id(&self) -> Option<String> {
        self.account.lock().unwrap().as_ref().map(|a| a.id.clone())
    }

    /// [`Self::user_id`] with the address to show it under — what a screen
    /// naming the account reads.
    pub fn account(&self) -> Option<Account> {
        self.account.lock().unwrap().clone()
    }

    /// Email a six-digit login code to `email`.
    pub fn send_login_code(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let email = email.to_owned();
        self.call("send_login_code", |services| async move {
            handlers::auth::send_login_code(&services, email).await
        })
    }

    /// Exchange an emailed code for a session and adopt the principal it
    /// proves. The host performs the identity switch; this only refreshes the
    /// cached answer to [`Library::account`] so rows written next carry it.
    ///
    /// The address needs no second read: a code is only ever verified against
    /// the address it was mailed to, so this call already holds the label the
    /// new session will be shown under.
    pub fn verify_login_code(
        &self,
        email: &str,
        code: &str,
    ) -> impl Future<Output = Result<String, LibraryError>> + use<> {
        let (asked, code) = (email.to_owned(), code.to_owned());
        let pending = self.call("verify_login_code", |services| async move {
            handlers::auth::verify_login_code(&services, asked, code).await
        });
        let cache = Arc::clone(&self.account);
        let email = email.trim().to_string();
        async move {
            let user_id = pending.await?;
            *cache.lock().unwrap() = Some(Account {
                id: user_id.clone(),
                email: Some(email),
            });
            Ok(user_id)
        }
    }

    /// Whether this library talks to the cloud at all — the one thing the
    /// status bar's sync item is conditioned on.
    pub(crate) fn cloud_sync_enabled(&self) -> bool {
        #[cfg(feature = "agent")]
        if self.sync_status_fixture.is_some() {
            return true;
        }
        self.cloud
    }

    pub(crate) fn sync_status(
        &self,
    ) -> impl Future<Output = Result<luma_lib::models::sync::SyncStatus, LibraryError>> + use<>
    {
        #[cfg(feature = "agent")]
        let fixture = self.sync_status_fixture.clone();
        let pending = self.call("sync_status", |services| async move {
            handlers::sync::sync_status(&services).await
        });
        async move {
            #[cfg(feature = "agent")]
            if let Some(fixture) = fixture {
                return Ok(fixture.lock().unwrap().clone());
            }
            pending.await
        }
    }

    /// Ask Supabase to turn the stored session back into a principal, and
    /// adopt whichever answer comes back.
    ///
    /// The refresh is the host's `get_session_item` on the session key —
    /// reading it is what refreshes an expiring token and re-arms admission
    /// for the identity it proves — followed by `current_account` for the
    /// label. Sequenced inside one spawned task for the same reason
    /// [`Self::sign_out`] is: [`Self::call`] starts immediately, and the
    /// label must be read *after* the identity switch it describes.
    ///
    /// `Ok(None)` is a session Supabase would not renew; `Err` is a network
    /// that would not say. Neither is a principal, and the caller treats them
    /// alike — the difference is only worth a log line.
    pub fn refresh_session(
        &self,
    ) -> impl Future<Output = Result<Option<Account>, LibraryError>> + use<> {
        let cache = Arc::clone(&self.account);
        let task = self.task("refresh_session", |services| async move {
            handlers::auth::get_session_item(&services, SUPABASE_SESSION_KEY.to_owned())
                .await
                .map_err(refused("get_session_item"))?;
            let account = handlers::auth::current_account(&services)
                .await
                .map_err(refused("current_account"))?;
            Ok(account.map(Account::from))
        });
        async move {
            let account = task.await?;
            *cache.lock().unwrap() = account.clone();
            Ok(account)
        }
    }

    /// Sign out: make the signed-in projection durable, wipe it, then release
    /// the session.
    ///
    /// Two commands in this order because they are two commit points and only
    /// this order is recoverable — `wipe_database` is the durability boundary
    /// and leaves a journal that `remove_session_item` consumes. A host that
    /// reversed them would delete the
    /// credential it still needs to flush with.
    ///
    /// Both run inside *one* spawned task, not two: [`Self::call`] starts
    /// the moment it is called, so holding two of its futures and awaiting
    /// them in order sequences the awaits and nothing else — the commands
    /// themselves race for the sync lock, and a release that won would clear
    /// the credential the wipe is still flushing with. That is the reversal
    /// this comment says is unrecoverable, arrived at by accident.
    pub fn sign_out(&self) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let cache = Arc::clone(&self.account);
        let task = self.task("sign_out", |services| async move {
            handlers::auth::wipe_database(&services)
                .await
                .map_err(refused("wipe_database"))?;
            handlers::auth::remove_session_item(&services, SUPABASE_SESSION_KEY.to_owned())
                .await
                .map_err(refused("remove_session_item"))
        });
        async move {
            task.await?;
            *cache.lock().unwrap() = None;
            Ok(())
        }
    }

    /// Fires when Supabase refuses to renew the stored session — its refresh
    /// token was spent by another process, or the session was revoked. The
    /// backend stops presenting it; the app's answer is the sign-in gate.
    /// The tables a download just wrote, coalesced by the sync service.
    ///
    /// A download is not an edit anybody made on this device, so nothing the
    /// UI holds knows it happened. This is how it finds out.
    pub fn replica_changed(&self) -> tokio::sync::broadcast::Receiver<Vec<String>> {
        self.replica_changed.subscribe()
    }

    pub fn session_revoked(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.session_revoked.subscribe()
    }
}
