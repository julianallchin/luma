//! The agent harness's stand-ins at the library boundary: raw DJ-source
//! answers, navigation timing, and outside writes. The shipped app never
//! installs one.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

use luma_lib::dispatch::{
    handlers, ArtworkRef, EngineDjPlaylist, EngineDjTrack, RekordboxLibraryInfo, RekordboxPlaylist,
    RekordboxTrack, TrackSources,
};

use super::sources::engine_source_track;
use super::{
    refused, Cause, Library, LibraryError, SourceLibrary, SourcePlaylist, SourceTrack, TrackSource,
};

#[derive(Clone, Debug, Default)]
pub struct NavigationFixture {
    /// Hold the venue catalogue so an outside-in test can observe the real
    /// loading route instead of racing a local SQLite read.
    pub venues_delay: Duration,
    /// Substitute a catalogue failure at the Library boundary. The shipped
    /// app never installs a fixture.
    pub venues_error: Option<String>,
    /// Per-venue delays for forcing two real track reads to land out of order.
    pub track_delays: HashMap<String, Duration>,
    /// Per-call catalogue timing/failure, consumed in request order. This is
    /// the deterministic seam for proving a dismissed picker cannot be
    /// overwritten by its older answer.
    pub catalogue_responses: Vec<(Duration, Option<String>)>,
    /// Per-call venue reads, matched and consumed by venue id. Rows still come
    /// from the real dispatcher; the optional title rewrite makes two reads of
    /// the same venue observably distinct to the outside-in harness.
    pub track_responses: Vec<(String, Duration, Option<String>)>,
    /// Per-write delays consumed by the FIFO session actor. A delayed A then
    /// immediate B must still persist B.
    pub session_write_delays: Vec<Duration>,
    /// Another writer's edit, landed while the editor's next score save is in
    /// flight: written and announced as a download, and the save held until
    /// the editor has begun re-reading the score. Taken once.
    pub outside_score_edit: Option<luma_patterns::ScoreEdit>,
    /// Woken by every score read, for [`Self::outside_score_edit`].
    pub score_reads: Arc<tokio::sync::Notify>,
}

/// Raw adapter answers used only by the GPUI agent harness, decoded into the
/// adapters' own row types so the public Library methods normalize them
/// exactly as they normalize a real source.
#[derive(Clone, Debug)]
pub struct SourceAdapterFixture {
    pub library: Value,
    pub playlists: Value,
    pub tracks: Value,
    pub playlist_tracks: HashMap<String, Value>,
    pub searches: HashMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct SourceSearchFixtureResponse {
    pub query: String,
    pub delay: Duration,
    pub rows: Value,
}

pub(super) struct FixtureTrackSources {
    pub(super) fixture: Arc<Mutex<Option<SourceAdapterFixture>>>,
    pub(super) import_delay: Arc<Mutex<Option<Duration>>>,
    pub(super) fallback: Arc<dyn TrackSources>,
}

impl TrackSources for FixtureTrackSources {
    fn engine_tracks<'a>(
        &'a self,
        library_path: &'a str,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<(String, Vec<EngineDjTrack>), String>> + Send + 'a>,
    > {
        let fixture = self.fixture.lock().unwrap().clone();
        let delay = *self.import_delay.lock().unwrap();
        if fixture.is_none() {
            return self.fallback.engine_tracks(library_path);
        }
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let fixture =
                fixture.ok_or_else(|| "Engine DJ source fixture is not installed".to_string())?;
            let library: EngineDjLibraryWire = serde_json::from_value(fixture.library)
                .map_err(|error| format!("invalid Engine DJ library fixture: {error}"))?;
            let tracks = serde_json::from_value(fixture.tracks)
                .map_err(|error| format!("invalid Engine DJ track fixture: {error}"))?;
            Ok((library.database_uuid, tracks))
        })
    }

    fn rekordbox_tracks(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<RekordboxTrack>, String>> + Send + '_>>
    {
        let fixture = self.fixture.lock().unwrap().clone();
        let delay = *self.import_delay.lock().unwrap();
        if fixture.is_none() {
            return self.fallback.rekordbox_tracks();
        }
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let fixture =
                fixture.ok_or_else(|| "Rekordbox source fixture is not installed".to_string())?;
            serde_json::from_value(fixture.tracks)
                .map_err(|error| format!("invalid Rekordbox track fixture: {error}"))
        })
    }
}

/// A fixture's Engine DJ library: [`luma_lib::dispatch::EngineDjLibraryInfo`]
/// without the path, which a fixture has no reason to name.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineDjLibraryWire {
    database_uuid: String,
    track_count: i64,
}

/// A fixture's Rekordbox row: [`RekordboxTrack`] without the content id,
/// which the browser never reads.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RekordboxTrackWire {
    uuid: String,
    file_path: Option<String>,
    #[serde(default)]
    artwork_path: Option<String>,
    filename: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    bpm: Option<f64>,
    duration_seconds: Option<f64>,
}

impl From<RekordboxTrackWire> for SourceTrack {
    fn from(row: RekordboxTrackWire) -> Self {
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

pub(super) fn normalize_source_library(
    source: &TrackSource,
    value: Value,
) -> Result<SourceLibrary, LibraryError> {
    match source {
        TrackSource::EngineDj { .. } => {
            let opened: EngineDjLibraryWire =
                decode_source_fixture("engine_dj_open_library", value)?;
            Ok(SourceLibrary {
                identity: Some(opened.database_uuid),
                track_count: opened.track_count.try_into().unwrap_or(0),
            })
        }
        TrackSource::Rekordbox => {
            let opened: RekordboxLibraryInfo =
                decode_source_fixture("rekordbox_open_library", value)?;
            Ok(SourceLibrary {
                identity: None,
                track_count: opened.track_count,
            })
        }
    }
}

pub(super) fn normalize_source_playlists(
    source: &TrackSource,
    value: Value,
) -> Result<Vec<SourcePlaylist>, LibraryError> {
    match source {
        TrackSource::EngineDj { .. } => {
            let rows: Vec<EngineDjPlaylist> =
                decode_source_fixture("engine_dj_list_playlists", value)?;
            Ok(rows.into_iter().map(SourcePlaylist::from).collect())
        }
        TrackSource::Rekordbox => {
            let rows: Vec<RekordboxPlaylist> =
                decode_source_fixture("rekordbox_list_playlists", value)?;
            Ok(rows.into_iter().map(SourcePlaylist::from).collect())
        }
    }
}

pub(super) fn normalize_source_tracks(
    source: &TrackSource,
    command: &'static str,
    value: Value,
) -> Result<Vec<SourceTrack>, LibraryError> {
    match source {
        TrackSource::EngineDj { library_path } => {
            let rows: Vec<EngineDjTrack> = decode_source_fixture(command, value)?;
            Ok(rows
                .into_iter()
                .map(|row| engine_source_track(library_path, row))
                .collect())
        }
        TrackSource::Rekordbox => {
            let rows: Vec<RekordboxTrackWire> = decode_source_fixture(command, value)?;
            Ok(rows.into_iter().map(SourceTrack::from).collect())
        }
    }
}

fn decode_source_fixture<T: DeserializeOwned>(
    command: &'static str,
    value: Value,
) -> Result<T, LibraryError> {
    serde_json::from_value(value)
        .map_err(|error| LibraryError::at(command, Cause::Shape(error.to_string())))
}

impl Library {
    /// Replace DJ source I/O with raw adapter answers in the agent harness.
    /// Normalization remains production code; only external database reads
    /// are substituted.
    pub fn set_source_adapter_fixture(&mut self, fixture: SourceAdapterFixture) {
        *self.source_fixture.lock().unwrap() = Some(fixture);
    }

    /// Keep a fixture read pending long enough for an outside-in loading-state
    /// assertion. Production adapters and their timing are untouched.
    pub fn set_source_adapter_fixture_delay(&mut self, delay: Duration) {
        self.source_fixture_delay = Some(delay);
    }

    pub fn set_source_search_fixture_responses(
        &mut self,
        responses: Vec<SourceSearchFixtureResponse>,
    ) {
        *self.source_search_fixture.lock().unwrap() = responses.into();
    }

    pub fn set_source_import_fixture_delay(&mut self, delay: Duration) {
        *self.source_import_fixture_delay.lock().unwrap() = Some(delay);
    }

    pub fn set_navigation_fixture(&mut self, fixture: NavigationFixture) {
        *self.navigation_fixture.lock().unwrap() = fixture;
    }

    pub(super) fn source_adapter_fixture(&self) -> Option<SourceAdapterFixture> {
        self.source_fixture.lock().unwrap().clone()
    }

    /// Supply live sync snapshots to the harness without enabling networking.
    pub fn set_sync_status_fixture(
        &mut self,
        status: Arc<Mutex<luma_lib::models::sync::SyncStatus>>,
    ) {
        self.sync_status_fixture = Some(status);
    }

    /// [`NavigationFixture::outside_score_edit`], once: write it, announce it
    /// as a download, and resolve when the editor has begun re-reading.
    pub(super) fn outside_score_edit(
        &self,
        score_id: &str,
    ) -> Option<impl Future<Output = Result<(), LibraryError>> + Send + use<>> {
        let (edit, reread) = {
            let mut fixture = self.navigation_fixture.lock().unwrap();
            (
                fixture.outside_score_edit.take()?,
                fixture.score_reads.clone(),
            )
        };
        let services = self.services.clone();
        let replica_changed = self.replica_changed.clone();
        let score_id = score_id.to_owned();
        Some(async move {
            handlers::scores::edit_score_document(&services, score_id, edit)
                .await
                .map_err(refused("edit_score_document"))?;
            let read = reread.notified();
            tokio::pin!(read);
            read.as_mut().enable();
            let _ = replica_changed.send(vec!["clips".into()]);
            read.await;
            Ok(())
        })
    }
}
