//! Venues: the catalogue, a venue's room (environment and haze), and the
//! folders its songs are filed in.

use std::future::Future;
#[cfg(not(feature = "agent"))]
use std::time::Duration;

use luma_lib::dispatch::handlers;
use luma_lib::models::folders::Folder;
use luma_lib::models::venues::Venue;
use luma_render::scene_desc::{VenueEnvironment, VenueHaze};

use super::{Cause, Library, LibraryError};

impl Library {
    /// Every venue in the library, newest activity first.
    pub fn venues(&self) -> impl Future<Output = Result<Vec<Venue>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let (delay, error) = {
            let mut fixture = self.navigation_fixture.lock().unwrap();
            if fixture.catalogue_responses.is_empty() {
                (fixture.venues_delay, fixture.venues_error.clone())
            } else {
                fixture.catalogue_responses.remove(0)
            }
        };
        #[cfg(not(feature = "agent"))]
        let (delay, error): (Duration, Option<String>) = (Duration::ZERO, None);
        let pending = self.call_after("list_venues", delay, |services| async move {
            handlers::venues::list_venues(&services).await
        });
        async move {
            let result = pending.await;
            if let Some(error) = error {
                return Err(LibraryError::at("list_venues", Cause::Shape(error)));
            }
            result
        }
    }

    /// Write one venue's lighting environment: what kind of room it is and how
    /// far up its one dial is.
    ///
    /// Not a graph verb — nothing moves — so it answers with nothing and the
    /// caller re-reads the rig, which is where the environment arrives from in
    /// the first place ([`Rig::environment`](super::Rig::environment)).
    pub fn set_venue_environment(
        &self,
        venue_id: &str,
        environment: VenueEnvironment,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("set_venue_environment", move |services| async move {
            handlers::venues::set_venue_environment(&services, venue_id, environment).await
        })
    }

    /// Write one venue's atmosphere: whether the room has haze in it, how much,
    /// and what it looks like.
    ///
    /// A sibling of [`Self::set_venue_environment`] in every respect, including
    /// answering with nothing — the value comes back on the next rig read
    /// ([`Rig::haze`](super::Rig::haze)). The march's cost (`steps`, `resolution`) is deliberately
    /// not here: that is local, and [`VenueHaze`] says why.
    pub fn set_venue_haze(
        &self,
        venue_id: &str,
        haze: VenueHaze,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("set_venue_haze", move |services| async move {
            handlers::venues::set_venue_haze(&services, venue_id, haze).await
        })
    }

    /// Create a venue and return the durable row written by the catalog.
    pub fn create_venue(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> impl Future<Output = Result<Venue, LibraryError>> + use<> {
        let (name, description) = (name.to_owned(), description.map(str::to_owned));
        self.call("create_venue", |services| async move {
            handlers::venues::create_venue(&services, name, description).await
        })
    }

    // -- folders --------------------------------------------------------------

    /// The venue's folders, by name, each with the songs it holds.
    pub fn folders(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<Folder>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("list_folders", |services| async move {
            handlers::folders::list_folders(&services, venue_id).await
        })
    }

    /// A new, empty folder. The backend trims the name and refuses an empty
    /// one.
    pub fn create_folder(
        &self,
        venue_id: &str,
        name: &str,
    ) -> impl Future<Output = Result<Folder, LibraryError>> + use<> {
        let (venue_id, name) = (venue_id.to_owned(), name.to_owned());
        self.call("create_folder", |services| async move {
            handlers::folders::create_folder(&services, venue_id, name).await
        })
    }

    pub fn rename_folder(
        &self,
        folder_id: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (folder_id, name) = (folder_id.to_owned(), name.to_owned());
        self.call("rename_folder", |services| async move {
            handlers::folders::rename_folder(&services, folder_id, name).await
        })
    }

    /// Delete a folder and its links. Its songs and their scores stay.
    pub fn delete_folder(
        &self,
        folder_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let folder_id = folder_id.to_owned();
        self.call("delete_folder", |services| async move {
            handlers::folders::delete_folder(&services, folder_id).await
        })
    }

    /// Put a song in a folder (`linked`), or take it out.
    pub fn set_folder_track(
        &self,
        folder_id: &str,
        track_id: &str,
        linked: bool,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (folder_id, track_id) = (folder_id.to_owned(), track_id.to_owned());
        self.call("set_folder_track", move |services| async move {
            handlers::folders::set_folder_track(&services, folder_id, track_id, linked).await
        })
    }
}
