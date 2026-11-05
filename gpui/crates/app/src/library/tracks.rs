//! The library's tracks: browser rows, importing, and what analysis wrote
//! down about each one (waveforms, beats and their validation).

use std::future::Future;
use std::path::PathBuf;
#[cfg(not(feature = "agent"))]
use std::time::Duration;

use luma_lib::dispatch::handlers;
use luma_lib::models::node_graph::BeatGrid;
use luma_lib::models::tracks::{TrackBrowserRow, TrackImportProgress, TrackImportResult};
use luma_lib::models::waveforms::{TrackWaveform, WaveformSignal};

use super::{Cause, Library, LibraryError, TrackSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackImportRequest {
    Files(Vec<PathBuf>),
    Source {
        source: TrackSource,
        track_ids: Vec<String>,
    },
}

impl Library {
    /// The track browser's rows for `venue_id`, carrying that venue's
    /// annotation counts.
    pub fn tracks(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<TrackBrowserRow>, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        let (delay, first_title) = {
            let mut fixture = self.navigation_fixture.lock().unwrap();
            if let Some(index) = fixture
                .track_responses
                .iter()
                .position(|response| response.0 == venue_id)
            {
                let (_, delay, first_title) = fixture.track_responses.remove(index);
                (delay, first_title)
            } else {
                (
                    fixture
                        .track_delays
                        .get(venue_id)
                        .copied()
                        .unwrap_or_default(),
                    None,
                )
            }
        };
        #[cfg(not(feature = "agent"))]
        let (delay, first_title): (Duration, Option<String>) = (Duration::ZERO, None);
        let venue_id = Some(venue_id.to_owned());
        let pending = self.call_after("list_tracks_enriched", delay, |services| async move {
            handlers::tracks::list_tracks_enriched(&services, venue_id).await
        });
        async move {
            let mut rows: Vec<TrackBrowserRow> = pending.await?;
            if let (Some(title), Some(first)) = (first_title, rows.first_mut()) {
                first.title = Some(title);
            }
            Ok(rows)
        }
    }

    /// Every visible track in Luma, newest first, without a venue decoration.
    pub fn all_tracks(
        &self,
    ) -> impl Future<Output = Result<Vec<TrackBrowserRow>, LibraryError>> + use<> {
        self.call("list_tracks_enriched", |services| async move {
            handlers::tracks::list_tracks_enriched(&services, None).await
        })
    }

    /// Import from disk or either DJ source through one typed request/result
    /// contract. The returned rows are already durable; analysis continues on
    /// the service-owned task group after this future and its dialog are gone.
    pub fn import_tracks(
        &self,
        request: TrackImportRequest,
    ) -> impl Future<Output = Result<TrackImportResult, LibraryError>> + use<> {
        let files = match &request {
            TrackImportRequest::Files(paths) => {
                let file_paths = paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                Some(self.call("import_tracks", |services| async move {
                    handlers::tracks::import_tracks(&services, file_paths).await
                }))
            }
            _ => None,
        };
        let mut request_error = None;
        let engine = match &request {
            TrackImportRequest::Source {
                source: TrackSource::EngineDj { library_path },
                track_ids,
            } => {
                match track_ids
                    .iter()
                    .map(|id| id.parse::<i64>())
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(track_ids) => {
                        let library_path = library_path.clone();
                        Some(self.call("engine_dj_import_tracks", |services| async move {
                            handlers::engine_dj::engine_dj_import_tracks(
                                &services,
                                library_path,
                                track_ids,
                            )
                            .await
                        }))
                    }
                    Err(error) => {
                        request_error = Some(LibraryError::at(
                            "engine_dj_import_tracks",
                            Cause::Shape(format!("Engine DJ track id is not an integer: {error}")),
                        ));
                        None
                    }
                }
            }
            _ => None,
        };
        let rekordbox = match &request {
            TrackImportRequest::Source {
                source: TrackSource::Rekordbox,
                track_ids,
            } => {
                let track_uuids = track_ids.clone();
                Some(self.call("rekordbox_import_tracks", |services| async move {
                    handlers::rekordbox::rekordbox_import_tracks(&services, track_uuids).await
                }))
            }
            _ => None,
        };
        async move {
            if let Some(error) = request_error {
                return Err(error);
            }
            if let Some(files) = files {
                return files.await;
            }
            if let Some(engine) = engine {
                return engine.await;
            }
            if let Some(rekordbox) = rekordbox {
                return rekordbox.await;
            }
            unreachable!("every import request has one dispatch path")
        }
    }

    /// Subscribe to host-neutral import progress. Lag is explicit rather than
    /// silently replaying stale state; callers reconcile from `all_tracks`.
    pub fn import_progress(&self) -> tokio::sync::broadcast::Receiver<TrackImportProgress> {
        self.import_progress.subscribe()
    }

    /// The rendered envelope of a track, at both resolutions. Large — a full
    /// track is tens of thousands of floats — so a screen reads it once.
    pub fn track_waveform(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<TrackWaveform, LibraryError>> + use<> {
        let track_id = track_id.to_owned();
        self.call("get_track_waveform", |services| async move {
            handlers::waveforms::get_track_waveform(&services, track_id).await
        })
    }

    /// Full filtered bands, read once. Not a JSON command at all: tens of
    /// millions of samples have no business being encoded.
    pub fn track_waveform_signal(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<WaveformSignal, LibraryError>> + use<> {
        let track_id = track_id.to_owned();
        self.call("get_track_waveform_signal", |services| async move {
            handlers::waveforms::get_track_waveform_signal(&services, track_id).await
        })
    }

    /// A track's analyzed beats, or `None` when it has not been analyzed.
    pub fn track_beats(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<Option<BeatGrid>, LibraryError>> + use<> {
        let track_id = track_id.to_owned();
        self.call("get_track_beats", |services| async move {
            handlers::tracks::get_track_beats(&services, track_id).await
        })
    }

    pub fn track_beat_validation(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<Option<luma_lib::models::tracks::BeatValidation>, LibraryError>>
           + use<> {
        let track_id = track_id.to_owned();
        self.call("get_track_beat_validation", |services| async move {
            handlers::tracks::get_track_beat_validation(&services, track_id).await
        })
    }

    pub fn set_track_beat_validation(
        &self,
        track_id: &str,
        grid: &BeatGrid,
        verdict: luma_lib::models::tracks::BeatValidationVerdict,
        reason: Option<luma_lib::models::tracks::BeatValidationReason>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (track_id, grid) = (track_id.to_owned(), grid.clone());
        self.call("set_track_beat_validation", move |services| async move {
            handlers::tracks::set_track_beat_validation(&services, track_id, grid, verdict, reason)
                .await
        })
    }
}
