//! App settings and host-session values, with writes queued in order
//! through the library's session-write actor.

use std::future::Future;

use luma_lib::dispatch::{handlers, CommandError, SharedServices};
use luma_lib::settings::AppSettings;

use super::{refused, Cause, Library, LibraryError, SessionWrite};

impl Library {
    /// Every app setting, typed and defaulted.
    pub fn settings(&self) -> impl Future<Output = Result<AppSettings, LibraryError>> + use<> {
        self.call("get_settings", |services| async move {
            handlers::settings::get_settings(&services).await
        })
    }

    /// Read one host-session value. App navigation uses this for last-open
    /// state; it is intentionally not an app setting with subsystem effects.
    pub fn get_session_item(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<String>, LibraryError>> + use<> {
        let key = key.to_owned();
        self.call("get_session_item", |services| async move {
            handlers::auth::get_session_item(&services, key).await
        })
    }

    /// Persist one host-session value.
    pub fn set_session_item(
        &self,
        key: &str,
        value: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.ordered_write("set_session_item", |services| async move {
            handlers::auth::set_session_item(&services, key, value).await
        })
    }

    /// Remove one host-session value.
    pub fn remove_session_item(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let key = key.to_owned();
        self.ordered_write("remove_session_item", |services| async move {
            handlers::auth::remove_session_item(&services, key).await
        })
    }

    /// Write one setting. The seam owns what a key means and what a write
    /// costs (DMX output and audio both reload on one), so the caller's only job
    /// afterwards is to read the settings back — which is also the only honest
    /// proof the write landed.
    ///
    /// Through the same FIFO the session writes use, and for the same reason
    /// stated there: a detached call finishes whenever SQLite gets to it, so
    /// the *last* value a control sent is not necessarily the one left in the
    /// database. That was a rarity while every settings control was a checkbox
    /// or a picker; a dragged slider sends a value per pointer move, which
    /// makes it routine.
    pub fn set_setting(
        &self,
        key: &str,
        value: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.ordered_write("set_setting", |services| async move {
            handlers::settings::set_setting(&services, key, value).await
        })
    }

    /// One durable write, queued behind the last one.
    ///
    /// Named for the ordering it provides rather than for the session items it
    /// was first written for — settings share the queue, and a name that said
    /// "session" would make that read as a mistake.
    fn ordered_write<F, Fut>(
        &self,
        command: &'static str,
        run: F,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<F, Fut>
    where
        F: FnOnce(SharedServices) -> Fut,
        Fut: Future<Output = Result<(), CommandError>> + Send + 'static,
    {
        let pending = run(self.services.clone());
        let (reply, answer) = tokio::sync::oneshot::channel();
        let queued = self.session_writes.send(SessionWrite {
            write: Box::pin(async move { pending.await.map_err(refused(command)) }),
            reply,
        });
        async move {
            queued
                .map_err(|error| LibraryError::at(command, Cause::Cancelled(error.to_string())))?;
            answer
                .await
                .map_err(|error| LibraryError::at(command, Cause::Cancelled(error.to_string())))?
        }
    }
}
