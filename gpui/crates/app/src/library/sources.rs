//! DJ-library sources (Engine DJ, Rekordbox) through one normalized shape:
//! the catalog, its playlists, its tracks, search, and a row's cover.

use std::future::Future;

use luma_lib::dispatch::{
    handlers, ArtworkRef, EngineDjPlaylist, EngineDjTrack, RekordboxPlaylist, RekordboxTrack,
};

#[cfg(feature = "agent")]
use super::fixtures::{
    normalize_source_library, normalize_source_playlists, normalize_source_tracks,
};
use super::{Cause, Library, LibraryError};

/// DJ catalog selected by the import flow. Source-specific identifiers remain
/// opaque strings above this seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackSource {
    EngineDj { library_path: String },
    Rekordbox,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceLibrary {
    pub identity: Option<String>,
    pub track_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePlaylist {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub track_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceTrack {
    pub id: String,
    /// Absolute: a source with relative paths has them resolved on the way in.
    pub file_path: Option<String>,
    pub filename: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bpm: Option<f64>,
    pub duration_seconds: Option<f64>,
    /// Where the source library keeps this row's cover, when it keeps one.
    /// The file's own tags are the fallback — see [`Library::source_artwork`].
    pub art: Option<ArtworkRef>,
}

impl From<EngineDjPlaylist> for SourcePlaylist {
    fn from(row: EngineDjPlaylist) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.title,
            parent_id: row.parent_id.map(|id| id.to_string()),
            track_count: row.track_count.try_into().unwrap_or(0),
        }
    }
}

impl From<RekordboxPlaylist> for SourcePlaylist {
    fn from(row: RekordboxPlaylist) -> Self {
        Self {
            id: row.id,
            name: row.name,
            parent_id: row.parent_id,
            track_count: row.track_count,
        }
    }
}

/// The library root an Engine DJ source resolves its rows' paths against;
/// empty for a source whose rows carry absolute paths.
fn engine_root(source: &TrackSource) -> String {
    match source {
        TrackSource::EngineDj { library_path } => library_path.clone(),
        TrackSource::Rekordbox => String::new(),
    }
}

/// An Engine DJ row as a source row. Engine stores paths relative to the
/// library folder (`../../Music/x.mp3`), so they are resolved here, once,
/// rather than by every consumer that wants to open the file — a preview,
/// a cover read.
pub(super) fn engine_source_track(library_path: &str, row: EngineDjTrack) -> SourceTrack {
    let file_path = luma_lib::dispatch::resolve_engine_path(library_path, &row.path);
    SourceTrack {
        id: row.id.to_string(),
        file_path: Some(file_path.to_string_lossy().into_owned()),
        filename: Some(row.filename),
        title: row.title,
        artist: row.artist,
        album: row.album,
        bpm: row.bpm_analyzed,
        duration_seconds: row.length,
        art: row.album_art_id.map(|album_art_id| ArtworkRef::EngineDj {
            library_path: library_path.to_string(),
            album_art_id,
        }),
    }
}

impl From<RekordboxTrack> for SourceTrack {
    fn from(row: RekordboxTrack) -> Self {
        Self {
            id: row.uuid,
            file_path: row.file_path,
            filename: row.filename,
            title: row.title,
            artist: row.artist,
            album: row.album,
            bpm: row.bpm,
            duration_seconds: row.duration_seconds,
            art: row.artwork_path.map(ArtworkRef::File),
        }
    }
}

impl Library {
    /// Resolve the installed Engine DJ library root. The source picker owns no
    /// product-specific path rules; it asks the adapter for its default and
    /// carries the opaque result in [`TrackSource`].
    pub fn default_engine_dj_source(
        &self,
    ) -> impl Future<Output = Result<TrackSource, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self.source_adapter_fixture().is_some();
        #[cfg(not(feature = "agent"))]
        let fixture = false;
        let path = (!fixture).then(|| {
            self.call("engine_dj_default_library_path", |services| async move {
                handlers::engine_dj::engine_dj_default_library_path(&services).await
            })
        });
        async move {
            let library_path = match path {
                Some(path) => path.await?,
                None => "/fixture/engine-dj".to_string(),
            };
            Ok(TrackSource::EngineDj { library_path })
        }
    }

    /// Open either supported DJ catalog through one GPUI-owned shape.
    pub fn source_library(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<SourceLibrary, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture_delay = self.source_fixture_delay.map(|delay| {
            self.runtime().spawn(async move {
                tokio::time::sleep(delay).await;
            })
        });
        #[cfg(not(feature = "agent"))]
        let fixture_delay: Option<tokio::task::JoinHandle<()>> = None;
        #[cfg(feature = "agent")]
        let fixture = self
            .source_adapter_fixture()
            .map(|fixture| normalize_source_library(&source, fixture.library));
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<SourceLibrary, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                let library_path = library_path.clone();
                Some(self.call("engine_dj_open_library", |services| async move {
                    handlers::engine_dj::engine_dj_open_library(&services, library_path).await
                }))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                Some(self.call("rekordbox_open_library", |services| async move {
                    handlers::rekordbox::rekordbox_open_library(&services).await
                }))
            }
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                if let Some(delay) = fixture_delay {
                    let _ = delay.await;
                }
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(opened), None) => {
                    let opened = opened.await?;
                    Ok(SourceLibrary {
                        identity: Some(opened.database_uuid),
                        track_count: opened.track_count.try_into().unwrap_or(0),
                    })
                }
                (None, Some(opened)) => {
                    let opened = opened.await?;
                    Ok(SourceLibrary {
                        identity: None,
                        track_count: opened.track_count,
                    })
                }
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// Flat source playlists; `parent_id` is enough for the dialog to build a
    /// shared tree without knowing which DJ product produced it.
    pub fn source_playlists(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<Vec<SourcePlaylist>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self
            .source_adapter_fixture()
            .map(|fixture| normalize_source_playlists(&source, fixture.playlists));
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourcePlaylist>, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                let library_path = library_path.clone();
                Some(
                    self.call("engine_dj_list_playlists", |services| async move {
                        handlers::engine_dj::engine_dj_list_playlists(&services, library_path).await
                    }),
                )
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => Some(
                self.call("rekordbox_list_playlists", |services| async move {
                    handlers::rekordbox::rekordbox_list_playlists(&services).await
                }),
            ),
            _ => None,
        };
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(rows), None) => {
                    Ok(rows.await?.into_iter().map(SourcePlaylist::from).collect())
                }
                (None, Some(rows)) => {
                    Ok(rows.await?.into_iter().map(SourcePlaylist::from).collect())
                }
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// The whole selected source library in its native stable ordering.
    pub fn source_tracks(
        &self,
        source: TrackSource,
    ) -> impl Future<Output = Result<Vec<SourceTrack>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self
            .source_adapter_fixture()
            .map(|fixture| normalize_source_tracks(&source, "source_list_tracks", fixture.tracks));
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourceTrack>, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                let library_path = library_path.clone();
                Some(self.call("engine_dj_list_tracks", |services| async move {
                    handlers::engine_dj::engine_dj_list_tracks(&services, library_path).await
                }))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                Some(self.call("rekordbox_list_tracks", |services| async move {
                    handlers::rekordbox::rekordbox_list_tracks(&services).await
                }))
            }
            _ => None,
        };
        let engine_root = engine_root(&source);
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(rows), None) => Ok(rows
                    .await?
                    .into_iter()
                    .map(|row| engine_source_track(&engine_root, row))
                    .collect()),
                (None, Some(rows)) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// One playlist/crate through the same row shape as [`Self::source_tracks`].
    pub fn source_playlist_tracks(
        &self,
        source: TrackSource,
        playlist_id: &str,
    ) -> impl Future<Output = Result<Vec<SourceTrack>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let fixture = self.source_adapter_fixture().map(|fixture| {
            fixture
                .playlist_tracks
                .get(playlist_id)
                .cloned()
                .ok_or_else(|| {
                    LibraryError::at(
                        "source_playlist_tracks",
                        Cause::Shape(format!("fixture has no playlist {playlist_id}")),
                    )
                })
                .and_then(|value| normalize_source_tracks(&source, "source_playlist_tracks", value))
        });
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourceTrack>, LibraryError>> = None;
        let engine_id = match &source {
            TrackSource::EngineDj { .. } => playlist_id.parse::<i64>().ok(),
            TrackSource::Rekordbox => None,
        };
        let invalid_engine_id = fixture.is_none()
            && matches!(&source, TrackSource::EngineDj { .. })
            && engine_id.is_none();
        let engine = match (&source, engine_id, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, Some(playlist_id), true) => {
                let library_path = library_path.clone();
                Some(self.call(
                    "engine_dj_get_playlist_tracks",
                    move |services| async move {
                        handlers::engine_dj::engine_dj_get_playlist_tracks(
                            &services,
                            library_path,
                            playlist_id,
                        )
                        .await
                    },
                ))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                let playlist_id = playlist_id.to_owned();
                Some(
                    self.call("rekordbox_get_playlist_tracks", |services| async move {
                        handlers::rekordbox::rekordbox_get_playlist_tracks(&services, playlist_id)
                            .await
                    }),
                )
            }
            _ => None,
        };
        let engine_root = engine_root(&source);
        async move {
            if let Some(fixture) = fixture {
                return fixture;
            }
            if invalid_engine_id {
                return Err(LibraryError::at(
                    "engine_dj_get_playlist_tracks",
                    Cause::Shape("playlist id was not an Engine DJ integer".into()),
                ));
            }
            match (engine, rekordbox) {
                (Some(rows), None) => Ok(rows
                    .await?
                    .into_iter()
                    .map(|row| engine_source_track(&engine_root, row))
                    .collect()),
                (None, Some(rows)) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// Search either source and return the same row contract.
    pub fn search_source_tracks(
        &self,
        source: TrackSource,
        query: &str,
    ) -> impl Future<Output = Result<Vec<SourceTrack>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let sequenced = {
            let mut responses = self.source_search_fixture.lock().unwrap();
            responses
                .front()
                .is_some_and(|response| response.query == query)
                .then(|| responses.pop_front().unwrap())
        };
        #[cfg(feature = "agent")]
        let sequenced = sequenced.map(|response| {
            let delay = self.runtime().spawn(async move {
                tokio::time::sleep(response.delay).await;
            });
            (
                normalize_source_tracks(&source, "source_search_tracks", response.rows),
                delay,
            )
        });
        #[cfg(feature = "agent")]
        let fixture = sequenced
            .is_none()
            .then(|| self.source_adapter_fixture())
            .flatten()
            .map(|fixture| {
                fixture
                    .searches
                    .get(query)
                    .cloned()
                    .ok_or_else(|| {
                        LibraryError::at(
                            "source_search_tracks",
                            Cause::Shape(format!("fixture has no search {query:?}")),
                        )
                    })
                    .and_then(|value| {
                        normalize_source_tracks(&source, "source_search_tracks", value)
                    })
            });
        #[cfg(not(feature = "agent"))]
        let fixture: Option<Result<Vec<SourceTrack>, LibraryError>> = None;
        let engine = match (&source, fixture.is_none()) {
            (TrackSource::EngineDj { library_path }, true) => {
                let (library_path, query) = (library_path.clone(), query.to_owned());
                Some(self.call("engine_dj_search_tracks", |services| async move {
                    handlers::engine_dj::engine_dj_search_tracks(&services, library_path, query)
                        .await
                }))
            }
            _ => None,
        };
        let rekordbox = match (&source, fixture.is_none()) {
            (TrackSource::Rekordbox, true) => {
                let query = query.to_owned();
                Some(self.call("rekordbox_search_tracks", |services| async move {
                    handlers::rekordbox::rekordbox_search_tracks(&services, query).await
                }))
            }
            _ => None,
        };
        let engine_root = engine_root(&source);
        async move {
            #[cfg(feature = "agent")]
            if let Some((rows, delay)) = sequenced {
                let _ = delay.await;
                return rows;
            }
            if let Some(fixture) = fixture {
                return fixture;
            }
            match (engine, rekordbox) {
                (Some(rows), None) => Ok(rows
                    .await?
                    .into_iter()
                    .map(|row| engine_source_track(&engine_root, row))
                    .collect()),
                (None, Some(rows)) => Ok(rows.await?.into_iter().map(SourceTrack::from).collect()),
                _ => unreachable!("one source produces exactly one call"),
            }
        }
    }

    /// A source row's cover as encoded image bytes: its library's own image,
    /// else the one embedded in the file. `Ok(None)` when it has neither.
    pub fn source_artwork(
        &self,
        track: &SourceTrack,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, LibraryError>> + use<> {
        let art = track.art.clone();
        let audio = track.file_path.clone();
        self.call("source_track_artwork", move |_| async move {
            handlers::tracks::source_track_artwork(art, audio).await
        })
    }
}
